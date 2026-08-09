//! HelixDB queries and row types owned by the media app.
//!
//! Per `docs/ARCHITECTURE.md` §11 the crate that owns a domain owns its
//! queries: every query here is authored against the shared graph schema
//! (`db::schema`) and executed through the `db` client. The `#[query]`
//! attribute turns the parameters of each function into a typed
//! [`QueryRequest`] describing the `read_batch`.

use db::query;
use db::schema::{edges, labels, props};
use db::{g, read_batch, write_batch, Expr, NodeRef, Order, Predicate, PropertyInput, PropertyProjection};

/// A single media item row as projected by the media queries.
///
/// Properties follow the shared schema conventions (camelCase on the wire,
/// e.g. `mediaItemId`, `releaseYear`, `streamKey`).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    pub media_item_id: Option<String>,
    pub title: Option<String>,
    pub release_year: Option<i64>,
    pub duration_sec: Option<i64>,
    pub stream_key: Option<String>,
    /// The tenant that owns this item; `None`/absent = platform item only
    /// visible to the platform owner.
    pub tenant_id: Option<i64>,
    /// Tenant-admin controlled: shown on the tenant's public surface.
    pub is_public: Option<bool>,
    /// Sale price in minor units (`currency`); absent/0 = free to view and
    /// download. Set by the owning tenant's admins from their portal.
    pub price: Option<i64>,
    /// ISO-4217 currency code for [`MediaItem::price`].
    pub currency: Option<String>,
}

/// Response shape of [`list_library`].
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MediaLibrary {
    pub items: Vec<MediaItem>,
}

/// Response shape of [`media_item_by_id`].
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MediaDetail {
    pub item: Vec<MediaItem>,
}

/// Response shape of [`count_library`].
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MediaLibraryCount {
    pub total: i64,
}

/// Soft-deleted rows are excluded up front on every library traversal.
fn not_deleted() -> Predicate {
    Predicate::is_null(props::DELETED_AT)
}

/// The canonical item projection: id, meta, storage key, tenancy, sale price.
fn project_item() -> Vec<PropertyProjection> {
    vec![
        PropertyProjection::new(props::MEDIA_ITEM_ID),
        PropertyProjection::new(props::TITLE),
        PropertyProjection::new(props::RELEASE_YEAR),
        PropertyProjection::new(props::DURATION_SEC),
        PropertyProjection::new(props::STREAM_KEY),
        PropertyProjection::new(props::TENANT_ID),
        PropertyProjection::new(props::IS_PUBLIC),
        PropertyProjection::new(props::PRICE),
        PropertyProjection::new(props::CURRENCY),
    ]
}

/// Public library: non-deleted media items, newest release year first.
#[query]
pub fn list_library() -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(not_deleted())
                .order_by(props::RELEASE_YEAR, Order::Desc)
                .limit(100usize)
                .project(project_item()),
        )
        .returning(["items"])
}

/// One tenant's library: items whose `tenantId` matches, newest first.
#[query]
pub fn list_library_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .order_by(props::RELEASE_YEAR, Order::Desc)
                .limit(100usize)
                .project(project_item()),
        )
        .returning(["items"])
}

/// Number of titles in the public library.
#[query]
pub fn count_library() -> db::ReadBatch {
    read_batch()
        .var_as(
            "total",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(not_deleted())
                .count(),
        )
        .returning(["total"])
}

/// Number of titles in one tenant's library.
#[query]
pub fn count_library_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "total",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .count(),
        )
        .returning(["total"])
}

/// Media items owned by one user, anchored on the `User` node's unique
/// `userId` index, then traversed `BELONGS_TO` to the library.
#[query]
pub fn list_media_by_user(user_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "user",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USER_ID, user_id)),
        )
        .var_as(
            "items",
            g().n(NodeRef::var("user"))
                .out(Some(edges::BELONGS_TO))
                .where_(not_deleted())
                .value_map(Some(vec![
                    props::MEDIA_ITEM_ID,
                    props::TITLE,
                    props::RELEASE_YEAR,
                    props::DURATION_SEC,
                ])),
        )
        .returning(["items"])
}

/// One media item by its `mediaItemId`, for the `/media-library/{id}` page.
#[query]
pub fn media_item_by_id(media_item_id: String) -> db::ReadBatch {
    read_batch()
        .var_as(
            "item",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::eq(props::MEDIA_ITEM_ID, media_item_id))
                .project(project_item()),
        )
        .returning(["item"])
}

/// A tenant's storage accounting, read before an upload so the quota gate can
/// be enforced at the app layer.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantStorage {
    pub storage_quota_bytes: Option<i64>,
    pub storage_used_bytes: Option<i64>,
}

/// Response shape of [`tenant_storage_info`].
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TenantStorageInfo {
    pub tenant: Vec<TenantStorage>,
}

#[query]
pub fn tenant_storage_info(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .project(vec![
                    PropertyProjection::new(props::STORAGE_QUOTA_BYTES),
                    PropertyProjection::new(props::STORAGE_USED_BYTES),
                ]),
        )
        .returning(["tenant"])
}

/// Create a media item row that points at an object already stored in S3.
/// `tenant_id` 0 = platform-level item (owner upload).
#[query]
pub fn create_media_item(
    media_item_id: String,
    title: String,
    release_year: i64,
    duration_sec: i64,
    stream_key: String,
    tenant_id: i64,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "item",
            g().add_n(
                labels::MEDIA_ITEM,
                vec![
                    (props::MEDIA_ITEM_ID, PropertyInput::param("media_item_id")),
                    (props::TITLE, PropertyInput::param("title")),
                    (props::RELEASE_YEAR, PropertyInput::param("release_year")),
                    (props::DURATION_SEC, PropertyInput::param("duration_sec")),
                    (props::STREAM_KEY, PropertyInput::param("stream_key")),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                    (props::STATUS, PropertyInput::from("active")),
                    (props::CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                    (props::UPDATED_AT, PropertyInput::Expr(Expr::timestamp())),
                ],
            ),
        )
        .returning(["item"])
}

/// Carry a tenant's `storageUsedBytes` after an upload.
#[query]
pub fn set_tenant_used_bytes(tenant_id: i64, used_bytes: i64) -> db::WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::STORAGE_USED_BYTES, PropertyInput::param("used_bytes"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["tenant"])
}
/// The most recent public item of a tenant, used as the directory card
/// thumbnail.
#[query]
pub fn first_public_item_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "item",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .where_(Predicate::eq(props::IS_PUBLIC, true))
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::CREATED_AT, Order::Desc)
                .limit(1usize)
                .project(vec![
                    PropertyProjection::new(props::MEDIA_ITEM_ID),
                    PropertyProjection::new(props::TITLE),
                    PropertyProjection::new(props::STREAM_KEY),
                ]),
        )
        .returning(["item"])
}

/// Published, live titles of a tenant, shown to guests on its space page.
#[query]
pub fn public_items_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .where_(Predicate::eq(props::IS_PUBLIC, true))
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::RELEASE_YEAR, Order::Desc)
                .limit(100usize)
                .project(project_item()),
        )
        .returning(["items"])
}

/// Tenant-admin controlled: publish / unpublish a single item. Only the
/// owning tenant's admins (or the platform owner) reach this query.
#[query]
pub fn set_item_public(media_item_id: String, is_public: bool) -> db::WriteBatch {
    write_batch()
        .var_as(
            "item",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::eq(props::MEDIA_ITEM_ID, media_item_id))
                .set_property(props::IS_PUBLIC, PropertyInput::param("is_public"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["item"])
}

/// Tenant-admin SET: the item's sale price in minor units (`price` 0 = free).
#[query]
pub fn set_item_price(media_item_id: String, price: i64) -> db::WriteBatch {
    write_batch()
        .var_as(
            "item",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::eq(props::MEDIA_ITEM_ID, media_item_id))
                .set_property(props::PRICE, PropertyInput::param("price"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["item"])
}

/// A media purchase ticket: a signed-in user asked for the item. Statuses
/// mirror the marketplace: `pending` → `accepted`|`cancelled`, then `done`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaOrder {
    pub order_id: Option<String>,
    pub media_item_id: Option<String>,
    pub tenant_id: Option<i64>,
    /// The platform user who placed the order.
    pub user_id: Option<i64>,
    pub status: Option<String>,
    pub created_at: Option<i64>,
}

/// Response shape of the order queries.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MediaOrderPage {
    pub items: Vec<MediaOrder>,
}

fn project_order() -> Vec<PropertyProjection> {
    vec![
        PropertyProjection::new(props::ORDER_ID),
        PropertyProjection::new(props::MEDIA_ITEM_ID),
        PropertyProjection::new(props::TENANT_ID),
        PropertyProjection::new(props::USER_ID),
        PropertyProjection::new(props::STATUS),
        PropertyProjection::new(props::CREATED_AT),
    ]
}

/// The buyer's own order on one item (at most one row in practice).
#[query]
pub fn media_order_by_item_and_user(media_item_id: String, user_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ORDER)
                .where_(Predicate::eq(props::MEDIA_ITEM_ID, media_item_id))
                .where_(Predicate::eq(props::USER_ID, user_id))
                .limit(1usize)
                .project(project_order()),
        )
        .returning(["items"])
}

/// One media order by id (agent protocol / status lookups).
#[query]
pub fn media_order_by_id(order_id: String) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ORDER)
                .where_(Predicate::eq(props::ORDER_ID, order_id))
                .limit(1usize)
                .project(project_order()),
        )
        .returning(["items"])
}

/// Every order a buyer has placed (their purchases page).
#[query]
pub fn media_orders_by_user(user_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ORDER)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .order_by(props::CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_order()),
        )
        .returning(["items"])
}

/// Every order against one tenant's items: the portal sales board.
#[query]
pub fn media_orders_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ORDER)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .order_by(props::CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_order()),
        )
        .returning(["items"])
}

/// Place an order: a new `pending` ticket for the buying user.
#[query]
pub fn create_media_order(
    order_id: String,
    media_item_id: String,
    tenant_id: i64,
    user_id: i64,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "order",
            g().add_n(
                labels::MEDIA_ORDER,
                vec![
                    (props::ORDER_ID, PropertyInput::param("order_id")),
                    (props::MEDIA_ITEM_ID, PropertyInput::param("media_item_id")),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                    (props::USER_ID, PropertyInput::param("user_id")),
                    (props::STATUS, PropertyInput::from("pending")),
                    (props::CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                    (props::UPDATED_AT, PropertyInput::Expr(Expr::timestamp())),
                ],
            ),
        )
        .returning(["order"])
}

/// Tenant staff flips a sales ticket's status (accept / cancel / done).
#[query]
pub fn set_media_order_status(order_id: String, status: String) -> db::WriteBatch {
    write_batch()
        .var_as(
            "order",
            g().n_with_label(labels::MEDIA_ORDER)
                .where_(Predicate::eq(props::ORDER_ID, order_id))
                .set_property(props::STATUS, PropertyInput::param("status"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["order"])
}
