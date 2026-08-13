//! The recipe engine: event → condition → ordered actions, driven by a
//! [`RecipeDb`] trait so the engine stays free of any concrete store/SQL.
//!
//! Ported from zerowrapper's `zw-recipe`; the only structural change is the
//! `zw_db::DbPool` dependency replaced by the [`RecipeDb`] seam.

use crate::actions::{ActionKind, RunReport, apply_action};
use crate::context::EventContext;
use crate::rule::{EventKind, Recipe};
use crate::secrets::{Secrets, substitute_value_ctx};
use serde_json::{Map, Value};

const MAX_ACTIONS: usize = 50;

/// Outcome of one DB-backed outbound call (`$call` / `$notify`).
#[derive(Debug, Clone, Default)]
pub struct CallOutcome {
    pub status: u16,
    pub duration_ms: u64,
}

/// The seam the engine needs from a board store. Implemented by
/// `TursoStore` (serverless-app) — every method must reuse the same
/// guards as the REST routes (netguard SSRF check, role checks are the
/// caller's job; the store methods enforce nothing beyond data rules).
#[async_trait::async_trait]
pub trait RecipeDb: Send + Sync {
    /// Resolve the board's owner key (for owner-scoped outbound calls).
    async fn board_owner(&self, board_id: &str) -> anyhow::Result<String>;

    /// Insert into another board (`$upsert_other`); `upsert` replaces the
    /// row holding that board's unique key value.
    async fn record_insert(
        &self,
        board_id: &str,
        payload: &Value,
        writer: Option<&str>,
        upsert: bool,
    ) -> anyhow::Result<i64>;

    /// Persist an updated payload back to the same board. This is the raw
    /// write path — it must NOT re-dispatch recipes (prevents loops).
    async fn record_set(
        &self,
        board_id: &str,
        seq: i64,
        payload: &Value,
        writer: Option<&str>,
    ) -> anyhow::Result<()>;

    /// SSRF-guarded outbound HTTP call (`$call` / `$notify`).
    async fn http_call(
        &self,
        board_id: &str,
        owner: &str,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
        timeout_ms: u64,
    ) -> anyhow::Result<CallOutcome>;

    /// Enqueue a deferred cron job (`$schedule`).
    async fn job_add(
        &self,
        board_id: &str,
        name: &str,
        schedule: &str,
        action: &Value,
    ) -> anyhow::Result<i64>;

    /// Dedup bookkeeping for `dedup_on` keys (hashed at rest).
    async fn recipe_ran(&self, board_id: &str, recipe: &str, key: &Value) -> anyhow::Result<bool>;
    async fn recipe_mark_ran(&self, board_id: &str, recipe: &str, key: &Value) -> anyhow::Result<()>;

    /// `$patch_other`: apply a patch ($set/$inc/$dec/$mul/$unset) to the
    /// first record matching `filter` (same filter syntax as the REST
    /// surface) on `board_id` — same board when the action omits `board`.
    /// Returns the patched record's seq, or `None` when nothing matched.
    /// Raw write path: must NOT re-dispatch recipes.
    async fn record_patch_other(
        &self,
        board_id: &str,
        filter: &Value,
        patch: &Value,
        writer: Option<&str>,
    ) -> anyhow::Result<Option<i64>>;
}

#[derive(Debug, Clone, Default)]
pub struct RunOutcome {
    pub matched: bool,
    pub report: RunReport,
}

pub struct Engine<'a> {
    pub db: &'a dyn RecipeDb,
}

impl<'a> Engine<'a> {
    pub fn new(db: &'a dyn RecipeDb) -> Self {
        Self { db }
    }

    pub async fn run_recipes(
        &self,
        recipes: &[Recipe],
        secrets: &Secrets,
        ctx: &EventContext,
        dry_run: bool,
    ) -> Vec<(String, RunOutcome)> {
        let mut out = Vec::new();
        for recipe in recipes {
            let outcome = self.run_recipe(recipe, secrets, ctx, dry_run).await;
            out.push((recipe.name.clone(), outcome));
        }
        out
    }

    pub async fn run_recipe(
        &self,
        recipe: &Recipe,
        secrets: &Secrets,
        ctx: &EventContext,
        dry_run: bool,
    ) -> RunOutcome {
        if !recipe.enabled {
            return RunOutcome { matched: false, report: RunReport::default() };
        }
        let matches_event = match recipe.event_kind() {
            Some(k) => k == ctx.kind,
            None => false,
        };
        if !matches_event {
            return RunOutcome { matched: false, report: RunReport::default() };
        }
        if ctx.kind == EventKind::Cron {
            if let Some(want) = &ctx.cron_name {
                if !recipe.is_cron_for(want) {
                    return RunOutcome { matched: false, report: RunReport::default() };
                }
            }
        }
        if let Some(f) = &recipe.match_json {
            if let Err(e) = crate::actions::match_public(f, &ctx.payload) {
                let mut report = RunReport::default();
                report.logs.push(format!("recipe match error: {e}"));
                return RunOutcome { matched: true, report };
            }
            if !crate::actions::match_public(f, &ctx.payload).unwrap_or(false) {
                return RunOutcome { matched: true, report: RunReport::default() };
            }
        }
        let mut payload = ctx.payload.clone();
        let mut report = RunReport::default();
        if let Some(key) = &recipe.dedup_on {
            let k = serverless_expr::get_path(&payload, key);
            if !k.is_null() && self.already_ran(&ctx.board_id, recipe, &k).await {
                return RunOutcome { matched: true, report: RunReport::default() };
            }
            if !dry_run {
                self.mark_ran(&ctx.board_id, recipe, &k).await;
            }
        }
        let mut i = 0;
        for action in &recipe.actions {
            if i >= MAX_ACTIONS {
                report.logs.push("max actions reached, stopping".to_string());
                break;
            }
            let Some((kind, detail)) = action_kind(action) else {
                report.logs.push(format!("skipping unknown action {}", action));
                continue;
            };
            let action_obj = action.get(detail).cloned().unwrap_or_else(|| Value::Object(Map::new()));
            let action_obj = if dry_run {
                action_obj
            } else {
                match substitute_value_ctx(secrets, ctx, &action_obj) {
                    Ok(v) => v,
                    Err(e) => {
                        report.logs.push(format!("{kind:?}: {e} — action skipped"));
                        i += 1;
                        continue;
                    }
                }
            };
            if let Some(cond) = action_condition(action) {
                match serverless_expr::truthy(&cond, &payload) {
                    Ok(true) => {}
                    Ok(false) => {
                        report.actions_run.push(format!("{kind:?} skipped (condition false)"));
                        continue;
                    }
                    Err(e) => {
                        report.actions_run.push(format!("{kind:?} skipped (bad condition: {e})"));
                        continue;
                    }
                }
            }
            if let Err(e) = apply_action(&kind, &action_obj, &mut payload, secrets, &mut report, ctx) {
                report.logs.push(format!("{kind:?}: {e}"));
                break;
            }
            if !dry_run {
                if let Err(e) = self.execute_external(&kind, &action_obj, ctx, &mut report).await {
                    report.logs.push(format!("{kind:?}: {e}"));
                    break;
                }
            }
            i += 1;
        }
        if !dry_run && report.changed && ctx.kind != EventKind::Inbound && ctx.kind != EventKind::Cron {
            if let (Some(seq), true) = (ctx.seq, payload != ctx.payload) {
                match self.db.record_set(&ctx.board_id, seq, &payload, None).await {
                    Ok(()) => report.actions_run.push("persisted updated record".to_string()),
                    Err(e) => report.logs.push(format!("persist: {e}")),
                }
            }
        }
        RunOutcome { matched: true, report }
    }

    async fn execute_external(
        &self,
        kind: &ActionKind,
        action: &Value,
        ctx: &EventContext,
        report: &mut RunReport,
    ) -> anyhow::Result<()> {
        match kind {
            ActionKind::UpsertOther => self.external_upsert(action, ctx).await?,
            ActionKind::PatchOther => self.external_patch_other(action, ctx).await?,
            ActionKind::Notify | ActionKind::Call => self.external_call(action, ctx, report).await?,
            ActionKind::Schedule => self.external_schedule(action, ctx).await?,
            _ => {}
        }
        Ok(())
    }

    async fn external_upsert(&self, action: &Value, ctx: &EventContext) -> anyhow::Result<()> {
        let map = action.as_object().ok_or_else(|| anyhow::anyhow!("$upsert_other must be an object"))?;
        let board = map
            .get("board")
            .and_then(|b| b.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| ctx.board_id.clone());
        let payload = map.get("payload").cloned().unwrap_or_else(|| Value::Object(Map::new()));
        self.db
            .record_insert(&board, &payload, Some(&format!("recipe:{}", ctx.board_id)), true)
            .await?;
        Ok(())
    }

    async fn external_patch_other(&self, action: &Value, ctx: &EventContext) -> anyhow::Result<()> {
        let map = action.as_object().ok_or_else(|| anyhow::anyhow!("$patch_other must be an object"))?;
        let board = map
            .get("board")
            .and_then(|b| b.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| ctx.board_id.clone());
        let filter = map.get("filter").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
        let patch = map.get("patch").cloned().ok_or_else(|| anyhow::anyhow!("$patch_other needs \"patch\""))?;
        self.db.record_patch_other(&board, &filter, &patch, Some(&format!("recipe:{}", ctx.board_id))).await?;
        Ok(())
    }

    async fn external_call(&self, action: &Value, ctx: &EventContext, report: &mut RunReport) -> anyhow::Result<()> {
        let map = action.as_object().ok_or_else(|| anyhow::anyhow!("$call/$notify must be an object"))?;
        let url = map.get("url").and_then(|u| u.as_str()).ok_or_else(|| anyhow::anyhow!("missing url"))?;
        let method = map.get("method").and_then(|m| m.as_str()).unwrap_or("POST");
        let timeout = map.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(15_000);
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(h) = map.get("headers") {
            match h {
                Value::Object(o) => {
                    for (k, v) in o {
                        headers.push((k.clone(), v.as_str().unwrap_or("").to_string()));
                    }
                }
                Value::Array(arr) => {
                    for el in arr {
                        if let Value::Array(pair) = el {
                            if pair.len() == 2 {
                                headers.push((pair[0].as_str().unwrap_or("").to_string(), pair[1].as_str().unwrap_or("").to_string()));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let body = map
            .get("body")
            .map(|b| serde_json::to_string(b).unwrap_or_else(|_| b.to_string()));
        let owner = self.db.board_owner(&ctx.board_id).await?;
        let res = self
            .db
            .http_call(&ctx.board_id, &owner, method, url, &headers, body.as_deref(), timeout)
            .await?;
        report.logs.push(format!("call {url} -> {} ({}ms)", res.status, res.duration_ms));
        Ok(())
    }

    async fn external_schedule(&self, action: &Value, ctx: &EventContext) -> anyhow::Result<()> {
        let map = action.as_object().ok_or_else(|| anyhow::anyhow!("$schedule must be an object"))?;
        let name = map.get("name").and_then(|n| n.as_str()).unwrap_or("recipe");
        let at = map.get("at").and_then(|a| a.as_str())
            .or_else(|| map.get("schedule").and_then(|s| s.as_str()))
            .ok_or_else(|| anyhow::anyhow!("$schedule needs \"at\" (cron or @every)"))?;
        let action_json = map.get("action").cloned().unwrap_or(Value::Null);
        self.db.job_add(&ctx.board_id, name, at, &action_json).await?;
        Ok(())
    }

    async fn already_ran(&self, board_id: &str, recipe: &Recipe, key: &Value) -> bool {
        matches!(self.db.recipe_ran(board_id, &recipe.name, key).await, Ok(true))
    }

    async fn mark_ran(&self, board_id: &str, recipe: &Recipe, key: &Value) {
        let _ = self.db.recipe_mark_ran(board_id, &recipe.name, key).await;
    }
}

pub fn action_kind(action: &Value) -> Option<(ActionKind, &str)> {
    let obj = action.as_object()?;
    let key = obj.keys().next()?.as_str();
    let kind = ActionKind::parse(key)?;
    Some((kind, key))
}

pub fn action_condition(action: &Value) -> Option<String> {
    let obj = action.as_object()?;
    let (_, v) = obj.iter().next()?;
    v.as_object()
        .and_then(|m| m.get("when"))
        .and_then(|w| w.as_str())
        .map(|s| s.to_string())
}
