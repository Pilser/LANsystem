//! Shared record shapes for serverless boards, mirrored from the
//! `zerowrapper/zw-db` models so a migration path stays obvious.

use serde::{Deserialize, Serialize};

/// One serverless board (a per-tenant SQL-backed record store).
///
/// The board's *catalog* row lives in HelixDB (`ServerlessApp` label); the
/// records it points at live in a turso file on the container volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrvApp {
    pub board_id: String,
    /// Secret key shown once at creation; doubles as an unscoped `admin` key.
    pub owner_key: String,
    pub title: String,
    /// Optional JSON-path schema validated on insert.
    pub schema_json: Option<String>,
    /// Whether reads require no key.
    pub public_reads: bool,
    /// Optional payload property that makes inserts upserts.
    pub unique_key: Option<String>,
    pub record_count: i64,
    pub created_at: String,
}

/// One record inside a board's store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrvRecord {
    pub seq: i64,
    pub payload: serde_json::Value,
    pub created_at: String,
    /// The key holder (or owner) who wrote it; `None` for owner writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<String>,
    /// Optional relevance score (BM25/vector-style results).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// A scoped API key issued against a board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrvApiKey {
    /// Public key id (the "bucket"); the secret is stored salted and shown
    /// once at issue time.
    pub bucket: String,
    #[serde(rename = "role")]
    pub role: KeyRole,
    /// Fixed writer identity stamped on records written with this key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<String>,
    /// Customer scope id (role `customer`): the key may only read/create
    /// records whose `payload.customer_id == scope`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

/// What a board key may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyRole {
    /// Read records only.
    Reader,
    /// Read + write records.
    Writer,
    /// Everything, including issuing/revoking keys and hooks.
    Admin,
    /// A storefront customer: reads/writes only its own records (enforced
    /// server-side via `scope`); never staff surfaces (rules/recipes/
    /// secrets/config/keys/events).
    Customer,
}

impl KeyRole {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyRole::Reader => "reader",
            KeyRole::Writer => "writer",
            KeyRole::Admin => "admin",
            KeyRole::Customer => "customer",
        }
    }
}

/// A webhook registered against a board (delivered by the backlog task).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrvHook {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub created_at: String,
}

/// Make a path segment filesystem-safe (same rule as `zerowrapper`).
pub fn sanitize_key(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' })
        .collect()
}

/// Relative path of a board's meta file inside its directory.
pub fn app_meta_path() -> &'static str {
    "meta.toml"
}