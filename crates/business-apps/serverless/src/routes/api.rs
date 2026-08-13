//! The `x-srv-key`-less REST surface: `Authorization: Bearer` (API keys /
//! owner key), CORS, SSE events, MCP catalog + JSON-RPC, and the board
//! config/recipes/secrets/events hardening routes.
//!
//! Session routes (console pages) live in `super` (mod.rs).

use serde::Deserialize;
use serde_json::{json, Value};
use topcoat::{
    Result,
    context::{Cx, app_context},
    router::{
        Body,
        content::Json,
        content::multipart::Multipart,
        content::sse::{Event, KeepAlive, Sse},
        error::{RouterErrorExt, SeeOther, bad_request, not_found, too_many_requests, unauthorized},
        parse_query_params, path_param, route,
    },
    router::request::headers,
    router::response::IntoResponse,
};
use crate::{cli, store::TursoStore};
use futures_core::Stream;
use serverless_core::{BoardStore, KeyRole};

path_param!(board_id);
path_param!(seq);
path_param!(bucket);
path_param!(job_id);
path_param!(tool);
path_param!(encoded);
path_param!(tenant_id);
path_param!(name);

use super::{ApiAuth, NO_TENANT, RateForm, UrlForm, api_auth, api_auth_unlimited, board_row, resolve_key, requires, truthy, writer_of};

/// `GET /api/srv/{board}`: board meta (public under `public_reads`).
#[derive(Deserialize)]
pub(crate) struct McpInvoke {
    pub tool: String,
    #[serde(default)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

impl McpInvoke {
    /// `args` may be an array of tokens or a single command string.
    fn args_list(&self) -> Vec<String> {
        match self.params.get("args") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect(),
            Some(Value::String(raw)) => raw.split_whitespace().map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    }
}

#[route(GET "/api/srv/{board_id}")]
async fn api_board(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let store = app_context::<TursoStore>(cx);
    let count = store.record_count(NO_TENANT, raw).await.unwrap_or(0);
    Ok(Json(json!({
        "boardId": board.board_id,
        "title": board.board_title,
        "publicReads": board.board_public_reads.unwrap_or(false),
        "uniqueKey": board.board_unique_key,
        "schemaJson": board.board_schema_json,
        "recordCount": count,
        "deployedAt": board.board_deployed_at,
    })))
}

/// `POST /api/srv/{board}/records`: `{"payload": {...}, "upsert": false?}`.
#[route(POST "/api/srv/{board_id}/records")]
async fn api_record_insert(Json(req): Json<RecordInsert>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    let seq = store
        .record_insert(NO_TENANT, raw, &req.payload, writer.as_deref(), req.upsert)
        .await
        .map_err(|e| bad_request(format!("insert failed: {e}")))?;
    Ok(Json(json!({ "seq": seq, "boardId": raw })))
}

/// `GET /api/srv/{board}/records?limit&before&offset`, enriched with
/// `q` (substring search, `hl=1` → `<mark>` highlight), `filter`
/// (JSON conditions `[{"field","op","value"}]` or a single object),
/// `order`/`desc` (sort by a JSON path) — search/filter take precedence.
#[route(GET "/api/srv/{board_id}/records")]
async fn api_record_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let q = parse_query_params::<ListQuery>(cx).unwrap_or_default();
    let limit = q.limit.unwrap_or(25).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let store = app_context::<TursoStore>(cx);
    let items = if let Some(query) = q.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        store
            .record_search(raw, query, q.hl.as_deref().is_some_and(truthy), limit, offset)
            .await
            .map_err(|e| bad_request(format!("search failed: {e}")))?
    } else if let Some(path) = q.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        let eq = q
            .eq
            .as_deref()
            .filter(|e| !e.trim().is_empty())
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|_| bad_request("eq must be JSON"))?;
        store
            .record_query(raw, path, eq.as_ref(), limit, offset)
            .await
            .map_err(|e| bad_request(format!("query failed: {e}")))?
    } else if let Some(filter) = q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
        let filter_value: Value = serde_json::from_str(filter)
            .map_err(|_| bad_request("filter must be JSON"))?;
        let parsed = crate::dsl::parse_filter(&filter_value)
            .map_err(|e| bad_request(format!("bad filter: {e}")))?;
        let orders = q
            .order
            .as_deref()
            .filter(|o| !o.is_empty())
            .map(|o| vec![(o.to_string(), q.desc.as_deref().map(|d| !truthy(d)).unwrap_or(true))])
            .unwrap_or_default();
        store
            .record_filter(raw, &parsed.conds, &orders, limit, offset)
            .await
            .map_err(|e| bad_request(format!("filter failed: {e}")))?
    } else {
        store
            .record_list(NO_TENANT, raw, limit, q.before, offset)
            .await
            .map_err(|e| bad_request(format!("list failed: {e}")))?
    };
    Ok(Json(json!({ "items": items })))
}

/// `GET /api/srv/{board}/records/{seq}`.
#[route(GET "/api/srv/{board_id}/records/{seq}")]
async fn api_record_get(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    match store.record_get(NO_TENANT, raw, seq).await {
        Ok(Some(rec)) => Ok(Json(json!(rec))),
        Ok(None) => Err(not_found().into()),
        Err(e) => Err(bad_request(format!("get failed: {e}")).into()),
    }
}

/// `DELETE /api/srv/{board}/records/{seq}`.
#[route(DELETE "/api/srv/{board_id}/records/{seq}")]
async fn api_record_delete(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let deleted = store
        .record_delete(NO_TENANT, raw, seq)
        .await
        .map_err(|e| bad_request(format!("delete failed: {e}")))?;
    Ok(Json(json!({ "deleted": deleted })))
}

/// `POST /api/srv/{board}/keys`: `{"role": "reader|writer|admin", "writer"?}`.
#[route(POST "/api/srv/{board_id}/keys")]
async fn api_key_issue(Json(req): Json<KeyMint>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let role = match req.role.as_deref() {
        Some(r) if r == "reader" => KeyRole::Reader,
        Some(r) if r == "writer" => KeyRole::Writer,
        Some(r) if r == "admin" => KeyRole::Admin,
        _ => return Err(bad_request("invalid role").into()),
    };
    let store = app_context::<TursoStore>(cx);
    let (bucket, secret) = store
        .key_issue(NO_TENANT, raw, role, req.writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("key issue failed: {e}")))?;
    let token = format!("{raw}:{bucket}:{secret}");
    Ok(Json(json!({ "bucket": bucket, "token": token })))
}

/// `GET /api/srv/{board}/keys`.
#[route(GET "/api/srv/{board_id}/keys")]
async fn api_key_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let keys = store.key_list(NO_TENANT, raw).await.map_err(|e| bad_request(format!("list failed: {e}")))?;
    Ok(Json(json!({ "items": keys })))
}

/// `POST /api/srv/{board}/keys/{bucket}/revoke`.
#[route(POST "/api/srv/{board_id}/keys/{bucket}/revoke")]
async fn api_key_revoke(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let bucket: &str = path_param::<Bucket>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let revoked = store
        .key_revoke(NO_TENANT, raw, bucket)
        .await
        .map_err(|e| bad_request(format!("revoke failed: {e}")))?;
    Ok(Json(json!({ "revoked": revoked })))
}

/// `POST /api/srv/{board}/hooks`: `{"url": ..., "secret"?}`.
#[route(POST "/api/srv/{board_id}/hooks")]
async fn api_hook_register(Json(req): Json<HookMint>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Some(url) = req.url.filter(|u| u.starts_with("http")) else {
        return Err(bad_request("invalid hook url").into());
    };
    let store = app_context::<TursoStore>(cx);
    store
        .hook_register(NO_TENANT, raw, &url, req.secret.as_deref())
        .await
        .map_err(|e| bad_request(format!("hook failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/srv/{board}/hooks`.
#[route(GET "/api/srv/{board_id}/hooks")]
async fn api_hook_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let hooks = store.hook_list(NO_TENANT, raw).await.map_err(|e| bad_request(format!("list failed: {e}")))?;
    Ok(Json(json!({ "items": hooks })))
}

/// `DELETE /api/srv/{board}/hooks?url=<encoded>`.
#[route(DELETE "/api/srv/{board_id}/hooks")]
async fn api_hook_remove(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Some(url) = parse_query_params::<UrlForm>(cx).ok().and_then(|f| f.url) else {
        return Err(bad_request("missing url query param").into());
    };
    let store = app_context::<TursoStore>(cx);
    let removed = store
        .hook_remove(NO_TENANT, raw, &url)
        .await
        .map_err(|e| bad_request(format!("hook failed: {e}")))?;
    Ok(Json(json!({ "removed": removed })))
}

// ---------------------------------------------------------------------------
// Records: set / patch / filter / search / aggregate / bulk / delete-filter
// ---------------------------------------------------------------------------

/// `PUT /api/srv/{board}/records/{seq}`: replace the whole payload.
#[route(PUT "/api/srv/{board_id}/records/{seq}")]
async fn api_record_set(Json(req): Json<RecordInsert>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    store
        .record_set(NO_TENANT, raw, seq, &req.payload, writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("set failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "seq": seq })))
}

/// `PATCH /api/srv/{board}/records/{seq}`: `{"patch": {"$set": {...}, ...}}`.
#[route(PATCH "/api/srv/{board_id}/records/{seq}")]
async fn api_record_patch(Json(req): Json<PatchForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    let updated = store
        .record_patch(raw, seq, &req.patch, writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("patch failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "seq": seq, "payload": updated })))
}

/// `GET /api/srv/{board}/aggregate?op=count|sum|avg|min|max&field=&group_by=&filter=`.
#[route(GET "/api/srv/{board_id}/aggregate")]
async fn api_record_aggregate(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let q = parse_query_params::<AggregateQuery>(cx).unwrap_or_default();
    let Some(op) = q.op.as_deref().and_then(crate::dsl::Agg::parse) else {
        return Err(bad_request("op must be count|sum|avg|min|max").into());
    };
    let conds = match q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
        Some(filter) => {
            let filter_value: Value = serde_json::from_str(filter)
                .map_err(|_| bad_request("filter must be JSON"))?;
            crate::dsl::parse_filter(&filter_value)
                .map_err(|e| bad_request(format!("bad filter: {e}")))?
                .conds
        }
        None => Vec::new(),
    };
    let store = app_context::<TursoStore>(cx);
    let result = store
        .record_aggregate(raw, op, q.field.as_deref().unwrap_or("$"), q.group_by.as_deref(), &conds)
        .await
        .map_err(|e| bad_request(format!("aggregate failed: {e}")))?;
    Ok(Json(result))
}

/// `POST /api/srv/{board}/records/bulk`: `{"payloads": [{...}, ...]}` (alias `items`).
#[route(POST "/api/srv/{board_id}/records/bulk")]
async fn api_record_bulk(Json(req): Json<BulkInsert>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    let seqs = store
        .record_bulk_insert(raw, req.payloads, writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("bulk failed: {e}")))?;
    Ok(Json(json!({ "seqs": seqs, "count": seqs.len() })))
}

/// `DELETE /api/srv/{board}/records?filter=<json>` — delete everything
/// matching the filter (no `filter` param on this route is a 400 guard).
#[route(DELETE "/api/srv/{board_id}/records")]
async fn api_record_delete_filter(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Some(filter) = parse_query_params::<FilterQuery>(cx).ok().and_then(|f| f.filter) else {
        return Err(bad_request("missing filter query param").into());
    };
    let filter_value: Value = serde_json::from_str(&filter)
        .map_err(|_| bad_request("filter must be JSON"))?;
    let parsed = crate::dsl::parse_filter(&filter_value)
        .map_err(|e| bad_request(format!("bad filter: {e}")))?;
    let store = app_context::<TursoStore>(cx);
    let deleted = store
        .record_delete_filter(raw, &parsed.conds)
        .await
        .map_err(|e| bad_request(format!("delete failed: {e}")))?;
    Ok(Json(json!({ "deleted": deleted })))
}

/// `POST /api/srv/{board}/records/{seq}/links`: `{"parent": seq, "via": "x"}`.
#[route(POST "/api/srv/{board_id}/records/{seq}/links")]
async fn api_link_set(Json(req): Json<LinkForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let (Some(parent), Some(via)) = (req.parent, req.via.as_deref().filter(|v| !v.is_empty())) else {
        return Err(bad_request("need parent seq and via").into());
    };
    let store = app_context::<TursoStore>(cx);
    store
        .link_set(raw, seq, parent, via)
        .await
        .map_err(|e| bad_request(format!("link failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/srv/{board}/records/{seq}/links` — resolve the record's parents.
#[route(GET "/api/srv/{board_id}/records/{seq}/links")]
async fn api_link_get(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let links = store.link_get(raw, seq).await.map_err(|e| bad_request(format!("links failed: {e}")))?;
    Ok(Json(json!({ "items": links })))
}

/// `DELETE /api/srv/{board}/records/{seq}/links?via=` — remove one link.
#[route(DELETE "/api/srv/{board_id}/records/{seq}/links")]
async fn api_link_clear(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let Some(via) = parse_query_params::<ViaQuery>(cx).ok().and_then(|v| v.via) else {
        return Err(bad_request("missing via query param").into());
    };
    let store = app_context::<TursoStore>(cx);
    let cleared = store.link_clear(raw, seq, &via).await.map_err(|e| bad_request(format!("link failed: {e}")))?;
    Ok(Json(json!({ "cleared": cleared })))
}

// ---------------------------------------------------------------------------
// Calls (SSRF-guarded outbound HTTP), jobs, deliveries, config
// ---------------------------------------------------------------------------

/// `POST /api/srv/{board}/call`: `{"url", "method"?, "headers"?, "body"?,
/// "timeout_ms"?}` — server-side HTTP with the SSRF guard; response body
/// capped at 2MB.
#[route(POST "/api/srv/{board_id}/call")]
async fn api_record_call(Json(req): Json<CallBody>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Some(url) = req.url.filter(|u| crate::netguard::valid_url(u)) else {
        return Err(bad_request("url rejected (public http(s) only)").into());
    };
    let method = req.method.as_deref().unwrap_or("POST").to_uppercase();
    let Ok(method) = reqwest::Method::from_bytes(method.as_bytes()) else {
        return Err(bad_request("invalid method").into());
    };
    let timeout_ms = req.timeout_ms.unwrap_or(10_000).clamp(500, 60_000);
    let mut builder = reqwest::Client::new()
        .request(method, &url)
        .timeout(std::time::Duration::from_millis(timeout_ms));
    if let Some(headers) = req.headers.as_ref() {
        for (k, v) in headers {
            if let Some(v) = v.as_str() {
                builder = builder.header(k, v);
            }
        }
    }
    if let Some(body) = req.body.as_ref() {
        builder = builder.body(body.to_string());
    }
    match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let mut text = resp.text().await.unwrap_or_default();
            let truncated = text.len() > 2 * 1024 * 1024;
            text.truncate(2 * 1024 * 1024);
            Ok(Json(json!({ "status": status, "body": text, "truncated": truncated })))
        }
        Err(e) => Err(bad_request(format!("call failed: {e}")).into()),
    }
}

/// `POST /api/srv/{board}/jobs`: `{"name", "schedule", "action"}` —
/// action is `{"type":"http"|"poll","url","method"?,"headers"?,"body"?,
/// "timeout_ms"?,"path"?}` (zw app_jobs shape).
#[route(POST "/api/srv/{board_id}/jobs")]
async fn api_job_add(Json(req): Json<JobForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let (Some(name), Some(schedule), Some(action)) = (
        req.name.filter(|n| !n.is_empty()),
        req.schedule.as_deref().filter(|s| !s.is_empty()),
        req.action.as_ref(),
    ) else {
        return Err(bad_request("need name, schedule and action").into());
    };
    let store = app_context::<TursoStore>(cx);
    let id = store
        .job_add(raw, &name, schedule, action)
        .await
        .map_err(|e| bad_request(format!("job failed: {e}")))?;
    Ok(Json(json!({ "id": id, "ok": true })))
}

/// `GET /api/srv/{board}/jobs`.
#[route(GET "/api/srv/{board_id}/jobs")]
async fn api_job_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let jobs = store.job_list(raw).await.map_err(|e| bad_request(format!("jobs failed: {e}")))?;
    Ok(Json(json!({ "items": jobs })))
}

/// `DELETE /api/srv/{board}/jobs/{id}`.
#[route(DELETE "/api/srv/{board_id}/jobs/{job_id}")]
async fn api_job_remove(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let job_id: &str = path_param::<JobId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Ok(id) = job_id.parse::<i64>() else {
        return Err(bad_request("invalid job id").into());
    };
    let store = app_context::<TursoStore>(cx);
    let removed = store.job_remove(raw, id).await.map_err(|e| bad_request(format!("job failed: {e}")))?;
    Ok(Json(json!({ "removed": removed })))
}

/// `GET /api/srv/{board}/hooks/deliveries?status=&limit=` — retry queue.
#[route(GET "/api/srv/{board_id}/hooks/deliveries")]
async fn api_delivery_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let q = parse_query_params::<DeliveriesQuery>(cx).unwrap_or_default();
    let store = app_context::<TursoStore>(cx);
    let items = store
        .delivery_list(raw, q.status.as_deref().filter(|s| !s.is_empty()), q.limit.unwrap_or(50).clamp(1, 500))
        .await
        .map_err(|e| bad_request(format!("deliveries failed: {e}")))?;
    Ok(Json(json!({ "items": items })))
}

/// `POST /api/srv/{board}/hooks/replay`: `{"id"?}` — re-queue failed or
/// dead deliveries (all of them when no id).
#[route(POST "/api/srv/{board_id}/hooks/replay")]
async fn api_delivery_replay(body: Body, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let bytes = topcoat::router::body_limit(cx);
    let raw_body = topcoat::router::to_bytes(body, bytes).await.map_err(|e| bad_request(format!("body too large: {e}")))?;
    let req: ReplayForm = if raw_body.is_empty() {
        ReplayForm::default()
    } else {
        serde_json::from_slice(&raw_body).map_err(|e| bad_request(format!("replay body: {e}")))?
    };
    let store = app_context::<TursoStore>(cx);
    let requeued = store
        .delivery_replay(raw, req.id)
        .await
        .map_err(|e| bad_request(format!("replay failed: {e}")))?;
    Ok(Json(json!({ "requeued": requeued })))
}

/// `GET /api/srv/{board}/config` — title/schema/ttl/rate/audit + more.
#[route(GET "/api/srv/{board_id}/config")]
async fn api_config_get(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let config = store.board_config(raw).await.map_err(|e| bad_request(format!("config failed: {e}")))?;
    Ok(Json(config))
}

/// `PUT /api/srv/{board}/ttl`: `{"seconds": N, "field": "expires_at"}` —
/// records with `payload[field]` older than N seconds are hidden + swept.
#[route(PUT "/api/srv/{board_id}/ttl")]
async fn api_ttl_set(Json(req): Json<TtlForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let (Some(seconds), Some(field)) = (req.seconds, req.field.as_deref().filter(|f| !f.is_empty())) else {
        return Err(bad_request("need seconds and field").into());
    };
    if seconds <= 0 {
        return Err(bad_request("seconds must be positive").into());
    }
    let store = app_context::<TursoStore>(cx);
    store.ttl_set(raw, seconds, field).await.map_err(|e| bad_request(format!("ttl failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "seconds": seconds, "field": field })))
}

/// `DELETE /api/srv/{board}/ttl` — disable TTL expiry.
#[route(DELETE "/api/srv/{board_id}/ttl")]
async fn api_ttl_clear(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    store.ttl_clear(raw).await.map_err(|e| bad_request(format!("ttl failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// `PUT /api/srv/{board}/audit`: `{"enabled": true|false}`.
#[route(PUT "/api/srv/{board_id}/audit")]
async fn api_audit_set(Json(req): Json<EnabledForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    store
        .audit_enable(raw, req.enabled)
        .await
        .map_err(|e| bad_request(format!("audit failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "enabled": req.enabled })))
}

/// `GET /api/srv/{board}/audit?limit=` — the append-only audit trail.
#[route(GET "/api/srv/{board_id}/audit")]
async fn api_audit_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let q = parse_query_params::<LimitQuery>(cx).unwrap_or_default();
    let store = app_context::<TursoStore>(cx);
    let items = store.audit_list(raw, q.limit.unwrap_or(50).clamp(1, 1000)).await.map_err(|e| bad_request(format!("audit failed: {e}")))?;
    Ok(Json(json!({ "items": items })))
}

/// `PUT /api/srv/{board}/rate`: `{"per_minute": N}` — in-memory sliding
/// window per bucket (default bucket = "api").
#[route(PUT "/api/srv/{board_id}/rate")]
async fn api_rate_set(Json(req): Json<RateForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth_unlimited(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Some(per_minute) = req.per_minute else {
        return Err(bad_request("need per_minute").into());
    };
    if per_minute <= 0 {
        return Err(bad_request("per_minute must be positive").into());
    }
    let store = app_context::<TursoStore>(cx);
    store.rate_set(raw, per_minute).await.map_err(|e| bad_request(format!("rate failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "per_minute": per_minute })))
}

/// `DELETE /api/srv/{board}/rate` — disable rate limiting.
#[route(DELETE "/api/srv/{board_id}/rate")]
async fn api_rate_clear(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth_unlimited(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    store.rate_set(raw, 0).await.map_err(|e| bad_request(format!("rate failed: {e}")))?;
    store.rate_reset(raw).await;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/srv/{board}/dump` — the whole board (config + records +
/// hooks + links) as one JSON document; `POST /restore` puts it back.
#[route(GET "/api/srv/{board_id}/dump")]
async fn api_dump(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let dump = store.record_dump(raw).await.map_err(|e| bad_request(format!("dump failed: {e}")))?;
    Ok(Json(dump))
}

#[route(POST "/api/srv/{board_id}/restore")]
async fn api_restore(Json(dump): Json<serde_json::Value>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let counts = store.record_restore(raw, &dump).await.map_err(|e| bad_request(format!("restore failed: {e}")))?;
    Ok(Json(counts))
}

// ---------------------------------------------------------------------------
// Events (SSE) — realtime record stream with `after=`/`Last-Event-ID` resume
// ---------------------------------------------------------------------------

/// The SSE stream: replayed events first, then the live broadcast.
pub(crate) struct EventStream {
    pub replay: std::vec::IntoIter<serde_json::Value>,
    pub rx: tokio::sync::broadcast::Receiver<String>,
    pub filter: Option<String>,
}

impl futures_core::Stream for EventStream {
    type Item = Result<Event>;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        if let Some(data) = self.replay.next() {
            let id = data.get("id").and_then(serde_json::Value::as_i64).unwrap_or(0).to_string();
            let kind = data.get("event").and_then(serde_json::Value::as_str).unwrap_or("message");
            return match Event::new().event(kind).id(id).json_data(&data) {
                Ok(ev) => Poll::Ready(Some(Ok(ev))),
                Err(e) => Poll::Ready(Some(Err(e))),
            };
        }
        loop {
            match self.rx.try_recv() {
                Ok(raw) => {
                    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                    if let Some(filter) = self.filter.as_deref() {
                        let kind = value.get("event").and_then(Value::as_str).unwrap_or("");
                        if kind != filter {
                            continue;
                        }
                    }
                    let id = value.get("seq").and_then(Value::as_i64).unwrap_or(0).to_string();
                    let kind = value.get("event").and_then(Value::as_str).unwrap_or("message");
                    return match Event::new().event(kind).id(id).json_data(&value) {
                        Ok(ev) => Poll::Ready(Some(Ok(ev))),
                        Err(e) => Poll::Ready(Some(Err(e))),
                    };
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    // No poll API on this tokio's broadcast receiver: re-arm
                    // a wakeup so we re-poll shortly after events land (the
                    // spawned task never touches the receiver itself).
                    let waker = cx.waker().clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        waker.wake();
                    });
                    return Poll::Pending;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Poll::Ready(None),
            }
        }
    }
}

/// `GET /api/srv/{board}/events?after=&filter=` — the board's realtime
/// event stream (`record.created/updated/deleted`). `after` (or the
/// `Last-Event-ID` header) resumes from the newest events; `filter` keeps
/// one kind only. Public on `public_reads` boards.
#[route(GET "/api/srv/{board_id}/events")]
async fn api_events(cx: &Cx) -> Result<Sse<impl Stream<Item = Result<Event>> + use<>>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let q = parse_query_params::<EventsQuery>(cx).unwrap_or_default();
    let last_event = topcoat::router::content::sse::last_event_id(cx)
        .and_then(|s| s.parse::<i64>().ok());
    let after = q.after.or(last_event).unwrap_or(0).max(0);
    let filter = q.filter.as_deref().filter(|f| !f.is_empty()).map(str::to_owned);
    let store = app_context::<TursoStore>(cx);
    let replay = store
        .event_replay(raw, after, filter.as_deref(), 500)
        .await
        .map_err(|e| bad_request(format!("events failed: {e}")))?;
    let replay_values: Vec<Value> = replay
        .into_iter()
        .map(|(id, kind, seq, payload)| json!({
            "id": id,
            "event": kind,
            "seq": seq,
            "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
        }))
        .collect();
    let rx = store.subscribe(raw).await;
    Ok(Sse::new(EventStream {
        replay: replay_values.into_iter(),
        rx,
        filter,
    })
    .keep_alive(KeepAlive::new()))
}

// ---------------------------------------------------------------------------
// CORS preflight
// ---------------------------------------------------------------------------

fn cors_headers(cx: &Cx) -> topcoat::router::HeaderMap {
    let mut cors = topcoat::router::HeaderMap::new();
    cors.insert(
        "access-control-allow-origin",
        headers(cx)
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("*")
            .parse()
            .unwrap(),
    );
    cors.insert("access-control-allow-methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS".parse().unwrap());
    cors.insert("access-control-allow-headers", "authorization, content-type, x-srv-key, last-event-id".parse().unwrap());
    cors.insert("access-control-max-age", "86400".parse().unwrap());
    cors
}

macro_rules! cors_routes {
    ($( $name:ident => $path:literal ),* $(,)?) => {
        $(
            #[route(OPTIONS $path)]
            async fn $name(cx: &Cx) -> Result<impl IntoResponse> {
                let (cors, body) = (cors_headers(cx), Body::empty());
                Ok((cors, body))
            }
        )*
    };
}

cors_routes! {
    cors_records => "/api/srv/{board_id}/records",
    cors_record_seq => "/api/srv/{board_id}/records/{seq}",
    cors_record_bulk => "/api/srv/{board_id}/records/bulk",
    cors_record_links => "/api/srv/{board_id}/records/{seq}/links",
    cors_aggregate => "/api/srv/{board_id}/aggregate",
    cors_call => "/api/srv/{board_id}/call",
    cors_events => "/api/srv/{board_id}/events",
    cors_keys => "/api/srv/{board_id}/keys",
    cors_key_revoke => "/api/srv/{board_id}/keys/{bucket}/revoke",
    cors_hooks => "/api/srv/{board_id}/hooks",
    cors_deliveries => "/api/srv/{board_id}/hooks/deliveries",
    cors_replay => "/api/srv/{board_id}/hooks/replay",
    cors_jobs => "/api/srv/{board_id}/jobs",
    cors_job_seq => "/api/srv/{board_id}/jobs/{job_id}",
    cors_config => "/api/srv/{board_id}/config",
    cors_ttl => "/api/srv/{board_id}/ttl",
    cors_audit => "/api/srv/{board_id}/audit",
    cors_rate => "/api/srv/{board_id}/rate",
    cors_dump => "/api/srv/{board_id}/dump",
    cors_restore => "/api/srv/{board_id}/restore",
}

// ---------------------------------------------------------------------------
// MCP tool: ONE `cli` tool, zerowrapper-style (help layers + argv parsing).
// Public: catalog, doc, guide. Mutations: `POST /api/srv/mcp` behind
// `x-srv-key` — the id in the key must match the space id in the command.
// ---------------------------------------------------------------------------

/// `GET /api/srv/mcp`: help layer 0 — the single tool's overview.
#[route(GET "/api/srv/mcp")]
async fn mcp_catalog(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(json!({
        "mcp": "serverless-spaces",
        "tool": {
            "name": "cli",
            "summary": "The space CLI: deploy and manage a space (build files, records, keys, hooks) using the space id from your x-srv-key.",
            "usage": "cli <group> <verb> [args]   (groups: space, records, keys, hooks)",
            "input": {"args": ["string", "..."]},
            "auth": "header x-srv-key: <space_id>:owner:<secret> (or <space_id>:<bucket>:<secret>); every command manages that space, --space is optional",
            "helpLayers": [
                "cli / cli --help          overview (no key needed)",
                "cli about                  what a space is + how to get your key (no key needed)",
                "cli help <group>           per-group help (no key needed)",
                "cli help <group> <verb>    per-verb help — most detail (no key needed)",
                "GET /api/srv/mcp/guide     full manual"
            ],
        },
    })))
}

/// `GET /api/srv/mcp/doc/{tool}`: the single tool's doc (help layer 2).
#[route(GET "/api/srv/mcp/doc/{tool}")]
async fn mcp_tool_docs(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let tool: &str = path_param::<Tool>(cx);
    if tool != "cli" {
        return Err(not_found().into());
    }
    Ok(Json(json!({
        "tool": "cli",
        "usage": cli::GLOBAL_USAGE,
        "about": cli::ABOUT,
        "groups": ["space", "records", "keys", "hooks"],
        "example": {"tool": "cli", "params": {"args": ["space", "deploy", "--publish", "on"]}},
        "helpIsPublic": "every help layer answers without x-srv-key: cli, cli about, cli --help, cli help <group>, cli help <group> <verb>",
        "spaceIdDefaultsToKey": "commands with no --space manage the space in your x-srv-key",
        "errors": ["401 no key (commands only)", "403 role too low", "error JSON with usage/hint layers"]
    })))
}

/// `GET /api/srv/mcp/guide`: help layer 3 — the full manual.
#[route(GET "/api/srv/mcp/guide")]
async fn mcp_guide_route(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(mcp_guide()))
}

fn mcp_guide() -> serde_json::Value {
    json!({
        "title": "Serverless Spaces — MCP guide (one `cli` tool)",
        "sections": [
            {"heading": "1. What a space is",
             "body": cli::ABOUT},
            {"heading": "2. Create the space (web)",
             "body": "Sign in, open Portal → Spaces (or /sl) and create a board. The one-time owner key is shown once in the redirect URL: /sl/<space_id>?owner_key=<secret> — copy it."},
            {"heading": "3. Configure your MCP client",
             "body": "EASY MODE (no config JSON): hand the agent the ONE key string from the space page (<space_id>:owner:<secret> or an issued <space_id>:<bucket>:<token>) — it passes it as the id: cli space show --space <space_id>:owner:<secret> (or --key …). The leading part is the space id; the rest authenticates the call itself, so the client just points at the url and the agent drives the space. HEADER MODE: register one tool with header x-srv-key: <space_id>:owner:<secret> and every command manages the space in the header. Request body {\"tool\":\"cli\",\"params\":{\"args\":[...]}}. Sanity check: curl -X POST /api/srv/mcp -H \"content-type: application/json\" -d '{\"tool\":\"cli\",\"params\":{\"args\":[\"space\",\"show\",\"--space\",\"<space_id>:owner:<key>\"]}}'."},
            {"heading": "4. First call",
             "body": "cli space show → meta + record count for your space (with the key string as --space, or plain with the header). Running `cli` with no args prints the overview; all help layers are PUBLIC (no key needed): cli --help, cli help <group>, cli help <group> <verb> and /api/srv/mcp/guide — read them before you own a space."},
            {"heading": "5. Upload a build and deploy",
             "body": "For each file of your dist/: cli space files upload <path> --content <base64>. Then cli space deploy --publish on. The staging build is copied to dist/ and the space goes live at /s/<space_id>."},
            {"heading": "6. Records + webhooks",
             "body": "cli records insert/list/get/delete/count work like the HTTP API. cli hooks register lets you receive inserts via POST."},
            {"heading": "7. Troubleshooting",
             "body": "401/403 on a command — revoked or wrong-role key (cli keys issue). \"scoped to\" — your --space must match the key's space, or just omit it. \"nothing staged\" — upload files first. \"unknown space id\" — the key has no space; create one on the web at /sl."}
        ]
    })
}

/// `POST /api/srv/mcp`: run the single `cli` tool with
/// `{"tool":"cli","params":{"args":[...]}}` authenticated by `x-srv-key`.
/// Help-layer invocations (no args, `about`, `--help`, `help …`) are
/// public — answered before auth so an agent without a space can still
/// learn the tool from every help layer.
#[route(POST "/api/srv/mcp")]
async fn mcp_run(Json(req): Json<McpInvoke>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    if req.tool != "cli" {
        return Err(not_found().into());
    }
    let args = req.args_list();
    if let Some(value) = cli::help_response(&args) {
        return Ok(Json(value));
    }
    let store = app_context::<TursoStore>(cx);
    // The call may carry its own key — `--key` or `--space <composite>`
    // (the console's one copyable string <space_id>:<bucket>:<secret>); the
    // x-srv-key header is then optional, but when both exist they must name
    // the same space.
    let embedded = cli::embedded_key(&args);
    let header_token = headers(cx)
        .get("x-srv-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let (token, board_id) = match (embedded, header_token) {
        (Some((board, token)), Some(header)) => {
            let header_board = header.split(':').next().unwrap_or_default();
            if header_board != board {
                return Ok(Json(json!({
                    "status": "error",
                    "error": format!("key mismatch: the header is for {header_board}, the passed --space/--key string is for {board}"),
                    "hint": "use one key only — either the x-srv-key header or the string passed in the call"
                })));
            }
            (token, board)
        }
        (Some((board, token)), None) => (token, board),
        (None, Some(header)) => (header.to_string(), header.split(':').next().unwrap_or_default().to_string()),
        (None, None) => return Err(unauthorized().into()),
    };
    if board_row(cx, &board_id).await?.is_none() {
        return Err(not_found().into());
    }
    let Some(auth) = resolve_key(cx, &board_id, &token).await? else {
        return Err(unauthorized().into());
    };
    let cli = cli::Cli { cx, store, key_board: board_id, auth };
    match cli.run(&args).await {
        Ok(value) => Ok(Json(value)),
        Err(value) => Ok(Json(value)),
    }
}

// ---------------------------------------------------------------------------
// Native MCP over HTTP: `POST /mcp` speaks real MCP JSON-RPC (streamable-HTTP
// style) so any MCP client (opencode `type: "remote"`, Claude, …) points
// straight at the URL — no stdio bridge. ONE `cli` tool, same CLI + help
// layers as the envelope endpoint. Commands authenticate with the
// `x-srv-key` header — `<space_id>:owner:<secret>` or
// `<space_id>:<bucket>:<token>` — whose leading id IS the space being
// managed (--space is optional and must match), so the agent manages a space
// with the key alone. All help layers are public and need no key.
// ---------------------------------------------------------------------------

/// The MCP protocol version this server speaks.
const MCP_PROTOCOL: &str = "2025-06-18";

/// One JSON-RPC request body, leniently parsed (`id` optional for
/// notifications, `params` optional for `ping`).
#[derive(Deserialize)]
pub(crate) struct McpRpcMessage {
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

fn mcp_ok(id: Option<serde_json::Value>, result: serde_json::Value) -> serde_json::Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn mcp_error(id: Option<serde_json::Value>, code: i32, message: &str) -> serde_json::Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool result: text content, flagged as an error when the CLI failed.
fn mcp_tool_result(id: Option<serde_json::Value>, text: String, is_error: bool) -> serde_json::Value {
    mcp_ok(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
    )
}

/// The single `cli` tool's MCP definition (same shape as the bridge had).
fn mcp_cli_tool() -> serde_json::Value {
    json!({
        "name": "cli",
        "description": "Serverless-spaces CLI for LANsystem: deploy and manage a space (build files, records, keys, hooks). \
Syntax: cli <group> <verb> [args] with groups space/records/keys/hooks. Every command manages the space in the \
x-srv-key header (--space is optional and must match when given). NO-HEADER MODE: pass the space's key string \
as the id and the call authenticates itself — cli space show --space <space_id>:owner:<secret> (or \
--key <composite> / an issued <space_id>:<bucket>:<token>); the leading part is the space id. \
ALL help layers are PUBLIC (no key): cli, cli about, cli --help, cli help <group>, cli help <group> <verb>.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "CLI tokens, e.g. ['space','show'] or ['space','show','--space','<space_id>:owner:<secret>'] or ['help','space']"
                }
            },
            "required": ["args"]
        }
    })
}

/// `GET /mcp`: human/agent-readable catalog; MCP clients use `POST /mcp`.
#[route(GET "/mcp")]
async fn mcp_http_catalog(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(json!({
        "mcp": "serverless-spaces",
        "transport": "JSON-RPC over HTTP (MCP protocol 2025-06-18)",
        "endpoint": "POST /mcp  (initialize | tools/list | tools/call | ping)",
        "tool": mcp_cli_tool(),
        "auth": "header x-srv-key: <space_id>:owner:<secret> or <space_id>:<bucket>:<token> — commands only; each help layer is public. NO-HEADER MODE: pass the key string as the id in the call (cli space show --space <space_id>:owner:<secret> / --key) — that string IS the credential, no config JSON needed",
        "clients": [
            "opencode: {\"mcp\":{\"lansrv-spaces\":{\"type\":\"remote\",\"url\":\"http://<host>:4321/mcp\"}}}",
            "claude: mcpServers lansrv-spaces type http url http://<host>:4321/mcp"
        ]
    })))
}

/// `POST /mcp`: the real MCP endpoint — one `cli` tool, help layers public.
#[route(POST "/mcp")]
async fn mcp_http_rpc(Json(raw): Json<serde_json::Value>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    // A non-json-rpc body should still answer as a JSON-RPC parse error.
    let Ok(msg) = serde_json::from_value::<McpRpcMessage>(raw) else {
        return Ok(Json(mcp_error(None, -32700, "Parse error: expected a JSON-RPC request")));
    };
    let id = msg.id.clone();
    match msg.method.as_str() {
        "initialize" => Ok(Json(mcp_ok(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "lansrv-spaces", "version": "1.0.0" },
            }),
        ))),
        "notifications/initialized" | "notifications/cancelled" => Ok(Json(serde_json::Value::Null)),
        "ping" => Ok(Json(mcp_ok(id, json!({})))),
        "tools/list" => Ok(Json(mcp_ok(id, json!({ "tools": [mcp_cli_tool()] })))),
        "tools/call" => {
            let params = msg.params.unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            if name != "cli" {
                return Ok(Json(mcp_error(id, -32601, "Method not found: tools/call for unknown tool")));
            }
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            // Reuse the envelope's tokenizer (array of tokens or one string).
            let envelope = McpInvoke { tool: "cli".to_string(), params: arguments.as_object().cloned().unwrap_or_default() };
            let args = envelope.args_list();
            // Help layers are public — no key needed.
            if let Some(value) = cli::help_response(&args) {
                return Ok(Json(mcp_tool_result(id, serde_json::to_string(&value).unwrap_or_default(), false)));
            }
            let store = app_context::<TursoStore>(cx);
            // Commands need a key. It may be the x-srv-key header, OR the
            // key string itself passed as the id (`--key`/`--space
            // <space_id>:<bucket>:<secret>`) — the console's one copyable
            // string, so a client needs NO config JSON. When both exist
            // they must name the same space.
            let embedded = cli::embedded_key(&args);
            let header_token = headers(cx)
                .get("x-srv-key")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let (token, board_id) = match (embedded, header_token) {
                (Some((board, token)), Some(header)) => {
                    let header_board = header.split(':').next().unwrap_or_default();
                    if header_board != board {
                        let text = serde_json::to_string(&json!({
                            "status": "error",
                            "error": format!("key mismatch: the header is for {header_board}, the passed --space/--key string is for {board}"),
                            "hint": "use one key only — either the x-srv-key header or the string passed in the call"
                        })).unwrap_or_default();
                        return Ok(Json(mcp_tool_result(id, text, true)));
                    }
                    (token, board)
                }
                (Some((board, token)), None) => (token, board),
                (None, Some(header)) => (header.to_string(), header.split(':').next().unwrap_or_default().to_string()),
                (None, None) => {
                    let text = serde_json::to_string(&json!({
                        "status": "error",
                        "error": "401: no key — no x-srv-key header and no key string in the call",
                        "hint": "set the header x-srv-key: <space_id>:owner:<secret>, or pass the key string as the id: cli space show --space <space_id>:owner:<secret>; help layers need no key"
                    })).unwrap_or_default();
                    return Ok(Json(mcp_tool_result(id, text, true)));
                }
            };
            if board_row(cx, &board_id).await?.is_none() {
                let text = serde_json::to_string(&json!({
                    "status": "error",
                    "error": "404: unknown space id in key",
                    "hint": "create a space on the web at /sl (or /portal/serverless) and copy its one-time owner key"
                })).unwrap_or_default();
                return Ok(Json(mcp_tool_result(id, text, true)));
            }
            let Some(auth) = resolve_key(cx, &board_id, &token).await? else {
                let text = serde_json::to_string(&json!({
                    "status": "error",
                    "error": "401: bad or revoked key",
                    "hint": "issue a scoped key (cli keys issue) or use the owner key from the create redirect"
                })).unwrap_or_default();
                return Ok(Json(mcp_tool_result(id, text, true)));
            };
            let cli = cli::Cli { cx, store, key_board: board_id, auth };
            match cli.run(&args).await {
                Ok(value) => Ok(Json(mcp_tool_result(id, serde_json::to_string(&value).unwrap_or_default(), false))),
                Err(value) => Ok(Json(mcp_tool_result(id, serde_json::to_string(&value).unwrap_or_default(), true))),
            }
        }
        other => Ok(Json(mcp_error(id, -32601, &format!("Method not found: {other}")))),
    }
}

/// Borrow an `ApiAuth` so a checked-in value can flow through `requires()`.

#[derive(Deserialize, Default)]
pub(crate) struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub eq: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub hl: Option<String>,
}

/// `PATCH /records/{seq}` body: the JSON-patch ops object.
#[derive(Deserialize)]
pub(crate) struct PatchForm {
    pub patch: serde_json::Value,
}

/// `GET /aggregate` query params.
#[derive(Deserialize, Default)]
pub(crate) struct AggregateQuery {
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
}

/// `POST /records/bulk` body.
#[derive(Deserialize)]
pub(crate) struct BulkInsert {
    #[serde(alias = "items")]
    pub payloads: Vec<serde_json::Value>,
}

/// `DELETE /records?filter=` query param.
#[derive(Deserialize)]
pub(crate) struct FilterQuery {
    pub filter: Option<String>,
}

/// `POST /call` body.
#[derive(Deserialize)]
pub(crate) struct CallBody {
    pub url: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `POST /jobs` body.
#[derive(Deserialize)]
pub(crate) struct JobForm {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub action: Option<serde_json::Value>,
}

/// `GET /hooks/deliveries` query params.
#[derive(Deserialize, Default)]
pub(crate) struct DeliveriesQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `POST /hooks/replay` body.
#[derive(Deserialize, Default)]
pub(crate) struct ReplayForm {
    #[serde(default)]
    pub id: Option<i64>,
}

/// `PUT /ttl` body.
#[derive(Deserialize)]
pub(crate) struct TtlForm {
    pub seconds: Option<i64>,
    #[serde(default)]
    pub field: Option<String>,
}

/// `PUT /audit` body.
#[derive(Deserialize)]
pub(crate) struct EnabledForm {
    pub enabled: bool,
}

/// `PUT /rate` body.
#[derive(Deserialize)]

pub(crate) struct WebJobForm {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
}

/// `POST /sl/{board}/jobs/remove` form.
#[derive(Deserialize)]
pub(crate) struct JobIdForm {
    #[serde(default)]
    pub id: Option<i64>,
}

/// `POST /sl/{board}/audit` form — checkbox value is `on`/`off`.
#[derive(Deserialize)]
pub(crate) struct WebEnabledForm {
    #[serde(default)]
    pub enabled: Option<String>,
}

/// `POST /sl/{board}/restore` form — dump JSON in a textarea.
#[derive(Deserialize)]
pub(crate) struct RestoreForm {
    pub dump: String,
}

/// `GET /sl/{board}` — records view controls.
#[derive(Deserialize, Default)]

pub(crate) struct LimitQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `POST /records/{seq}/links` body.
#[derive(Deserialize)]
pub(crate) struct LinkForm {
    pub parent: Option<i64>,
    #[serde(default)]
    pub via: Option<String>,
}

/// `DELETE /records/{seq}/links?via=` query param.
#[derive(Deserialize)]
pub(crate) struct ViaQuery {
    pub via: Option<String>,
}

/// `GET /events?after=&filter=` query params.
#[derive(Deserialize, Default)]
pub(crate) struct EventsQuery {
    #[serde(default)]
    pub after: Option<i64>,
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Deserialize)]


pub(crate) struct ErrorQuery {
    #[serde(default)]
    pub err: Option<String>,
}

#[derive(Deserialize)]

pub(crate) struct RecordInsert {
    pub payload: serde_json::Value,
    #[serde(default)]
    pub upsert: bool,
}

#[derive(Deserialize)]
pub(crate) struct KeyMint {
    pub role: Option<String>,
    #[serde(default)]
    pub writer: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct HookMint {
    pub url: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
}

// ---------------------------------------------------------------------------
// ids
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// hardening: computed/validate/redact config, recipes, secrets, events
// ---------------------------------------------------------------------------

type ConfigSetForm = serde_json::Map<String, Value>;

/// `PUT /api/srv/{board}/config` — set/clear `computed` / `validate` /
/// `redact` rules (`null` clears a key). Rules apply on the next write.
#[route(PUT "/api/srv/{board_id}/config")]
async fn api_config_set(Json(req): Json<ConfigSetForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let mut touched: Vec<(&str, bool)> = Vec::new();
    for (key, json_key) in [("computed", "computed_json"), ("validate", "validate_json"), ("redact", "redact_json")] {
        let Some(value) = req.get(key) else { continue };
        if value.is_null() {
            let store = app_context::<TursoStore>(cx);
            store.board_config_set(raw, json_key, None).await.map_err(|e| bad_request(format!("config failed: {e}")))?;
            touched.push((key, false));
            continue;
        }
        if !value.is_array() && !value.is_object() {
            return Err(bad_request(format!("{key} must be an array or object (or null to clear)")).into());
        }
        let store = app_context::<TursoStore>(cx);
        store.board_config_set(raw, json_key, Some(value)).await.map_err(|e| bad_request(format!("config failed: {e}")))?;
        touched.push((key, true));
    }
    if touched.is_empty() {
        return Err(bad_request("nothing to set: use computed/validate/redact (null clears)").into());
    }
    Ok(Json(json!({ "ok": true, "set": touched })))
}

#[derive(Deserialize)]
pub(crate) struct RecipeForm {
    pub name: String,
    /// Event name pattern, e.g. `record.created` or `cron:ping`.
    pub when: String,
    #[serde(default)]
    pub match_json: Option<Value>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub dedup_on: Option<String>,
    /// Action list: `{"$secrets": {...}}` / `{"http": {...}}` entries.
    pub actions: Value,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub(crate) struct EnabledBody {
    pub enabled: bool,
}

#[derive(Deserialize)]
pub(crate) struct SecretBody {
    pub value: String,
}

/// `GET /api/srv/{board}/recipes` — recipe rows (actions included).
#[route(GET "/api/srv/{board_id}/recipes")]
async fn api_recipe_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let recipes = store.recipe_list(raw).await.map_err(|e| bad_request(format!("recipes failed: {e}")))?;
    Ok(Json(json!(recipes)))
}

/// `POST /api/srv/{board}/recipes` — add a recipe.
#[route(POST "/api/srv/{board_id}/recipes")]
async fn api_recipe_add(Json(req): Json<RecipeForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    if req.name.trim().is_empty() || req.when.trim().is_empty() {
        return Err(bad_request("need name and when").into());
    }
    let store = app_context::<TursoStore>(cx);
    let id = store
        .recipe_add(raw, &req.name, &req.when, req.match_json.as_ref().map(Value::to_string).as_deref(), req.enabled, req.dedup_on.as_deref(), &req.actions.to_string())
        .await
        .map_err(|e| bad_request(format!("recipe add failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "id": id, "name": req.name })))
}

/// `PUT /api/srv/{board}/recipes/{name}/enabled` — enable/disable a recipe.
#[route(PUT "/api/srv/{board_id}/recipes/{name}/enabled")]
async fn api_recipe_enabled(Json(req): Json<EnabledBody>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let name: &str = path_param::<Name>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let found = store.recipe_set_enabled(raw, name, req.enabled).await.map_err(|e| bad_request(format!("recipe failed: {e}")))?;
    if !found {
        return Err(not_found().into());
    }
    Ok(Json(json!({ "ok": true, "name": name, "enabled": req.enabled })))
}

/// `DELETE /api/srv/{board}/recipes/{name}` — remove a recipe.
#[route(DELETE "/api/srv/{board_id}/recipes/{name}")]
async fn api_recipe_remove(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let name: &str = path_param::<Name>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let found = store.recipe_remove(raw, name).await.map_err(|e| bad_request(format!("recipe failed: {e}")))?;
    if !found {
        return Err(not_found().into());
    }
    Ok(Json(json!({ "ok": true, "name": name })))
}

/// `GET /api/srv/{board}/secrets` — secret names + fingerprints only
/// (values never leave the store).
#[route(GET "/api/srv/{board_id}/secrets")]
async fn api_secret_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let secrets = store.secret_list(raw).await.map_err(|e| bad_request(format!("secrets failed: {e}")))?;
    Ok(Json(json!(secrets)))
}

/// `PUT /api/srv/{board}/secrets/{name}` — create or rotate a secret.
#[route(PUT "/api/srv/{board_id}/secrets/{name}")]
async fn api_secret_set(Json(req): Json<SecretBody>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let name: &str = path_param::<Name>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    if req.value.trim().is_empty() {
        return Err(bad_request("value must not be empty").into());
    }
    let store = app_context::<TursoStore>(cx);
    store.secret_set(raw, name, &req.value).await.map_err(|e| bad_request(format!("secret failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "name": name })))
}

/// `DELETE /api/srv/{board}/secrets/{name}` — remove a secret.
#[route(DELETE "/api/srv/{board_id}/secrets/{name}")]
async fn api_secret_remove(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let name: &str = path_param::<Name>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let found = store.secret_remove(raw, name).await.map_err(|e| bad_request(format!("secret failed: {e}")))?;
    if !found {
        return Err(not_found().into());
    }
    Ok(Json(json!({ "ok": true, "name": name })))
}

/// `POST /api/srv/{board}/events` — dispatch inbound-event recipes.
/// Body is a raw JSON payload (or `{"payload": {...}}`).
#[route(POST "/api/srv/{board_id}/events")]
async fn api_event_inbound(Json(req): Json<Value>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let payload = req.get("payload").cloned().unwrap_or(req);
    let writer = match &auth {
        Some(ApiAuth::Key(key)) => key.writer.as_deref(),
        _ => None,
    };
    let store = app_context::<TursoStore>(cx);
    store.dispatch_recipes(raw, serverless_recipe::EventKind::Inbound, None, &payload, writer, None).await;
    Ok(Json(json!({ "ok": true })))
}
