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

/// Formats a duration in seconds for display in the grid.
fn format_secs(secs: i64) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes}:{seconds:02}")
}