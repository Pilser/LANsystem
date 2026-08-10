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
    router::content::Form,
    router::error::{RouterErrorExt, SeeOther, redirect, see_other},
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

/// A `tenantId` of 0 means "platform-level" (the owner has no tenant).
const NO_TENANT: i64 = 0;

/// Where a staff principal bounces after a mutation: the owner to the console
/// (media is managed from the console's tenant page), a tenant admin back to
/// their portal media page.
fn back_for(role: platform_core::Role) -> &'static str {
    if role == platform_core::Role::Owner {
        "/admin/tenants"
    } else {
        "/portal/media"
    }
}

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
    // the library outright. Tenant staff (the tenant's own admin, or the
    // owner) always manage their own library — the webhook gates customers,
    // not the business.
    let staff = current.user.role().is_at_least(platform_core::Role::TenantAdmin)
        || current.user.role() == platform_core::Role::Owner;
    if !staff {
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
    }

    let (library, count): (models::MediaLibrary, models::MediaLibraryCount) =
        match current.user.tenant_id {
            Some(tenant_id) if tenant_id != NO_TENANT => (
                db.query(models::list_library_by_tenant(tenant_id)?).await?,
                db.query(models::count_library_by_tenant(tenant_id)?).await?,
            ),
            _ => (
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
/// The public tenant feed (the LAN landing page): no login required. Renders
/// only the tenants that opted in — no content previews, no headers.
#[page("/public")]
async fn public_directory(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let tenants = auth.public_tenants().await?;
    let signed_out = auth.current_user(cx).await?.is_none();

    view! {
        <div class="mb-6 flex items-start justify-between gap-4">
            <div>
                <h1 class="text-2xl font-bold">"Public directory"</h1>
                <p class="mt-1 text-sm text-zinc-400">
                    "Services running on this LAN."
                </p>
            </div>
            if signed_out {
                <a
                    class="rounded-md border border-sky-700 bg-sky-900/40 px-3 py-1.5 text-sm text-sky-200 transition hover:bg-sky-800/40"
                    href="/apply"
                >
                    "Create a business"
                </a>
            }
        </div>
        if tenants.is_empty() {
            <p class="py-10 text-sm text-zinc-400">
                "Nothing listed yet — tenant admins open their tenant from the Console."
            </p>
        } else {
            <ul class="grid list-none grid-cols-1 gap-4 p-0 sm:grid-cols-2 lg:grid-cols-3">
                for tenant in &tenants {
                    <li>
                        <a
                            class="block rounded-xl border border-zinc-800 bg-zinc-900 transition hover:-translate-y-0.5 hover:border-emerald-500"
                            href=("/tenant/".to_owned() + tenant.tenant_id.map(|id| id.to_string()).as_deref().unwrap_or("0"))
                        >
                            <span
                                class="rounded-full border border-emerald-900 bg-emerald-950/60 px-2 py-0.5 text-xs font-semibold text-emerald-300"
                            >
                                "Open service"
                            </span>
                            <div class="mt-3 flex flex-wrap items-center gap-2">
                                <div class="text-lg font-semibold text-zinc-100">
                                    (tenant.tenant_name.as_deref().unwrap_or("—"))
                                </div>
                                if tenant.has_app("media") {
                                    <span class="rounded-full border border-emerald-900 bg-emerald-950/60 px-2 py-0.5 text-xs font-semibold text-emerald-300">
                                        "Media"
                                    </span>
                                }
                                if tenant.has_app("marketplace") {
                                    <span class="rounded-full border border-sky-700 bg-sky-900/40 px-2 py-0.5 text-xs font-semibold text-sky-200">
                                        "Marketplace"
                                    </span>
                                }
                            </div>
                            <p class="mt-1 text-sm text-zinc-400">
                                (tenant.description.as_deref().filter(|t| !t.is_empty()).unwrap_or("A service running on this LAN"))
                            </p>
                        </a>
                    </li>
                }
            </ul>
        }
    }
}

/// A tenant's public space: their services, viewable by guests. The only
/// entry points are the landing feed (back button) and the Console.
#[page("/tenant/{id}")]
async fn tenant_space(cx: &Cx) -> Result {
    let raw_id: &str = path_param::<Id>(cx);
    let Ok(tenant_id) = raw_id.parse::<i64>() else {
        return Err(redirect("/public").into());
    };

    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);

    let tenant = auth.tenant_by_id(tenant_id).await?.ok_or_redirect("/public")?;
    if !tenant.is_public.unwrap_or(false) {
        return Err(redirect("/public").into());
    }

    let library: models::MediaLibrary = db.query(models::public_items_by_tenant(tenant_id)?).await?;

    view! {
        <a
            class="mb-6 inline-block text-sm text-sky-400 hover:text-sky-300"
            href="/public"
        >
            "← Back to the feed"
        </a>
        <h1 class="mb-1 text-3xl font-bold">
            (tenant.tenant_name.as_deref().unwrap_or("—"))
        </h1>
        <p class="mb-6 text-sm text-zinc-400">
            (tenant.description.as_deref().filter(|t| !t.is_empty()).unwrap_or("A service running on this LAN"))
        </p>
        if tenant.has_app("marketplace") {
            <a
                class="mb-6 inline-block rounded-md border border-sky-700 bg-sky-900/40 px-3 py-1.5 text-sm text-sky-200 transition hover:bg-sky-800/40"
                href=("/shop/".to_owned() + &tenant_id.to_string())
            >
                "Open the shop"
            </a>
        }
        if library.items.is_empty() {
            <p class="py-10 text-sm text-zinc-400">
                "Nothing open to guests yet."
            </p>
        } else {
            <ul class="grid list-none grid-cols-1 gap-4 p-0 sm:grid-cols-2 lg:grid-cols-3">
                for item in &library.items {
                    <li class="overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900">
                        match item.stream_key.as_deref() {
                            Some(key) => {
                                let src = "/blob/".to_owned() + &platform_core::encode_key(key);
                                <video
                                    class="aspect-video w-full bg-black object-cover"
                                    src=(src)
                                    controls=""
                                ></video>
                            },
                            None => { <div class="aspect-video w-full bg-zinc-800" /> },
                        }
                        <div class="p-4">
                            <div class="text-base font-semibold text-zinc-100">
                                (item.title.clone().unwrap_or_default())
                            </div>
                            <p class="mt-1 text-xs text-zinc-500">
                                (duration_text(item.duration_sec))
                            </p>
                        </div>
                    </li>
                }
            </ul>
        }
    }
}

/// Render a duration in seconds as a readable label.
fn duration_text(secs: Option<i64>) -> String {
    match secs {
        Some(s) if s > 0 => format!("{}m {}s", s / 60, s % 60),
        _ => "—".to_string(),
    }
}

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

    // Billing/AAA seam: the tenant's authorizer says whether this playback
    // may proceed at all. Tenant staff (admin or owner) manage their own
    // library — the webhook gates customers, not the business.
    let staff = current.user.role().is_at_least(platform_core::Role::TenantAdmin)
        || current.user.role() == platform_core::Role::Owner;
    let decision = if !staff {
        auth.authorize_access(
            &current.user,
            current.tenant.as_ref(),
            "media",
            raw_id.to_string(),
        )
        .await?
    } else {
        platform_core::AuthDecision::Allow
    };
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
        (Some(user_tenant), _) if user_tenant == NO_TENANT => true,
        (Some(user_tenant), Some(item_tenant)) => user_tenant == item_tenant,
        (Some(_), None) => false,
    };
    if !allowed {
        return Err(redirect("/media-library").into());
    }

    let stream_url = match item.and_then(|it| it.stream_key.as_deref()) {
        Some(key) => Some("/blob/".to_owned() + &platform_core::encode_key(key)),
        None => None,
    };

    // The platform owner or the owning tenant's admin may publish/unpublish
    // this item to the public surface.
    let can_manage = match (current.user.role(), item.and_then(|it| it.tenant_id)) {
        (platform_core::Role::Owner, _) => true,
        (role, Some(item_tenant)) => {
            role.is_at_least(platform_core::Role::TenantAdmin)
                && current.user.tenant_id == Some(item_tenant)
        }
        (_, None) => false,
    };
    let item_is_public = item.and_then(|it| it.is_public) == Some(true);

    // Sale state: the current user's own order on this item (if any) and
    // whether they are entitled to download the file.
    let my_order = match (raw_id, current.user.user_id) {
        (_, Some(user_id)) => {
            let orders: models::MediaOrderPage =
                db.query(models::media_order_by_item_and_user(raw_id.to_owned(), user_id)?)
                    .await?;
            orders.items.into_iter().next()
        }
        (_, None) => None,
    };
    let my_order_ok = my_order
        .as_ref()
        .and_then(|o| o.status.as_deref())
        .is_some_and(|s| matches!(s, "accepted" | "done"));
    let item_is_free = item.and_then(|it| it.price).unwrap_or(0) <= 0;
    let same_tenant = item
        .and_then(|it| it.tenant_id)
        .zip(current.user.tenant_id)
        .map_or(false, |(a, b)| a == b && a > 0);
    let can_download = !can_manage && (item_is_free || my_order_ok || same_tenant);
    let sale_cents = item.and_then(|it| it.price).unwrap_or(0);

    view! {
        <a class="mb-4 inline-block text-sm text-sky-400 hover:text-sky-300" href="/media-library">
            "← Back to library"
        </a>
        match item {
            Some(item) => {
                <h1 class="text-3xl font-bold">"Now playing"</h1>
                components::player(item: item, stream_url: stream_url.as_deref())
                if !can_manage && sale_cents > 0 {
                    <div
                        class="mt-4 flex flex-wrap items-center gap-3 rounded-xl border border-zinc-800 bg-zinc-900 px-4 py-3"
                    >
                        <span class="text-lg font-semibold text-amber-300">
                            (format_price(sale_cents))
                        </span>
                        match my_order.as_ref().and_then(|o| o.status.as_deref()) {
                            Some("pending") => {
                                <span class="text-sm text-zinc-400">"Order pending — the seller will confirm."</span>
                            },
                            Some("accepted" | "done") => {
                                <a
                                    class="rounded-md border border-emerald-700 bg-emerald-900/40 px-3 py-1 text-sm text-emerald-200 transition hover:bg-emerald-800/40"
                                    href=("/media/".to_owned() + raw_id + "/download")
                                >
                                    "Download"
                                </a>
                            },
                            Some("cancelled") | Some(_) => {
                                <form method="post" action=("/media/".to_owned() + raw_id + "/order")>
                                    <button
                                        class="cursor-pointer rounded-md bg-amber-600 px-3 py-1 text-sm font-semibold text-white transition hover:bg-amber-500"
                                    >
                                        "Buy again"
                                    </button>
                                </form>
                            },
                            None => {
                                <form method="post" action=("/media/".to_owned() + raw_id + "/order")>
                                    <button
                                        class="cursor-pointer rounded-md bg-amber-600 px-3 py-1 text-sm font-semibold text-white transition hover:bg-amber-500"
                                    >
                                        "Buy"
                                    </button>
                                </form>
                            },
                        }
                    </div>
                } else if !can_manage && can_download {
                    <div class="mt-4 rounded-xl border border-zinc-800 bg-zinc-900 px-4 py-3">
                        <a
                            class="inline-block rounded-md border border-emerald-700 bg-emerald-900/40 px-3 py-1 text-sm text-emerald-200 transition hover:bg-emerald-800/60"
                            href=("/media/".to_owned() + raw_id + "/download")
                        >
                            "Download"
                        </a>
                    </div>
                }
                if can_manage {
                    <form
                        class="mt-6 flex items-center gap-3 rounded-xl border border-zinc-800 bg-zinc-900 px-4 py-3"
                        method="post"
                        action=("/media-library/".to_owned() + raw_id + "/visibility")
                    >
                        <label class="flex items-center gap-2 text-sm text-zinc-300">
                            <input
                                type="checkbox"
                                name="is_public"
                                value="on"
                                checked=(item_is_public)
                                class="h-4 w-4 accent-emerald-500"
                            />
                            "Show on the public directory"
                        </label>
                        <button
                            class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-emerald-500"
                            type="submit"
                        >
                            "Publish settings"
                        </button>
                    </form>
                    <form
                        class="mt-3 flex flex-wrap items-end gap-3 rounded-xl border border-zinc-800 bg-zinc-900 px-4 py-3"
                        method="post"
                        action=("/media-library/".to_owned() + raw_id + "/price")
                    >
                        <label class="flex flex-col gap-1 text-sm text-zinc-300">
                            "Sale price (UGX, 0 = free)"
                            <input
                                class="w-32 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400"
                                type="number"
                                name="price_ugx"
                                min="0"
                                step="1"
                                value=(sale_cents)
                            />
                        </label>
                        <button
                            class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-emerald-500"
                            type="submit"
                        >
                            "Set price"
                        </button>
                    </form>
                }
            },
            None => <p class="py-10 text-sm text-zinc-400">"Item not found"</p>,
        }
    }
}

#[derive(Deserialize)]
struct VisibilityForm {
    #[serde(default)]
    is_public: Option<String>,
}

/// `POST /media-library/{id}/visibility`: flip an item's public flag.
#[route(POST "/media-library/{id}/visibility")]
async fn item_visibility(Form(form): Form<VisibilityForm>, cx: &Cx) -> Result<SeeOther> {
    let raw_id: &str = path_param::<Id>(cx);
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);

    let detail: models::MediaDetail =
        db.query(models::media_item_by_id(raw_id.to_owned())?).await?;
    let item = detail.item.first();

    let allowed = match (current.user.role(), item.and_then(|it| it.tenant_id)) {
        (platform_core::Role::Owner, _) => true,
        (role, Some(item_tenant)) => {
            role.is_at_least(platform_core::Role::TenantAdmin)
                && current.user.tenant_id == Some(item_tenant)
        }
        (_, None) => false,
    };
    if !allowed {
        return Ok(see_other(&format!("/media-library/{raw_id}")));
    }

    // Unchecked checkboxes aren't submitted — absence means "private".
    let is_public = form.is_public.is_some();
    let _: serde_json::Value =
        db.query(models::set_item_public(raw_id.to_owned(), is_public)?).await?;
    Ok(see_other(&format!("/media-library/{raw_id}")))
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

/// `POST /media/{id}/order`: a signed-in user asks for an item; the owning
/// tenant's admins settle it from their portal sales board. Free items and
/// already-entitled users are redirected straight to the item page.
#[route(POST "/media/{id}/order")]
async fn media_order(cx: &Cx) -> Result<SeeOther> {
    let raw_id: &str = path_param::<Id>(cx);
    let auth = platform_core::auth(cx);
    let Some(current) = auth.current_user(cx).await? else {
        return Ok(see_other("/login"));
    };
    let Some(user_id) = current.user.user_id else {
        return Ok(see_other(&format!("/media-library/{raw_id}")));
    };
    let db = platform_core::db(cx);

    let detail: models::MediaDetail =
        db.query(models::media_item_by_id(raw_id.to_owned())?).await?;
    let Some(item) = detail.item.first() else {
        return Ok(see_other(&format!("/media-library/{raw_id}")));
    };
    let Some(tenant_id) = item.tenant_id.filter(|t| *t > 0) else {
        return Ok(see_other(&format!("/media-library/{raw_id}")));
    };
    let buyable = item.is_public == Some(true)
        || current.user.tenant_id.map_or(false, |t| t == tenant_id);
    if !buyable {
        return Ok(see_other(&format!("/media-library/{raw_id}")));
    }
    if item.price.unwrap_or(0) <= 0 {
        return Ok(see_other(&format!("/media-library/{raw_id}")));
    }

    let pattern: models::MediaOrderPage =
        db.query(models::media_order_by_item_and_user(raw_id.to_owned(), user_id)?)
            .await?;
    if let Some(order) = pattern.items.first() {
        if matches!(order.status.as_deref(), Some("accepted" | "done")) {
            return Ok(see_other(&format!("/media-library/{raw_id}")));
        }
        if order.status.as_deref() == Some("pending") {
            return Ok(see_other(&format!("/media-library/{raw_id}")));
        }
    }

    let order_id = format!("mo-{}-{:x}", raw_id, std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0));
    let _: serde_json::Value = db
        .query(models::create_media_order(
            order_id,
            raw_id.to_owned(),
            tenant_id,
            user_id,
        )?)
        .await?;
    Ok(see_other(&format!("/media-library/{raw_id}")))
}

/// Stable ticket statuses mirroring the marketplace: pending → accepted |
/// cancelled, then done by the seller's staff.
const MEDIA_ORDER_STATUSES: &[(&str, &str)] = &[
    ("accepted", "Accept"),
    ("cancelled", "Cancel"),
    ("done", "Mark done"),
];

/// `POST /media/orders/<id>/status`: the owning tenant's admin (or the
/// platform owner) settles a purchase ticket.
#[route(POST "/media/orders/{id}/status")]
async fn media_order_status(Form(form): Form<OrderStatusForm>, cx: &Cx) -> Result<SeeOther> {
    let raw_id: &str = path_param::<Id>(cx);
    let auth = platform_core::auth(cx);
    let Some(current) = auth.current_user(cx).await? else {
        return Ok(see_other("/login"));
    };
    let db = platform_core::db(cx);

    let tenant_id = current.user.tenant_id.filter(|t| *t > 0);
    let is_owner = current.user.role() == platform_core::Role::Owner;
    if !is_owner && tenant_id.is_none() {
        return Ok(see_other(back_for(current.user.role())));
    }
    let allowed_status = MEDIA_ORDER_STATUSES
        .iter()
        .any(|(value, _)| *value == form.status);
    if !allowed_status {
        return Ok(see_other(back_for(current.user.role())));
    }
    if !is_owner {
        let orders: models::MediaOrderPage =
            db.query(models::media_orders_by_tenant(tenant_id.unwrap_or(0))?).await?;
        if !orders.items.iter().any(|o| o.order_id.as_deref() == Some(raw_id)) {
            return Ok(see_other(back_for(current.user.role())));
        }
    }
    let _: serde_json::Value =
        db.query(models::set_media_order_status(raw_id.to_owned(), form.status.clone())?)
            .await?;
    Ok(see_other(back_for(current.user.role())))
}

#[derive(Deserialize)]
struct OrderStatusForm {
    status: String,
}

/// The file itself for an entitled user. Free items and items with an
/// `accepted`/`done` order presign the stored object and bounce there.
/// (The presigned URL is the download: it expires in 15 minutes.)
#[route(GET "/media/{id}/download")]
async fn media_download(cx: &Cx) -> Result {
    let raw_id: &str = path_param::<Id>(cx);
    let auth = platform_core::auth(cx);
    let Some(current) = auth.current_user(cx).await? else {
        return Err(redirect("/login").into());
    };
    let db = platform_core::db(cx);

    let detail: models::MediaDetail =
        db.query(models::media_item_by_id(raw_id.to_owned())?).await?;
    let Some(item) = detail.item.first() else {
        return Err(redirect("/media-library").into());
    };
    let Some(key) = item.stream_key.as_deref() else {
        return Err(redirect("/media-library").into());
    };

    if item.price.unwrap_or(0) > 0 {
        let staff = current
            .user
            .tenant_id
            .zip(item.tenant_id)
            .map_or(false, |(a, b)| a == b && a > 0);
        if !staff {
            let orders: models::MediaOrderPage = db
                .query(models::media_order_by_item_and_user(
                    raw_id.to_owned(),
                    current.user.user_id.unwrap_or(0),
                )?)
                .await?;
            if !orders.items.iter().any(|o| {
                matches!(o.status.as_deref(), Some("accepted" | "done"))
            }) {
                return Err(redirect(&format!("/media-library/{raw_id}")).into());
            }
        }
    }

    let url = "/blob/".to_owned() + &platform_core::encode_key(key);
    Err(redirect(&url).into())
}

/// Format minor units as a currency string.
fn format_price(price: i64) -> String {
    format!("UGX {price}")
}

/// `POST /media-library/<id>/price`: the owning tenant's admin (or platform
/// owner) sets the item's sale price in UGX shillings (0 = free).
#[route(POST "/media-library/{id}/price")]
async fn media_price(Form(form): Form<PriceForm>, cx: &Cx) -> Result<SeeOther> {
    let raw_id: &str = path_param::<Id>(cx);
    let auth = platform_core::auth(cx);
    let Some(current) = auth.current_user(cx).await? else {
        return Ok(see_other("/login"));
    };
    let db = platform_core::db(cx);

    let detail: models::MediaDetail =
        db.query(models::media_item_by_id(raw_id.to_owned())?).await?;
    let Some(item) = detail.item.first() else { return Ok(see_other("/media-library")) };

    let can_manage = match (current.user.role(), item.tenant_id) {
        (platform_core::Role::Owner, _) => true,
        (role, Some(item_tenant)) if item_tenant > 0 => {
            role.is_at_least(platform_core::Role::TenantAdmin)
                && current.user.tenant_id == Some(item_tenant)
        }
        _ => false,
    };
    if !can_manage {
        return Ok(see_other(&format!("/media-library/{raw_id}")));
    }
    let _: serde_json::Value =
        db.query(models::set_item_price(raw_id.to_owned(), form.price_ugx)?).await?;
    Ok(see_other(&format!("/media-library/{raw_id}")))
}

#[derive(Deserialize)]
struct PriceForm {
    price_ugx: i64,
}