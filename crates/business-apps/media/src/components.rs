//! Reusable `view!` components for the media app pages.

use topcoat::{
    Result,
    view::{component, view},
};

use crate::models::MediaItem;

/// Grid of media cards rendered on the `/media` home page.
#[component]
pub async fn media_grid(items: &[MediaItem]) -> Result {
    view! {
        <ul class="grid list-none grid-cols-1 gap-4 p-0 sm:grid-cols-2 lg:grid-cols-3">
            for item in items.iter() {
                <li>
                    <a
                        class="block rounded-xl border border-zinc-800 bg-zinc-900 p-5 transition hover:-translate-y-0.5 hover:border-sky-400"
                        href=(
                            match item.media_item_id.as_deref() {
                                Some(id) => "/media-library/".to_owned() + id,
                                None => "/media-library".to_owned(),
                            }
                        )
                    >
                        <h2 class="mb-2 text-lg font-semibold text-zinc-100">
                            (item.title.clone())
                        </h2>
                        <p class="text-sm text-zinc-400">
                            "Released: "
                            (item.release_year)
                        </p>
                        <p class="text-sm text-zinc-400">
                            "Duration: "
                            if let Some(secs) = item.duration_sec {
                                (format_secs(secs))
                            }
                        </p>
                    </a>
                </li>
            }
        </ul>
    }
}

/// Inline player for one media item, used by the `/media-library/{id}` page.
#[component]
pub async fn player(item: &MediaItem, stream_url: Option<&str>) -> Result {
    view! {
        <figure class="mt-8">
            <figcaption class="mb-3 text-lg font-semibold">
                (item.title.clone())
            </figcaption>
            <video
                class="block w-full max-w-3xl rounded-xl bg-black"
                src=(stream_url)
                controls=""
            ></video>
        </figure>
    }
}

/// A denied-access state rendered when the tenant's billing authorizer (or
/// local policy) says no to a media access.
#[component]
pub async fn access_denied() -> Result {
    view! {
        <div class="grid max-w-md gap-3 rounded-xl border border-amber-800 bg-amber-950/40 p-6">
            <h2 class="text-xl font-semibold text-amber-200">"Access denied"</h2>
            <p class="text-sm text-amber-300/80">
                "Your tenant's billing authorization rejected this media request. \
                 Watch access is on hold until your provider re-enables it."
            </p>
        </div>
    }
}

/// File upload form. Submits `multipart/form-data` to `/media-library/upload`
/// with htmx so the library card list is the only thing that re-renders.
#[component]
pub async fn media_upload_form(error: Option<&str>) -> Result {
    view! {
        if let Some(message) = error {
            <p class="mb-4 rounded-md border border-red-800 bg-red-950/60 px-4 py-3 text-sm text-red-200">
                (message)
            </p>
        }
        <form
            class="grid w-full max-w-md gap-4"
            hx-post="/media-library/upload"
            hx-encoding="multipart/form-data"
            hx-target="#main-content"
            hx-swap="innerHTML"
        >
            <label class="grid gap-1 text-sm text-zinc-300">
                "Title"
                <input
                    class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100"
                    type="text"
                    name="title"
                    required=""
                />
            </label>
            <label class="grid gap-1 text-sm text-zinc-300">
                "Release year"
                <input
                    class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100"
                    type="number"
                    name="release_year"
                    min="1900"
                    max="2100"
                    placeholder="2026"
                />
            </label>
            <label class="grid gap-1 text-sm text-zinc-300">
                "File"
                <input
                    class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100"
                    type="file"
                    name="file"
                    required=""
                />
            </label>
            <button
                class="rounded-md border border-sky-600 bg-sky-800 px-4 py-2 text-sm font-semibold text-white transition hover:bg-sky-700"
                type="submit"
            >
                "Upload"
            </button>
        </form>
    }
}

/// Formats a duration in seconds for display in the grid.
fn format_secs(secs: i64) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes}:{seconds:02}")
}