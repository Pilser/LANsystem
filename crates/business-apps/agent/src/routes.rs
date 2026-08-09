//! The `/api/agent` wire endpoints. Thin: dispatch into `actions::dispatch`
//! and never introduce logic here, so the catalog stays the single source of
//! truth for what an agent can do.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{content::Json, route, request::headers},
};

use crate::actions;

/// `GET /api/agent`: the action catalog as JSON, for discovery.
#[route(GET "/api/agent")]
async fn catalog(_cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(actions::catalog_json()))
}

/// `POST /api/agent`: execute `{"action": ..., "params": {...}}`.
#[route(POST "/api/agent")]
async fn execute(Json(req): Json<AgentRequest>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let key = headers(cx)
        .get("x-agent-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    Ok(Json(actions::dispatch(cx, key, &req.action, &req.params).await))
}

#[derive(Deserialize)]
struct AgentRequest {
    #[serde(rename = "action")]
    action: String,
    #[serde(default, rename = "params")]
    params: serde_json::Value,
}