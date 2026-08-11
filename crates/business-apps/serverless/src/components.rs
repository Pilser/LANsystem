//! Reusable `view!` components for the serverless-space pages.

use std::collections::HashMap;

use topcoat::{
    Result,
    context::app_context,
    view::{component, view},
};

use crate::{models::ServerlessBoard, store::TursoStore};
use serverless_core::BoardStore;

/// Record count per board id, for cards that show stats. One store query per
/// board — fine for the handful of spaces a tenant or the platform has.
pub async fn board_record_counts(cx: &topcoat::context::Cx, boards: &[ServerlessBoard]) -> HashMap<String, i64> {
    let store = app_context::<TursoStore>(cx);
    let mut out = HashMap::new();
    for board in boards {
        if let Some(id) = board.board_id.as_deref() {
            let count = store.record_count(0, id).await.unwrap_or(0);
            out.insert(id.to_string(), count);
        }
    }
    out
}

/// A space overview card: the space's own public identity (icon, title,
/// copyable id, banner, gallery) plus quick actions. All build/media/records/
/// keys/hooks management happens in the space's Console (`/sl/{id}`).
#[component]
pub async fn space_manage_card(space: &ServerlessBoard, counts: &HashMap<String, i64>) -> Result {
    let board_id = space.board_id.clone().unwrap_or_default();
    let is_live = space.board_public_reads.unwrap_or(false);
    let is_deployed = space.board_deployed_at.is_some();
    let title = space.board_title.clone().unwrap_or_default();
    let record_count = counts.get(&board_id).copied().unwrap_or(0);
    let gallery: Vec<String> = space
        .board_gallery_keys
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let manage_url = |suffix: &str| format!("/sl/{board_id}{suffix}");
    let app_url = format!("/s/{board_id}");
    let icon_key = space.board_icon_key.as_deref().filter(|k| !k.is_empty());
    let banner_key = space.board_banner_key.as_deref().filter(|k| !k.is_empty());
    let initial = title.chars().next().unwrap_or('?');
    view! {
        <li class="relative flex flex-col overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900">
            <span
                class=(
                    if !is_live || !is_deployed {
                        "absolute right-3 top-3 z-10 rounded-full border border-amber-700 bg-amber-950/80 px-2 py-0.5 text-xs font-semibold text-amber-300"
                    } else {
                        "absolute right-3 top-3 z-10 rounded-full border border-emerald-900 bg-emerald-950/80 px-2 py-0.5 text-xs font-semibold text-emerald-300"
                    }
                )
            >
                if is_live && is_deployed { "LIVE" } else if is_deployed { "Offline" } else { "Draft" }
            </span>
            match banner_key {
                Some(key) => {
                    <img class="aspect-video w-full bg-zinc-800 object-cover"
                        src=("/blob/".to_owned() + &platform_core::encode_key(key))
                        alt=(title.clone()) />
                },
                None => {
                    <div class="flex aspect-video w-full items-center justify-center bg-zinc-800 text-sm text-zinc-600">
                        "No public banner yet — set it in the Console"
                    </div>
                },
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
            <div class="flex flex-1 flex-col p-4">
                <div class="flex items-start gap-3">
                    match icon_key {
                        Some(key) => {
                            <img class="h-12 w-12 shrink-0 rounded-lg border border-zinc-700 bg-zinc-800 object-cover"
                                src=("/blob/".to_owned() + &platform_core::encode_key(key))
                                alt=(title.clone()) />
                        },
                        None => {
                            <div class="flex h-12 w-12 shrink-0 items-center justify-center rounded-lg border border-zinc-700 bg-zinc-800 text-lg text-zinc-500">
                                (initial)
                            </div>
                        },
                    }
                    <div class="min-w-0 flex-1">
                        <div class="text-base font-semibold text-zinc-100">
                            (title.clone())
                        </div>
                        <div class="mt-0.5 flex items-center gap-2 text-xs text-zinc-500">
                            <code class="truncate text-zinc-500">(board_id.clone())</code>
                            <button
                                onclick=(format!("navigator.clipboard.writeText('{}')", board_id.clone()))
                                title="Copy space id"
                                class="shrink-0 cursor-pointer rounded border border-zinc-700 px-1 py-px text-[10px] text-zinc-400 transition hover:border-sky-500 hover:text-sky-300"
                            >
                                "copy"
                            </button>
                        </div>
                    </div>
                    if is_deployed {
                        <a
                            class="shrink-0 rounded-md bg-sky-600 px-3 py-1 text-xs font-semibold text-white transition hover:bg-sky-500"
                            href=(app_url.clone())
                        >
                            "Open app"
                        </a>
                    }
                </div>
                <p class="mt-2 text-sm text-zinc-400">
                    (space.board_description.clone().unwrap_or_default())
                </p>
                <p class="mt-2 text-xs text-zinc-500">
                    "records: " (record_count)
                    " · deployed: "
                    (space.board_deployed_at.map(|t| t.to_string()).unwrap_or_else(|| "never".to_string()))
                </p>
                <div class="mt-3 flex flex-wrap items-center gap-2">
                    if !is_live {
                        <form method="post" action=(manage_url("/visibility"))>
                            <input type="hidden" name="is_public" value="on" />
                            <button class="cursor-pointer rounded-md bg-emerald-600 px-3 py-1 text-xs font-semibold text-white transition hover:bg-emerald-500">
                                "Publish"
                            </button>
                        </form>
                    } else {
                        <form method="post" action=(manage_url("/visibility"))>
                            <button class="cursor-pointer rounded-md border border-amber-700 px-3 py-1 text-xs text-amber-300 transition hover:bg-amber-950/40">
                                "Unpublish"
                            </button>
                        </form>
                    }
                    <a
                        class="rounded-md border border-zinc-700 px-3 py-1 text-xs text-zinc-300 transition hover:border-sky-500"
                        href=(manage_url("/preview"))
                    >
                        "Preview build"
                    </a>
                    <a
                        class="rounded-md border border-zinc-700 px-3 py-1 text-xs text-zinc-300 transition hover:border-sky-500"
                        href=(manage_url(""))
                    >
                        "Console"
                    </a>
                </div>
                <p class="mt-3 border-t border-zinc-800 pt-3 text-xs text-zinc-500">
                    "Icon, banner, gallery, build, deploy, records, keys and hooks — all in the "
                    <a class="text-sky-400 hover:text-sky-300" href=(manage_url(""))>"Console"</a> "."
                </p>
            </div>
        </li>
    }
}
