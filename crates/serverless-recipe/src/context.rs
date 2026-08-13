use crate::rule::EventKind;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct EventContext {
    pub kind: EventKind,
    pub board_id: String,
    pub seq: Option<i64>,
    pub payload: Value,
    pub writer: Option<String>,
    pub cron_name: Option<String>,
}

impl EventContext {
    pub fn record_created(board_id: &str, seq: i64, payload: &Value, writer: Option<&str>) -> Self {
        Self { kind: EventKind::Created, board_id: board_id.to_string(), seq: Some(seq), payload: payload.clone(), writer: writer.map(|s| s.to_string()), cron_name: None }
    }

    pub fn record_updated(board_id: &str, seq: i64, payload: &Value, writer: Option<&str>) -> Self {
        Self { kind: EventKind::Updated, board_id: board_id.to_string(), seq: Some(seq), payload: payload.clone(), writer: writer.map(|s| s.to_string()), cron_name: None }
    }

    pub fn record_deleted(board_id: &str, seq: i64, payload: &Value, writer: Option<&str>) -> Self {
        Self { kind: EventKind::Deleted, board_id: board_id.to_string(), seq: Some(seq), payload: payload.clone(), writer: writer.map(|s| s.to_string()), cron_name: None }
    }

    pub fn inbound(board_id: &str, payload: &Value) -> Self {
        Self { kind: EventKind::Inbound, board_id: board_id.to_string(), seq: None, payload: payload.clone(), writer: None, cron_name: None }
    }

    pub fn cron(board_id: &str, job_name: &str, payload: &Value) -> Self {
        Self { kind: EventKind::Cron, board_id: board_id.to_string(), seq: None, payload: payload.clone(), writer: None, cron_name: Some(job_name.to_string()) }
    }
}