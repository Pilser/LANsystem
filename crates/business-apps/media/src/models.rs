//! HelixDB queries and row types owned by the media app.
//!
//! Per `docs/ARCHITECTURE.md` §11 the crate that owns a domain owns its
//! queries: every query here is authored against the shared graph schema
//! (`db::schema`) and executed through the `db` client. The `#[query]`
//! attribute turns the parameters of each function into a typed
//! [`QueryRequest`] describing the `read_batch`.

use db::query;
use db::schema::{edges, labels, props};
use db::{g, read_batch, NodeRef, Order, Predicate, PropertyProjection};

/// A single media item row as projected by the media queries.
///
/// Properties follow the shared schema conventions (camelCase on the wire,
/// e.g. `mediaItemId`, `releaseYear`, `streamKey`).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    pub media_item_id: Option<String>,
    pub title: Option<String>,
    pub release_year: Option<i64>,
    pub duration_sec: Option<i64>,
    pub stream_key: Option<String>,
}

/// Response shape of [`list_library`].
#[derive(Debug, serde::Deserialize)]
pub struct MediaLibrary {
    pub items: Vec<MediaItem>,
}

/// Response shape of [`media_item_by_id`].
#[derive(Debug, serde::Deserialize)]
pub struct MediaDetail {
    pub item: Vec<MediaItem>,
}

/// Response shape of [`count_library`].
#[derive(Debug, serde::Deserialize)]
pub struct MediaLibraryCount {
    pub total: i64,
}

/// Soft-deleted rows are excluded up front on every library traversal.
fn not_deleted() -> Predicate {
    Predicate::is_null(props::DELETED_AT)
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
                .project(vec![
                    PropertyProjection::new(props::MEDIA_ITEM_ID),
                    PropertyProjection::new(props::TITLE),
                    PropertyProjection::new(props::RELEASE_YEAR),
                    PropertyProjection::new(props::DURATION_SEC),
                    PropertyProjection::new(props::STREAM_KEY),
                ]),
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
                .project(vec![
                    PropertyProjection::new(props::MEDIA_ITEM_ID),
                    PropertyProjection::new(props::TITLE),
                    PropertyProjection::new(props::RELEASE_YEAR),
                    PropertyProjection::new(props::DURATION_SEC),
                    PropertyProjection::new(props::STREAM_KEY),
                ]),
        )
        .returning(["item"])
}