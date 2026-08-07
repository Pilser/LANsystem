//! Page handlers for the media app.
//!
//! Each handler pulls the shared `Db`/`Storage` clients from the request
//! context (`platform_core`), runs one of the app-owned queries from
//! [`crate::models`], and renders the result with `view!`.
//!
//! These handlers render content fragments only — the document shell (styles,
//! htmx, top bar) comes from the `#[layout("/")]` in `apps/lan`. When the
//! layout sees an htmx request it returns the fragment alone, so the
//! `hx-get` refresh button below swaps just its section.

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
        <div class="mb-8 flex flex-wrap items-baseline gap-4">
            <h1 class="text-3xl font-bold">"Media Library"</h1>
            <p class="text-sm text-zinc-400">
                "Your library holds " (count.total) " titles."
            </p>
            <button
                class="ml-auto cursor-pointer rounded-md border border-zinc-700 bg-zinc-800 px-4 py-1.5 text-sm text-zinc-100 transition hover:border-sky-400"
                hx-get="/media-library"
                hx-target="#main-content"
                hx-swap="innerHTML"
            >
                "Refresh"
            </button>
        </div>
        <main>
            if library.items.is_empty() {
                <p class="py-10 text-sm text-zinc-400">
                    "Nothing in the library yet — upload files to get started."
                </p>
            } else {
                components::media_grid(items: &library.items)
            }
        </main>
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
        <a class="mb-4 inline-block text-sm text-sky-400 hover:text-sky-300" href="/media-library">
            "← Back to library"
        </a>
        match item {
            Some(item) => {
                <h1 class="text-3xl font-bold">"Now playing"</h1>
                components::player(item: item, stream_url: stream_url.as_deref())
            },
            None => <p class="py-10 text-sm text-zinc-400">"Item not found"</p>,
        }
    }
}