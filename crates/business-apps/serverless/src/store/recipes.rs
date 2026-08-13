//! Event recipes: `srv_recipes` / `srv_recipe_runs` CRUD, the event dispatch
//! bridge into the [`serverless_recipe`] engine, and the [`RecipeDb`] seam
//! implementation over the store's raw write paths.
//!
//! The engine is pure (no SQL); everything here is wiring + persistence:
//! recipes list, dedup bookkeeping, secrets loading, SSRF-guarded outbound
//! calls and the raw (non-re-dispatching) record writes that prevent loops.

use std::time::Duration;

use serde_json::Value as Json;
use serverless_recipe::rule::parse_recipe;
use serverless_recipe::{Engine, EventKind, Recipe, Secrets};
use sha2::{Digest, Sha256};
use turso::params::Params;
use turso::Value as SqlValue;

use serverless_core::ServerlessError;

use crate::netguard::valid_url;

use super::TursoStore;

const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SrvRecipe {
    pub id: i64,
    pub board_id: String,
    pub name: String,
    pub when_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_json: Option<Json>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedup_on: Option<String>,
    pub actions_json: Json,
    pub created_at: String,
}

fn dedup_key(value: &Json) -> String {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_string(value).unwrap_or_default().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn recipe_from_row(row: &turso::Row) -> SrvRecipe {
    let id = row.get_value(0).ok().and_then(|v| v.as_integer().copied()).unwrap_or(0);
    let board_id = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
    let name = row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
    let when_json = row.get_value(3).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
    let match_json = row
        .get_value(4)
        .ok()
        .and_then(|v| v.as_text().cloned())
        .and_then(|s| serde_json::from_str(&s).ok());
    let enabled = row.get_value(5).ok().and_then(|v| v.as_integer().copied()).unwrap_or(1) != 0;
    let dedup_on = row.get_value(6).ok().and_then(|v| v.as_text().cloned());
    let actions_json = row
        .get_value(7)
        .ok()
        .and_then(|v| v.as_text().cloned())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| Json::Array(Vec::new()));
    let created_at = row.get_value(8).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
    SrvRecipe { id, board_id, name, when_json, match_json, enabled, dedup_on, actions_json, created_at }
}

fn to_recipe(sr: &SrvRecipe) -> Option<Recipe> {
    parse_recipe(&serde_json::json!({
        "name": sr.name,
        "when": sr.when_json,
        "match_json": sr.match_json,
        "enabled": sr.enabled,
        "dedup_on": sr.dedup_on,
        "actions": sr.actions_json,
    }))
    .ok()
}

impl TursoStore {
    // ---- recipe CRUD ------------------------------------------------------

    pub(crate) async fn recipe_list(&self, board_id: &str) -> Result<Vec<SrvRecipe>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT id, board_id, name, when_json, match_json, enabled, dedup_on, actions_json, created_at FROM srv_recipes ORDER BY id")
            .await
            .map_err(Self::map_db)?;
        let mut rows = stmt.query(Params::None).await.map_err(Self::map_db)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(Self::map_db)? {
            out.push(recipe_from_row(&row));
        }
        drop(_guard);
        Ok(out)
    }

    /// Add a recipe; returns its id. Name must be unique per board.
    pub(crate) async fn recipe_add(
        &self,
        board_id: &str,
        name: &str,
        when_json: &str,
        match_json: Option<&str>,
        enabled: bool,
        dedup_on: Option<&str>,
        actions_json: &str,
    ) -> Result<i64, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        conns
            .conn
            .execute(
                "INSERT INTO srv_recipes (board_id, name, when_json, match_json, enabled, dedup_on, actions_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(board_id.to_string()),
                    SqlValue::Text(name.to_string()),
                    SqlValue::Text(when_json.to_string()),
                    match match_json {
                        Some(m) => SqlValue::Text(m.to_string()),
                        None => SqlValue::Null,
                    },
                    SqlValue::Integer(enabled as i64),
                    match dedup_on {
                        Some(d) => SqlValue::Text(d.to_string()),
                        None => SqlValue::Null,
                    },
                    SqlValue::Text(actions_json.to_string()),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        let id = conns.conn.last_insert_rowid();
        drop(_guard);
        Ok(id)
    }

    pub(crate) async fn recipe_set_enabled(&self, board_id: &str, name: &str, enabled: bool) -> Result<bool, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "UPDATE srv_recipes SET enabled = ? WHERE board_id = ? AND name = ?",
                Params::Positional(vec![
                    SqlValue::Integer(enabled as i64),
                    SqlValue::Text(board_id.to_string()),
                    SqlValue::Text(name.to_string()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        drop(_guard);
        Ok(n > 0)
    }

    pub(crate) async fn recipe_remove(&self, board_id: &str, name: &str) -> Result<bool, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "DELETE FROM srv_recipes WHERE board_id = ? AND name = ?",
                Params::Positional(vec![SqlValue::Text(board_id.to_string()), SqlValue::Text(name.to_string())]),
            )
            .await
            .map_err(Self::map_db)?;
        drop(_guard);
        Ok(n > 0)
    }

    /// Has this recipe already run for this dedup key?
    pub(crate) async fn recipe_ran(&self, board_id: &str, recipe: &str, key: &Json) -> Result<bool, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT 1 FROM srv_recipe_runs WHERE recipe = ? AND dedup_key = ? LIMIT 1")
            .await
            .map_err(Self::map_db)?;
        let mut rows = stmt
            .query(Params::Positional(vec![
                SqlValue::Text(recipe.to_string()),
                SqlValue::Text(dedup_key(key)),
            ]))
            .await
            .map_err(Self::map_db)?;
        let ran = rows.next().await.map_err(Self::map_db)?.is_some();
        drop(_guard);
        Ok(ran)
    }

    pub(crate) async fn recipe_mark_ran(&self, board_id: &str, recipe: &str, key: &Json) -> Result<(), ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        conns
            .conn
            .execute(
                "INSERT OR IGNORE INTO srv_recipe_runs (recipe, dedup_key, created_at) VALUES (?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(recipe.to_string()),
                    SqlValue::Text(dedup_key(key)),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        drop(_guard);
        Ok(())
    }

    // ---- dispatch ---------------------------------------------------------

    /// Run every enabled recipe matching `kind` (and, for cron, the job
    /// name). Called AFTER the record event is published, outside the board
    /// lock — recipe writes go through the raw paths and re-enter
    /// `record_insert`/`record_set`, which dispatch again: the engine's
    /// `persist-only-when-changed` + dedup keep that from looping.
    pub(crate) async fn dispatch_recipes(
        &self,
        board_id: &str,
        kind: EventKind,
        seq: Option<i64>,
        payload: &Json,
        writer: Option<&str>,
        cron_name: Option<&str>,
    ) {
        let Ok(recipes) = self.recipe_list(board_id).await else { return };
        let mut secrets = Secrets::new();
        if let Ok(map) = self.secrets_map(board_id).await {
            for (name, value) in map {
                secrets.insert(&name, &value);
            }
        }
        let ctx = serverless_recipe::EventContext {
            kind,
            board_id: board_id.to_string(),
            seq,
            payload: payload.clone(),
            writer: writer.map(str::to_string),
            cron_name: cron_name.map(str::to_string),
        };
        let engine = Engine::new(self);
        for sr in recipes {
            let Some(recipe) = to_recipe(&sr) else { continue };
            if sr.enabled {
                let outcome = engine.run_recipe(&recipe, &secrets, &ctx, false).await;
                if !outcome.report.logs.is_empty() {
                    tracing::debug!("recipes[{}] {}: {:?}", board_id, recipe.name, outcome.report.logs);
                }
            }
        }
    }

    // ---- RecipeDb seam ----------------------------------------------------
}

#[async_trait::async_trait]
impl serverless_recipe::RecipeDb for TursoStore {
    /// LAN boards don't persist the owner key (it's hashed into the keys
    /// table and shown once) — the server is the caller, so the board id
    /// serves as the owner identity for outbound calls.
    async fn board_owner(&self, board_id: &str) -> anyhow::Result<String> {
        Ok(board_id.to_string())
    }

    async fn record_insert(
        &self,
        board_id: &str,
        payload: &Json,
        writer: Option<&str>,
        upsert: bool,
    ) -> anyhow::Result<i64> {
        Ok(self.record_insert_raw(board_id, payload, writer, upsert).await?.0)
    }

    async fn record_set(&self, board_id: &str, seq: i64, payload: &Json, writer: Option<&str>) -> anyhow::Result<()> {
        Ok(self.record_set_raw(board_id, seq, payload, writer).await.map(|_| ())?)
    }

    /// `$patch_other`: patch the newest record matching `filter` (raw path —
    /// no re-dispatch, so recipe chains cannot loop). Returns the patched
    /// seq, or `None` when nothing matched.
    async fn record_patch_other(
        &self,
        board_id: &str,
        filter: &Json,
        patch: &Json,
        writer: Option<&str>,
    ) -> anyhow::Result<Option<i64>> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let parsed = crate::dsl::parse_filter(filter).map_err(|e| anyhow::anyhow!("bad filter: {e}"))?;
        let (where_clause, params) = Self::filter_sql(&parsed.conds);
        let sql = format!("SELECT seq, payload FROM srv_records {where_clause} ORDER BY created_at DESC LIMIT 1");
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| anyhow::anyhow!("patch_other prepare: {e}"))?;
        let mut rows = stmt
            .query(Params::Positional(params))
            .await
            .map_err(|e| anyhow::anyhow!("patch_other query: {e}"))?;
        let Some(row) = rows.next().await.map_err(|e| anyhow::anyhow!("patch_other row: {e}"))? else {
            return Ok(None);
        };
        let seq: i64 = row
            .get_value(0).ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(0);
        let current = row
            .get_value(1).ok()
            .and_then(|v| v.as_text().cloned())
            .unwrap_or_default();
        let mut payload: Json = serde_json::from_str(&current).map_err(|e| anyhow::anyhow!("stored payload: {e}"))?;
        crate::dsl::apply_patch(&mut payload, patch).map_err(|e| anyhow::anyhow!("patch: {e}"))?;
        let text = serde_json::to_string(&payload).map_err(|e| anyhow::anyhow!("encode: {e}"))?;
        conns
            .conn
            .execute(
                "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                Params::Positional(vec![
                    SqlValue::Text(text),
                    SqlValue::Text(writer.unwrap_or("").to_string()),
                    SqlValue::Integer(seq),
                ]),
            )
            .await
            .map_err(|e| anyhow::anyhow!("patch_other write: {e}"))?;
        Ok(Some(seq))
    }

    async fn http_call(
        &self,
        _board_id: &str,
        _owner: &str,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
        timeout_ms: u64,
    ) -> anyhow::Result<serverless_recipe::CallOutcome> {
        if !valid_url(url) {
            anyhow::bail!("url must be a public http(s) address (private/loopback targets are blocked)");
        }
        let method = match method.to_uppercase().as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            other => anyhow::bail!("unsupported http method '{other}'"),
        };
        let effective = if timeout_ms == 0 { DEFAULT_TIMEOUT_MS } else { timeout_ms };
        let timeout = Duration::from_millis(effective.clamp(500, MAX_TIMEOUT_MS));

        let client = reqwest::Client::builder().timeout(timeout).build()?;
        let mut req = client.request(method, url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(b) = body {
            req = req.body(b.to_string());
        }

        let started = std::time::Instant::now();
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => anyhow::bail!("request failed: {e}"),
        };
        let status = resp.status().as_u16();
        let content_len = resp.content_length().unwrap_or(0);
        if content_len > MAX_RESPONSE_BYTES {
            anyhow::bail!("response too large ({content_len} bytes, max {MAX_RESPONSE_BYTES})");
        }
        let bytes = resp.bytes().await?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            anyhow::bail!("response too large ({} bytes, max {MAX_RESPONSE_BYTES})", bytes.len());
        }
        let _ = String::from_utf8_lossy(&bytes);
        let duration_ms = started.elapsed().as_millis() as u64;
        Ok(serverless_recipe::CallOutcome { status, duration_ms })
    }

    async fn job_add(&self, board_id: &str, name: &str, schedule: &str, action: &Json) -> anyhow::Result<i64> {
        Ok(TursoStore::job_add(self, board_id, name, schedule, action).await?)
    }

    async fn recipe_ran(&self, board_id: &str, recipe: &str, key: &Json) -> anyhow::Result<bool> {
        Ok(TursoStore::recipe_ran(self, board_id, recipe, key).await?)
    }

    async fn recipe_mark_ran(&self, board_id: &str, recipe: &str, key: &Json) -> anyhow::Result<()> {
        Ok(TursoStore::recipe_mark_ran(self, board_id, recipe, key).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_key_is_stable_and_distinct() {
        let a = dedup_key(&Json::from(serde_json::json!({ "b": 2, "a": 1 })));
        let b = dedup_key(&Json::from(serde_json::json!({ "a": 1, "b": 2 })));
        let c = dedup_key(&Json::from(serde_json::json!({ "a": 1, "b": 3 })));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn to_recipe_roundtrips_known_shapes() {
        let sr = SrvRecipe {
            id: 1,
            board_id: "t".into(),
            name: "notify".into(),
            when_json: "record.created".into(),
            match_json: None,
            enabled: true,
            dedup_on: Some("$.id".into()),
            actions_json: serde_json::json!([{ "http": { "url": "https://example.com/hook" } }]),
            created_at: "0".into(),
        };
        let recipe = to_recipe(&sr).expect("known recipe shape parses");
        assert_eq!(recipe.name, "notify");
        assert_eq!(recipe.dedup_on, Some("$.id".into()));
    }

    #[test]
    fn to_recipe_rejects_garbage_when() {
        let sr = SrvRecipe {
            id: 2,
            board_id: "t".into(),
            name: "bad".into(),
            when_json: "created".into(),
            match_json: None,
            enabled: true,
            dedup_on: None,
            actions_json: serde_json::json!([{ "http": { "url": "not-a-url" } }]),
            created_at: "0".into(),
        };
        assert!(to_recipe(&sr).is_none());
    }
}
