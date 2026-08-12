//! The turso-backed [`BoardStore`] implementation.
//!
//! One turso database file per board under `$LAN_DATA_DIR/dbs/{board_id}/`
//! (default `/data`, the container volume), plus a `meta.toml` mirroring the
//! board's config. Board ids are globally unique, so paths need no tenant
//! segment — the turso file never knows its tenant.
//!
//! Connections are cached per board and serialized through a per-board
//! mutex. The engine's `experimental_strict` + passive-checkpoint toggles
//! are enabled at open. Records, keys and hooks are plain SQL tables — a
//! real DELETE here, unlike Helix's append-only graph.
//!
//! Secrets: API-key secrets are hashed `sha256(board_id ‖ bucket ‖ secret)`;
//! the owner key is hashed `sha256(board_id ‖ owner_key)`. Board ids are
//! random, so they act as the salt — nothing secret is ever stored.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use turso::params::Params;
use turso::Value as SqlValue;

use serverless_core::{
    BoardStore, KeyRole, ServerlessError, SrvApp, SrvApiKey, SrvHook, SrvRecord, app_meta_path,
    sanitize_key,
};

/// Data dir for board files: `LAN_DATA_DIR` (default `/data`).
fn data_dir() -> PathBuf {
    std::env::var("LAN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/data"))
}

/// Page-count quota per board (4 KiB pages): `LAN_BOARD_MAX_PAGES`
/// (default 262_144 → 1 GiB).
fn max_pages() -> i64 {
    std::env::var("LAN_BOARD_MAX_PAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(262_144)
}

/// A cached turso connection plus its serialization lock.
struct BoardConn {
    conn: turso::Connection,
    lock: Mutex<()>,
}

/// One realtime event pushed to every `/events` subscriber of a board.
#[derive(Clone)]
pub struct SrvEvent {
    pub board_id: String,
    pub kind: String,
    pub seq: i64,
    pub payload: String,
}

/// Cheap-to-clone board store; all state lives in the connection cache.
#[derive(Clone, Default)]
pub struct TursoStore {
    conns: Arc<Mutex<HashMap<String, Arc<BoardConn>>>>,
    /// Per-board event fan-out for the SSE `/events` stream.
    events: Arc<std::sync::Mutex<HashMap<String, tokio::sync::broadcast::Sender<String>>>>,
    /// Sliding-window rate state: `{board}:{bucket}` → timestamps (ms).
    rates: Arc<Mutex<HashMap<String, Vec<i64>>>>,
}

impl TursoStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh owner key: 16 random bytes as lowercase hex.
    pub fn new_owner_key() -> String {
        Self::random_hex(16)
    }

    /// Spawn the background workers once per process: webhook delivery
    /// (every 2s), cron scheduler (every 15s) and the TTL sweeper (30s).
    fn ensure_workers(&self) {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let store = self.clone();
            tokio::spawn(async move { crate::workers::hook_worker(store.clone()).await });
            let store = self.clone();
            tokio::spawn(async move { crate::workers::scheduler_worker(store.clone()).await });
            let store = self.clone();
            tokio::spawn(async move { crate::workers::ttl_worker(store).await });
        });
    }

    /// List every board id with a database on disk (worker scan source).
    pub async fn board_ids(&self) -> Vec<String> {
        let dir = data_dir().join("dbs");
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if entry.path().join("board.db").exists() {
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        out
    }

    /// Fan an event out to the board's SSE subscribers (best effort).
    fn publish_event(&self, board_id: &str, kind: &str, seq: i64, payload: &str) {
        let body = serde_json::json!({ "boardId": board_id, "event": kind, "seq": seq, "payload": serde_json::from_str::<Json>(payload).unwrap_or(Json::Null) });
        let body = body.to_string();
        if let Some(tx) = self.events.lock().unwrap().get(board_id).cloned() {
            let _ = tx.send(body);
        }
    }

    /// Persist an event for SSE catch-up (pruned to the newest 500).
    pub async fn event_append(&self, board_id: &str, kind: &str, seq: i64, payload: &str) {
        let Ok(conns) = self.board_conn(board_id).await else { return };
        let _guard = conns.lock.lock().await;
        let _ = conns
            .conn
            .execute(
                "INSERT INTO srv_events (kind, seq, payload, created_at) VALUES (?, ?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(kind.to_string()),
                    SqlValue::Integer(seq),
                    SqlValue::Text(payload.to_string()),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await;
        let _ = conns
            .conn
            .execute(
                "DELETE FROM srv_events WHERE id <= (SELECT MAX(id) - 500 FROM srv_events)",
                Params::None,
            )
            .await;
    }

    /// Events newer than `after_id` (0 = all kept), optionally filtered by
    /// kind; used to resume an SSE stream from `after=`/`Last-Event-ID`.
    pub async fn event_replay(
        &self,
        board_id: &str,
        after_id: i64,
        filter: Option<&str>,
        limit: i64,
    ) -> Result<Vec<(i64, String, i64, String)>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut params: Vec<SqlValue> = Vec::new();
        let mut where_clause = String::from("id > ?1");
        params.push(SqlValue::Integer(after_id));
        if let Some(kind) = filter {
            where_clause.push_str(" AND kind = ?");
            params.push(SqlValue::Text(kind.to_string()));
        }
        let sql = format!(
            "SELECT id, kind, seq, payload FROM srv_events WHERE {where_clause} ORDER BY id LIMIT ?{lim}",
            lim = params.len() + 1
        );
        params.push(SqlValue::Integer(limit));
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(params))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push((
                row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                row.get_value(2).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                row.get_value(3).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            ));
        }
        Ok(out)
    }

    /// Subscribe to a board's event stream (creates the fan-out on demand).
    pub async fn subscribe(&self, board_id: &str) -> tokio::sync::broadcast::Receiver<String> {
        let mut events = self.events.lock().unwrap();
        if let Some(tx) = events.get(board_id) {
            return tx.subscribe();
        }
        let (tx, rx) = tokio::sync::broadcast::channel(256);
        events.insert(board_id.to_string(), tx);
        rx
    }

    fn board_dir(board_id: &str) -> PathBuf {
        data_dir().join("dbs").join(sanitize_key(board_id))
    }

    /// Open (creating + migrating if needed) and cache the board's database.
    async fn board_conn(&self, board_id: &str) -> Result<Arc<BoardConn>, ServerlessError> {
        if let Some(cached) = self.conns.lock().await.get(board_id).cloned() {
            return Ok(cached);
        }
        self.ensure_workers();
        let dir = Self::board_dir(board_id);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("board.db");
        let db = turso::Builder::new_local(&db_path.to_string_lossy())
            .experimental_strict(true)
            .experimental_mvcc_passive_checkpoint(true)
            .build()
            .await
            .map_err(|e| ServerlessError::Db(format!("open {db_path:?}: {e}")))?;
        let conn = db
            .connect()
            .map_err(|e| ServerlessError::Db(format!("connect {db_path:?}: {e}")))?;
        conn.busy_timeout(std::time::Duration::from_secs(8))
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        conn.pragma_update("max_page_count", max_pages())
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS srv_records (\n\
             seq INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             payload TEXT NOT NULL,\n\
             created_at TEXT NOT NULL,\n\
             writer TEXT\n\
             );\n\
             CREATE INDEX IF NOT EXISTS idx_srv_records_created ON srv_records(created_at DESC);\n\
             CREATE TABLE IF NOT EXISTS srv_keys (\n\
             bucket TEXT PRIMARY KEY,\n\
             role TEXT NOT NULL,\n\
             writer TEXT,\n\
             secret_hash TEXT NOT NULL,\n\
             created_at TEXT NOT NULL,\n\
             revoked_at TEXT\n\
             );\n\
             CREATE TABLE IF NOT EXISTS srv_hooks (\n\
             url TEXT PRIMARY KEY,\n\
             secret TEXT,\n\
             created_at TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS srv_jobs (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             name TEXT NOT NULL,\n\
             schedule TEXT NOT NULL,\n\
             action TEXT NOT NULL,\n\
             next_run_at INTEGER NOT NULL,\n\
             last_run_at TEXT,\n\
             last_status TEXT,\n\
             last_message TEXT,\n\
             created_at TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS srv_hook_deliveries (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             url TEXT NOT NULL,\n\
             event TEXT NOT NULL,\n\
             seq INTEGER,\n\
             payload TEXT NOT NULL,\n\
             attempt INTEGER NOT NULL DEFAULT 0,\n\
             next_attempt_at INTEGER NOT NULL,\n\
             status TEXT NOT NULL DEFAULT 'pending',\n\
             last_message TEXT,\n\
             created_at TEXT NOT NULL\n\
             );\n\
             CREATE INDEX IF NOT EXISTS idx_srv_deliveries_due ON srv_hook_deliveries(status, next_attempt_at);\n\
             CREATE TABLE IF NOT EXISTS srv_audit (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             action TEXT NOT NULL,\n\
             seq INTEGER,\n\
             payload_hash TEXT,\n\
             created_at TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS srv_links (\n\
             from_seq INTEGER NOT NULL,\n\
             parent_seq INTEGER NOT NULL,\n\
             via TEXT NOT NULL,\n\
             PRIMARY KEY (from_seq, via)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS srv_events (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             kind TEXT NOT NULL,\n\
             seq INTEGER NOT NULL,\n\
             payload TEXT NOT NULL,\n\
             created_at TEXT NOT NULL\n\
             );",
        )
        .await
        .map_err(|e| ServerlessError::Db(format!("migrate {db_path:?}: {e}")))?;

        let conns = Arc::new(BoardConn { conn, lock: Mutex::new(()) });
        self.conns.lock().await.insert(board_id.to_string(), conns.clone());
        Ok(conns)
    }

    /// Drop the cached connection and delete the board's files forever.
    async fn drop_board_files(&self, board_id: &str) {
        self.conns.lock().await.remove(board_id);
        let dir = Self::board_dir(board_id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- meta.toml ------------------------------------------------------

    fn meta_path(board_id: &str) -> PathBuf {
        Self::board_dir(board_id).join(app_meta_path())
    }

    fn read_meta(board_id: &str) -> Result<HashMap<String, String>, ServerlessError> {
        let raw = std::fs::read_to_string(Self::meta_path(board_id))?;
        let mut out = HashMap::new();
        for line in raw.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            out.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
        Ok(out)
    }

    /// Merge key/value pairs into the board's meta.toml (read-modify-write).
    fn write_meta_kv(board_id: &str, pairs: &[(&str, &str)]) -> Result<(), ServerlessError> {
        let mut meta = Self::read_meta(board_id).unwrap_or_default();
        for (k, v) in pairs {
            meta.insert(k.to_string(), v.to_string());
        }
        let mut body = String::new();
        for (k, v) in meta {
            body.push_str(&format!("{k} = {v:?}\n"));
        }
        let dir = Self::board_dir(board_id);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(Self::meta_path(board_id), body)?;
        Ok(())
    }

    fn write_meta(
        board_id: &str,
        title: &str,
        schema_json: Option<&str>,
        public_reads: bool,
        unique_key: Option<&str>,
        created_at: &str,
    ) -> Result<(), ServerlessError> {
        let dir = Self::board_dir(board_id);
        std::fs::create_dir_all(&dir)?;
        let mut body = String::new();
        body.push_str(&format!("title = {:?}\n", title));
        body.push_str(&format!("schema_json = {:?}\n", schema_json.unwrap_or("")));
        body.push_str(&format!("public_reads = {public_reads}\n"));
        body.push_str(&format!("unique_key = {:?}\n", unique_key.unwrap_or("")));
        body.push_str(&format!("created_at = {:?}\n", created_at));
        std::fs::write(Self::meta_path(board_id), body)?;
        Ok(())
    }

    // ---- helpers --------------------------------------------------------

    fn now_millis() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
            .to_string()
    }

    fn now_millis_i64() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()) as i64
    }

    /// `n` random bytes as lowercase hex (thread_rng: ChaCha12 seeded from
    /// the OS — fine for LAN-scoped secrets).
    fn random_hex(bytes: usize) -> String {
        let mut out = String::with_capacity(bytes * 2);
        for _ in 0..bytes {
            out.push_str(&format!("{:02x}", rand::random::<u8>()));
        }
        out
    }

    fn sha256_hex(parts: &[&str]) -> String {
        let mut hasher = Sha256::new();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                hasher.update(b":");
            }
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        let mut out = String::with_capacity(64);
        for b in digest {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    fn hash_secret(board_id: &str, bucket: &str, secret: &str) -> String {
        Self::sha256_hex(&[board_id, bucket, secret])
    }

    fn map_db(err: impl std::fmt::Display) -> ServerlessError {
        ServerlessError::Db(err.to_string())
    }

    fn row_to_record(row: &turso::Row) -> Result<SrvRecord, String> {
        let seq = row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0);
        let payload = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
        let created_at = row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
        let writer = row.get_value(3).ok().and_then(|v| v.as_text().cloned()).filter(|s| !s.is_empty());
        let payload = serde_json::from_str(&payload).unwrap_or(Json::Null);
        Ok(SrvRecord { seq, payload, created_at, writer, score: None, snippet: None })
    }

    fn key_from_row(row: &turso::Row) -> SrvApiKey {
        let bucket = row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
        let role = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
        let writer = row.get_value(2).ok().and_then(|v| v.as_text().cloned()).filter(|s| !s.is_empty());
        let created_at = row.get_value(3).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
        let revoked_at = row.get_value(4).ok().and_then(|v| v.as_text().cloned());
        let role = serde_json::from_str::<KeyRole>(&format!("\"{role}\"")).unwrap_or(KeyRole::Reader);
        SrvApiKey { bucket, role, writer, created_at, revoked_at }
    }

    /// Read hook rows (caller holds the board lock).
    async fn hooks_locked(conn: &turso::Connection) -> Result<Vec<SrvHook>, ServerlessError> {
        let mut stmt = conn
            .prepare("SELECT url, secret, created_at FROM srv_hooks")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            let url = row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            let secret = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).filter(|s| !s.is_empty());
            let created_at = row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            out.push(SrvHook { url, secret, created_at });
        }
        Ok(out)
    }

    /// Queue webhook deliveries into `srv_hook_deliveries`; the background
    /// worker performs the POST with retry backoff. `payload` is the full
    /// `{boardId,event,seq,payload}` body sent to the hook.
    async fn enqueue_hooks(
        conn: &turso::Connection,
        board_id: &str,
        event: &str,
        seq: i64,
        payload: &Json,
        hooks: Vec<SrvHook>,
    ) -> Result<(), ServerlessError> {
        if hooks.is_empty() {
            return Ok(());
        }
        let body = serde_json::json!({
            "boardId": board_id,
            "event": event,
            "seq": seq,
            "payload": payload,
        });
        let body = serde_json::to_string(&body).unwrap_or_default();
        let now = Self::now_millis_i64();
        for hook in hooks {
            conn.execute(
                "INSERT INTO srv_hook_deliveries (url, event, seq, payload, next_attempt_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                Params::Positional(vec![
                    SqlValue::Text(hook.url),
                    SqlValue::Text(event.to_string()),
                    SqlValue::Integer(seq),
                    SqlValue::Text(body.clone()),
                    SqlValue::Integer(now),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl BoardStore for TursoStore {
    // ---- boards ---------------------------------------------------------

    async fn board_open(
        &self,
        tenant_id: i64,
        board_id: &str,
        _owner_key: &str,
        title: &str,
        schema_json: Option<&str>,
        public_reads: bool,
        unique_key: Option<&str>,
    ) -> serverless_core::Result<SrvApp> {
        let _ = tenant_id; // tenant lives only in the Helix catalog row.
        let created_at = Self::now_millis();
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        Self::write_meta(board_id, title, schema_json, public_reads, unique_key, &created_at)?;

        let mut stmt = conns
            .conn
            .prepare("SELECT count(*) FROM srv_records")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let count = stmt
            .query_row(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?
            .get_value(0).ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(0);

        Ok(SrvApp {
            board_id: board_id.to_string(),
            owner_key: String::new(), // never persisted; shown once by the caller
            title: title.to_string(),
            schema_json: schema_json.map(str::to_string),
            public_reads,
            unique_key: unique_key.map(str::to_string),
            record_count: count,
            created_at,
        })
    }

    async fn board_get(&self, tenant_id: i64, board_id: &str) -> serverless_core::Result<Option<SrvApp>> {
        let _ = tenant_id;
        if !Self::board_dir(board_id).join("board.db").exists() {
            return Ok(None);
        }
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let meta = Self::read_meta(board_id).ok().unwrap_or_default();
        let mut stmt = conns
            .conn
            .prepare("SELECT count(*) FROM srv_records")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let count = stmt
            .query_row(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?
            .get_value(0).ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(0);
        drop(stmt);

        Ok(Some(SrvApp {
            board_id: board_id.to_string(),
            owner_key: String::new(),
            title: meta.get("title").cloned().unwrap_or_default(),
            schema_json: meta.get("schema_json").filter(|s| !s.is_empty()).cloned(),
            public_reads: meta.get("public_reads").is_some_and(|v| v == "true"),
            unique_key: meta.get("unique_key").filter(|s| !s.is_empty()).cloned(),
            record_count: count,
            created_at: meta.get("created_at").cloned().unwrap_or_default(),
        }))
    }

    async fn board_update(
        &self,
        tenant_id: i64,
        board_id: &str,
        title: Option<&str>,
        schema_json: Option<Option<&str>>,
        public_reads: Option<bool>,
        unique_key: Option<Option<&str>>,
    ) -> serverless_core::Result<SrvApp> {
        let _ = tenant_id;
        let Some(current) = self.board_get(0, board_id).await? else {
            return Err(ServerlessError::NotFound(board_id.to_string()));
        };
        let new_title = title.map(str::to_string).unwrap_or(current.title.clone());
        let new_schema = match schema_json {
            Some(inner) => inner.map(str::to_string),
            None => current.schema_json.clone(),
        };
        let new_public = public_reads.unwrap_or(current.public_reads);
        let new_ukey = match unique_key {
            Some(inner) => inner.map(str::to_string),
            None => current.unique_key.clone(),
        };
        Self::write_meta(
            board_id,
            &new_title,
            new_schema.as_deref(),
            new_public,
            new_ukey.as_deref(),
            &current.created_at,
        )?;
        Ok(SrvApp {
            board_id: board_id.to_string(),
            owner_key: String::new(),
            title: new_title,
            schema_json: new_schema,
            public_reads: new_public,
            unique_key: new_ukey,
            record_count: 0,
            created_at: current.created_at,
        })
    }

    async fn board_delete(&self, tenant_id: i64, board_id: &str) -> serverless_core::Result<()> {
        // The caller (route) tombstones the Helix node first.
        let _ = tenant_id;
        self.drop_board_files(board_id).await;
        Ok(())
    }

    // ---- records --------------------------------------------------------

    async fn record_insert(
        &self,
        tenant_id: i64,
        board_id: &str,
        payload: &Json,
        writer: Option<&str>,
        upsert: bool,
    ) -> serverless_core::Result<i64> {
        let _ = tenant_id;
        if !payload.is_object() {
            return Err(ServerlessError::BadPayload("payload must be a JSON object".to_string()));
        }
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let payload_text = serde_json::to_string(payload)
            .map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        let writer = writer.unwrap_or("").to_string();

        let mut seq = 0;
        if upsert {
            let ukey = Self::read_meta(board_id).ok().and_then(|m| m.get("unique_key").cloned()).filter(|s| !s.is_empty());
            if let Some(key) = ukey {
                let value = payload.get(&key).and_then(Json::as_str);
                let Some(value) = value else {
                    return Err(ServerlessError::BadPayload(format!(
                        "upsert: payload has no string property `{key}`"
                    )));
                };
                let mut existing = conns
                    .conn
                    .prepare("SELECT seq FROM srv_records WHERE json_extract(payload, ?) = ? LIMIT 1")
                    .await
                    .map_err(|e| ServerlessError::Db(e.to_string()))?;
                let mut rows = existing
                    .query(Params::Positional(vec![
                        SqlValue::Text(format!("$.{key}")),
                        SqlValue::Text(value.to_string()),
                    ]))
                    .await
                    .map_err(|e| ServerlessError::Db(e.to_string()))?;
                let found = rows
                    .next()
                    .await
                    .map_err(|e| ServerlessError::Db(e.to_string()))?
                    .and_then(|row| row.get_value(0).ok().and_then(|v| v.as_integer().copied()));
                if let Some(target) = found {
                    conns
                        .conn
                        .execute(
                            "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                            Params::Positional(vec![
                                SqlValue::Text(payload_text.clone()),
                                SqlValue::Text(writer.clone()),
                                SqlValue::Integer(target),
                            ]),
                        )
                        .await
                        .map_err(Self::map_db)?;
                    let _ = Self::audit_append_locked(&conns.conn, board_id, "record.updated", target, Some(&payload_text)).await;
                    let hooks = Self::hooks_locked(&conns.conn).await?;
                    drop(_guard);
                    let _ = Self::enqueue_hooks(&conns.conn, board_id, "record.updated", target, payload, hooks).await;
                    let payload_text_i = serde_json::to_string(payload).unwrap_or_default();
                    let _ = self.event_append(board_id, "record.updated", target, &payload_text_i).await;
                    self.publish_event(board_id, "record.updated", target, &payload_text_i);
                    return Ok(target);
                }
            }
        }

        conns
            .conn
            .execute(
                "INSERT INTO srv_records (payload, created_at, writer) VALUES (?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(payload_text.clone()),
                    SqlValue::Text(Self::now_millis()),
                    SqlValue::Text(writer),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        seq = conns.conn.last_insert_rowid();
        let _ = Self::audit_append_locked(&conns.conn, board_id, "record.created", seq, Some(&payload_text)).await;
        let hooks = Self::hooks_locked(&conns.conn).await?;
        drop(_guard);
        let _ = Self::enqueue_hooks(&conns.conn, board_id, "record.created", seq, payload, hooks).await;
        let payload_text_i = serde_json::to_string(payload).unwrap_or_default();
        let _ = self.event_append(board_id, "record.created", seq, &payload_text_i).await;
        self.publish_event(board_id, "record.created", seq, &payload_text_i);
        Ok(seq)
    }

    async fn record_set(
        &self,
        tenant_id: i64,
        board_id: &str,
        seq: i64,
        payload: &Json,
        writer: Option<&str>,
    ) -> serverless_core::Result<()> {
        let _ = tenant_id;
        if !payload.is_object() {
            return Err(ServerlessError::BadPayload("payload must be a JSON object".to_string()));
        }
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let payload_text = serde_json::to_string(payload)
            .map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        let writer_text = writer.unwrap_or("").to_string();
        let changed = conns
            .conn
            .execute(
                "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                Params::Positional(vec![
                    SqlValue::Text(payload_text.clone()),
                    SqlValue::Text(writer_text.clone()),
                    SqlValue::Integer(seq),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        if changed == 0 {
            conns
                .conn
                .execute(
                    "INSERT INTO srv_records (seq, payload, created_at, writer) VALUES (?, ?, ?, ?)",
                    Params::Positional(vec![
                        SqlValue::Integer(seq),
                        SqlValue::Text(payload_text.clone()),
                        SqlValue::Text(Self::now_millis_i64().to_string()),
                        SqlValue::Text(writer_text),
                    ]),
                )
                .await
                .map_err(Self::map_db)?;
        }
        let action = if changed == 0 { "record.created" } else { "record.updated" };
        let _ = Self::audit_append_locked(&conns.conn, board_id, action, seq, Some(&payload_text)).await;
        let hooks = Self::hooks_locked(&conns.conn).await?;
        drop(_guard);
        let _ = Self::enqueue_hooks(&conns.conn, board_id, action, seq, payload, hooks).await;
        let payload_text_i = serde_json::to_string(payload).unwrap_or_default();
        let _ = self.event_append(board_id, action, seq, &payload_text_i).await;
        self.publish_event(board_id, action, seq, &payload_text_i);
        Ok(())
    }

    async fn record_get(
        &self,
        tenant_id: i64,
        board_id: &str,
        seq: i64,
    ) -> serverless_core::Result<Option<SrvRecord>> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(&format!("SELECT seq, payload, created_at, writer FROM srv_records WHERE seq = ? {}", Self::ttl_hide_sql(board_id)))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(seq)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        match rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            Some(row) => Self::row_to_record(&row).map(Some).map_err(ServerlessError::Db),
            None => Ok(None),
        }
    }

    async fn record_list(
        &self,
        tenant_id: i64,
        board_id: &str,
        limit: i64,
        before: Option<i64>,
        offset: i64,
    ) -> serverless_core::Result<Vec<SrvRecord>> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(
                &format!("SELECT seq, payload, created_at, writer FROM srv_records \
                 WHERE (?1 IS NULL OR seq < ?1) {} ORDER BY seq DESC LIMIT ?2 OFFSET ?3", Self::ttl_hide_sql(board_id)),
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let params = match before {
            Some(b) => Params::Positional(vec![
                SqlValue::Integer(b),
                SqlValue::Integer(limit),
                SqlValue::Integer(offset),
            ]),
            None => Params::Positional(vec![
                SqlValue::Null,
                SqlValue::Integer(limit),
                SqlValue::Integer(offset),
            ]),
        };
        let mut rows = stmt
            .query(params)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(Self::row_to_record(&row).map_err(ServerlessError::Db)?);
        }
        Ok(out)
    }

    async fn record_count(&self, tenant_id: i64, board_id: &str) -> serverless_core::Result<i64> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT count(*) FROM srv_records")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let count = stmt
            .query_row(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?
            .get_value(0).ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(0);
        Ok(count)
    }

    async fn record_delete(
        &self,
        tenant_id: i64,
        board_id: &str,
        seq: i64,
    ) -> serverless_core::Result<bool> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "DELETE FROM srv_records WHERE seq = ?",
                Params::Positional(vec![SqlValue::Integer(seq)]),
            )
            .await
            .map_err(Self::map_db)?;
        if n > 0 {
            let _ = Self::audit_append_locked(&conns.conn, board_id, "record.deleted", seq, None).await;
            let hooks = Self::hooks_locked(&conns.conn).await?;
            drop(_guard);
            let _ = Self::enqueue_hooks(&conns.conn, board_id, "record.deleted", seq, &Json::Null, hooks).await;
            let _ = self.event_append(board_id, "record.deleted", seq, "null").await;
            self.publish_event(board_id, "record.deleted", seq, "null");
        }
        Ok(n > 0)
    }

    // ---- keys -----------------------------------------------------------

    async fn key_issue(
        &self,
        tenant_id: i64,
        board_id: &str,
        role: KeyRole,
        writer: Option<&str>,
    ) -> serverless_core::Result<(String, String)> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let bucket = format!("k-{}", Self::random_hex(8));
        let secret = Self::random_hex(24);
        let hash = Self::hash_secret(board_id, &bucket, &secret);
        conns
            .conn
            .execute(
                "INSERT INTO srv_keys (bucket, role, writer, secret_hash, created_at) VALUES (?, ?, ?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(bucket.clone()),
                    SqlValue::Text(role.as_str().to_string()),
                    SqlValue::Text(writer.unwrap_or("").to_string()),
                    SqlValue::Text(hash),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok((bucket, secret))
    }

    async fn key_list(&self, tenant_id: i64, board_id: &str) -> serverless_core::Result<Vec<SrvApiKey>> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT bucket, role, writer, created_at, revoked_at FROM srv_keys ORDER BY created_at DESC")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(Self::key_from_row(&row));
        }
        Ok(out)
    }

    async fn key_revoke(&self, tenant_id: i64, board_id: &str, bucket: &str) -> serverless_core::Result<bool> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "UPDATE srv_keys SET revoked_at = ? WHERE bucket = ? AND revoked_at IS NULL",
                Params::Positional(vec![
                    SqlValue::Text(Self::now_millis()),
                    SqlValue::Text(bucket.to_string()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(n > 0)
    }

    async fn key_auth(&self, secret: &str) -> serverless_core::Result<Option<(String, SrvApiKey)>> {
        // Token shape: `{board_id}:{bucket}:{secret}`.
        let Some((board_id, rest)) = secret.split_once(':') else {
            return Ok(None);
        };
        let Some((bucket, key_secret)) = rest.split_once(':') else {
            return Ok(None);
        };
        if !Self::board_dir(board_id).join("board.db").exists() {
            return Ok(None);
        }
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(
                "SELECT bucket, role, writer, created_at, revoked_at, secret_hash \
                 FROM srv_keys WHERE bucket = ?",
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Text(bucket.to_string())]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? else {
            return Ok(None);
        };
        let key = Self::key_from_row(&row);
        if key.revoked_at.is_some() {
            return Ok(None);
        }
        let stored = row
            .get_value(5).ok()
            .and_then(|v| v.as_text().cloned())
            .unwrap_or_default();
        if Self::hash_secret(board_id, &key.bucket, key_secret) != stored {
            return Ok(None);
        }
        Ok(Some((board_id.to_string(), key)))
    }

    // ---- hooks ----------------------------------------------------------

    async fn hook_register(
        &self,
        tenant_id: i64,
        board_id: &str,
        url: &str,
        secret: Option<&str>,
    ) -> serverless_core::Result<()> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        conns
            .conn
            .execute(
                "INSERT INTO srv_hooks (url, secret, created_at) VALUES (?, ?, ?) \
                 ON CONFLICT(url) DO UPDATE SET secret = excluded.secret",
                Params::Positional(vec![
                    SqlValue::Text(url.to_string()),
                    SqlValue::Text(secret.unwrap_or("").to_string()),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(())
    }

    async fn hook_remove(&self, tenant_id: i64, board_id: &str, url: &str) -> serverless_core::Result<bool> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "DELETE FROM srv_hooks WHERE url = ?",
                Params::Positional(vec![SqlValue::Text(url.to_string())]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(n > 0)
    }

    async fn hook_list(&self, tenant_id: i64, board_id: &str) -> serverless_core::Result<Vec<SrvHook>> {
        let _ = tenant_id;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT url, secret, created_at FROM srv_hooks ORDER BY created_at DESC")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            let url = row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            let secret = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).filter(|s| !s.is_empty());
            let created_at = row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            out.push(SrvHook { url, secret, created_at });
        }
        Ok(out)
    }
}

// ---- serverless v2 surface (inherent methods, trait unchanged) -----------

/// Sentinel tenant id: boards are tenant-agnostic at the record layer.
pub const NO_TENANT: i64 = 0;

/// A job ready for the scheduler worker.
pub struct JobRow {
    pub id: i64,
    pub name: String,
    pub schedule: String,
    pub action: Json,
}

/// A webhook delivery due for the retry worker.
pub struct DeliveryRow {
    pub id: i64,
    pub url: String,
    pub event: String,
    pub seq: Option<i64>,
    pub payload: String,
    pub attempt: i64,
    pub status: String,
}

impl TursoStore {
    // ---- records: patch / query / filter / search / aggregate / bulk ----

    /// Apply a Mongo-ish patch (`$set/$inc/$dec/$mul/$unset`) to the record
    /// at `seq`; returns the new payload.
    pub async fn record_patch(
        &self,
        board_id: &str,
        seq: i64,
        patch: &Json,
        writer: Option<&str>,
    ) -> Result<Json, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT payload FROM srv_records WHERE seq = ?")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(seq)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? else {
            return Err(ServerlessError::NotFound("record not found".to_string()));
        };
        let current = row
            .get_value(0).ok()
            .and_then(|v| v.as_text().cloned())
            .unwrap_or_default();
        let current: Json = serde_json::from_str(&current)
            .map_err(|e| ServerlessError::Db(format!("stored payload: {e}")))?;
        let mut updated = current;
        crate::dsl::apply_patch(&mut updated, patch)
            .map_err(ServerlessError::BadPayload)?;
        if !updated.is_object() {
            return Err(ServerlessError::BadPayload("patched payload must be a JSON object".to_string()));
        }
        let text = serde_json::to_string(&updated)
            .map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        conns
            .conn
            .execute(
                "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                Params::Positional(vec![
                    SqlValue::Text(text.clone()),
                    SqlValue::Text(writer.unwrap_or("").to_string()),
                    SqlValue::Integer(seq),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        let _ = Self::audit_append_locked(&conns.conn, board_id, "record.updated", seq, Some(&text)).await;
        let hooks = Self::hooks_locked(&conns.conn).await?;
        let _ = Self::enqueue_hooks(&conns.conn, board_id, "record.updated", seq, &updated, hooks).await;
        drop(_guard);
        let _ = self.event_append(board_id, "record.updated", seq, &text).await;
        self.publish_event(board_id, "record.updated", seq, &text);
        Ok(updated)
    }

    /// Query a JSON path across records (`records query <path>`), returning
    /// rows whose extracted value equals `eq` (when given).
    pub async fn record_query(
        &self,
        board_id: &str,
        path: &str,
        eq: Option<&Json>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SrvRecord>, ServerlessError> {
        let sql_path = crate::dsl::sql_json_path(path).unwrap_or_else(|| "$".to_string());
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut params = vec![SqlValue::Text(sql_path.clone())];
        let mut where_clause = String::new();
        if let Some(v) = eq {
            params.push(json_to_sql(v));
            where_clause = format!(" AND json_extract(payload, ?1) = ?{}", params.len());
        }
        let sql = format!(
            "SELECT seq, payload, created_at, writer FROM srv_records \
             WHERE json_extract(payload, ?1) IS NOT NULL {where_clause} {ttl} \
             ORDER BY created_at DESC LIMIT ?{lim} OFFSET ?{off}",
            ttl = Self::ttl_hide_sql(board_id),
            lim = params.len() + 1,
            off = params.len() + 2,
        );
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        params.push(SqlValue::Integer(limit));
        params.push(SqlValue::Integer(offset));
        let mut rows = stmt
            .query(Params::Positional(params))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        Self::rows_to_records(&mut rows).await
    }

    /// Filter + sort records by `conds`/`orders` (JSON-path where clauses).
    pub async fn record_filter(
        &self,
        board_id: &str,
        conds: &[crate::dsl::Cond],
        orders: &[(String, bool)],
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SrvRecord>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let (where_clause, params) = Self::filter_sql(conds);
        let mut order_clause = String::from("ORDER BY created_at DESC");
        if !orders.is_empty() {
            let parts: Vec<String> = orders
                .iter()
                .map(|(f, asc)| {
                    let path = crate::dsl::sql_json_path(f).unwrap_or_else(|| "$".to_string());
                    format!("json_extract(payload, {}) {}", quote_sql_path(&path), if *asc { "ASC" } else { "DESC" })
                })
                .collect();
            order_clause = format!("ORDER BY {}", parts.join(", "));
        }
        let sql = format!(
            "SELECT seq, payload, created_at, writer FROM srv_records {where_clause} {ttl} {order_clause} LIMIT ?{lim} OFFSET ?{off}",
            ttl = Self::ttl_hide_sql(board_id),
            lim = params.len() + 1,
            off = params.len() + 2,
        );
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut all = params;
        all.push(SqlValue::Integer(limit));
        all.push(SqlValue::Integer(offset));
        let mut rows = stmt
            .query(Params::Positional(all))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        Self::rows_to_records(&mut rows).await
    }

    /// Build the `WHERE` clause + params for a filter (empty → all records).
    fn filter_sql(conds: &[crate::dsl::Cond]) -> (String, Vec<SqlValue>) {
        if conds.is_empty() {
            return (String::new(), Vec::new());
        }
        let mut params: Vec<SqlValue> = Vec::new();
        let mut parts = Vec::new();
        for cond in conds {
            let field = crate::dsl::sql_json_path(&cond.path).unwrap_or_else(|| "$".to_string());
            params.push(SqlValue::Text(field.clone()));
            let fidx = params.len();
            let clause = match &cond.op {
                crate::dsl::Op::Eq(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) = lower(?{vidx})")
                }
                crate::dsl::Op::Neq(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) != lower(?{vidx})")
                }
                crate::dsl::Op::Gt(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) > ?{vidx}")
                }
                crate::dsl::Op::Gte(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) >= ?{vidx}")
                }
                crate::dsl::Op::Lt(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) < ?{vidx}")
                }
                crate::dsl::Op::Lte(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) <= ?{vidx}")
                }
                crate::dsl::Op::Contains(s) => {
                    params.push(SqlValue::Text(format!("%{}%", escape_like(s))));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) LIKE ?{vidx} ESCAPE '\\'")
                }
                crate::dsl::Op::NotContains(s) => {
                    params.push(SqlValue::Text(format!("%{}%", escape_like(s))));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) NOT LIKE ?{vidx} ESCAPE '\\'")
                }
                crate::dsl::Op::In(vs) => {
                    let placeholders: Vec<String> = vs
                        .iter()
                        .map(|v| {
                            params.push(json_to_sql(v));
                            format!("?{}", params.len())
                        })
                        .collect();
                    format!("lower(json_extract(payload, ?{fidx})) IN ({})", placeholders.join(","))
                }
            };
            parts.push(clause);
        }
        (format!("WHERE {}", parts.join(" AND ")), params)
    }

    /// Full-text-ish substring search with optional `<mark>` highlight.
    pub async fn record_search(
        &self,
        board_id: &str,
        q: &str,
        hl: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SrvRecord>, ServerlessError> {
        let needle = format!("%{}%", escape_like(q.to_lowercase().as_str()));
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(
                &format!("SELECT seq, payload, created_at, writer FROM srv_records \
                 WHERE lower(payload) LIKE ?1 ESCAPE '\\' {} \
                 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3", Self::ttl_hide_sql(board_id)),
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![
                SqlValue::Text(needle),
                SqlValue::Integer(limit),
                SqlValue::Integer(offset),
            ]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Self::rows_to_records(&mut rows).await?;
        if hl && !q.is_empty() {
            for rec in &mut out {
                rec.payload = highlight(&rec.payload, q);
            }
        }
        Ok(out)
    }

    /// Aggregate over a JSON path: count/sum/avg/min/max, optional group_by.
    pub async fn record_aggregate(
        &self,
        board_id: &str,
        op: crate::dsl::Agg,
        field: &str,
        group_by: Option<&str>,
        conds: &[crate::dsl::Cond],
    ) -> Result<Json, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let (where_clause, params) = Self::filter_sql(conds);
        let field_path = crate::dsl::sql_json_path(field).unwrap_or_else(|| "$".to_string());
        let group_path = group_by.map(|g| crate::dsl::sql_json_path(g).unwrap_or_else(|| "$".to_string()));
        let op_sql = op.as_str().to_uppercase();
        let is_count = op == crate::dsl::Agg::Count;
        let agg_sql = |path: &str| {
            if is_count {
                "COUNT(*)".to_string()
            } else {
                format!("{op_sql}(json_extract(payload, {path}))", path = quote_sql_path(path))
            }
        };
        let (select, group_clause): (String, String) = match group_path {
            Some(g) => (
                format!(
                    "json_extract(payload, {}) AS grp, {} AS val",
                    quote_sql_path(&g),
                    agg_sql(&field_path)
                ),
                "GROUP BY grp".to_string(),
            ),
            None => (agg_sql(&field_path), String::new()),
        };
        let sql = format!(
            "SELECT {select} FROM srv_records {where_clause} {ttl} {group_clause}",
            ttl = Self::ttl_hide_sql(board_id),
        );
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(params))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            match group_by {
                Some(_) => {
                    let key = row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
                    let value = row.get_value(1).ok().map(|v| Self::sql_num(&v)).unwrap_or(0.0);
                    out.push(serde_json::json!({ "key": key, "value": value }));
                }
                None => {
                    let value = row.get_value(0).ok().map(|v| Self::sql_num(&v)).unwrap_or(0.0);
                    out.push(serde_json::json!({ "op": op.as_str(), "value": value }));
                }
            }
        }
        Ok(if group_by.is_some() {
            serde_json::json!({ "grouped_by": group_by, "rows": out })
        } else {
            serde_json::json!({ "field": field, "op": op.as_str(), "value": out.first().and_then(|v| v.get("value")).cloned().unwrap_or(Json::Null) })
        })
    }

    /// Insert many records (each gets its own seq + webhook + SSE event).
    pub async fn record_bulk_insert(
        &self,
        board_id: &str,
        payloads: Vec<Json>,
        writer: Option<&str>,
    ) -> Result<Vec<i64>, ServerlessError> {
        let mut seqs = Vec::with_capacity(payloads.len());
        for payload in payloads {
            seqs.push(self.record_insert(NO_TENANT, board_id, &payload, writer, false).await?);
        }
        Ok(seqs)
    }

    /// Delete every record matching the filter; returns how many were removed.
    pub async fn record_delete_filter(
        &self,
        board_id: &str,
        conds: &[crate::dsl::Cond],
    ) -> Result<i64, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let (where_clause, params) = Self::filter_sql(conds);
        let sql = format!("DELETE FROM srv_records {where_clause}");
        let n = conns
            .conn
            .execute(&sql, Params::Positional(params))
            .await
            .map_err(Self::map_db)?;
        if n > 0 {
            let _ = self.event_append(board_id, "record.deleted", 0, "null").await;
            self.publish_event(board_id, "record.deleted", 0, "null");
        }
        Ok(n as i64)
    }

    // ---- jobs (cron) ----------------------------------------------------

    /// Register a job; returns its id. Fails when the schedule never fires.
    pub async fn job_add(
        &self,
        board_id: &str,
        name: &str,
        schedule: &str,
        action: &Json,
    ) -> Result<i64, ServerlessError> {
        let Some(next) = crate::cron::next_run(schedule, Self::now_millis_i64()) else {
            return Err(ServerlessError::BadPayload(format!("invalid schedule `{schedule}`")));
        };
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        conns
            .conn
            .execute(
                "INSERT INTO srv_jobs (name, schedule, action, next_run_at, created_at) VALUES (?, ?, ?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(name.to_string()),
                    SqlValue::Text(schedule.to_string()),
                    SqlValue::Text(serde_json::to_string(action).map_err(|e| ServerlessError::BadPayload(e.to_string()))?),
                    SqlValue::Integer(next),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(conns.conn.last_insert_rowid())
    }

    pub async fn job_list(&self, board_id: &str) -> Result<Vec<Json>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(
                "SELECT id, name, schedule, action, next_run_at, last_run_at, last_status, last_message, created_at \
                 FROM srv_jobs ORDER BY id",
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            let action: Json = row.get_value(3).ok().and_then(|v| v.as_text().cloned())
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Json::Null);
            out.push(serde_json::json!({
                "id": row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "name": row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "schedule": row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "action": action,
                "next_run_at": row.get_value(4).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "last_run_at": row.get_value(5).ok().and_then(|v| v.as_text().cloned()),
                "last_status": row.get_value(6).ok().and_then(|v| v.as_text().cloned()),
                "last_message": row.get_value(7).ok().and_then(|v| v.as_text().cloned()),
                "created_at": row.get_value(8).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            }));
        }
        Ok(out)
    }

    pub async fn job_remove(&self, board_id: &str, id: i64) -> Result<bool, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "DELETE FROM srv_jobs WHERE id = ?",
                Params::Positional(vec![SqlValue::Integer(id)]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(n > 0)
    }

    pub async fn job_due(&self, board_id: &str, now: i64, limit: i64) -> Result<Vec<JobRow>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT id, name, schedule, action FROM srv_jobs WHERE next_run_at <= ? ORDER BY next_run_at LIMIT ?")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(now), SqlValue::Integer(limit)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(JobRow {
                id: row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                name: row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                schedule: row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                action: row.get_value(3).ok().and_then(|v| v.as_text().cloned())
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Json::Null),
            });
        }
        Ok(out)
    }

    pub async fn job_mark(
        &self,
        board_id: &str,
        id: i64,
        status: &str,
        message: &str,
    ) -> Result<(), ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        conns
            .conn
            .execute(
                "UPDATE srv_jobs SET last_run_at = ?, last_status = ?, last_message = ? WHERE id = ?",
                Params::Positional(vec![
                    SqlValue::Text(Self::now_millis()),
                    SqlValue::Text(status.to_string()),
                    SqlValue::Text(message.to_string()),
                    SqlValue::Integer(id),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(())
    }

    pub async fn job_reschedule(&self, board_id: &str, id: i64, next: i64) -> Result<(), ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        conns
            .conn
            .execute(
                "UPDATE srv_jobs SET next_run_at = ? WHERE id = ?",
                Params::Positional(vec![SqlValue::Integer(next), SqlValue::Integer(id)]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(())
    }

    // ---- webhook deliveries ---------------------------------------------

    pub async fn delivery_due(&self, board_id: &str, now: i64, limit: i64) -> Result<Vec<DeliveryRow>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(
                "SELECT id, url, event, seq, payload, attempt, status FROM srv_hook_deliveries \
                 WHERE status = 'pending' AND next_attempt_at <= ? ORDER BY id LIMIT ?",
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(now), SqlValue::Integer(limit)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(DeliveryRow {
                id: row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                url: row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                event: row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                seq: row.get_value(3).ok().and_then(|v| v.as_integer().copied()),
                payload: row.get_value(4).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                attempt: row.get_value(5).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                status: row.get_value(6).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Record the outcome of a delivery attempt: on failure, back off
    /// `2^attempt` seconds (capped 5 min); the 5th failure marks it dead.
    pub async fn delivery_mark(
        &self,
        board_id: &str,
        id: i64,
        ok: bool,
        message: &str,
    ) -> Result<(), ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT attempt FROM srv_hook_deliveries WHERE id = ?")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(id)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let attempt = rows
            .next()
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?
            .and_then(|r| r.get_value(0).ok().and_then(|v| v.as_integer().copied()))
            .unwrap_or(0);
        if ok {
            conns
                .conn
                .execute(
                    "UPDATE srv_hook_deliveries SET status = 'ok', last_message = ? WHERE id = ?",
                    Params::Positional(vec![SqlValue::Text(message.to_string()), SqlValue::Integer(id)]),
                )
                .await
                .map_err(Self::map_db)?;
            return Ok(());
        }
        let attempt = attempt + 1;
        let (status, delay) = if attempt >= 5 {
            ("dead".to_string(), 0)
        } else {
            ("pending".to_string(), (2u64.pow(attempt as u32) as i64).min(300) * 1000)
        };
        conns
            .conn
            .execute(
                "UPDATE srv_hook_deliveries SET attempt = ?, status = ?, next_attempt_at = ?, last_message = ? WHERE id = ?",
                Params::Positional(vec![
                    SqlValue::Integer(attempt),
                    SqlValue::Text(status),
                    SqlValue::Integer(Self::now_millis_i64() + delay),
                    SqlValue::Text(message.to_string()),
                    SqlValue::Integer(id),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(())
    }

    /// The hook's signing secret (None when the hook is gone or unsigned).
    pub async fn hook_secret(&self, board_id: &str, url: &str) -> Option<String> {
        let conns = self.board_conn(board_id).await.ok()?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT secret FROM srv_hooks WHERE url = ?")
            .await
            .ok()?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Text(url.to_string())]))
            .await
            .ok()?;
        rows.next().await.ok()?
            .and_then(|r| r.get_value(0).ok().and_then(|v| v.as_text().cloned()))
            .filter(|s| !s.is_empty())
    }

    pub async fn delivery_list(&self, board_id: &str, status: Option<&str>, limit: i64) -> Result<Vec<Json>, ServerlessError> {        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let sql = match status {
            Some(_) => "SELECT id, url, event, seq, attempt, status, last_message, created_at FROM srv_hook_deliveries WHERE status = ? ORDER BY id DESC LIMIT ?",
            None => "SELECT id, url, event, seq, attempt, status, last_message, created_at FROM srv_hook_deliveries ORDER BY id DESC LIMIT ?",
        };
        let mut stmt = conns
            .conn
            .prepare(sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut params: Vec<SqlValue> = Vec::new();
        if status.is_some() {
            params.push(SqlValue::Text(status.unwrap().to_string()));
        }
        params.push(SqlValue::Integer(limit));
        let mut rows = stmt
            .query(Params::Positional(params))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(serde_json::json!({
                "id": row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "url": row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "event": row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "seq": row.get_value(3).ok().and_then(|v| v.as_integer().copied()),
                "attempt": row.get_value(4).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "status": row.get_value(5).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "last_message": row.get_value(6).ok().and_then(|v| v.as_text().cloned()),
                "created_at": row.get_value(7).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            }));
        }
        Ok(out)
    }

    /// Re-queue failed/dead deliveries (optionally one id); returns count.
    pub async fn delivery_replay(&self, board_id: &str, id: Option<i64>) -> Result<i64, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let (sql, params) = match id {
            Some(id) => (
                "UPDATE srv_hook_deliveries SET status = 'pending', attempt = 0, next_attempt_at = ?, last_message = NULL \
                 WHERE id = ? AND status != 'ok'".to_string(),
                vec![SqlValue::Integer(Self::now_millis_i64()), SqlValue::Integer(id)],
            ),
            None => (
                "UPDATE srv_hook_deliveries SET status = 'pending', attempt = 0, next_attempt_at = ?, last_message = NULL \
                 WHERE status != 'ok'".to_string(),
                vec![SqlValue::Integer(Self::now_millis_i64())],
            ),
        };
        let n = conns.conn.execute(&sql, Params::Positional(params)).await.map_err(Self::map_db)?;
        Ok(n as i64)
    }

    // ---- audit -----------------------------------------------------------

    /// Append an audit row when the board has audit enabled.
    async fn audit_append_locked(
        conn: &turso::Connection,
        board_id: &str,
        action: &str,
        seq: i64,
        payload: Option<&str>,
    ) -> Result<(), ServerlessError> {
        let enabled = Self::read_meta(board_id)
            .ok()
            .and_then(|m| m.get("audit").cloned())
            .map(|v| v == "true")
            .unwrap_or(false);
        if !enabled {
            return Ok(());
        }
        let hash = payload.map(|p| Self::sha256_hex(&[p]));
        conn.execute(
            "INSERT INTO srv_audit (action, seq, payload_hash, created_at) VALUES (?, ?, ?, ?)",
            Params::Positional(vec![
                SqlValue::Text(action.to_string()),
                SqlValue::Integer(seq),
                SqlValue::Text(hash.unwrap_or_default()),
                SqlValue::Text(Self::now_millis()),
            ]),
        )
        .await
        .map_err(|e| ServerlessError::Db(e.to_string()))?;
        Ok(())
    }

    pub async fn audit_append(&self, board_id: &str, action: &str, seq: i64, payload: Option<&str>) -> Result<(), ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        Self::audit_append_locked(&conns.conn, board_id, action, seq, payload).await
    }

    pub async fn audit_list(&self, board_id: &str, limit: i64) -> Result<Vec<Json>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT id, action, seq, payload_hash, created_at FROM srv_audit ORDER BY id DESC LIMIT ?")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(limit)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(serde_json::json!({
                "id": row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "action": row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "seq": row.get_value(2).ok().and_then(|v| v.as_integer().copied()),
                "payload_hash": row.get_value(3).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "created_at": row.get_value(4).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            }));
        }
        Ok(out)
    }

    // ---- ttl ---------------------------------------------------------------

    fn ttl_config(board_id: &str) -> (Option<i64>, Option<String>) {
        match Self::read_meta(board_id) {
            Ok(m) => (
                m.get("ttl_seconds").and_then(|v| v.parse().ok()),
                m.get("ttl_field").cloned().filter(|f| !f.is_empty()),
            ),
            Err(_) => (None, None),
        }
    }

    pub async fn ttl_set(&self, board_id: &str, seconds: i64, field: &str) -> Result<(), ServerlessError> {
        Self::write_meta_kv(board_id, &[("ttl_seconds", &seconds.to_string()), ("ttl_field", field)])
    }

    pub async fn ttl_clear(&self, board_id: &str) -> Result<(), ServerlessError> {
        Self::write_meta_kv(board_id, &[("ttl_seconds", ""), ("ttl_field", "")])
    }

    /// Is this payload expired under the board's TTL config?
    fn record_expired(payload_text: &str, ttl_seconds: i64, field: &str) -> bool {
        let Ok(payload) = serde_json::from_str::<Json>(payload_text) else { return false };
        let Some(ts) = payload.get(field).and_then(Json::as_f64) else { return false };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        (ts + ttl_seconds as f64) < now
    }

    /// `AND`-fragment hiding records whose TTL field expired (empty when no
    /// TTL configured): `ts + ttl_seconds > now`.
    fn ttl_hide_sql(board_id: &str) -> String {
        match Self::ttl_config(board_id) {
            (Some(secs), Some(field)) if secs > 0 => {
                if let Some(path) = crate::dsl::sql_json_path(&field) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    format!(
                        " AND (json_extract(payload, {p}) IS NULL OR json_extract(payload, {p}) + {ttl} > {now})",
                        p = quote_sql_path(&path),
                        ttl = secs,
                        now = now,
                    )
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }

    /// Delete all expired records; returns how many were removed.
    pub async fn ttl_sweep(&self, board_id: &str) -> Result<i64, ServerlessError> {
        let (Some(secs), Some(field)) = Self::ttl_config(board_id) else { return Ok(0) };
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT seq, payload FROM srv_records")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut expired = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            let seq = row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0);
            let payload = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            if Self::record_expired(&payload, secs, &field) {
                expired.push(seq);
            }
        }
        let count = expired.len() as i64;
        for seq in expired {
            conns
                .conn
                .execute("DELETE FROM srv_records WHERE seq = ?", Params::Positional(vec![SqlValue::Integer(seq)]))
                .await
                .map_err(Self::map_db)?;
        }
        Ok(count)
    }

    // ---- links -------------------------------------------------------------

    pub async fn link_set(&self, board_id: &str, from_seq: i64, parent_seq: i64, via: &str) -> Result<(), ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        conns
            .conn
            .execute(
                "INSERT OR REPLACE INTO srv_links (from_seq, parent_seq, via) VALUES (?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Integer(from_seq),
                    SqlValue::Integer(parent_seq),
                    SqlValue::Text(via.to_string()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(())
    }

    pub async fn link_clear(&self, board_id: &str, from_seq: i64, via: &str) -> Result<bool, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "DELETE FROM srv_links WHERE from_seq = ? AND via = ?",
                Params::Positional(vec![SqlValue::Integer(from_seq), SqlValue::Text(via.to_string())]),
            )
            .await
            .map_err(Self::map_db)?;
        Ok(n > 0)
    }

    /// Resolve `via` links of a record to their parent records.
    pub async fn link_get(&self, board_id: &str, from_seq: i64) -> Result<Vec<Json>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(
                "SELECT l.via, r.seq, r.payload, r.created_at FROM srv_links l \
                 JOIN srv_records r ON r.seq = l.parent_seq WHERE l.from_seq = ? ORDER BY l.via",
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(from_seq)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            let payload: Json = row.get_value(2).ok().and_then(|v| v.as_text().cloned())
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Json::Null);
            out.push(serde_json::json!({
                "via": row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "seq": row.get_value(1).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "payload": payload,
                "created_at": row.get_value(3).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            }));
        }
        Ok(out)
    }

    // ---- rate limit ----------------------------------------------------------

    /// Check + record a request against the board's rate config
    /// (`{"per_minute": N}` in meta `rate`); returns whether it's allowed.
    /// Configure the in-memory rate limit: `per_minute = 0` disables it.
    pub async fn rate_set(&self, board_id: &str, per_minute: i64) -> Result<(), ServerlessError> {
        Self::write_meta_kv(board_id, &[("rate_per_minute", &per_minute.to_string())])
    }

    pub async fn rate_allow(&self, board_id: &str, bucket: &str) -> bool {
        let limit = Self::read_meta(board_id)
            .ok()
            .and_then(|m| m.get("rate_per_minute").cloned())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if limit <= 0 {
            return true;
        }
        let now = Self::now_millis_i64();
        let key = format!("{board_id}:{bucket}");
        let mut rates = self.rates.lock().await;
        let window = rates.entry(key).or_default();
        window.retain(|t| now - t < 60_000);
        if window.len() as i64 >= limit {
            return false;
        }
        window.push(now);
        true
    }

    pub async fn rate_reset(&self, board_id: &str) {
        self.rates.lock().await.retain(|k, _| !k.starts_with(&format!("{board_id}:")));
    }

    // ---- config ---------------------------------------------------------------

    /// The board's full public config (for `GET /config`).
    pub async fn board_config(&self, board_id: &str) -> Result<Json, ServerlessError> {
        let meta = Self::read_meta(board_id).unwrap_or_default();
        let (ttl_seconds, ttl_field) = Self::ttl_config(board_id);
        let rate = meta.get("rate_per_minute").cloned().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        Ok(serde_json::json!({
            "boardId": board_id,
            "title": meta.get("title").cloned().unwrap_or_default(),
            "schema": meta.get("schema").cloned().unwrap_or_default(),
            "public_reads": meta.get("public_reads").map(|v| v == "true").unwrap_or(false),
            "unique_key": meta.get("unique_key").cloned().filter(|s| !s.is_empty()),
            "ttl_seconds": ttl_seconds,
            "ttl_field": ttl_field,
            "rate_per_minute": rate,
            "audit": meta.get("audit").map(|v| v == "true").unwrap_or(false),
        }))
    }

    /// Turn the audit trail on or off for the board.
    pub async fn audit_enable(&self, board_id: &str, enabled: bool) -> Result<(), ServerlessError> {
        Self::write_meta_kv(board_id, &[("audit", if enabled { "true" } else { "false" })])
    }

    // ---- dump / restore ----------------------------------------------------------

    pub async fn record_dump(&self, board_id: &str) -> Result<Json, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut records = Vec::new();
        let mut stmt = conns
            .conn
            .prepare("SELECT seq, payload, created_at, writer FROM srv_records ORDER BY seq")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            let payload: Json = row.get_value(1).ok().and_then(|v| v.as_text().cloned())
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Json::Null);
            records.push(serde_json::json!({
                "seq": row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "payload": payload,
                "created_at": row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "writer": row.get_value(3).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            }));
        }
        let mut hooks = Vec::new();
        let mut stmt = conns
            .conn
            .prepare("SELECT url, secret, created_at FROM srv_hooks")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            hooks.push(serde_json::json!({
                "url": row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
                "secret": row.get_value(1).ok().and_then(|v| v.as_text().cloned()).filter(|s| !s.is_empty()),
                "created_at": row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            }));
        }
        let mut links = Vec::new();
        let mut stmt = conns
            .conn
            .prepare("SELECT from_seq, parent_seq, via FROM srv_links")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::None)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            links.push(serde_json::json!({
                "from": row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "parent": row.get_value(1).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0),
                "via": row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default(),
            }));
        }
        drop(_guard);
        Ok(serde_json::json!({
            "config": self.board_config(board_id).await?,
            "records": records,
            "hooks": hooks,
            "links": links,
        }))
    }

    /// Restore a dump into the board (explicit seqs are preserved).
    pub async fn record_restore(&self, board_id: &str, dump: &Json) -> Result<Json, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut records = 0i64;
        if let Some(rows) = dump.get("records").and_then(Json::as_array) {
            for row in rows {
                let seq = row.get("seq").and_then(Json::as_i64).unwrap_or(0);
                let payload = row.get("payload").cloned().unwrap_or(Json::Null);
                let created_at = row.get("created_at").and_then(Json::as_str).unwrap_or("0");
                let writer = row.get("writer").and_then(Json::as_str).unwrap_or("");
                let text = serde_json::to_string(&payload).map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
                conns
                    .conn
                    .execute(
                        "INSERT OR REPLACE INTO srv_records (seq, payload, created_at, writer) VALUES (?, ?, ?, ?)",
                        Params::Positional(vec![
                            SqlValue::Integer(seq),
                            SqlValue::Text(text.clone()),
                            SqlValue::Text(created_at.to_string()),
                            SqlValue::Text(writer.to_string()),
                        ]),
                    )
                    .await
                    .map_err(Self::map_db)?;
                records += 1;
            }
        }
        let mut hooks = 0i64;
        if let Some(rows) = dump.get("hooks").and_then(Json::as_array) {
            for row in rows {
                let url = row.get("url").and_then(Json::as_str).unwrap_or("").to_string();
                if url.is_empty() {
                    continue;
                }
                let secret = row.get("secret").and_then(Json::as_str).unwrap_or("");
                let created_at = row.get("created_at").and_then(Json::as_str).unwrap_or("0");
                conns
                    .conn
                    .execute(
                        "INSERT OR REPLACE INTO srv_hooks (url, secret, created_at) VALUES (?, ?, ?)",
                        Params::Positional(vec![
                            SqlValue::Text(url),
                            SqlValue::Text(secret.to_string()),
                            SqlValue::Text(created_at.to_string()),
                        ]),
                    )
                    .await
                    .map_err(Self::map_db)?;
                hooks += 1;
            }
        }
        let mut links = 0i64;
        if let Some(rows) = dump.get("links").and_then(Json::as_array) {
            for row in rows {
                conns
                    .conn
                    .execute(
                        "INSERT OR REPLACE INTO srv_links (from_seq, parent_seq, via) VALUES (?, ?, ?)",
                        Params::Positional(vec![
                            SqlValue::Integer(row.get("from").and_then(Json::as_i64).unwrap_or(0)),
                            SqlValue::Integer(row.get("parent").and_then(Json::as_i64).unwrap_or(0)),
                            SqlValue::Text(row.get("via").and_then(Json::as_str).unwrap_or("").to_string()),
                        ]),
                    )
                    .await
                    .map_err(Self::map_db)?;
                links += 1;
            }
        }
        drop(_guard);
        if let Some(config) = dump.get("config").and_then(Json::as_object) {
            let mut pairs: Vec<(&str, String)> = Vec::new();
            if let Some(title) = config.get("title").and_then(Json::as_str) {
                pairs.push(("title", title.to_string()));
            }
            if let Some(secs) = config.get("ttl_seconds").and_then(Json::as_i64) {
                let field = config.get("ttl_field").and_then(Json::as_str).unwrap_or("expires_at");
                pairs.push(("ttl_seconds", secs.to_string()));
                pairs.push(("ttl_field", field.to_string()));
            }
            let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
            Self::write_meta_kv(board_id, &refs)?;
        }
        Ok(serde_json::json!({ "records": records, "hooks": hooks, "links": links }))
    }

    // ---- shared row helpers -------------------------------------------------

    async fn rows_to_records(rows: &mut turso::Rows) -> Result<Vec<SrvRecord>, ServerlessError> {
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(Self::row_to_record(&row).map_err(ServerlessError::Db)?);
        }
        Ok(out)
    }
}

fn json_to_sql(v: &Json) -> SqlValue {
    match v {
        Json::Null => SqlValue::Null,
        Json::Bool(b) => SqlValue::Integer(*b as i64),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else {
                SqlValue::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Json::String(s) => SqlValue::Text(s.clone()),
        Json::Array(_) | Json::Object(_) => SqlValue::Text(v.to_string()),
    }
}

/// `a.b[2]`-style path → `'$.a.b[2]'` SQL literal.
fn quote_sql_path(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

impl TursoStore {
    fn sql_num(v: &turso::Value) -> f64 {
        v.as_integer()
            .copied()
            .map(|i| i as f64)
            .or_else(|| v.as_real().copied())
            .unwrap_or(0.0)
    }
}

/// Escape LIKE metacharacters so the needle is literal.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Wrap case-insensitive occurrences of `q` in `<mark>` tags (SSE search hl).
fn highlight(payload: &Json, q: &str) -> Json {
    if q.is_empty() {
        return payload.clone();
    }
    let text = serde_json::to_string(payload).unwrap_or_default();
    let lower = text.to_lowercase();
    let needle = q.to_lowercase();
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = &text[..];
    let mut idx = 0;
    while let Some(offset) = lower[idx..].find(&needle) {
        let start = idx + offset;
        let end = start + needle.len();
        out.push_str(&rest[..start - idx]);
        out.push_str("<mark>");
        out.push_str(&rest[start - idx..end - idx]);
        out.push_str("</mark>");
        rest = &rest[end - idx..];
        idx = end;
        if idx >= lower.len() {
            break;
        }
    }
    out.push_str(rest);
    Json::String(out)
}