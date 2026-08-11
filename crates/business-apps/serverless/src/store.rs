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

/// Cheap-to-clone board store; all state lives in the connection cache.
#[derive(Clone, Default)]
pub struct TursoStore {
    conns: Arc<Mutex<HashMap<String, Arc<BoardConn>>>>,
}

impl TursoStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh owner key: 16 random bytes as lowercase hex.
    pub fn new_owner_key() -> String {
        Self::random_hex(16)
    }

    fn board_dir(board_id: &str) -> PathBuf {
        data_dir().join("dbs").join(sanitize_key(board_id))
    }

    /// Open (creating + migrating if needed) and cache the board's database.
    async fn board_conn(&self, board_id: &str) -> Result<Arc<BoardConn>, ServerlessError> {
        if let Some(cached) = self.conns.lock().await.get(board_id).cloned() {
            return Ok(cached);
        }
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

    /// Fire webhook deliveries off-thread (best effort; failures log only).
    fn fire_hooks(board_id: &str, event: &str, seq: i64, payload: &Json, hooks: Vec<SrvHook>) {
        if hooks.is_empty() {
            return;
        }
        let body = serde_json::json!({
            "boardId": board_id,
            "event": event,
            "seq": seq,
            "payload": payload,
        });
        let body = serde_json::to_string(&body).unwrap_or_default();
        for hook in hooks {
            let url = hook.url;
            let signature = hook.secret.as_deref().map(|s| {
                let mut hasher = Sha256::new();
                hasher.update(s.as_bytes());
                hasher.update(body.as_bytes());
                let digest = hasher.finalize();
                let mut out = String::with_capacity(64);
                for b in digest {
                    out.push_str(&format!("{b:02x}"));
                }
                out
            });
            let body = body.clone();
            tokio::spawn(async move {
                let mut req = reqwest::Client::new()
                    .post(&url)
                    .header("content-type", "application/json");
                if let Some(sig) = signature {
                    req = req.header("x-srv-signature", sig);
                }
                match req.body(body).send().await {
                    Ok(resp) if resp.status().is_success() => {}
                    Ok(resp) => tracing::warn!("serverless hook {url} -> {}", resp.status()),
                    Err(e) => tracing::warn!("serverless hook {url} failed: {e}"),
                }
            });
        }
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
                    let hooks = Self::hooks_locked(&conns.conn).await?;
                    drop(_guard);
                    Self::fire_hooks(board_id, "record.updated", target, payload, hooks);
                    return Ok(target);
                }
            }
        }

        conns
            .conn
            .execute(
                "INSERT INTO srv_records (payload, created_at, writer) VALUES (?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(payload_text),
                    SqlValue::Text(Self::now_millis()),
                    SqlValue::Text(writer),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        seq = conns.conn.last_insert_rowid();
        let hooks = Self::hooks_locked(&conns.conn).await?;
        drop(_guard);
        Self::fire_hooks(board_id, "record.created", seq, payload, hooks);
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
        conns
            .conn
            .execute(
                "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                Params::Positional(vec![
                    SqlValue::Text(payload_text),
                    SqlValue::Text(writer.unwrap_or("").to_string()),
                    SqlValue::Integer(seq),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        let hooks = Self::hooks_locked(&conns.conn).await?;
        drop(_guard);
        Self::fire_hooks(board_id, "record.updated", seq, payload, hooks);
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
            .prepare("SELECT seq, payload, created_at, writer FROM srv_records WHERE seq = ?")
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
                "SELECT seq, payload, created_at, writer FROM srv_records \
                 WHERE (?1 IS NULL OR seq < ?1) ORDER BY seq DESC LIMIT ?2 OFFSET ?3",
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
            let hooks = Self::hooks_locked(&conns.conn).await?;
            drop(_guard);
            Self::fire_hooks(board_id, "record.deleted", seq, &Json::Null, hooks);
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