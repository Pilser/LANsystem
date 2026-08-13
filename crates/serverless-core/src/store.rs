//! The storage seam every serverless plugin talks to.

use crate::types::{KeyRole, SrvApp, SrvApiKey, SrvHook, SrvRecord};
use crate::Result;

/// A per-tenant SQL-backed board store (turso file per board).
///
/// Implementations own the connection lifecycle (lazy open, idle eviction,
/// quota via `PRAGMA max_page_count`); callers never see paths or SQL.
/// `async_trait` pins `Send + Sync` futures so the store can be called from
/// tokio tasks.
#[async_trait::async_trait]
pub trait BoardStore: Send + Sync {
    /// Open an existing board, or create + migrate it on first access.
    async fn board_open(
        &self,
        tenant_id: i64,
        board_id: &str,
        owner_key: &str,
        title: &str,
        schema_json: Option<&str>,
        public_reads: bool,
        unique_key: Option<&str>,
    ) -> Result<SrvApp>;

    /// Fetch a board's catalog row (for portal pages and key auth).
    async fn board_get(&self, tenant_id: i64, board_id: &str) -> Result<Option<SrvApp>>;

    /// Update a board's editable fields (title/schema/visibility/unique key).
    async fn board_update(
        &self,
        tenant_id: i64,
        board_id: &str,
        title: Option<&str>,
        schema_json: Option<Option<&str>>,
        public_reads: Option<bool>,
        unique_key: Option<Option<&str>>,
    ) -> Result<SrvApp>;

    /// Permanently drop a board: records, keys, hooks. The caller is
    /// responsible for deleting the backing file + MinIO objects.
    async fn board_delete(&self, tenant_id: i64, board_id: &str) -> Result<()>;

    // ---- records -------------------------------------------------------

    /// Insert a record; with `upsert` it replaces the row holding the board's
    /// `unique_key` value. Returns the new `seq`.
    async fn record_insert(
        &self,
        tenant_id: i64,
        board_id: &str,
        payload: &serde_json::Value,
        writer: Option<&str>,
        upsert: bool,
    ) -> Result<i64>;

    /// Replace a record's payload wholesale.
    async fn record_set(
        &self,
        tenant_id: i64,
        board_id: &str,
        seq: i64,
        payload: &serde_json::Value,
        writer: Option<&str>,
    ) -> Result<()>;

    async fn record_get(
        &self,
        tenant_id: i64,
        board_id: &str,
        seq: i64,
    ) -> Result<Option<SrvRecord>>;

    /// Newest-first page; `before` = exclusive upper seq bound for cursoring.
    async fn record_list(
        &self,
        tenant_id: i64,
        board_id: &str,
        limit: i64,
        before: Option<i64>,
        offset: i64,
    ) -> Result<Vec<SrvRecord>>;

    /// Count rows (portal card stat).
    async fn record_count(&self, tenant_id: i64, board_id: &str) -> Result<i64>;

    /// Delete one record (real DELETE — turso, not the append-only graph).
    async fn record_delete(&self, tenant_id: i64, board_id: &str, seq: i64) -> Result<bool>;

    // ---- keys ----------------------------------------------------------

    /// Issue a key; returns `(bucket, secret)` — the secret is shown once.
    /// `scope` binds a `customer` key to a customer id (records are
    /// filtered/enforced to `payload.customer_id == scope`); None for
    /// reader/writer/admin.
    async fn key_issue(
        &self,
        tenant_id: i64,
        board_id: &str,
        role: KeyRole,
        writer: Option<&str>,
        scope: Option<&str>,
    ) -> Result<(String, String)>;

    async fn key_list(&self, tenant_id: i64, board_id: &str) -> Result<Vec<SrvApiKey>>;

    async fn key_revoke(&self, tenant_id: i64, board_id: &str, bucket: &str) -> Result<bool>;

    /// Resolve a Bearer secret to its board + key (salted hash lookup).
    async fn key_auth(&self, secret: &str) -> Result<Option<(String, SrvApiKey)>>;

    // ---- hooks ---------------------------------------------------------

    async fn hook_register(
        &self,
        tenant_id: i64,
        board_id: &str,
        url: &str,
        secret: Option<&str>,
    ) -> Result<()>;

    async fn hook_remove(&self, tenant_id: i64, board_id: &str, url: &str) -> Result<bool>;

    async fn hook_list(&self, tenant_id: i64, board_id: &str) -> Result<Vec<SrvHook>>;
}