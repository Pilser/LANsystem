//! Background workers: webhook delivery with retry backoff (2s loop), cron
//! job scheduler (15s loop) and the TTL sweeper (30s loop). Spawned once per
//! process from `TursoStore::ensure_workers`.

use crate::dsl;
use crate::store::TursoStore;
use sha2::{Digest, Sha256};
use serverless_core::BoardStore;
use std::time::Duration;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis()) as i64
}

fn sha256_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update(b":");
        }
        hasher.update(part.as_bytes());
    }
    let mut out = String::with_capacity(64);
    for b in hasher.finalize() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Deliver pending webhooks: one POST per delivery, signature header when
/// the hook has a secret, exponential backoff on failure, dead after 5 tries.
pub async fn hook_worker(store: TursoStore) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    loop {
        for board_id in store.board_ids().await {
            let due = match store.delivery_due(&board_id, now_ms(), 16).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            for delivery in due {
                let secret = store.hook_secret(&board_id, &delivery.url).await;
                let mut req = client.post(&delivery.url).header("content-type", "application/json");
                if let Some(secret) = secret {
                    let sig = sha256_hex(&[&secret, &delivery.payload]);
                    req = req.header("x-srv-signature", sig);
                }
                let outcome = match req.body(delivery.payload.clone()).send().await {
                    Ok(resp) if resp.status().is_success() => (true, format!("{}", resp.status())),
                    Ok(resp) => (false, format!("http {}", resp.status())),
                    Err(e) => (false, e.to_string()),
                };
                let _ = store
                    .delivery_mark(&board_id, delivery.id, outcome.0, &outcome.1)
                    .await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Run due cron jobs: recompute `next_run_at` *before* executing, then fire
/// the action (http post or poll-and-store), recording last status/message.
/// After the job fires, `cron:<job.name>` recipes are dispatched with the
/// stored record (or the polled record when the action stored one).
pub async fn scheduler_worker(store: TursoStore) {
    let client = reqwest::Client::builder().build().expect("reqwest client");
    loop {
        for board_id in store.board_ids().await {
            let due = match store.job_due(&board_id, now_ms(), 32).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            for job in due {
                let next = crate::cron::next_run(&job.schedule, now_ms()).unwrap_or(now_ms() + 60_000);
                if let Err(e) = store.job_reschedule(&board_id, job.id, next).await {
                    tracing::warn!("serverless job {} reschedule: {e}", job.id);
                    continue;
                }
                let (status, message, event_payload) = run_job(&client, &store, &board_id, &job).await;
                if let Err(e) = store.job_mark(&board_id, job.id, status, &message).await {
                    tracing::warn!("serverless job {} mark: {e}", job.id);
                }
                store
                    .dispatch_recipes(
                        &board_id,
                        serverless_recipe::EventKind::Cron,
                        None,
                        &event_payload.unwrap_or_else(|| serde_json::Value::Null),
                        None,
                        Some(&job.name),
                    )
                    .await;
            }
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

async fn run_job(
    client: &reqwest::Client,
    store: &TursoStore,
    board_id: &str,
    job: &crate::store::JobRow,
) -> (&'static str, String, Option<serde_json::Value>) {
    let Some(action) = job.action.as_object() else {
        return ("failed", "action is not a JSON object".to_string(), None);
    };
    let Some(kind) = action.get("type").and_then(serde_json::Value::as_str) else {
        return ("failed", "action missing `type`".to_string(), None);
    };
    let Some(url) = action.get("url").and_then(serde_json::Value::as_str) else {
        return ("failed", "action missing `url`".to_string(), None);
    };
    if !crate::netguard::valid_url(url) {
        return ("failed", "url rejected by netguard (private/loopback)".to_string(), None);
    }
    let method = action.get("method").and_then(serde_json::Value::as_str).unwrap_or("POST").to_uppercase();
    let timeout_ms = action.get("timeout_ms").and_then(serde_json::Value::as_u64).unwrap_or(10_000).clamp(500, 60_000);
    let mut req = client.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST), url);
    if let Some(headers) = action.get("headers").and_then(serde_json::Value::as_object) {
        for (k, v) in headers {
            if let Some(v) = v.as_str() {
                req = req.header(k, v);
            }
        }
    }
    if let Some(body) = action.get("body") {
        req = req.body(body.to_string());
    }
    let resp = match req.timeout(Duration::from_millis(timeout_ms)).send().await {
        Ok(r) => r,
        Err(e) => return ("failed", format!("request: {e}"), None),
    };
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return ("failed", format!("read body: {e}"), None),
    };
    if kind == "http" {
        return ("ok", format!("{} bytes", body.len()), None);
    }
    // kind == "poll": extract a JSON path from the response and store it.
    if body.len() > 2 * 1024 * 1024 {
        return ("failed", "response exceeds 2MB".to_string(), None);
    }
    let Ok(text) = String::from_utf8(body.to_vec()) else {
        return ("failed", "response is not utf-8".to_string(), None);
    };
    let Ok(response_json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ("failed", "response is not JSON".to_string(), None);
    };
    let path = action.get("path").and_then(serde_json::Value::as_str).unwrap_or("$");
    let Some(value) = dsl::json_path(&response_json, path) else {
        return ("failed", format!("path `{path}` not found"), None);
    };
    let record = serde_json::json!({
        "job_id": job.id,
        "job_name": job.name,
        "value": value,
        "polled_at": now_ms(),
    });
    match store.record_insert(crate::store::NO_TENANT, board_id, &record, Some("cron"), false).await {
        Ok(seq) => ("ok", format!("stored seq {seq}"), Some(record)),
        Err(e) => ("failed", e.to_string(), None),
    }
}

/// Drop records older than the board's TTL (lazy sweep; reads filter too).
pub async fn ttl_worker(store: TursoStore) {
    loop {
        for board_id in store.board_ids().await {
            let _ = store.ttl_sweep(&board_id).await;
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}