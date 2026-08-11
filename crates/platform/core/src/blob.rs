//! Platform file proxy: `GET /blob/{encoded}` streams stored objects to the
//! browser.
//!
//! Browsers cannot resolve the MinIO container hostname from presigned URLs,
//! so every image/video link in the UI goes through this route (base64url
//! key — object keys contain `/`, which route params cannot hold). It is
//! platform infrastructure, not any single business app's concern, hence it
//! lives here: every app's `blob_url`/`encode_key` helper points at it.

use topcoat::{
    Result,
    context::Cx,
    router::error::redirect,
    router::response::IntoResponse,
    router::{path_param, route},
};

/// The base64url-encoded object key captured by `/blob/{encoded}`.
path_param!(blob);

/// `GET /blob/{encoded}`: stream one stored object back through the app.
///
/// Only keys under the platform's known object prefixes are served; anything
/// else bounces to the directory feed.
#[route(GET "/blob/{blob}")]
async fn blob(cx: &Cx) -> Result<topcoat::router::response::Response> {
    let encoded: &str = path_param::<Blob>(cx);
    let storage = super::storage(cx);
    let key = crate::decode_key(encoded)
        .filter(|key| {
            key.starts_with("shop/")
                || key.starts_with("media/")
                || key.starts_with("profile/")
                || key.starts_with("srv/")
        })
        .ok_or_else(|| topcoat::Error::from(redirect("/public")))?;
    let Some(bytes) = storage.get_bytes(&key).await? else {
        return Err(topcoat::Error::from(redirect("/public")));
    };
    let content_type = match key.split('.').next_back() {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    };
    ((
        [("content-type", content_type), ("cache-control", "public, max-age=3600")],
        bytes,
    )
        .into_response(cx))
}