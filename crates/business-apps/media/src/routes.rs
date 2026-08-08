//! Page handlers for the media app.
//!
//! Every handler resolves the signed-in principal first (redirect to `/login`
//! when absent), then scopes the library to the user's tenant: tenant members
//! see only their tenant's items (`tenantId`), while the platform owner sees
//! everything. Rendering is fragments; the shell layout lives in `apps/lan`.

use bytes::Bytes;
use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::content::multipart::Multipart,
    router::error::{redirect, RouterErrorExt, SeeOther, see_other},
    router::{parse_query_params, path_param, route, page},
    view::view,
};

use crate::{components, models};

/// The `id` segment captured by `/media-library/{id}`, read as the raw `str`.
path_param!(id);

/// `?error=` codes rendered by the upload page.
const UPLOAD_ERRORS: &[(&str, &str)] = &[
    ("quota", "This upload would exceed the tenant's storage quota."),
    ("file", "No file was attached, or the file was empty."),
];

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(default)]
    error: Option<String>,
}

/// The `/media-library` home page: the signed-in user's library plus count.
#[page("/media-library")]
async fn media_home(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);

    // Billing seam: the tenant's webhook (when billing is enabled) can deny
    // the library outright.
    let decision = auth
        .authorize_access(
            &current.user,
            current.tenant.as_ref(),
            "media",
            "library".to_string(),
        )
        .await?;
    if decision == platform_core::AuthDecision::Deny {
        return view! { <div> components::access_denied() </div> };
    }

    let (library, count): (models::MediaLibrary, models::MediaLibraryCount) =
        match current.user.tenant_id {
            Some(tenant_id) => (
                db.query(models::list_library_by_tenant(tenant_id)?).await?,
                db.query(models::count_library_by_tenant(tenant_id)?).await?,
            ),
            None => (
                db.query(models::list_library()?).await?,
                db.query(models::count_library()?).await?,
            ),
        };

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
            <a
                class="cursor-pointer rounded-md border border-sky-700 bg-sky-800 px-4 py-1.5 text-sm text-white transition hover:bg-sky-700"
                href="/media-library/upload"
            >
                "Upload"
            </a>
        </div>
        <main>
            if library.items.is_empty() {
                <p class="py-10 text-sm text-zinc-400">
                    "Nothing in your library yet — upload files to get started."
                </p>
            } else {
                components::media_grid(items: &library.items)
            }
        </main>
    }
}

/// The `/media-library/upload` page: title + file, posted as multipart.
#[page("/media-library/upload")]
async fn media_upload_page(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    auth.current_user(cx).await?.ok_or_redirect("/login")?;

    let query = parse_query_params::<UploadQuery>(cx);
    let error = query.ok().and_then(|q| q.error).and_then(|code| {
        UPLOAD_ERRORS
            .iter()
            .find(|(key, _)| *key == code)
            .map(|(_, message)| message.to_string())
    });

    view! {
        <a class="mb-4 inline-block text-sm text-sky-400 hover:text-sky-300" href="/media-library">
            "← Back to library"
        </a>
        <h1 class="mb-6 text-3xl font-bold">"Upload to Media Library"</h1>
        components::media_upload_form(error: error.as_deref())
    }
}

/// `POST /media-library/upload`: store the file object, record the item row,
/// gate on the tenant's quota, and land back on the item's player page.
#[route(POST "/media-library/upload")]
async fn media_upload(mut upload: Multipart, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);

    // Collect the small text fields and the file payload from the form.
    let mut title = String::new();
    let mut release_year = 0i64;
    let mut file_name: Option<String> = None;
    let mut file_bytes: Option<Bytes> = None;
    while let Some(field) = upload.next_field().await? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "title" => title = field.text().await?.trim().to_string(),
            "release_year" => {
                release_year = field.text().await?.trim().parse().unwrap_or(0);
            }
            "file" => {
                file_name = field.file_name().map(str::to_owned);
                file_bytes = Some(field.bytes().await?);
            }
            _ => {}
        }
    }

    // Empty uploads bounce back with a hint instead of a generic 400.
    let Some(file_bytes) = file_bytes.filter(|bytes| !bytes.is_empty()) else {
        return Ok(see_other("/media-library/upload?error=file"));
    };
    let size = file_bytes.len() as i64;

    // Quota gate: a tenant-owned upload must fit in the tenant's remaining
    // quota before any object is written. Platform uploads (owner, no tenant)
    // are uncapped.
    let tenant_id = current.user.tenant_id.unwrap_or(0);
    let used_after = if tenant_id > 0 {
        let info: models::TenantStorageInfo =
            db.query(models::tenant_storage_info(tenant_id)?).await?;
        let tenant = info.tenant.first();
        let used = tenant.and_then(|t| t.storage_used_bytes).unwrap_or(0);
        let quota = tenant.and_then(|t| t.storage_quota_bytes).unwrap_or(0);
        if quota > 0 && used + size > quota {
            return Ok(see_other("/media-library/upload?error=quota"));
        }
        used + size
    } else {
        0
    };

    // Stream the object into MinIO under the media key scheme, then record it.
    let id = next_media_id();
    let owner = if tenant_id > 0 {
        tenant_id.to_string()
    } else {
        "platform".to_string()
    };
    let file_name = file_name.unwrap_or_default();
    let key = storage_key(&owner, &id, &file_name);
    storage
        .put(&key, file_bytes, infer_content_type(&file_name))
        .await?;

    let _: serde_json::Value = db
        .query(models::create_media_item(
            id.clone(),
            title,
            release_year,
            0,
            key,
            tenant_id,
        )?)
        .await?;

    if tenant_id > 0 {
        let _: serde_json::Value =
            db.query(models::set_tenant_used_bytes(tenant_id, used_after)?).await?;
    }

    Ok(see_other(&format!("/media-library/{id}")))
}

/// The `/media-library/{id}` page: one scoped item with a presigned URL.
#[page("/media-library/{id}")]
async fn media_detail(cx: &Cx) -> Result {
    let raw_id: &str = path_param::<Id>(cx);
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);

    // Billing/AAA seam: the tenant's authorizer says whether this playback
    // may proceed at all.
    let decision = auth
        .authorize_access(
            &current.user,
            current.tenant.as_ref(),
            "media",
            raw_id.to_string(),
        )
        .await?;
    if decision == platform_core::AuthDecision::Deny {
        return view! { <div> components::access_denied() </div> };
    }

    let detail: models::MediaDetail =
        db.query(models::media_item_by_id(raw_id.to_owned())?).await?;
    let item = detail.item.first();

    // Tenant scope: a tenant member may only see their own tenant's items;
    // platform items (no tenant) are owner-only.
    let allowed = match (current.user.tenant_id, item.and_then(|it| it.tenant_id)) {
        (None, _) => true,
        (Some(user_tenant), Some(item_tenant)) => user_tenant == item_tenant,
        (Some(_), None) => false,
    };
    if !allowed {
        return Err(redirect("/media-library").into());
    }

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

/// A collision-resistant id for a freshly uploaded item.
fn next_media_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("mi-{millis:x}-{:04x}", random_bits())
}

fn random_bits() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_add(u16::try_from(rand16()).unwrap_or(0))
}

fn rand16() -> u16 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (now & 0xffff) as u16
}

/// The canonical object key under `media/{owner}/{id}/{file}`.
fn storage_key(owner: &str, id: &str, file_name: &str) -> String {
    let file = sanitize_file_name(file_name);
    format!("media/{owner}/{id}/{file}")
}

/// Keep only a safe base name + extension so the file name cannot escape the
/// key's path hierarchy.
fn sanitize_file_name(file_name: &str) -> String {
    let base = file_name
        .rsplit('/')
        .next()
        .unwrap_or(file_name)
        .trim()
        .to_string();
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if sanitized.len() > 120 {
        sanitized[..120].to_string()
    } else {
        sanitized
    }
}

fn infer_content_type(file_name: &str) -> &'static str {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".mp4") || lower.ends_with(".m4v") { "video/mp4" }
    else if lower.ends_with(".mp3") { "audio/mpeg" }
    else if lower.ends_with(".flac") { "audio/flac" }
    else if lower.ends_with(".webm") { "video/webm" }
    else if lower.ends_with(".ogg") { "audio/ogg" }
    else if lower.ends_with(".mkv") { "video/x-matroska" }
    else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") { "image/jpeg" }
    else if lower.ends_with(".png") { "image/png" }
    else { "application/octet-stream" }
}