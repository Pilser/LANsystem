//! Page handlers for the media app.
//!
//! Each handler pulls the shared `Db`/`Storage` clients from the request
//! context (`platform_core`), runs one of the app-owned queries from
//! [`crate::models`], and renders the result with `view!`.

use topcoat::{
    Result,
    context::Cx,
    router::{page, path_param},
    view::view,
};

use crate::{components, models};

/// The `id` segment captured by `/media-library/{id}`, read as the raw `str`.
#[path_param]
struct Id(str);

/// The `/media-library` home page: library grid plus item count.
#[page("/media-library")]
async fn media_home(cx: &Cx) -> Result {
    let db = platform_core::db(cx);
    let library: models::MediaLibrary = db.query(models::list_library()?).await?;
    let count: models::MediaLibraryCount = db.query(models::count_library()?).await?;

    view! {
        <!DOCTYPE html>
        <html>
            <head>
                <title>"Media Library"</title>
            </head>
            <body>
                <header>
                    <h1>"Media Library"</h1>
                    <p>"Your library holds " (count.total) " titles."</p>
                </header>
                <main>
                    components::media_grid(items: &library.items)
                </main>
            </body>
        </html>
    }
}

/// The `/media-library/{id}` page: one item with a presigned stream URL.
#[page("/media-library/{id}")]
async fn media_detail(cx: &Cx) -> Result {
    let raw_id = path_param::<Id>(cx);
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);

    let detail: models::MediaDetail =
        db.query(models::media_item_by_id(raw_id.to_owned())?).await?;
    let item = detail.item.first();
    let stream_url = match item.and_then(|it| it.stream_key.as_deref()) {
        Some(key) => Some(storage.presign_get(key, 900).await?),
        None => None,
    };

    view! {
        <!DOCTYPE html>
        <html>
            <head>
                <title>"Media item"</title>
            </head>
            <body>
                match item {
                    Some(item) => {
                        <h1>"Now playing"</h1>
                        components::player(item: item, stream_url: stream_url.as_deref())
                    },
                    None => <p>"Item not found"</p>,
                }
            </body>
        </html>
    }
}