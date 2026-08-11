//! HelixDB catalog queries for serverless spaces.
//!
//! Each space is one `ServerlessApp` node in the shared graph — the *catalog*
//! row — pointing at a turso file store on the container volume (`boardId`)
//! plus its deployable frontend + media in object storage under `srv/{board}/…`.
//! The node carries the salted owner-key hash: `sha256(board_id ‖ owner_key)`
//! (`boardId` is random and acts as the salt, so no extra state is needed).
//!
//! The listing fields mirror the marketplace `Product` shape: `isPublic`
//! (a real JSON boolean — graph params round-trip Rust `bool`s, see
//! `set_product_public`) gates the public card, which only shows once
//! `boardDeployedAt` is set.

use db::query;
use db::schema::{labels, props};
use db::{g, read_batch, write_batch, Expr, Order, Predicate, PropertyInput, PropertyProjection};

/// A space's catalog row as projected by the queries below.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerlessBoard {
    pub board_id: Option<String>,
    pub board_title: Option<String>,
    pub board_description: Option<String>,
    pub board_schema_json: Option<String>,
    pub board_unique_key: Option<String>,
    pub board_public_reads: Option<bool>,
    pub board_owner_key_hash: Option<String>,
    pub board_created_at: Option<i64>,
    pub board_icon_key: Option<String>,
    pub board_banner_key: Option<String>,
    pub board_gallery_keys: Option<String>,
    pub board_deployed_at: Option<i64>,
    pub tenant_id: Option<i64>,
}

/// A page of catalog rows.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardPage {
    pub items: Vec<ServerlessBoard>,
}

fn project_board() -> Vec<PropertyProjection> {
    vec![
        PropertyProjection::new(props::BOARD_ID),
        PropertyProjection::new(props::BOARD_TITLE),
        PropertyProjection::new(props::BOARD_DESCRIPTION),
        PropertyProjection::new(props::BOARD_SCHEMA_JSON),
        PropertyProjection::new(props::BOARD_UNIQUE_KEY),
        PropertyProjection::new(props::BOARD_PUBLIC_READS),
        PropertyProjection::new(props::BOARD_OWNER_KEY_HASH),
        PropertyProjection::new(props::BOARD_CREATED_AT),
        PropertyProjection::new(props::BOARD_ICON_KEY),
        PropertyProjection::new(props::BOARD_BANNER_KEY),
        PropertyProjection::new(props::BOARD_GALLERY_KEYS),
        PropertyProjection::new(props::BOARD_DEPLOYED_AT),
        PropertyProjection::new(props::TENANT_ID),
    ]
}

/// Every live space of a tenant, including drafts (manage pages).
#[query]
pub fn boards_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::BOARD_CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_board()),
        )
        .returning(["items"])
}

/// Every live space, any tenant (owner console).
#[query]
pub fn boards_all() -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::BOARD_CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_board()),
        )
        .returning(["items"])
}

/// Spaces guests can open: published **and** deployed. The public card links
/// straight to `/s/{board_id}/`, which only serves deployed builds.
#[query]
pub fn spaces_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .where_(Predicate::eq(props::BOARD_PUBLIC_READS, true))
                .where_(Predicate::is_not_null(props::BOARD_DEPLOYED_AT))
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::BOARD_CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_board()),
        )
        .returning(["items"])
}

/// Every space guests can open across all tenants — the public feed's direct
/// app entries (same filters as `spaces_by_tenant`, no tenant scope).
#[query]
pub fn spaces_all() -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::BOARD_PUBLIC_READS, true))
                .where_(Predicate::is_not_null(props::BOARD_DEPLOYED_AT))
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::BOARD_CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_board()),
        )
        .returning(["items"])
}

/// One live space by tenant + id (tenant-scoped pages).
#[query]
pub fn board_by_tenant_and_id(tenant_id: i64, board_id: String) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .where_(Predicate::eq(props::BOARD_ID, board_id))
                .where_(Predicate::is_null(props::DELETED_AT))
                .limit(1usize)
                .project(project_board()),
        )
        .returning(["items"])
}

/// One live space by id alone (key/auth resolution — board ids are globally
/// unique).
#[query]
pub fn board_by_id(board_id: String) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::BOARD_ID, board_id))
                .where_(Predicate::is_null(props::DELETED_AT))
                .limit(1usize)
                .project(project_board()),
        )
        .returning(["items"])
}

/// Register a new space's catalog row. `owner_key_hash` is
/// `sha256(board_id ‖ owner_key)` — the hash, never the key. `public_reads`
/// is a real boolean (the graph round-trips Rust `bool` params).
#[query]
pub fn create_board(
    board_id: String,
    board_title: String,
    board_description: String,
    board_schema_json: String,
    board_unique_key: String,
    board_public_reads: bool,
    board_owner_key_hash: String,
    tenant_id: i64,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "board",
            g().add_n(
                labels::SERVERLESS_APP,
                vec![
                    (props::BOARD_ID, PropertyInput::param("board_id")),
                    (props::BOARD_TITLE, PropertyInput::param("board_title")),
                    (props::BOARD_DESCRIPTION, PropertyInput::param("board_description")),
                    (props::BOARD_SCHEMA_JSON, PropertyInput::param("board_schema_json")),
                    (props::BOARD_UNIQUE_KEY, PropertyInput::param("board_unique_key")),
                    (props::BOARD_PUBLIC_READS, PropertyInput::param("board_public_reads")),
                    (props::BOARD_OWNER_KEY_HASH, PropertyInput::param("board_owner_key_hash")),
                    (props::BOARD_CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                ],
            ),
        )
        .returning(["board"])
}

/// Update a space's editable listing fields (all values are always provided
/// by the caller, which reads the current row first — `#[query]` params have
/// no `Option` support).
#[query]
pub fn update_board(
    board_id: String,
    board_title: String,
    board_description: String,
    board_schema_json: String,
    board_unique_key: String,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "board",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::BOARD_ID, board_id))
                .set_property(props::BOARD_TITLE, PropertyInput::param("board_title"))
                .set_property(props::BOARD_DESCRIPTION, PropertyInput::param("board_description"))
                .set_property(props::BOARD_SCHEMA_JSON, PropertyInput::param("board_schema_json"))
                .set_property(props::BOARD_UNIQUE_KEY, PropertyInput::param("board_unique_key")),
        )
        .returning(["board"])
}

/// Replace the space's listing media keys (icon, banner, gallery JSON array).
/// Callers read the current row first and merge, like `add_product_gallery`.
#[query]
pub fn set_board_media(
    board_id: String,
    board_icon_key: String,
    board_banner_key: String,
    board_gallery_keys: String,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "board",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::BOARD_ID, board_id))
                .set_property(props::BOARD_ICON_KEY, PropertyInput::param("board_icon_key"))
                .set_property(props::BOARD_BANNER_KEY, PropertyInput::param("board_banner_key"))
                .set_property(props::BOARD_GALLERY_KEYS, PropertyInput::param("board_gallery_keys")),
        )
        .returning(["board"])
}

/// Staff-controlled: publish / unpublish a space (the public card + `/s/`
/// route only consider published, deployed spaces).
#[query]
pub fn set_board_public(board_id: String, is_public: bool) -> db::WriteBatch {
    write_batch()
        .var_as(
            "board",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::BOARD_ID, board_id))
                .set_property(props::BOARD_PUBLIC_READS, PropertyInput::param("is_public")),
        )
        .returning(["board"])
}

/// Mark the space deployed: the stamp of the last `staging → dist` copy.
#[query]
pub fn set_board_deployed(board_id: String) -> db::WriteBatch {
    write_batch()
        .var_as(
            "board",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::BOARD_ID, board_id))
                .set_property(props::BOARD_DEPLOYED_AT, Expr::timestamp()),
        )
        .returning(["board"])
}

/// Tombstone a space's catalog row (keeps the audit trail; the store's files
/// and `srv/{board}/…` objects are removed by the caller).
#[query]
pub fn tombstone_board(board_id: String, tenant_id: i64) -> db::WriteBatch {
    write_batch()
        .var_as(
            "board",
            g().n_with_label(labels::SERVERLESS_APP)
                .where_(Predicate::eq(props::BOARD_ID, board_id))
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::DELETED_AT, Expr::timestamp()),
        )
        .returning(["board"])
}