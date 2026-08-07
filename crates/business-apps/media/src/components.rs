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
        <ul class="media-grid">
            for (index, item) in items.iter().enumerate() {
                <li class="media-card" data-index=(index)>
                    <h2 class="media-title">(item.title.clone())</h2>
                    <p class="media-meta">
                        "Released: "
                        (item.release_year)
                    </p>
                    <p class="media-meta">
                        "Duration: "
                        if let Some(secs) = item.duration_sec {
                            (format_secs(secs))
                        }
                    </p>
                </li>
            }
        </ul>
    }
}

/// Inline player for one media item, used by the `/media-library/{id}` page.
#[component]
pub async fn player(item: &MediaItem, stream_url: Option<&str>) -> Result {
    view! {
        <figure class="media-player">
            <figcaption>(item.title.clone())</figcaption>
            <video src=(stream_url) controls=""></video>
        </figure>
    }
}

/// Formats a duration in seconds for display in the grid.
fn format_secs(secs: i64) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes}:{seconds:02}")
}