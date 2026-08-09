//! HelixDB queries and row types owned by the marketplace app.
//!
//! The marketplace app owns `Product` and `MarketplaceOrder` nodes against
//! the shared graph schema (`db::schema`), executed through the `db` client.
//! Products carry a `tenantId` (the shop), a price in minor units, an image
//! in object storage (`productImageKey`), and a stock counter. Orders carry
//! the buyer's `userId` and a quantity.

use db::query;
use db::schema::{labels, props};
use db::{g, read_batch, write_batch, Expr, Order, Predicate, PropertyInput, PropertyProjection};

/// A single product row as projected by the catalog queries.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub product_id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub price: Option<i64>,
    pub currency: Option<String>,
    pub product_image_key: Option<String>,
    pub stock: Option<i64>,
    pub tenant_id: Option<i64>,
    pub is_public: Option<bool>,
}

/// A page of products.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductPage {
    pub items: Vec<Product>,
}

/// A catalog count.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductCount {
    pub total: i64,
}

/// An order ticket waiting for the shop tenant's staff.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderTicket {
    pub order_id: Option<String>,
    pub product_id: Option<String>,
    pub tenant_id: Option<i64>,
    /// The signed-in user who placed the order (a platform user id).
    pub user_id: Option<i64>,
    pub quantity: Option<i64>,
    pub status: Option<String>,
    pub created_at: Option<i64>,
}

/// A page of order tickets.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderPage {
    pub items: Vec<OrderTicket>,
}

fn project_product() -> Vec<PropertyProjection> {
    vec![
        PropertyProjection::new(props::PRODUCT_ID),
        PropertyProjection::new(props::TITLE),
        PropertyProjection::new(props::DESCRIPTION),
        PropertyProjection::new(props::PRICE),
        PropertyProjection::new(props::CURRENCY),
        PropertyProjection::new(props::PRODUCT_IMAGE_KEY),
        PropertyProjection::new(props::STOCK),
        PropertyProjection::new(props::TENANT_ID),
        PropertyProjection::new(props::IS_PUBLIC),
    ]
}

/// Every product of a tenant, including drafts.
#[query]
pub fn products_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::PRODUCT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_product()),
        )
        .returning(["items"])
}

/// Products visible to guests on a public shop: published, in stock status.
#[query]
pub fn shop_products_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::PRODUCT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .where_(Predicate::eq(props::IS_PUBLIC, true))
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_product()),
        )
        .returning(["items"])
}

/// One product by its id.
#[query]
pub fn product_by_id(product_id: String) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::PRODUCT)
                .where_(Predicate::eq(props::PRODUCT_ID, product_id))
                .limit(1usize)
                .project(project_product()),
        )
        .returning(["items"])
}

/// Record a new product for a tenant.
#[query]
pub fn create_product(
    product_id: String,
    title: String,
    description: String,
    price: i64,
    currency: String,
    product_image_key: String,
    stock: i64,
    tenant_id: i64,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "product",
            g().add_n(
                labels::PRODUCT,
                vec![
                    (props::PRODUCT_ID, PropertyInput::param("product_id")),
                    (props::TITLE, PropertyInput::param("title")),
                    (props::DESCRIPTION, PropertyInput::param("description")),
                    (props::PRICE, PropertyInput::param("price")),
                    (props::CURRENCY, PropertyInput::param("currency")),
                    (props::PRODUCT_IMAGE_KEY, PropertyInput::param("product_image_key")),
                    (props::STOCK, PropertyInput::param("stock")),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                    (props::STATUS, PropertyInput::from("active")),
                    (props::CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                    (props::UPDATED_AT, PropertyInput::Expr(Expr::timestamp())),
                ],
            ),
        )
        .returning(["product"])
}

/// Update a product's listing details and stock.
#[query]
pub fn update_product(
    product_id: String,
    title: String,
    description: String,
    price: i64,
    stock: i64,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "product",
            g().n_with_label(labels::PRODUCT)
                .where_(Predicate::eq(props::PRODUCT_ID, product_id))
                .set_property(props::TITLE, PropertyInput::param("title"))
                .set_property(props::DESCRIPTION, PropertyInput::param("description"))
                .set_property(props::PRICE, PropertyInput::param("price"))
                .set_property(props::STOCK, PropertyInput::param("stock"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["product"])
}

/// Tenant-admin controlled: publish / unpublish a single product.
#[query]
pub fn set_product_public(product_id: String, is_public: bool) -> db::WriteBatch {
    write_batch()
        .var_as(
            "product",
            g().n_with_label(labels::PRODUCT)
                .where_(Predicate::eq(props::PRODUCT_ID, product_id))
                .set_property(props::IS_PUBLIC, PropertyInput::param("is_public"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["product"])
}

/// Soft-delete a product.
#[query]
pub fn delete_product(product_id: String) -> db::WriteBatch {
    write_batch()
        .var_as(
            "product",
            g().n_with_label(labels::PRODUCT)
                .where_(Predicate::eq(props::PRODUCT_ID, product_id))
                .set_property(props::DELETED_AT, Expr::timestamp()),
        )
        .returning(["product"])
}

fn project_order() -> Vec<PropertyProjection> {
    vec![
        PropertyProjection::new(props::ORDER_ID),
        PropertyProjection::new(props::PRODUCT_ID),
        PropertyProjection::new(props::TENANT_ID),
        PropertyProjection::new(props::USER_ID),
        PropertyProjection::new(props::QUANTITY),
        PropertyProjection::new(props::STATUS),
        PropertyProjection::new(props::CREATED_AT),
    ]
}

/// Orders for a tenant, newest first.
#[query]
pub fn orders_by_tenant(tenant_id: i64) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MARKETPLACE_ORDER)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .order_by(props::CREATED_AT, Order::Desc)
                .limit(500usize)
                .project(project_order()),
        )
        .returning(["items"])
}

/// One order by its id.
#[query]
pub fn order_by_id(order_id: String) -> db::ReadBatch {
    read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MARKETPLACE_ORDER)
                .where_(Predicate::eq(props::ORDER_ID, order_id))
                .limit(1usize)
                .project(project_order()),
        )
        .returning(["items"])
}

/// Record an order ticket from a buyer.
#[query]
pub fn create_order(
    order_id: String,
    product_id: String,
    tenant_id: i64,
    user_id: i64,
    quantity: i64,
) -> db::WriteBatch {
    write_batch()
        .var_as(
            "order",
            g().add_n(
                labels::MARKETPLACE_ORDER,
                vec![
                    (props::ORDER_ID, PropertyInput::param("order_id")),
                    (props::PRODUCT_ID, PropertyInput::param("product_id")),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                    (props::USER_ID, PropertyInput::param("user_id")),
                    (props::QUANTITY, PropertyInput::param("quantity")),
                    (props::STATUS, PropertyInput::from("pending")),
                    (props::CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                ],
            ),
        )
        .returning(["order"])
}

/// Advance an order ticket: pending → accepted / cancelled / done.
#[query]
pub fn set_order_status(order_id: String, status: String) -> db::WriteBatch {
    write_batch()
        .var_as(
            "order",
            g().n_with_label(labels::MARKETPLACE_ORDER)
                .where_(Predicate::eq(props::ORDER_ID, order_id))
                .set_property(props::STATUS, PropertyInput::param("status"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["order"])
}