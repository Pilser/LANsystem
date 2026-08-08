//! Page handlers for the media app.
//!
//! Every handler resolves the signed-in principal first (redirect to `/login`
//! when absent), then scopes the library to the user's tenant: tenant members
//! see only their tenant's items (`tenantId`), while the platform owner sees
//! everything. Rendering is fragments; the shell layout lives in `apps/lan`.

use topcoat::{
    Result,
    context::Cx,
    router::error::{RouterErrorExt, redirect},
    router::{path_param, page},
    view::view,
};

use crate::{components, models};

/// The `id` segment captured by `/media-library/{id}`, read as the raw `str`.
path_param!(id);

/// The `/media-library` home page: the signed-in user's library plus count.
#[page("/media-library")]
async fn media_home(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);

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

/// The `/media-library/{id}` page: one scoped item with a presigned URL.
#[page("/media-library/{id}")]
async fn media_detail(cx: &Cx) -> Result {
    let raw_id: &str = path_param::<Id>(cx);
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);

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