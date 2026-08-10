//! Page handlers for the marketplace app.
//!
//! Three audiences: guests browse a public shop (`/shop/{tenant_id}`) and
//! place orders; signed-in buyers place orders; tenant admins (or the owner)
//! manage products and order tickets on `/shop-manage/{tenant_id}`. Every
//! step keeps a back button to where the visitor came from.

use bytes::Bytes;
use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::content::multipart::Multipart,
    router::content::Form,
    router::error::{RouterErrorExt, SeeOther, redirect, see_other},
    router::response::IntoResponse,
    router::{page, parse_query_params, path_param, route},
    view::view,
};

use crate::models;

/// The `tenantId` segment captured by `/shop/{tenant_id}`.
path_param!(tenant_id);

/// The `productId` segment captured by `/shop/{tenant_id}/product/{product_id}`.
path_param!(product_id);

/// The `orderId` segment captured by the order-status route.
path_param!(order_id);

/// The base64url-encoded object key captured by `/blob/{encoded}`.
path_param!(blob);

/// A `tenantId` of 0 means "platform-level" (the owner has no tenant).
const NO_TENANT: i64 = 0;

/// Where a staff principal bounces after a mutation: the owner to the console
/// (into the shop feature page), a tenant admin back to their portal shop.
fn back_home(current: &platform_auth::CurrentUser) -> &'static str {
    if current.user.role() == platform_auth::Role::Owner {
        "/admin"
    } else {
        "/portal/shop"
    }
}

/// Where a shop mutation bounces: the owner to the shop management page, a
/// tenant admin back to the shop feature page of their portal.
fn shop_after(current: &platform_auth::CurrentUser, tenant_id: i64) -> String {
    if current.user.role() == platform_auth::Role::Owner {
        format!("/shop-manage/{tenant_id}")
    } else {
        "/portal/shop".to_string()
    }
}

/// Display currency for prices stored in minor units.
const CURRENCY: &str = "UGX";

/// `GET /blob/{encoded}`: stream one stored object back through the app.
/// Browsers cannot resolve the MinIO container hostname from presigned URLs,
/// so every image/video/media link in the UI goes through this proxy
/// (base64url key — object keys contain `/`, which route params cannot hold).
#[route(GET "/blob/{blob}")]
async fn blob(cx: &Cx) -> Result<topcoat::router::response::Response> {
    let encoded: &str = path_param::<Blob>(cx);
    let storage = platform_core::storage(cx);
    let key = platform_core::decode_key(encoded)
        .filter(|key| {
            key.starts_with("shop/")
                || key.starts_with("media/")
                || key.starts_with("profile/")
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

#[derive(Deserialize)]
struct ShopQuery {
    #[serde(default)]
    ordered: bool,
}

/// `GET /shop/{tenant_id}`: a tenant's public shop for guests and buyers.
#[page("/shop/{tenant_id}")]
async fn shop(cx: &Cx) -> Result {
    let raw: &str = path_param::<TenantId>(cx);
    let Ok(tenant_id) = raw.parse::<i64>() else {
        return Err(redirect("/public").into());
    };

    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let current = auth.current_user(cx).await?;

    let Some(tenant) = auth.tenant_by_id(tenant_id).await? else {
        return Err(redirect("/public").into());
    };
    if !tenant.is_public.unwrap_or(false) {
        return Err(redirect("/public").into());
    }
    let staff = current.as_ref().is_some_and(|c| {
        c.user.role() == platform_core::Role::Owner
            || (c.user.role().is_at_least(platform_core::Role::TenantAdmin)
                && c.user.tenant_id == Some(tenant_id))
    });

    let catalog: models::ProductPage = db.query(models::shop_products_by_tenant(tenant_id)?).await?;
    let ordered = parse_query_params::<ShopQuery>(cx)
        .map(|q| q.ordered)
        .unwrap_or(false);

    view! {
        <a
            class="mb-6 inline-block text-sm text-sky-400 hover:text-sky-300"
            href=("/tenant/".to_owned() + &tenant_id.to_string())
        >
            "← Back to the space"
        </a>
        <div class="mb-6 flex items-baseline gap-3">
            <h1 class="text-3xl font-bold">
                (tenant.tenant_name.as_deref().unwrap_or("Shop"))
            </h1>
            <span class="rounded-full border border-sky-700 bg-sky-900/40 px-2 py-0.5 text-xs text-sky-200">
                "Shop"
            </span>
            if staff {
                <a class="ml-auto rounded-md border border-zinc-700 px-3 py-1 text-xs text-zinc-300 transition hover:border-sky-500"
                    href=("/shop-manage/".to_owned() + &tenant_id.to_string())>
                    "Manage shop"
                </a>
            }
        </div>
        if ordered {
            <p class="mb-4 rounded-md border border-emerald-900 bg-emerald-950/60 px-3 py-2 text-sm text-emerald-300">
                "Order placed — the shop staff has been notified."
            </p>
        }
        if catalog.items.is_empty() {
            <p class="py-10 text-sm text-zinc-400">
                "This shop has nothing on the shelves yet."
            </p>
        } else {
            <ul class="grid list-none grid-cols-1 gap-4 p-0 sm:grid-cols-2 lg:grid-cols-3">
                for product in &catalog.items {
                    <li class="overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900">
                        match product.product_image_key.as_deref() {
                            Some(key) => {
                                <img
                                    class="aspect-video w-full bg-zinc-800 object-cover"
                                    src=("/blob/".to_owned() + &platform_core::encode_key(key))
                                    alt=(product.title.clone().unwrap_or_default())
                                />
                            },
                            None => { <div class="aspect-video w-full bg-zinc-800" /> },
                        }
                        let gallery: Vec<String> = product
                            .gallery_keys
                            .as_deref()
                            .and_then(|json| serde_json::from_str(json).ok())
                            .unwrap_or_default();
                        if gallery.len() > 1 {
                            <div class="flex gap-1 px-4 pt-3">
                                for key in gallery.iter().skip(1).take(8) {
                                    <img class="h-10 w-10 rounded border border-zinc-800 bg-zinc-800 object-cover"
                                        src=("/blob/".to_owned() + &platform_core::encode_key(key))
                                        alt="" loading="lazy" />
                                }
                                if gallery.len() > 9 {
                                    <span class="flex h-10 w-10 items-center justify-center rounded border border-zinc-800 bg-zinc-900 text-xs text-zinc-400">
                                        "+9"
                                    </span>
                                }
                            </div>
                        }
                        <div class="p-4">
                            <div class="flex items-center justify-between gap-2">
                                <div class="text-base font-semibold text-zinc-100">
                                    (product.title.clone().unwrap_or_default())
                                </div>
                                <span class="text-sm font-bold text-emerald-300">
                                    (price_text(product.price))
                                </span>
                            </div>
                            <p class="mt-1 text-sm text-zinc-400">
                                (product.description.clone().unwrap_or_default())
                            </p>
                            let stock = product.stock.unwrap_or(0);
                            if stock > 0 {
                                <form
                                    class="mt-3 flex items-center gap-3"
                                    method="post"
                                    action=("/shop/".to_owned() + &tenant_id.to_string() + "/product/" + &product.product_id.clone().unwrap_or_default())
                                >
                                    <select name="quantity" class="w-16 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm outline-none focus:border-sky-400">
                                        for n in 1..=stock.min(9) {
                                            <option value=(n)> (n) </option>
                                        }
                                    </select>
                                    <button
                                        class="cursor-pointer rounded-md bg-sky-600 px-3 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500"
                                    >
                                        "Request to buy"
                                    </button>
                                </form>
                            } else {
                                <p class="mt-3 text-xs font-semibold uppercase tracking-wide text-red-300">
                                    "Out of stock"
                                </p>
                            }
                        </div>
                    </li>
                }
            </ul>
        }
    }
}

/// `POST /shop/{tenant_id}/product/{product_id}`: place an order ticket.
#[route(POST "/shop/{tenant_id}/product/{product_id}")]
async fn place_order(Form(form): Form<QuantityForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);

    let raw_tenant: &str = path_param::<TenantId>(cx);
    let raw_product: &str = path_param::<ProductId>(cx);
    let Ok(tenant_id) = raw_tenant.parse::<i64>() else {
        return Ok(see_other("/public"));
    };

    // Re-read the product so the ticket is only created against a live shelf
    // item of that tenant.
    let catalog: models::ProductPage =
        db.query(models::product_by_id(raw_product.to_string())?).await?;
    let Some(product) = catalog.items.into_iter().next() else {
        return Ok(see_other(&format!("/shop/{tenant_id}")));
    };
    if product.tenant_id != Some(tenant_id)
        || !product.is_public.unwrap_or(false)
        || product.product_id.as_deref() != Some(raw_product)
    {
        return Ok(see_other(&format!("/shop/{tenant_id}")));
    }
    let stock = product.stock.unwrap_or(0);
    let quantity = form.quantity.max(1);
    if stock <= 0 || quantity > stock {
        return Ok(see_other(&format!("/shop/{tenant_id}")));
    }

    let buyer_id = current.user.user_id;
    let _: serde_json::Value = db
        .query(models::create_order(
            next_order_id(),
            raw_product.to_string(),
            tenant_id,
            buyer_id.unwrap_or(0),
            quantity,
        )?)
        .await?;

    Ok(see_other(&format!("/shop/{tenant_id}?ordered=1")))
}

/// The quantity select on a product card.
#[derive(Deserialize)]
struct QuantityForm {
    #[serde(default)]
    quantity: i64,
}

/// `POST /shop/orders/{order_id}/status`: accept / cancel / complete a ticket.
#[route(POST "/shop/orders/{order_id}/status")]
async fn set_order_status(Form(form): Form<StatusForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);

    let raw_order: &str = path_param::<OrderId>(cx);

    // Staff-only: the owner or a member of the order's tenant.
    let page: models::OrderPage = db.query(models::order_by_id(raw_order.to_string())?).await?;
    let Some(order) = page.items.into_iter().next() else {
        return Ok(see_other("/public"));
    };
    let tenant_id = order.tenant_id.unwrap_or(0);
    let is_owner = current.user.role() == platform_core::Role::Owner;
    let is_member = current
        .user
        .role()
        .is_at_least(platform_core::Role::TenantAdmin)
        && current.user.tenant_id == Some(tenant_id)
        && tenant_id != NO_TENANT;
    if !is_owner && !is_member {
        return Ok(see_other(&shop_after(&current, tenant_id)));
    }
    let status = form.status.as_str();
    if !["accepted", "cancelled", "done"].contains(&status) {
        return Ok(see_other(&shop_after(&current, tenant_id)));
    }

    let _: serde_json::Value =
        db.query(models::set_order_status(raw_order.to_string(), status.to_string())?).await?;
    Ok(see_other(&shop_after(&current, tenant_id)))
}

/// The status buttons on an order ticket.
#[derive(Deserialize)]
struct StatusForm {
    status: String,
}

/// `GET /shop-manage/{tenant_id}`: products + order tickets for the shop.
#[page("/shop-manage/{tenant_id}")]
async fn shop_manage(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);

    let raw: &str = path_param::<TenantId>(cx);
    let Ok(tenant_id) = raw.parse::<i64>() else {
        return Err(redirect("/public").into());
    };

    // Staff-only, scoped: the owner manages any tenant; a tenant admin only
    // their own.
    let is_owner = current.user.role() == platform_core::Role::Owner;
    let is_own_admin = current
        .user
        .role()
        .is_at_least(platform_core::Role::TenantAdmin)
        && current.user.tenant_id == Some(tenant_id)
        && tenant_id != NO_TENANT;
    if !is_owner && !is_own_admin {
        return Err(redirect("/admin").into());
    }

    let Some(tenant) = auth.tenant_by_id(tenant_id).await? else {
        return Err(redirect("/admin").into());
    };

    let catalog: models::ProductPage = db.query(models::products_by_tenant(tenant_id)?).await?;
    let tickets: models::OrderPage = db.query(models::orders_by_tenant(tenant_id)?).await?;

    view! {
        <a class="mb-6 inline-block text-sm text-sky-400 hover:text-sky-300" href="/admin">
            "← Back to the console"
        </a>
        <h1 class="mb-1 text-3xl font-bold">
            (tenant.tenant_name.as_deref().unwrap_or("Shop"))
        </h1>
        <p class="mb-8 text-sm text-zinc-400">
            "Catalog and order tickets for this shop."
        </p>

        <h2 class="mb-3 text-lg font-semibold">"Shelf"</h2>
        <ul class="mb-8 grid list-none grid-cols-1 gap-4 p-0 sm:grid-cols-2 lg:grid-cols-3">
            for product in &catalog.items {
                <li class="overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900">
                    match product.product_image_key.as_deref() {
                        Some(key) => {
                            <img class="aspect-video w-full bg-zinc-800 object-cover"
                                src=("/blob/".to_owned() + &platform_core::encode_key(key))
                                alt=(product.title.clone().unwrap_or_default()) />
                        },
                        None => { <div class="aspect-video w-full bg-zinc-800" /> },
                    }
                    let gallery: Vec<String> = product
                        .gallery_keys
                        .as_deref()
                        .and_then(|json| serde_json::from_str(json).ok())
                        .unwrap_or_default();
                    if !gallery.is_empty() {
                        <div class="flex gap-1 px-4 pt-3">
                            for key in gallery.iter().skip(1).take(8) {
                                <img class="h-10 w-10 rounded border border-zinc-800 bg-zinc-800 object-cover"
                                    src=("/blob/".to_owned() + &platform_core::encode_key(key))
                                    alt="" loading="lazy" />
                            }
                        </div>
                    }
                    <div class="p-4">
                        <div class="flex items-center justify-between gap-2">
                            <div class="text-base font-semibold text-zinc-100">
                                (product.title.clone().unwrap_or_default())
                            </div>
                            <span class="text-sm font-bold text-emerald-300">
                                (price_text(product.price))
                            </span>
                        </div>
                        <p class="mt-1 text-sm text-zinc-400">
                            (product.description.clone().unwrap_or_default())
                        </p>
                        <p class="mt-2 text-xs text-zinc-500">
                            "Stock: " (product.stock.unwrap_or(0))
                        </p>
                        <form
                            class="mt-3 flex items-center gap-2"
                            method="post"
                            action=("/shop/product/".to_owned() + &product.product_id.clone().unwrap_or_default() + "/photos")
                            enctype="multipart/form-data"
                        >
                            <input name="photo" type="file" accept="image/*" multiple=""
                                class="text-xs text-zinc-400" />
                            <button class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-sky-500" type="submit">
                                "Add photos"
                            </button>
                        </form>
                        <form
                            class="mt-2 flex items-center gap-2"
                            method="post"
                            action=("/shop/product/".to_owned() + &product.product_id.clone().unwrap_or_default() + "/visibility")
                        >
                            <label class="flex items-center gap-2 text-sm text-zinc-300">
                                <input
                                    type="checkbox"
                                    name="is_public"
                                    value="on"
                                    checked=(product.is_public.unwrap_or(false))
                                    class="h-4 w-4 accent-emerald-500"
                                />
                                "On the shelf"
                            </label>
                            <button class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-emerald-500" type="submit">
                                "Save"
                            </button>
                        </form>
                    </div>
                </li>
            }
        </ul>

        <h2 class="mb-3 text-lg font-semibold">"Add a product"</h2>
        <form
            class="mb-10 flex flex-wrap items-end gap-4 rounded-xl border border-zinc-800 bg-zinc-900 p-5"
            method="post"
            enctype="multipart/form-data"
            action=("/shop-manage/".to_owned() + &tenant_id.to_string() + "/product")
        >
            <label class="flex flex-col gap-1 text-sm">
                "Title"
                <input name="title" required="" class="w-48 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
            </label>
            <label class="flex flex-col gap-1 text-sm">
                "Description"
                <input name="description" class="w-64 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
            </label>
            <label class="flex flex-col gap-1 text-sm">
                "Price (UGX)"
                <input name="price_ugx" type="number" step="1" min="0" required="" class="w-28 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
            </label>
            <label class="flex flex-col gap-1 text-sm">
                "Stock"
                <input name="stock" type="number" min="0" value="0" class="w-20 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
            </label>
            <label class="flex flex-col gap-1 text-sm">
                "Photos (first is the cover)"
                <input name="photo" type="file" accept="image/*" multiple="" class="text-sm text-zinc-400" />
            </label>
            <button class="cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500">
                "Add"
            </button>
        </form>

        <h2 class="mb-3 text-lg font-semibold">"Order tickets"</h2>
        if tickets.items.is_empty() {
            <p class="py-6 text-sm text-zinc-400">
                "No orders yet."
            </p>
        } else {
            <ul class="mb-4 list-none space-y-2 p-0">
                for order in &tickets.items {
                    <li class="flex items-center gap-4 rounded-xl border border-zinc-800 bg-zinc-900 px-4 py-3">
                        <div class="min-w-0 flex-1">
                            <div class="text-sm font-semibold text-zinc-100">
                                (order.product_id.clone().unwrap_or_default())
                                " × " (order.quantity.unwrap_or(1))
                            </div>
                            <div class="text-xs text-zinc-500">
                                "Buyer #" (order.user_id.unwrap_or(0))
                                " · " (order.created_at.unwrap_or(0))
                            </div>
                        </div>
                        <span
                            class=("rounded-full border px-2 py-0.5 text-xs ".to_owned() + status_class(order.status.as_deref().unwrap_or("")))
                        >
                            (order.status.clone().unwrap_or_default())
                        </span>
                        if order.status.as_deref() == Some("pending") {
                            <form method="post" action=("/shop/orders/".to_owned() + &order.order_id.clone().unwrap_or_default() + "/status")>
                                <input type="hidden" name="status" value="accepted" />
                                <button class="cursor-pointer rounded-md border border-emerald-700 px-2 py-1 text-xs transition hover:border-emerald-400">
                                    "Accept"
                                </button>
                            </form>
                            <form method="post" action=("/shop/orders/".to_owned() + &order.order_id.clone().unwrap_or_default() + "/status")>
                                <input type="hidden" name="status" value="cancelled" />
                                <button class="cursor-pointer rounded-md border border-red-800 px-2 py-1 text-xs transition hover:border-red-400">
                                    "Cancel"
                                </button>
                            </form>
                        } else if order.status.as_deref() == Some("accepted") {
                            <form method="post" action=("/shop/orders/".to_owned() + &order.order_id.clone().unwrap_or_default() + "/status")>
                                <input type="hidden" name="status" value="done" />
                                <button class="cursor-pointer rounded-md border border-zinc-700 px-2 py-1 text-xs transition hover:border-sky-400">
                                    "Done"
                                </button>
                            </form>
                        }
                    </li>
                }
            </ul>
        }
    }
}

/// `POST /shop-manage/{tenant_id}/product`: add a product to the shelf.
#[route(POST "/shop-manage/{tenant_id}/product")]
async fn create_product(mut upload: Multipart, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);

    let raw: &str = path_param::<TenantId>(cx);
    let Ok(tenant_id) = raw.parse::<i64>() else {
        return Ok(see_other(back_home(&current)));
    };
    let is_owner = current.user.role() == platform_core::Role::Owner;
    let is_own_admin = current
        .user
        .role()
        .is_at_least(platform_core::Role::TenantAdmin)
        && current.user.tenant_id == Some(tenant_id)
        && tenant_id != NO_TENANT;
    if !is_owner && !is_own_admin {
        return Ok(see_other(back_home(&current)));
    }

    let mut title = String::new();
    let mut description = String::new();
    let mut price_ugx = String::new();
    let mut stock = 0i64;
    let mut photos: Vec<(String, Bytes)> = Vec::new();
    while let Some(field) = upload.next_field().await? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "title" => title = field.text().await?.trim().to_string(),
            "description" => description = field.text().await?.trim().to_string(),
            "price_ugx" => price_ugx = field.text().await?.trim().to_string(),
            "stock" => stock = field.text().await?.trim().parse().unwrap_or(0),
            "photo" => {
                let file_name = field.file_name().map(str::to_owned).unwrap_or_default();
                let bytes = field.bytes().await?;
                if !bytes.is_empty() {
                    photos.push((file_name, bytes));
                }
            }
            _ => {}
        }
    }
    if title.is_empty() {
        return Ok(see_other(&format!("/shop-manage/{tenant_id}")));
    }

    // Prices are entered in UGX shillings and stored as-is (minor units).
    let price = price_ugx.parse::<i64>().unwrap_or(0);

    // One id for the whole product: the photo keys and the node share it.
    let id = next_product_id();

    // Upload every photo under the shop key scheme; the first becomes the
    // cover, the rest land in the gallery (JSON array of keys).
    let mut image_keys: Vec<String> = Vec::new();
    for (index, (file_name, bytes)) in photos.into_iter().enumerate() {
        let ext = image_extension(&file_name);
        let key = format!("shop/{tenant_id}/{id}/photo-{index}.{ext}");
        storage.put(&key, bytes, infer_image_content_type(&file_name)).await?;
        image_keys.push(key);
    }
    let image_key = image_keys.first().cloned().unwrap_or_default();
    let gallery_json = serde_json::to_string(&image_keys).unwrap_or_else(|_| "[]".to_string());

    let _: serde_json::Value = db
        .query(models::create_product(
            id,
            title,
            description,
            price,
            CURRENCY.to_string(),
            image_key,
            gallery_json,
            stock,
            tenant_id,
        )?)
        .await?;

    Ok(see_other(&shop_after(&current, tenant_id)))
}

/// `POST /shop/product/{product_id}/photos`: append more photos to a
/// product's gallery (multipart `photo` fields, any number).
#[route(POST "/shop/product/{product_id}/photos")]
async fn add_product_photos(mut upload: Multipart, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);

    let raw: &str = path_param::<ProductId>(cx);
    let catalog: models::ProductPage = db.query(models::product_by_id(raw.to_string())?).await?;
    let Some(product) = catalog.items.into_iter().next() else {
        return Ok(see_other(back_home(&current)));
    };
    let tenant_id = product.tenant_id.unwrap_or(NO_TENANT);
    let is_owner = current.user.role() == platform_core::Role::Owner;
    let is_own_admin = current
        .user
        .role()
        .is_at_least(platform_core::Role::TenantAdmin)
        && current.user.tenant_id == Some(tenant_id)
        && tenant_id != NO_TENANT;
    if !is_owner && !is_own_admin {
        return Ok(see_other(back_home(&current)));
    }

    let mut photos: Vec<(String, Bytes)> = Vec::new();
    while let Some(field) = upload.next_field().await? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "photo" {
            let file_name = field.file_name().map(str::to_owned).unwrap_or_default();
            let bytes = field.bytes().await?;
            if !bytes.is_empty() {
                photos.push((file_name, bytes));
            }
        }
    }

    let mut existing: Vec<String> = product
        .gallery_keys
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    for (file_name, bytes) in photos {
        let ext = image_extension(&file_name);
        let key = format!("shop/{tenant_id}/{raw}/photo-{}.{ext}", existing.len());
        storage.put(&key, bytes, infer_image_content_type(&file_name)).await?;
        existing.push(key);
    }
    let _: serde_json::Value = db
        .query(models::add_product_gallery(
            raw.to_string(),
            serde_json::to_string(&existing).unwrap_or_else(|_| "[]".to_string()),
        )?)
        .await?;
    Ok(see_other(&shop_after(&current, tenant_id)))
}

/// The file extension to bake into a photo key so the `/blob` proxy can serve
/// a correct content type (`image/jpeg` fallback for unknown sources).
fn image_extension(file_name: &str) -> &'static str {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "png"
    } else if lower.ends_with(".gif") {
        "gif"
    } else if lower.ends_with(".webp") {
        "webp"
    } else if lower.ends_with(".svg") {
        "svg"
    } else {
        "jpg"
    }
}

/// Sniff a content type for an uploaded photo by extension; defaults to
/// `image/jpeg` like the pre-gallery behaviour.
fn infer_image_content_type(file_name: &str) -> &'static str {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "image/jpeg"
    }
}

/// `POST /shop/product/{product_id}/visibility`: publish / unpublish.
#[route(POST "/shop/product/{product_id}/visibility")]
async fn set_product_visibility(Form(form): Form<ProductVisibilityForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let db = platform_core::db(cx);

    let raw: &str = path_param::<ProductId>(cx);
    let catalog: models::ProductPage = db.query(models::product_by_id(raw.to_string())?).await?;
    let Some(product) = catalog.items.into_iter().next() else {
        return Ok(see_other(back_home(&current)));
    };
    let tenant_id = product.tenant_id.unwrap_or(NO_TENANT);
    let is_owner = current.user.role() == platform_core::Role::Owner;
    let is_own_admin = current
        .user
        .role()
        .is_at_least(platform_core::Role::TenantAdmin)
        && current.user.tenant_id == Some(tenant_id)
        && tenant_id != NO_TENANT;
    if !is_owner && !is_own_admin {
        return Ok(see_other(back_home(&current)));
    }

    let _: serde_json::Value = db
        .query(models::set_product_public(raw.to_string(), form.is_public.is_some())?)
        .await?;
    Ok(see_other(&shop_after(&current, tenant_id)))
}

/// The "On the shelf" checkbox.
#[derive(Deserialize)]
struct ProductVisibilityForm {
    #[serde(default)]
    is_public: Option<String>,
}

/// Format minor units as a currency string.
fn price_text(cents: Option<i64>) -> String {
    match cents {
        Some(c) if c > 0 => format!("UGX {c}"),
        _ => "—".to_string(),
    }
}

/// Tailwind class for an order ticket status badge.
fn status_class(status: &str) -> &'static str {
    match status {
        "pending" => "border-amber-800 bg-amber-950/40 text-amber-200",
        "accepted" => "border-sky-700 bg-sky-900/40 text-sky-200",
        "done" => "border-emerald-900 bg-emerald-950/60 text-emerald-300",
        "cancelled" => "border-red-800 bg-red-950/40 text-red-300",
        _ => "border-zinc-700 bg-zinc-800 text-zinc-400",
    }
}

fn next_product_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("pd-{millis:x}-{:04x}", random_bits())
}

fn next_order_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("od-{millis:x}-{:04x}", random_bits())
}

fn random_bits() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(u16::try_from(rand16()).unwrap_or(0))
}

fn rand16() -> u16 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (now & 0xffff) as u16
}