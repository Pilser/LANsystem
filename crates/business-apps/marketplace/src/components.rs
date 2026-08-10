//! Reusable `view!` components for the marketplace pages.

use topcoat::{
    Result,
    view::{component, view},
};

use crate::models::Product;

/// A product edit card for tenant staff: cover + gallery preview exactly like
/// the public shop cards, a one-click shelf toggle, photo upload, inline edit
/// details and delete.
#[component]
pub async fn product_manage_card(product: &Product) -> Result {
    let is_off_shelf = !product.is_public.unwrap_or(false);
    let gallery: Vec<String> = product
        .gallery_keys
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let manage_url = |suffix: &str| {
        "/shop/product/".to_owned() + &product.product_id.clone().unwrap_or_default() + suffix
    };
    view! {
        <li class="relative overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900">
            <span
                class=(
                    if is_off_shelf {
                        "absolute right-3 top-3 z-10 rounded-full border border-amber-700 bg-amber-950/80 px-2 py-0.5 text-xs font-semibold text-amber-300"
                    } else {
                        "absolute right-3 top-3 z-10 rounded-full border border-emerald-900 bg-emerald-950/80 px-2 py-0.5 text-xs font-semibold text-emerald-300"
                    }
                )
            >
                if is_off_shelf { "Draft — not on the shelf" } else { "On the shelf" }
            </span>
            match product.product_image_key.as_deref() {
                Some(key) => {
                    <img class="aspect-video w-full bg-zinc-800 object-cover"
                        src=("/blob/".to_owned() + &platform_core::encode_key(key))
                        alt=(product.title.clone().unwrap_or_default()) />
                },
                None => { <div class="aspect-video w-full bg-zinc-800" /> },
            }
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
                <p class="mt-2 text-xs text-zinc-500">
                    "Stock: " (product.stock.unwrap_or(0))
                </p>
                <div class="mt-3 flex flex-wrap items-center gap-2">
                    if is_off_shelf {
                        <form method="post"
                            action=(manage_url("/visibility"))
                        >
                            <input type="hidden" name="is_public" value="on" />
                            <button class="cursor-pointer rounded-md bg-emerald-600 px-3 py-1 text-xs font-semibold text-white transition hover:bg-emerald-500">
                                "Push to shelf"
                            </button>
                        </form>
                    } else {
                        <form method="post"
                            action=(manage_url("/visibility"))
                        >
                            <button class="cursor-pointer rounded-md border border-amber-700 px-3 py-1 text-xs text-amber-300 transition hover:bg-amber-950/40">
                                "Take off shelf"
                            </button>
                        </form>
                    }
                    <form
                        class="flex items-center gap-2"
                        method="post"
                        action=(manage_url("/photos"))
                        enctype="multipart/form-data"
                    >
                        <input name="photo" type="file" accept="image/*" multiple=""
                            class="text-xs text-zinc-400" />
                        <button class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-sky-500" type="submit">
                            "Add photos"
                        </button>
                    </form>
                </div>
                <details class="mt-3 rounded-lg border border-zinc-800 bg-zinc-950">
                    <summary class="cursor-pointer px-3 py-2 text-xs font-semibold text-zinc-300 transition hover:text-zinc-100">
                        "Edit details · delete"
                    </summary>
                    <div class="border-t border-zinc-800 p-3">
                        <form method="post" action=(manage_url("/update"))
                            class="flex flex-wrap items-end gap-3"
                        >
                            <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                "Title"
                                <input name="title" required="" value=(product.title.clone().unwrap_or_default())
                                    class="w-40 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm outline-none focus:border-sky-400" />
                            </label>
                            <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                "Price (UGX)"
                                <input name="price_ugx" type="number" min="0" value=(product.price.unwrap_or(0))
                                    class="w-24 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm outline-none focus:border-sky-400" />
                            </label>
                            <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                "Stock"
                                <input name="stock" type="number" min="0" value=(product.stock.unwrap_or(0))
                                    class="w-20 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm outline-none focus:border-sky-400" />
                            </label>
                            <textarea name="description" rows="2"
                                class="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm outline-none focus:border-sky-400"
                            >(product.description.clone().unwrap_or_default())</textarea>
                            <button class="cursor-pointer rounded-md bg-sky-600 px-3 py-1 text-xs font-semibold text-white transition hover:bg-sky-500">
                                "Save changes"
                            </button>
                        </form>
                        <form method="post" action=(manage_url("/delete"))
                            class="mt-3 border-t border-zinc-800 pt-3"
                            onsubmit="return confirm('Delete this product and its photos?')"
                        >
                            <button class="cursor-pointer rounded-md border border-red-900 px-3 py-1 text-xs text-red-300 transition hover:bg-red-950/40">
                                "Delete product"
                            </button>
                        </form>
                    </div>
                </details>
            </div>
        </li>
    }
}

/// Format minor units as a currency string.
pub fn price_text(cents: Option<i64>) -> String {
    match cents {
        Some(c) if c > 0 => format!("UGX {c}"),
        _ => "—".to_string(),
    }
}