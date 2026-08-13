//! Board operations: cron jobs, webhook deliveries, audit, TTL, links,
//! rate limit, config and dump/restore.

use serde_json::Value as Json;
use turso::params::Params;
use turso::Value as SqlValue;

use serverless_core::ServerlessError;

use super::{DeliveryRow, JobRow, TursoStore, quote_sql_path};

impl TursoStore {
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
    pub(crate) async fn audit_append_locked(
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

    pub(crate) fn ttl_config(board_id: &str) -> (Option<i64>, Option<String>) {
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
    pub(crate) fn record_expired(payload_text: &str, ttl_seconds: i64, field: &str) -> bool {
        let Ok(payload) = serde_json::from_str::<Json>(payload_text) else { return false };
        let Some(ts) = payload.get(field).and_then(Json::as_f64) else { return false };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        (ts + ttl_seconds as f64) < now
    }

    /// `AND`-fragment hiding records whose TTL field expired (empty when no
    /// TTL configured): `ts + ttl_seconds > now`.
    pub(crate) fn ttl_hide_sql(board_id: &str) -> String {
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
        let config_json = |k: &str| {
            meta.get(k)
                .filter(|s| !s.is_empty())
                .and_then(|s| serde_json::from_str::<Json>(s).ok())
        };
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
            "computed": config_json("computed_json"),
            "validate": config_json("validate_json"),
            "redact": config_json("redact_json"),
            "webhook_secret_set": meta.get("webhook_secret").is_some_and(|s| !s.is_empty()),
        }))
    }

    /// The board's inbound webhook secret (`webhook_secret` config key), if
    /// set — used to verify `POST /events` signatures. Never exposed via
    /// `board_config` (only the `webhook_secret_set` flag is).
    pub async fn webhook_secret(&self, board_id: &str) -> Option<String> {
        Self::read_meta(board_id)
            .unwrap_or_default()
            .get("webhook_secret")
            .cloned()
            .and_then(|s| Some(serde_json::from_str::<String>(&s).unwrap_or(s)))
            .filter(|s| !s.is_empty())
    }

    /// Set or clear a meta.toml JSON config (`computed_json` / `validate_json`
    /// / `redact_json`); `None` clears. Empty object/array counts as unset.
    pub async fn board_config_set(&self, board_id: &str, key: &str, value: Option<&Json>) -> Result<(), ServerlessError> {
        let stored = value
            .filter(|v| !v.is_null())
            .map(|v| serde_json::to_string(v).map_err(|e| ServerlessError::BadPayload(e.to_string())))
            .transpose()?
            .unwrap_or_default();
        Self::write_meta_kv(board_id, &[(key, &stored)])?;
        Ok(())
    }

    /// Recipe rows + secret names for the config endpoint (ciphertext never
    /// leaves the store; recipes carry their actions json).
    pub async fn board_config_extended(&self, board_id: &str) -> Result<Json, ServerlessError> {
        let recipes = self.recipe_list(board_id).await?;
        let secrets = self.secret_list(board_id).await?;
        Ok(serde_json::json!({
            "recipes": recipes,
            "secrets": secrets,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::BoardStore;
    use crate::store::NO_TENANT;
    use serverless_core::KeyRole;
    use serde_json::json;

    /// Isolate meta.toml writes behind a per-test temp data dir.
    fn test_store() -> TursoStore {
        let _dir = crate::store::pin_test_data_dir();
        TursoStore::new()
    }

    #[tokio::test]
    async fn rules_pipeline_computes_validates_redacts() {
        let store = test_store();
        let board = "t-rules";
        // computed: total = price * qty (name order: 'total' sorts first)
        store
            .board_config_set(
                board,
                "computed_json",
                Some(&json!({ "total": "$.price * $.qty" })),
            )
            .await
            .unwrap();
        // validate: qty must be positive
        store
            .board_config_set(
                board,
                "validate_json",
                Some(&json!([{ "when": "$.qty <= 0", "error": "qty must be positive" }])),
            )
            .await
            .unwrap();
        // redact: hide token on internal-kind payloads
        store
            .board_config_set(
                board,
                "redact_json",
                Some(&json!(["$.token"])),
            )
            .await
            .unwrap();

        let prepared = store.prepare_payload(board, &json!({ "price": 3, "qty": 4 })).await.unwrap();
        assert_eq!(prepared["total"].as_f64(), Some(12.0));

        let rejected = store.prepare_payload(board, &json!({ "price": 3, "qty": 0 })).await;
        assert!(rejected.is_err());

        let redacted = store.redact_payload(board, &json!({ "kind": "internal", "token": "abc", "price": 3 }));
        assert_eq!(redacted["token"], json!("***"));
        assert_eq!(redacted["price"], json!(3));
    }

    #[tokio::test]
    async fn schema_validation_rejects_bad_payloads() {
        let store = test_store();
        let board = "t-schema";
        store
            .board_config_set(board, "schema_json", Some(&json!({ "type": "object", "required": ["x"], "properties": { "x": { "type": "integer" } } })))
            .await
            .unwrap();
        assert!(store.prepare_payload(board, &json!({})).await.is_err());
        assert!(store.prepare_payload(board, &json!({ "x": "nope" })).await.is_err());
        assert!(store.prepare_payload(board, &json!({ "x": 1 })).await.is_ok());
    }

    #[tokio::test]
    async fn customer_key_scope_round_trips() {
        let store = test_store();
        let board = "t-scope";
        let (bucket, secret) = store
            .key_issue(NO_TENANT, board, KeyRole::Customer, None, Some("cust-42"))
            .await
            .unwrap();
        let token = format!("{board}:{bucket}:{secret}");
        let (got_board, key) = store.key_auth(&token).await.unwrap().unwrap();
        assert_eq!(got_board, board);
        assert_eq!(key.bucket, bucket);
        assert_eq!(key.role, KeyRole::Customer);
        assert_eq!(key.scope.as_deref(), Some("cust-42"));
        // staff keys carry no scope
        let (_b2, s2) = store.key_issue(NO_TENANT, board, KeyRole::Writer, Some("me"), None).await.unwrap();
        let token2 = format!("{board}:{_b2}:{s2}");
        let (_, k2) = store.key_auth(&token2).await.unwrap().unwrap();
        assert_eq!(k2.scope, None);
    }

    #[tokio::test]
    async fn config_clear_disables_rules() {
        let store = test_store();
        let board = "t-clear";
        store
            .board_config_set(board, "computed_json", Some(&json!({ "n": "1" })))
            .await
            .unwrap();
        assert_eq!(store.prepare_payload(board, &json!({ "v": 2 })).await.unwrap()["n"], json!(1));
        store.board_config_set(board, "computed_json", None).await.unwrap();
        assert_eq!(store.prepare_payload(board, &json!({ "v": 2 })).await.unwrap().get("n"), None);
    }
}
