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
//!
//! Layout: `records` (record CRUD + read paths), `ops` (jobs/deliveries/
//! ttl/rate/audit/links/config/dump-restore), `logic` (computed/validate/
//! redact pure functions), `recipes` (recipe engine wiring + dispatch),
//! `secrets` (encrypted secrets at rest).

pub(crate) mod logic;
pub(crate) mod ops;
pub(crate) mod recipes;
pub(crate) mod records;
pub(crate) mod secrets;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use turso::params::Params;
use turso::Value as SqlValue;

use serverless_core::{
    BoardStore, KeyRole, ServerlessError, SrvApp, SrvApiKey, SrvHook, SrvRecord, app_meta_path,
    sanitize_key,
};
use serverless_recipe::EventKind;


static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

fn data_dir() -> PathBuf {
    DATA_DIR
        .get_or_init(|| {
            std::env::var("LAN_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/data"))
        })
        .clone()
}

/// Test-only: pin the data dir once per test binary (parallel tests all
/// agree on the same temp root and use distinct board ids).
#[cfg(test)]
pub(crate) fn pin_test_data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lan-store-test-{}", std::process::id()));
    let _ = DATA_DIR.set(dir.clone());
    dir
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
pub(crate) struct BoardConn {
    pub(crate) conn: turso::Connection,
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
    pub(crate) fn publish_event(&self, board_id: &str, kind: &str, seq: i64, payload: &str) {
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
    pub(crate) async fn board_conn(&self, board_id: &str) -> Result<Arc<BoardConn>, ServerlessError> {
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
             );\n\
             CREATE TABLE IF NOT EXISTS srv_recipes (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             board_id TEXT NOT NULL,\n\
             name TEXT NOT NULL,\n\
             when_json TEXT NOT NULL,\n\
             match_json TEXT,\n\
             enabled INTEGER NOT NULL DEFAULT 1,\n\
             dedup_on TEXT,\n\
             actions_json TEXT NOT NULL,\n\
             created_at TEXT NOT NULL,\n\
             UNIQUE (board_id, name)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS srv_recipe_runs (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             recipe TEXT NOT NULL,\n\
             dedup_key TEXT NOT NULL,\n\
             created_at TEXT NOT NULL,\n\
             UNIQUE (recipe, dedup_key)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS srv_app_secrets (\n\
             name TEXT PRIMARY KEY,\n\
             cipher TEXT NOT NULL,\n\
             fingerprint TEXT NOT NULL,\n\
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

    pub(crate) fn read_meta(board_id: &str) -> Result<HashMap<String, String>, ServerlessError> {
        let raw = std::fs::read_to_string(Self::meta_path(board_id))?;
        let mut out = HashMap::new();
        for line in raw.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let value = v.trim();
            let value = serde_json::from_str::<String>(value).unwrap_or_else(|_| value.trim_matches('"').to_string());
            out.insert(k.trim().to_string(), value);
        }
        Ok(out)
    }

    /// Merge key/value pairs into the board's meta.toml (read-modify-write).
    pub(crate) fn write_meta_kv(board_id: &str, pairs: &[(&str, &str)]) -> Result<(), ServerlessError> {
        let mut meta = Self::read_meta(board_id).unwrap_or_default();
        for (k, v) in pairs {
            meta.insert(k.to_string(), v.to_string());
        }
        let mut body = String::new();
        for (k, v) in meta {
            let encoded = serde_json::to_string(&v).unwrap_or_else(|_| v);
            body.push_str(&format!("{k} = {encoded}\n"));
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

    pub(crate) fn now_millis() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
            .to_string()
    }

    pub(crate) fn now_millis_i64() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()) as i64
    }

    /// `n` random bytes as lowercase hex (thread_rng: ChaCha12 seeded from
    /// the OS — fine for LAN-scoped secrets).
    pub(crate) fn random_hex(bytes: usize) -> String {
        let mut out = String::with_capacity(bytes * 2);
        for _ in 0..bytes {
            out.push_str(&format!("{:02x}", rand::random::<u8>()));
        }
        out
    }

    pub(crate) fn sha256_hex(parts: &[&str]) -> String {
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

    pub(crate) fn map_db(err: impl std::fmt::Display) -> ServerlessError {
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
    pub(crate) async fn hooks_locked(conn: &turso::Connection) -> Result<Vec<SrvHook>, ServerlessError> {
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
    pub(crate) async fn enqueue_hooks(
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
    // ---- records --------------------------------------------------------

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
        let prepared = self.prepare_payload(board_id, payload).await?;
        let (seq, replaced) = self.record_insert_raw(board_id, &prepared, writer, upsert).await?;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let payload_text = serde_json::to_string(&prepared)
            .map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        let action = if replaced { "record.updated" } else { "record.created" };
        let _ = Self::audit_append_locked(&conns.conn, board_id, action, seq, Some(&payload_text)).await;
        let hooks = Self::hooks_locked(&conns.conn).await?;
        drop(_guard);
        let _ = Self::enqueue_hooks(&conns.conn, board_id, action, seq, &prepared, hooks).await;
        let _ = self.event_append(board_id, action, seq, &payload_text).await;
        self.publish_event(board_id, action, seq, &payload_text);
        self.dispatch_recipes(
            board_id,
            if replaced { EventKind::Updated } else { EventKind::Created },
            Some(seq),
            &prepared,
            writer,
            None,
        )
        .await;
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
        let prepared = self.prepare_payload(board_id, payload).await?;
        let created = self.record_set_raw(board_id, seq, &prepared, writer).await?;
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let payload_text = serde_json::to_string(&prepared)
            .map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        let action = if created { "record.created" } else { "record.updated" };
        let _ = Self::audit_append_locked(&conns.conn, board_id, action, seq, Some(&payload_text)).await;
        let hooks = Self::hooks_locked(&conns.conn).await?;
        drop(_guard);
        let _ = Self::enqueue_hooks(&conns.conn, board_id, action, seq, &prepared, hooks).await;
        let _ = self.event_append(board_id, action, seq, &payload_text).await;
        self.publish_event(board_id, action, seq, &payload_text);
        self.dispatch_recipes(
            board_id,
            if created { EventKind::Created } else { EventKind::Updated },
            Some(seq),
            &prepared,
            writer,
            None,
        )
        .await;
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
            Some(row) => {
                let rec = Self::row_to_record(&row).map_err(ServerlessError::Db)?;
                Ok(self.redact_records(board_id, vec![rec]).into_iter().next())
            }
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
        Ok(self.redact_records(board_id, out))
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
            self.dispatch_recipes(board_id, EventKind::Deleted, Some(seq), &Json::Null, None, None).await;
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
    pub(crate) async fn rows_to_records(rows: &mut turso::Rows) -> Result<Vec<SrvRecord>, ServerlessError> {
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            out.push(Self::row_to_record(&row).map_err(ServerlessError::Db)?);
        }
        Ok(out)
    }
}

pub(crate) fn json_to_sql(v: &Json) -> SqlValue {
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
pub(crate) fn quote_sql_path(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

impl TursoStore {
    pub(crate) fn sql_num(v: &turso::Value) -> f64 {
        v.as_integer()
            .copied()
            .map(|i| i as f64)
            .or_else(|| v.as_real().copied())
            .unwrap_or(0.0)
    }
}

/// Escape LIKE metacharacters so the needle is literal.
pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Wrap case-insensitive occurrences of `q` in `<mark>` tags (SSE search hl).
pub(crate) fn highlight(payload: &Json, q: &str) -> Json {
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
