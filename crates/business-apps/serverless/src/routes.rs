//! Page + JSON API routes for serverless boards.
//!
//! Two audiences:
//! - Tenant pages at `/sl` (session auth): list boards, create a board
//!   (owner key shown once), view records, manage keys + hooks, delete.
//! - The JSON API at `/api/srv/{board}/*` (Bearer keys or the owner key):
//!   records CRUD, key management, hooks. Reads on `public_reads` boards
//!   need no key at all.

use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use topcoat::{
    Result,
    context::{Cx, app_context},
    router::{
        Body,
        content::Json,
        content::multipart::Multipart,
        content::sse::{Event, KeepAlive, Sse},
        error::{RouterErrorExt, SeeOther, bad_request, forbidden, not_found, redirect, see_other, too_many_requests, unauthorized},
        page, parse_query_params, path_param, route,
    },
    router::content::Form,
    router::request::headers,
    router::response::IntoResponse,
    view::view,
};

use crate::{cli, files, models, store::TursoStore};
use futures_core::Stream;
use serverless_core::{BoardStore, KeyRole};

// The `boardId` segment captured by `/sl/{board_id}` and the API routes.
path_param!(board_id);

// The `tenantId` segment of the public gallery.
path_param!(tenant_id);

// The `seq` segment of the record routes.
path_param!(seq);

// The `bucket` (public key id) segment of the key routes.
path_param!(bucket);

// The `tool` segment of the MCP documentation routes.
path_param!(tool);

// The `job_id` segment of the job routes.
path_param!(job_id);

// The base64url-encoded build path of the serve/preview routes.
path_param!(encoded);

/// A `tenantId` of 0 means "platform-level" (the owner has no tenant).
const NO_TENANT: i64 = 0;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Split a long-lived secret out of the `Authorization: Bearer` header.
fn bearer(cx: &Cx) -> Option<String> {
    headers(cx)
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").map(str::to_owned))
}

fn sha256_hex(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update(b":");
        }
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Is `secret` the board's owner key? (constant-time-ish hash compare)
fn owner_key_matches(row: &models::ServerlessBoard, secret: &str) -> bool {
    let Some(hash) = row.board_owner_key_hash.as_deref() else {
        return false;
    };
    hash == sha256_hex(&[row.board_id.as_deref().unwrap_or_default(), "owner", secret])
}

/// Fetch one live board's catalog row by id.
pub(crate) async fn board_row(cx: &Cx, board_id: &str) -> Result<Option<models::ServerlessBoard>> {
    let db = platform_core::db(cx);
    let page: models::BoardPage = db
        .query(models::board_by_id(board_id.to_string())?)
        .await?;
    Ok(page.items.into_iter().next())
}

/// Is the signed-in principal staff for this board's tenant?
fn is_staff_for(current: Option<&platform_auth::CurrentUser>, board: &models::ServerlessBoard) -> bool {
    let Some(current) = current else {
        return false;
    };
    let Some(tenant) = board.tenant_id else {
        return false;
    };
    if current.user.role() == platform_auth::Role::Owner {
        return true;
    }
    current.user.role() == platform_auth::Role::TenantAdmin && current.user.tenant_id == Some(tenant)
}

/// Where the owner bounces after a board mutation (no tenant): the console.
const OWNER_HOME: &str = "/admin";

/// Board page after-action target.
fn board_home(current: Option<&platform_auth::CurrentUser>) -> &'static str {
    match current {
        Some(c) if c.user.role() != platform_auth::Role::Owner => "/sl",
        _ => OWNER_HOME,
    }
}

// ---------------------------------------------------------------------------
// tenant pages
// ---------------------------------------------------------------------------

/// `GET /sl`: the serverless dashboard index — hard-redirects to the right
/// console so there is one clear home per audience: the tenant's Serverless
/// console (`/portal/serverless`) or the owner's (`/admin/serverless`). The
/// per-space consoles live at `/sl/{board_id}`.
#[page("/sl")]
async fn dashboard(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?;
    let target = match current {
        Some(c) if c.user.role() == platform_auth::Role::Owner => "/admin/serverless",
        Some(_) => "/portal/serverless",
        None => "/login",
    };
    Err(redirect(target).into())
}

/// `POST /sl/boards/create`: make a board (catalog row + turso file) and show
/// the owner key once.
#[route(POST "/sl/boards/create")]
async fn create_board(Form(form): Form<CreateForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let tenant_id = current
        .user
        .tenant_id
        .filter(|t| *t > NO_TENANT)
        .unwrap_or(NO_TENANT);

    let title = form.title.trim().to_string();
    if title.is_empty() {
        return Ok(see_other(board_home(Some(&current))));
    }
    let schema_json = empty_to_none(form.schema_json.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(s) = &schema_json {
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(v) if !v.is_object() => {
                return Ok(see_other("/sl?err=schema"));
            }
            Err(_) => return Ok(see_other("/sl?err=schema")),
            _ => {}
        }
    }
    let unique_key = empty_to_none(form.unique_key.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(k) = &unique_key {
        if !k
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Ok(see_other("/sl?err=key"));
        }
    }

    let board_id = new_board_id();
    let owner_key = TursoStore::new_owner_key();
    let owner_hash = sha256_hex(&[&board_id, "owner", &owner_key]);

    store
        .board_open(
            tenant_id,
            &board_id,
            &owner_key,
            &title,
            schema_json.as_deref(),
            form.public_reads,
            unique_key.as_deref(),
        )
        .await
        .map_err(|e| bad_request(format!("board create failed: {e}")))?;

    let _: serde_json::Value = db
        .query(models::create_board(
            board_id.clone(),
            title,
            form.description,
            schema_json.map(str::to_string).unwrap_or_default(),
            unique_key.map(str::to_string).unwrap_or_default(),
            form.public_reads,
            owner_hash,
            tenant_id,
        )?)
        .await?;

    let back = if current.user.role() == platform_auth::Role::Owner {
        OWNER_HOME.to_string()
    } else {
        format!("/sl/{}?owner_key={owner_key}", &board_id)
    };
    Ok(see_other(&back))
}

/// `GET /sl/{board}`: one board — records (latest 25), keys, hooks.
#[page("/sl/{board_id}")]
async fn board_page(cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }

    let app = store.board_get(NO_TENANT, raw).await?;
    let bq = parse_query_params::<BoardQuery>(cx).ok().unwrap_or_default();
    let query = bq.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let filter_json = bq.filter.as_deref().map(str::trim).filter(|f| !f.is_empty());
    let order = bq.order.as_deref().map(str::trim).filter(|o| !o.is_empty());
    let desc = bq.desc.as_deref().is_some_and(truthy);
    let mut conds: Vec<crate::dsl::Cond> = Vec::new();
    let filter_err: Option<String> = if let Some(f) = filter_json {
        match serde_json::from_str::<Value>(f)
            .map_err(|_| "filter must be JSON".to_string())
            .and_then(|v| crate::dsl::parse_filter(&v).map(|p| p.conds).map_err(|e| format!("bad filter: {e}")))
        {
            Ok(c) => {
                conds = c;
                None
            }
            Err(e) => {
                tracing::warn!(target: "lan", "board_page filter error: {e}");
                Some(e)
            }
        }
    } else {
        None
    };
    let orders = order.map(|o| vec![(o.to_string(), !desc)]).unwrap_or_default();
    let records = if let Some(q) = query {
        store.record_search(raw, q, false, 50, 0).await?
    } else if !conds.is_empty() || !orders.is_empty() {
        store.record_filter(raw, &conds, &orders, 50, 0).await?
    } else {
        store.record_list(NO_TENANT, raw, 25, None, 0).await?
    };
    let agg = match bq.agg_op.as_deref().and_then(crate::dsl::Agg::parse) {
        Some(op) => {
            let field = bq.agg_field.as_deref().filter(|f| !f.is_empty()).unwrap_or("$");
            let group = bq.agg_group.as_deref().filter(|g| !g.is_empty());
            store
                .record_aggregate(raw, op, field, group, &conds)
                .await
                .ok()
        }
        None => None,
    };
    let keys = store.key_list(NO_TENANT, raw).await?;
    let hooks = store.hook_list(NO_TENANT, raw).await?;
    let jobs = store.job_list(raw).await?;
    let deliveries = store.delivery_list(raw, None, 20).await?;
    let config = store.board_config(raw).await?;

    let owner_key = parse_query_params::<OwnerQuery>(cx)
        .ok()
        .and_then(|q| q.owner_key)
        .filter(|k| !k.is_empty());
    let issued_token = parse_query_params::<TokenQuery>(cx)
        .ok()
        .and_then(|q| q.token)
        .filter(|t| !t.is_empty());

    let host = headers(cx)
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let mcp_url = if host.is_empty() {
        "/mcp".to_string()
    } else {
        format!("http://{host}/mcp")
    };
    // When the one-time owner key is in the URL the snippets below are
    // copy-ready; otherwise they carry <key> placeholders. The space id
    // ({raw}) is the public handle — unique system-wide — the key is the
    // authority; the single composite header line carries both.
    let mcp_header = if let Some(key) = owner_key.as_ref() {
        format!("x-srv-key: {raw}:owner:{key}")
    } else {
        format!("x-srv-key: {raw}:owner:<key>")
    };
    let mcp_config = if let Some(key) = owner_key.as_ref() {
        format!(
            "{{\n  \"mcp\": {{\n    \"lansrv-spaces\": {{\n      \"type\": \"remote\",\n      \"url\": \"{mcp_url}\",\n      \"enabled\": true,\n      \"headers\": {{ \"x-srv-key\": \"{raw}:owner:{key}\" }}\n    }}\n  }}\n}}",
        )
    } else {
        format!(
            "{{\n  \"mcp\": {{\n    \"lansrv-spaces\": {{\n      \"type\": \"remote\",\n      \"url\": \"{mcp_url}\",\n      \"enabled\": true,\n      \"headers\": {{ \"x-srv-key\": \"{raw}:owner:<key>\" }}\n    }}\n  }}\n}}",
        )
    };
    let rpc_curl = if let Some(key) = owner_key.as_ref() {
        format!(
            r#"curl -X POST {mcp_url} -H "content-type: application/json" -H "x-srv-key: {raw}:owner:{key}" -d '{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"cli","arguments":{{"args":["space","show"]}}}}}}'"#
        )
    } else {
        format!(
            r#"curl -X POST {mcp_url} -H "content-type: application/json" -H "x-srv-key: {raw}:owner:<key>" -d '{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"cli","arguments":{{"args":["space","show"]}}}}}}'"#
        )
    };
    let icon_key = board.board_icon_key.as_deref().filter(|k| !k.is_empty());
    let banner_key = board.board_banner_key.as_deref().filter(|k| !k.is_empty());
    let gallery: Vec<String> = board
        .board_gallery_keys
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let media_flash = parse_query_params::<MediaQuery>(cx)
        .ok()
        .and_then(|q| q.media)
        .is_some();
    // Build inventory — makes deploy problems (missing staging, empty dist)
    // visible at a glance.
    let storage = platform_core::storage(cx);
    let staging_prefix = files::staging_key(raw, "");
    let dist_prefix = files::dist_key(raw, "");
    let staged_paths: Vec<String> = storage
        .list(&staging_prefix)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|k| k.strip_prefix(&staging_prefix).map(str::to_string))
        .collect();
    let dist_paths: Vec<String> = storage
        .list(&dist_prefix)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|k| k.strip_prefix(&dist_prefix).map(str::to_string))
        .collect();

    view! {
        <div class="mx-auto max-w-4xl px-4 py-10">
            <a class="mb-6 inline-block text-sm text-sky-400 hover:text-sky-300"
                href=(board_home(current.as_ref()))>
                "← Back"
            </a>
            <div class="mb-8 flex items-baseline gap-3">
                <h1 class="text-3xl font-bold">(board.board_title.as_deref().unwrap_or("Board"))</h1>
                <span class="flex items-center gap-2 rounded-full border border-sky-700 bg-sky-900/40 px-2 py-0.5 text-xs text-sky-200">
                    (board.board_id.as_deref().unwrap_or_default())
                    <button
                        onclick=(format!("navigator.clipboard.writeText('{}')", raw))
                        title="Copy space id"
                        class="cursor-pointer rounded border border-sky-800 px-1 py-px text-[10px] text-sky-300 transition hover:bg-sky-900 hover:text-sky-100"
                    >
                        "copy"
                    </button>
                </span>
                <form method="post" action=("/sl/".to_owned() + raw + "/delete") class="ml-auto">
                    <button class="rounded-md border border-red-900 px-3 py-1 text-xs text-red-400 hover:bg-red-950">
                        "Delete board"
                    </button>
                </form>
            </div>

            if let Some(key) = owner_key.as_deref() {
                <div class="mb-6 rounded-lg border border-amber-700 bg-amber-950/50 p-4">
                    <p class="mb-2 text-sm font-semibold text-amber-300">
                        "Owner key — copy it now, it will not be shown again:"
                    </p>
                    <div class="flex items-center gap-2">
                        <code class="min-w-0 flex-1 break-all rounded bg-zinc-950 px-2 py-1 text-sm text-amber-200">(key)</code>
                        <button
                            onclick=(format!("navigator.clipboard.writeText('{}')", key))
                            title="Copy owner key"
                            class="shrink-0 cursor-pointer rounded border border-amber-800 px-2 py-1 text-xs text-amber-300 transition hover:bg-amber-900/50"
                        >
                            "copy"
                        </button>
                    </div>
                    <p class="mt-2 text-xs text-amber-200/70">
                        "The MCP header and curl snippet in “Agent / MCP access” below are already filled in with this key."
                    </p>
                </div>
            }

            if let Some(token) = issued_token.as_deref() {
                <div class="mb-6 rounded-lg border border-emerald-800 bg-emerald-950/50 p-4">
                    <p class="mb-2 text-sm font-semibold text-emerald-300">
                        "Key token — copy it now, it will not be shown again:"
                    </p>
                    <div class="flex items-center gap-2">
                        <code class="min-w-0 flex-1 break-all rounded bg-zinc-950 px-2 py-1 text-sm text-emerald-200">(token)</code>
                        <button
                            onclick=(format!("navigator.clipboard.writeText('{}')", token))
                            title="Copy key token"
                            class="shrink-0 cursor-pointer rounded border border-emerald-800 px-2 py-1 text-xs text-emerald-300 transition hover:bg-emerald-900/50"
                        >
                            "copy"
                        </button>
                    </div>
                </div>
            }

            if let Some(app) = app {
                <div class="mb-6 flex flex-wrap gap-4 text-xs text-zinc-500">
                    <span>"records: " (app.record_count)</span>
                    <span>"public reads: " {if app.public_reads { "yes" } else { "no" }}</span>
                    match app.unique_key.as_deref() {
                        Some(k) => <span>"upsert key: " (k)</span>,
                        None => <span>"upsert key: none"</span>,
                    }
                </div>
            }

            <section class="mb-8 rounded-lg border border-zinc-800 bg-zinc-900 p-4">
                <h2 class="mb-3 text-sm font-semibold text-zinc-300">"Space"</h2>
                <div class="mb-4 flex flex-wrap items-center gap-3 text-xs">
                    <span class=(
                        if board.board_public_reads.unwrap_or(false) && board.board_deployed_at.is_some() {
                            "rounded-full border border-emerald-900 bg-emerald-950/60 px-2 py-0.5 font-semibold text-emerald-300"
                        } else if board.board_deployed_at.is_some() {
                            "rounded-full border border-amber-700 bg-amber-950/50 px-2 py-0.5 font-semibold text-amber-300"
                        } else {
                            "rounded-full border border-zinc-700 bg-zinc-800 px-2 py-0.5 font-semibold text-zinc-400"
                        }
                    )>
                        if board.board_public_reads.unwrap_or(false) && board.board_deployed_at.is_some() {
                            "LIVE"
                        } else if board.board_deployed_at.is_some() {
                            "Offline — not public"
                        } else {
                            "Draft — nothing deployed"
                        }
                    </span>
                    <form method="post" action=("/sl/".to_owned() + raw + "/visibility")>
                        if board.board_public_reads.unwrap_or(false) {
                            <button class="cursor-pointer rounded-md border border-amber-700 px-3 py-1 text-amber-300 transition hover:bg-amber-950/40">
                                "Unpublish"
                            </button>
                        } else {
                            <button class="cursor-pointer rounded-md bg-emerald-600 px-3 py-1 font-semibold text-white transition hover:bg-emerald-500">
                                "Publish"
                            </button>
                        }
                    </form>
                    if board.board_deployed_at.is_some() {
                        <a class="rounded-md bg-sky-600 px-3 py-1 font-semibold text-white transition hover:bg-sky-500"
                            href=("/s/".to_owned() + raw)>
                            "Open app"
                        </a>
                    }
                    <a class="rounded-md border border-zinc-700 px-3 py-1 transition hover:border-sky-500"
                        href=("/sl/".to_owned() + raw + "/preview")>
                        "Preview staged build"
                    </a>
                    <a class="rounded-md border border-zinc-700 px-3 py-1 transition hover:border-sky-500"
                        href=("/api/srv/mcp/guide")>
                        "MCP guide"
                    </a>
                </div>
                <form method="post" enctype="multipart/form-data" action=("/sl/".to_owned() + raw + "/files")
                    class="mb-4 flex flex-wrap items-end gap-3"
                >
                    <label class="flex flex-col gap-1 text-xs text-zinc-400">
                        "Build folder (dist/)"
                        <input name="file" type="file" webkitdirectory="" multiple=""
                            class="text-xs text-zinc-400" />
                    </label>
                    <button class="rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-sky-500">
                        "Stage build"
                    </button>
                    <span class="text-xs text-zinc-500">
                        "Uploaded files land under staging/ and are testable via `Preview` — then Deploy below."
                    </span>
                </form>
                <form method="post" action=("/sl/".to_owned() + raw + "/deploy")
                    class="mb-4 flex flex-wrap items-end gap-3"
                >
                    <label class="flex items-center gap-2 text-xs text-zinc-300">
                        <input type="checkbox" name="is_public" value="on" class="h-4 w-4 accent-emerald-500"/>
                        "Publish on deploy"
                    </label>
                    <button class="rounded-md bg-sky-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-sky-500">
                        "Deploy"
                    </button>
                </form>
                <div class="mb-4 grid list-none grid-cols-1 gap-3 p-0 sm:grid-cols-2">
                    <div class="rounded-lg border border-zinc-800 bg-zinc-950 p-3">
                        <div class="mb-1 flex items-baseline justify-between text-xs">
                            <span class="font-semibold text-zinc-300">"Staged (build)"</span>
                            <span class="text-zinc-500">(staged_paths.len()) " files"</span>
                        </div>
                        if staged_paths.is_empty() {
                            <p class="text-xs text-amber-300/80">
                                "Nothing staged yet — upload a build above, then Deploy."
                            </p>
                        } else {
                            <ul class="list-none space-y-0.5 p-0 text-xs text-zinc-400">
                                for path in staged_paths.iter().take(8) {
                                    <li class="truncate"><code>(path)</code></li>
                                }
                                if staged_paths.len() > 8 {
                                    <li class="text-zinc-600">"+ "(staged_paths.len() - 8)" more…"</li>
                                }
                            </ul>
                        }
                    </div>
                    <div class="rounded-lg border border-zinc-800 bg-zinc-950 p-3">
                        <div class="mb-1 flex items-baseline justify-between text-xs">
                            <span class="font-semibold text-zinc-300">"Live (dist)"</span>
                            <span class="text-zinc-500">(dist_paths.len()) " files"</span>
                        </div>
                        if dist_paths.is_empty() {
                            <p class="text-xs text-zinc-500">
                                if staged_paths.is_empty() {
                                    "Nothing live — no build staged yet."
                                } else {
                                    "Nothing live yet — Deploy to copy staging → dist."
                                }
                            </p>
                        } else {
                            <ul class="list-none space-y-0.5 p-0 text-xs text-zinc-400">
                                for path in dist_paths.iter().take(8) {
                                    <li class="truncate"><code>(path)</code></li>
                                }
                                if dist_paths.len() > 8 {
                                    <li class="text-zinc-600">"+ "(dist_paths.len() - 8)" more…"</li>
                                }
                            </ul>
                        }
                    </div>
                </div>
                <div class="border-t border-zinc-800 pt-4">
                    if media_flash {
                        <p class="mb-3 rounded-md border border-emerald-800 bg-emerald-950/40 px-3 py-2 text-xs text-emerald-300">
                            "Media saved — the previews below and the public card update immediately."
                        </p>
                    }
                    <h3 class="mb-2 text-xs font-semibold text-zinc-400">"Public card media (live preview)"</h3>
                    <div class="mb-3 grid list-none grid-cols-2 gap-3 p-0 sm:grid-cols-3">
                        <div class="rounded-lg border border-zinc-800 bg-zinc-950 p-2">
                            <div class="mb-1 flex items-center justify-between text-[10px] uppercase tracking-wide text-zinc-500">
                                "Banner"
                                if banner_key.is_some() {
                                    <form method="post" action=("/sl/".to_owned() + raw + "/media/remove")>
                                        <input type="hidden" name="which" value="banner" />
                                        <button title="Remove banner" class="cursor-pointer rounded border border-red-900 px-1 text-[10px] text-red-300 hover:bg-red-950">
                                            "× remove"
                                        </button>
                                    </form>
                                }
                            </div>
                            match banner_key {
                                Some(key) => {
                                    <img class="h-20 w-full rounded-md bg-zinc-900 object-cover"
                                        src=("/blob/".to_owned() + &platform_core::encode_key(key)) alt="banner" />
                                },
                                None => {
                                    <div class="flex h-20 w-full items-center justify-center rounded-md bg-zinc-900 text-[10px] text-zinc-600">
                                        "no banner"
                                    </div>
                                },
                            }
                        </div>
                        <div class="rounded-lg border border-zinc-800 bg-zinc-950 p-2">
                            <div class="mb-1 flex items-center justify-between text-[10px] uppercase tracking-wide text-zinc-500">
                                "Icon"
                                if icon_key.is_some() {
                                    <form method="post" action=("/sl/".to_owned() + raw + "/media/remove")>
                                        <input type="hidden" name="which" value="icon" />
                                        <button title="Remove icon" class="cursor-pointer rounded border border-red-900 px-1 text-[10px] text-red-300 hover:bg-red-950">
                                            "× remove"
                                        </button>
                                    </form>
                                }
                            </div>
                            match icon_key {
                                Some(key) => {
                                    <img class="h-20 w-full rounded-md bg-zinc-900 object-contain"
                                        src=("/blob/".to_owned() + &platform_core::encode_key(key)) alt="icon" />
                                },
                                None => {
                                    <div class="flex h-20 w-full items-center justify-center rounded-md bg-zinc-900 text-[10px] text-zinc-600">
                                        "no icon"
                                    </div>
                                },
                            }
                        </div>
                        <div class="col-span-2 rounded-lg border border-zinc-800 bg-zinc-950 p-2 sm:col-span-1">
                            <div class="mb-1 text-[10px] uppercase tracking-wide text-zinc-500">
                                "Gallery photos"
                            </div>
                            <div class="flex flex-wrap gap-2">
                                if gallery.is_empty() {
                                    <span class="text-[10px] text-zinc-600">"no photos yet"</span>
                                }
                                for (i, key) in gallery.iter().enumerate() {
                                    <div class="relative">
                                        <img class="h-16 w-16 rounded-md border border-zinc-800 bg-zinc-800 object-cover"
                                            src=("/blob/".to_owned() + &platform_core::encode_key(key)) alt=""
                                            loading="lazy" />
                                        <form method="post" action=("/sl/".to_owned() + raw + "/media/remove") class="absolute -right-1.5 -top-1.5">
                                            <input type="hidden" name="which" value="photo" />
                                            <input type="hidden" name="seq" value=(i.to_string()) />
                                            <button title="Remove photo" class="cursor-pointer rounded-full border border-red-900 bg-zinc-950 px-1.5 text-[10px] leading-none text-red-300 hover:bg-red-950">
                                                "×"
                                            </button>
                                        </form>
                                    </div>
                                }
                            </div>
                        </div>
                    </div>
                    <form method="post" enctype="multipart/form-data" action=("/sl/".to_owned() + raw + "/media")
                        class="flex flex-wrap items-end gap-3"
                    >
                        <label class="flex flex-col gap-1 text-xs text-zinc-400">
                            "Banner (replaces)"
                            <input name="banner" type="file" accept="image/*" class="text-xs text-zinc-400" />
                        </label>
                        <label class="flex flex-col gap-1 text-xs text-zinc-400">
                            "Icon (replaces)"
                            <input name="icon" type="file" accept="image/*" class="text-xs text-zinc-400" />
                        </label>
                        <label class="flex flex-col gap-1 text-xs text-zinc-400">
                            "Photos (appends)"
                            <input name="photos" type="file" accept="image/*" multiple="" class="text-xs text-zinc-400" />
                        </label>
                        <button class="rounded-md bg-sky-600 px-4 py-1.5 text-xs font-semibold text-white transition hover:bg-sky-500">
                            "Upload media"
                        </button>
                    </form>
                </div>
                <form method="post" action=("/sl/".to_owned() + raw + "/update")
                    class="flex flex-wrap items-end gap-3 border-t border-zinc-800 pt-4"
                >
                    <label class="flex flex-col gap-1 text-xs text-zinc-400">
                        "Title"
                        <input name="title" required="" value=(board.board_title.clone().unwrap_or_default())
                            class="w-44 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                    </label>
                    <label class="flex flex-col gap-1 text-xs text-zinc-400">
                        "Description"
                        <input name="description" value=(board.board_description.clone().unwrap_or_default())
                            class="w-64 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                    </label>
                    <button class="rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-sky-500">
                        "Save details"
                    </button>
                </form>
                <div class="mt-4 rounded-lg border border-zinc-800 bg-zinc-950 p-3">
                    <h3 class="mb-2 text-xs font-semibold text-zinc-300">"Agent (MCP) access"</h3>
                    <dl class="grid list-none gap-2 p-0 text-xs">
                        <div class="flex items-center gap-2">
                            <dt class="w-20 shrink-0 text-zinc-500">"Endpoint"</dt>
                            <code class="min-w-0 flex-1 break-all text-zinc-200">(mcp_url.clone())</code>
                            <button
                                onclick=(format!("navigator.clipboard.writeText('{}')", mcp_url))
                                title="Copy endpoint"
                                class="shrink-0 cursor-pointer rounded border border-zinc-700 px-1.5 py-0.5 text-[10px] text-zinc-400 transition hover:border-sky-500 hover:text-sky-300"
                            >
                                "copy"
                            </button>
                        </div>
                        <div class="flex items-center gap-2">
                            <dt class="w-20 shrink-0 text-zinc-500">"Space id"</dt>
                            <code class="min-w-0 flex-1 break-all text-zinc-200">(raw)</code>
                            <button
                                onclick=(format!("navigator.clipboard.writeText('{}')", raw))
                                title="Copy space id"
                                class="shrink-0 cursor-pointer rounded border border-zinc-700 px-1.5 py-0.5 text-[10px] text-zinc-400 transition hover:border-sky-500 hover:text-sky-300"
                            >
                                "copy"
                            </button>
                        </div>
                        <div class="flex items-center gap-2">
                            <dt class="w-20 shrink-0 text-zinc-500">"Header"</dt>
                            <code class="min-w-0 flex-1 break-all text-zinc-200">(mcp_header.clone())</code>
                            <button
                                onclick=(format!("navigator.clipboard.writeText('{}')", mcp_header))
                                title="Copy header line"
                                class="shrink-0 cursor-pointer rounded border border-zinc-700 px-1.5 py-0.5 text-[10px] text-zinc-400 transition hover:border-sky-500 hover:text-sky-300"
                            >
                                "copy"
                            </button>
                        </div>
                    </dl>
                    <p class="mt-2 text-xs text-zinc-500">
                        "The space id is unique across the whole system and public (it lives in every "
                        <code class="text-zinc-300">"/s/{raw}/"</code>
                        if let Some(owner_secret) = owner_key.as_deref() {
                            " URL). EASY MODE: pass the key string itself as the id — "
                            <code class="text-zinc-300">(format!("cli space show --space {raw}:owner:{owner_secret}"))</code>
                            " authenticates the call, no client config beyond the url. HEADER MODE: paste the same
                            string into the client config below. "
                        } else {
                            " URL). A command needs a secret: the key string below — paste the one-time owner key
                            shown when the space was created, or issue a scoped key in the Keys section ("
                            <code class="text-zinc-300">"x-srv-key: <space_id>:<bucket>:<token>"</code>
                            "). "
                        }
                        "Every command works off that one string, and all help layers answer without any key. "
                        <a class="text-sky-400 hover:text-sky-300" href="/api/srv/mcp/guide">"full guide"</a> "."
                    </p>
                    <p class="mt-2 text-xs text-zinc-400">
                        "opencode — paste under "
                        <code class="text-zinc-300">"\"mcp\""</code>
                        " in "
                        <code class="text-zinc-300">"~/.config/opencode/opencode.json"</code>
                        " (Claude Code: "
                        <code class="text-zinc-300">"mcpServers"</code>
                        ", "
                        <code class="text-zinc-300">"\"type\": \"http\""</code>
                        "):"
                    </p>
                    <div class="mt-2 flex items-start gap-2">
                        <pre class="min-w-0 flex-1 overflow-x-auto rounded bg-zinc-900 p-2 text-[11px] leading-relaxed text-zinc-300">(mcp_config.clone())</pre>
                        <button
                            onclick=(format!("navigator.clipboard.writeText('{}')", mcp_config))
                            title="Copy client config"
                            class="shrink-0 cursor-pointer rounded border border-zinc-700 px-2 py-1 text-[10px] text-zinc-400 transition hover:border-sky-500 hover:text-sky-300"
                        >
                            "copy"
                        </button>
                    </div>
                    <div class="mt-2 flex items-start gap-2">
                        <pre class="min-w-0 flex-1 overflow-x-auto rounded bg-zinc-900 p-2 text-[11px] leading-relaxed text-zinc-300">(rpc_curl.clone())</pre>
                        <button
                            onclick=(format!("navigator.clipboard.writeText('{}')", rpc_curl))
                            title="Copy curl call"
                            class="shrink-0 cursor-pointer rounded border border-zinc-700 px-2 py-1 text-[10px] text-zinc-400 transition hover:border-sky-500 hover:text-sky-300"
                        >
                            "copy"
                        </button>
                    </div>
                </div>
            </section>

            <section class="mb-8 rounded-lg border border-zinc-800 bg-zinc-900 p-4">
                <h2 class="mb-3 text-sm font-semibold text-zinc-300">"Records"</h2>
                match parse_query_params::<ErrorQuery>(cx).map(|q| q.err).unwrap_or_default() {
                    Some(err) => {
                        <p class="mb-2 text-sm text-red-400">(format!("API error: {err}"))</p>
                    }
                    None => {}
                }
                if let Some(e) = &filter_err {
                    <p class="mb-2 text-sm text-red-400">(format!("filter error: {e}"))</p>
                }
                <form method="get" action=(format!("/sl/{raw}"))
                    class="mb-3 grid grid-cols-2 gap-2 rounded-md bg-zinc-950 p-3"
                >
                    <input class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="q"
                        placeholder="search text (substring)" value=(bq.q.clone().unwrap_or_default()) />
                    <input class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="filter"
                        placeholder=r#"filter JSON: [{"field":"price","op":"gt","value":10}]"#
                        value=(bq.filter.clone().unwrap_or_default()) />
                    <input class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="order"
                        placeholder="order by path (e.g. price)" value=(order.unwrap_or_default().to_string()) />
                    <label class="flex items-center gap-2 text-xs text-zinc-400">
                        <input type="checkbox" name="desc" value="1" checked=(desc) />
                        "descending order"
                    </label>
                    <div class="col-span-2 flex flex-wrap items-center gap-2">
                        <select class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="agg_op">
                            <option value="">"agg: none"</option>
                            <option value="count" selected=(bq.agg_op.as_deref() == Some("count"))>"count"</option>
                            <option value="sum" selected=(bq.agg_op.as_deref() == Some("sum"))>"sum"</option>
                            <option value="avg" selected=(bq.agg_op.as_deref() == Some("avg"))>"avg"</option>
                            <option value="min" selected=(bq.agg_op.as_deref() == Some("min"))>"min"</option>
                            <option value="max" selected=(bq.agg_op.as_deref() == Some("max"))>"max"</option>
                        </select>
                        <input class="w-32 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="agg_field"
                            placeholder="field ($)" value=(bq.agg_field.clone().unwrap_or_default()) />
                        <input class="w-32 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="agg_group"
                            placeholder="group by" value=(bq.agg_group.clone().unwrap_or_default()) />
                        <button class="ml-auto rounded-md bg-sky-600 px-3 py-1 text-xs font-medium text-white hover:bg-sky-500">
                            "Apply"
                        </button>
                        <a class="rounded-md border border-zinc-700 px-3 py-1 text-xs text-zinc-400 hover:border-sky-500"
                            href=(format!("/sl/{raw}"))>"Clear"</a>
                    </div>
                </form>
                if let Some(a) = &agg {
                    <div class="mb-2 rounded-md bg-zinc-950 px-3 py-2 text-xs text-zinc-300">
                        "Aggregate"
                        if a.get("rows").is_some() {
                            for row in a.get("rows").and_then(|r| r.as_array()).into_iter().flatten() {
                                <span class="mx-2 inline-flex items-baseline gap-1 rounded bg-zinc-900 px-2 py-0.5">
                                    <code class="text-sky-300">(row.get("key").and_then(|v| v.as_str()).unwrap_or("?"))</code>
                                    "="
                                    <code class="text-zinc-200">(row.get("value").map(|v| v.to_string()).unwrap_or_default())</code>
                                </span>
                            }
                        }
                        if a.get("rows").is_none() {
                            <span>
                                <code class="text-sky-300">(a.get("op").and_then(|v| v.as_str()).unwrap_or("agg"))</code>
                                "("
                                <code>(a.get("field").and_then(|v| v.as_str()).unwrap_or("$"))</code>
                                ") = "
                                <code class="text-zinc-200">(a.get("value").map(|v| v.to_string()).unwrap_or_default())</code>
                            </span>
                        }
                    </div>
                }
                <ul class="grid list-none gap-2 p-0">
                    for rec in &records {
                        <li class="flex items-baseline gap-3 rounded-md bg-zinc-950 px-3 py-2">
                            <span class="text-xs text-zinc-500">(format!("#{}", rec.seq))</span>
                            <code class="min-w-0 flex-1 truncate text-xs text-zinc-200">
                                (serde_json::to_string(&rec.payload).unwrap_or_default())
                            </code>
                            <span class="text-xs text-zinc-600">(rec.created_at.clone())</span>
                            <button
                                onclick=(format!("navigator.clipboard.writeText('{}')", serde_json::to_string(&rec.payload).unwrap_or_default()))
                                title="Copy record JSON"
                                class="shrink-0 cursor-pointer rounded border border-zinc-700 px-1.5 py-0.5 text-[10px] text-zinc-400 transition hover:border-sky-500 hover:text-sky-300"
                            >
                                "copy"
                            </button>
                        </li>
                    }
                </ul>
                if records.is_empty() {
                    <p class="py-4 text-sm text-zinc-500">
                        "No records yet — write some via the API."
                    </p>
                }
                <p class="mt-3 rounded-md bg-zinc-950 px-3 py-2 text-xs text-zinc-400">
                    <code class="text-sky-300">(format!("POST /api/srv/{raw}/records"))</code>
                    " with " 
                    <code class="text-sky-300">(r#"{"payload":{"key":"value"}}"#)</code>
                    " + `Authorization: Bearer <token>`."
                </p>
            </section>

            <section class="mb-8 rounded-lg border border-zinc-800 bg-zinc-900 p-4">
                <h2 class="mb-3 text-sm font-semibold text-zinc-300">"Keys"</h2>
                <form method="post" action=("/sl/".to_owned() + raw + "/keys/issue") class="mb-3 flex items-center gap-3">
                    <select class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm" name="role">
                        <option value="reader">"reader"</option>
                        <option value="writer">"writer"</option>
                        <option value="admin">"admin"</option>
                    </select>
                    <input class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm" name="writer"
                        placeholder="Optional writer label"/>
                    <button class="rounded-md bg-sky-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-sky-500">
                        "Issue key"
                    </button>
                </form>
                <ul class="grid list-none gap-2 p-0">
                    for key in &keys {
                        <li class="flex items-baseline gap-3 rounded-md bg-zinc-950 px-3 py-2 text-xs">
                            <span class="text-zinc-400">(key.bucket.clone())</span>
                            <span class="text-zinc-600">(key.role.as_str())</span>
                            if let Some(w) = key.writer.as_deref() {
                                <span class="text-zinc-600">(format!("writer: {w}"))</span>
                            }
                            <span class="text-zinc-600">(key.created_at.clone())</span>
                            match key.revoked_at.as_deref() {
                                Some(_) => <span class="text-red-400">"revoked"</span>,
                                None => {
                                    <form method="post" action=("/sl/".to_owned() + raw + "/keys/revoke") class="ml-auto">
                                        <input type="hidden" name="bucket" value=(key.bucket.clone())/>
                                        <button class="rounded-md border border-red-900 px-2 py-0.5 text-red-400 hover:bg-red-950">
                                            "Revoke"
                                        </button>
                                    </form>
                                }
                            }
                        </li>
                    }
                </ul>
                if keys.is_empty() {
                    <p class="py-3 text-sm text-zinc-500">"No keys yet."</p>
                }
            </section>

            <section class="rounded-lg border border-zinc-800 bg-zinc-900 p-4">
                <h2 class="mb-3 text-sm font-semibold text-zinc-300">"Webhooks"</h2>
                <form method="post" action=("/sl/".to_owned() + raw + "/hooks/register") class="mb-3 flex items-center gap-3">
                    <input class="min-w-0 flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm"
                        name="url" placeholder="https://…/hook"/>
                    <input class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm" name="secret"
                        placeholder="Optional signing secret"/>
                    <button class="rounded-md bg-sky-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-sky-500">
                        "Register"
                    </button>
                </form>
                <ul class="grid list-none gap-2 p-0">
                    for hook in &hooks {
                        <li class="flex items-baseline gap-3 rounded-md bg-zinc-950 px-3 py-2 text-xs">
                            <code class="min-w-0 flex-1 truncate text-zinc-400">(hook.url.clone())</code>
                            <span class="text-zinc-600">(hook.created_at.clone())</span>
                            <form method="post" action=("/sl/".to_owned() + raw + "/hooks/remove")>
                                <input type="hidden" name="url" value=(hook.url.clone())/>
                                <button class="rounded-md border border-red-900 px-2 py-0.5 text-red-400 hover:bg-red-950">
                                    "Remove"
                                </button>
                            </form>
                        </li>
                    }
                </ul>
                if hooks.is_empty() {
                    <p class="py-3 text-sm text-zinc-500">"No hooks yet."</p>
                }
            </section>

            <section class="mb-8 rounded-lg border border-zinc-800 bg-zinc-900 p-4">
                <h2 class="mb-3 text-sm font-semibold text-zinc-300">"Jobs"</h2>
                <form method="post" action=(format!("/sl/{raw}/jobs/add")) class="mb-3 grid grid-cols-2 gap-2">
                    <input class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="name"
                        placeholder="name (e.g. ping)" />
                    <input class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="schedule"
                        placeholder="@every 30s or 0 */5 * * * *" />
                    <div class="col-span-2 flex items-center gap-2">
                        <input class="min-w-0 flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs"
                            name="action" placeholder=r#"action JSON: {"url":"https://…","method":"GET"}"# />
                        <button class="rounded-md bg-sky-600 px-3 py-1 text-xs font-medium text-white hover:bg-sky-500">
                            "Add job"
                        </button>
                    </div>
                </form>
                <ul class="grid list-none gap-2 p-0">
                    for job in &jobs {
                        <li class="grid grid-cols-[auto_1fr_auto] items-center gap-3 rounded-md bg-zinc-950 px-3 py-2 text-xs">
                            <span class="text-zinc-500">(format!("#{}", jv_i64(job, "id")))</span>
                            <div class="min-w-0">
                                <div class="truncate text-zinc-200">
                                    <code>(jv_str(job, "name"))</code>
                                    " — "
                                    <code class="text-zinc-400">(jv_str(job, "schedule"))</code>
                                </div>
                                <div class="truncate text-zinc-500">
                                    <code>(job.get("action").map(|a| a.to_string()).unwrap_or_default())</code>
                                </div>
                                <div class="truncate text-zinc-500">
                                    "next " <code class="text-zinc-400">(jv_i64(job, "next_run_at"))</code>
                                    " · last " <code class="text-zinc-400">(jv_str(job, "last_status"))</code>
                                    <code class="text-zinc-600">(jv_str(job, "last_message"))</code>
                                </div>
                            </div>
                            <form method="post" action=(format!("/sl/{raw}/jobs/remove"))>
                                <input type="hidden" name="id" value=(jv_i64(job, "id").to_string()) />
                                <button class="rounded-md border border-red-900 px-2 py-0.5 text-red-400 hover:bg-red-950">
                                    "Remove"
                                </button>
                            </form>
                        </li>
                    }
                </ul>
                if jobs.is_empty() {
                    <p class="py-3 text-sm text-zinc-500">"No jobs yet — scheduled HTTP calls or record writes."</p>
                }
            </section>

            <section class="mb-8 rounded-lg border border-zinc-800 bg-zinc-900 p-4">
                <h2 class="mb-3 text-sm font-semibold text-zinc-300">"Hook deliveries"</h2>
                <form method="post" action=(format!("/sl/{raw}/deliveries/replay")) class="mb-3 flex items-center gap-2">
                    <span class="text-xs text-zinc-500">"Failed and dead deliveries are retried with backoff; re-queue them now."</span>
                    <button class="ml-auto rounded-md border border-sky-700 px-3 py-1 text-xs text-sky-300 hover:bg-sky-950">
                        "Replay"
                    </button>
                </form>
                <ul class="grid list-none gap-2 p-0">
                    for d in &deliveries {
                        <li class="rounded-md bg-zinc-950 px-3 py-2 text-xs">
                            <div class="flex items-baseline gap-3">
                                <span class="text-zinc-500">(format!("#{}", jv_i64(d, "id")))</span>
                                <code class="text-sky-300">(jv_str(d, "event"))</code>
                                if let Some(seq) = d.get("seq").and_then(|v| v.as_i64()) {
                                    <code class="text-zinc-500">(format!("@seq {seq}"))</code>
                                }
                                <span class="ml-auto text-zinc-400">(format!("attempt {}", jv_i64(d, "attempt")))</span>
                                <span class=(if jv_str(d, "status") == "ok" { "text-emerald-400" } else { "text-amber-400" })>
                                    (jv_str(d, "status"))
                                </span>
                            </div>
                            <div class="mt-0.5 truncate text-zinc-500">
                                <code class="text-zinc-400">(jv_str(d, "url"))</code>
                                if !jv_str(d, "last_message").is_empty() {
                                    " — " <code class="text-zinc-600">(jv_str(d, "last_message"))</code>
                                }
                            </div>
                        </li>
                    }
                </ul>
                if deliveries.is_empty() {
                    <p class="py-3 text-sm text-zinc-500">"No deliveries yet — hook events are queued here."</p>
                }
            </section>

            <section class="rounded-lg border border-zinc-800 bg-zinc-900 p-4">
                <h2 class="mb-3 text-sm font-semibold text-zinc-300">"Config"</h2>
                <div class="mb-3 grid grid-cols-3 gap-2 text-xs text-zinc-400">
                    <div class="rounded-md bg-zinc-950 px-3 py-2">
                        "TTL: "
                        if let Some(s) = config.get("ttl_seconds").and_then(|v| v.as_i64()) {
                            <span class="text-zinc-200">(format!("{s}s on '{}'", jv_str(&config, "ttl_field")))</span>
                        }
                        if config.get("ttl_seconds").and_then(|v| v.as_i64()).is_none() {
                            <span class="text-zinc-600">"off"</span>
                        }
                    </div>
                    <div class="rounded-md bg-zinc-950 px-3 py-2">
                        "Audit: "
                        if config.get("audit").and_then(|v| v.as_bool()).unwrap_or(false) {
                            <span class="text-emerald-400">"on"</span>
                        }
                        if !config.get("audit").and_then(|v| v.as_bool()).unwrap_or(false) {
                            <span class="text-zinc-600">"off"</span>
                        }
                    </div>
                    <div class="rounded-md bg-zinc-950 px-3 py-2">
                        "Rate: "
                        if config.get("rate_per_minute").and_then(|v| v.as_i64()).unwrap_or(0) > 0 {
                            <span class="text-zinc-200">(format!("{} req/min", config.get("rate_per_minute").and_then(|v| v.as_i64()).unwrap_or(0)))</span>
                        }
                        if config.get("rate_per_minute").and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
                            <span class="text-zinc-600">"unlimited"</span>
                        }
                    </div>
                </div>
                <div class="mb-3 grid grid-cols-2 gap-3">
                    <form method="post" action=(format!("/sl/{raw}/ttl")) class="flex items-center gap-2">
                        <input class="w-20 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="seconds"
                            type="number" min="0" placeholder="secs" />
                        <input class="w-24 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="field"
                            placeholder="expires_at" />
                        <button class="rounded-md border border-zinc-700 px-2 py-1 text-xs hover:border-sky-500">"Set TTL"</button>
                        <button class="rounded-md border border-red-900 px-2 py-1 text-xs text-red-400 hover:bg-red-950"
                            name="seconds" value="0">"Clear"</button>
                    </form>
                    <form method="post" action=(format!("/sl/{raw}/audit")) class="flex items-center gap-2">
                        <input type="hidden" name="enabled"
                            value=(if config.get("audit").and_then(|v| v.as_bool()).unwrap_or(false) { "off" } else { "on" }) />
                        <span class="text-xs text-zinc-500">"Audit trail"</span>
                        <button class="ml-auto rounded-md border border-zinc-700 px-2 py-1 text-xs hover:border-sky-500">
                            (if config.get("audit").and_then(|v| v.as_bool()).unwrap_or(false) { "Turn off" } else { "Turn on" })
                        </button>
                    </form>
                    <form method="post" action=(format!("/sl/{raw}/rate")) class="flex items-center gap-2">
                        <input class="w-24 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs" name="per_minute"
                            type="number" min="0" placeholder="per minute" />
                        <button class="rounded-md border border-zinc-700 px-2 py-1 text-xs hover:border-sky-500">"Set rate"</button>
                        <button class="rounded-md border border-red-900 px-2 py-1 text-xs text-red-400 hover:bg-red-950"
                            name="per_minute" value="0">"Clear"</button>
                    </form>
                    <div class="flex items-center gap-2">
                        <span class="text-xs text-zinc-500">"Export / restore (JSON)"</span>
                        <a class="ml-auto rounded-md border border-zinc-700 px-2 py-1 text-xs hover:border-sky-500"
                            href=(format!("/sl/{raw}/dump"))>"Download dump"</a>
                    </div>
                </div>
                <form method="post" action=(format!("/sl/{raw}/restore")) class="flex items-end gap-2">
                    <textarea name="dump" rows="4" placeholder="Paste a dump JSON document here to restore…"
                        class="min-w-0 flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs outline-none focus:border-sky-400"></textarea>
                    <button class="rounded-md bg-sky-600 px-3 py-1 text-xs font-medium text-white hover:bg-sky-500">
                        "Restore"
                    </button>
                </form>
            </section>
        </div>
    }
}

/// `POST /sl/{board}/keys/issue`: mint a scoped key, show the token once.
#[route(POST "/sl/{board_id}/keys/issue")]
async fn issue_key(Form(form): Form<RoleForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let role = match form.role.as_deref() {
        Some("reader") => KeyRole::Reader,
        Some("writer") => KeyRole::Writer,
        Some("admin") => KeyRole::Admin,
        _ => return Err(bad_request("invalid role").into()),
    };
    let writer = empty_to_none(form.writer.as_deref()).map(str::to_string);
    let (bucket, secret) = store
        .key_issue(NO_TENANT, raw, role, writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("key issue failed: {e}")))?;
    let token = format!("{raw}:{bucket}:{secret}");
    let back = format!("/sl/{raw}?token={token}");
    Err(redirect(&back).into())
}

/// `POST /sl/{board}/keys/revoke`.
#[route(POST "/sl/{board_id}/keys/revoke")]
async fn revoke_key(Form(form): Form<BucketForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let Some(bucket) = form.bucket else {
        return Err(bad_request("missing bucket").into());
    };
    store
        .key_revoke(NO_TENANT, raw, &bucket)
        .await
        .map_err(|e| bad_request(format!("revoke failed: {e}")))?;
    let back = format!("/sl/{raw}");
    Err(redirect(&back).into())
}

/// `POST /sl/{board}/hooks/register`.
#[route(POST "/sl/{board_id}/hooks/register")]
async fn register_hook(Form(form): Form<HookForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let Some(url) = form.url.filter(|u| u.starts_with("http")) else {
        return Err(bad_request("invalid hook url").into());
    };
    let secret = empty_to_none(form.secret.as_deref());
    store
        .hook_register(NO_TENANT, raw, &url, secret)
        .await
        .map_err(|e| bad_request(format!("hook failed: {e}")))?;
    let back = format!("/sl/{raw}");
    Err(redirect(&back).into())
}

/// `POST /sl/{board}/hooks/remove`.
#[route(POST "/sl/{board_id}/hooks/remove")]
async fn remove_hook(Form(form): Form<UrlForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let Some(url) = form.url else {
        return Err(bad_request("missing url").into());
    };
    store
        .hook_remove(NO_TENANT, raw, &url)
        .await
        .map_err(|e| bad_request(format!("hook failed: {e}")))?;
    let back = format!("/sl/{raw}");
    Err(redirect(&back).into())
}

/// `POST /sl/{board}/jobs/add`: schedule a cron job (web form).
#[route(POST "/sl/{board_id}/jobs/add")]
async fn add_job(Form(form): Form<WebJobForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let (Some(name), Some(schedule), Some(action_text)) = (
        form.name.filter(|n| !n.trim().is_empty()),
        form.schedule.as_deref().filter(|s| !s.trim().is_empty()),
        form.action.as_deref().filter(|a| !a.trim().is_empty()),
    ) else {
        return Err(redirect(&format!("/sl/{raw}?err=job")).into());
    };
    let action: Value = match serde_json::from_str(action_text) {
        Ok(v) => v,
        Err(_) => return Err(redirect(&format!("/sl/{raw}?err=job")).into()),
    };
    if let Err(e) = store.job_add(raw, &name, schedule, &action).await {
        tracing::warn!(target: "lan", "job add failed: {e}");
        return Err(redirect(&format!("/sl/{raw}?err=job")).into());
    }
    Err(redirect(&format!("/sl/{raw}")).into())
}

/// `POST /sl/{board}/jobs/remove`.
#[route(POST "/sl/{board_id}/jobs/remove")]
async fn remove_job(Form(form): Form<JobIdForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    if let Some(id) = form.id {
        let _ = store.job_remove(raw, id).await;
    }
    Err(redirect(&format!("/sl/{raw}")).into())
}

/// `POST /sl/{board}/deliveries/replay`: re-queue failed/dead deliveries.
#[route(POST "/sl/{board_id}/deliveries/replay")]
async fn replay_deliveries(cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    if let Err(e) = store.delivery_replay(raw, None).await {
        tracing::warn!(target: "lan", "delivery replay failed: {e}");
        return Err(redirect(&format!("/sl/{raw}?err=hook")).into());
    }
    Err(redirect(&format!("/sl/{raw}")).into())
}

/// `POST /sl/{board}/ttl`: set (seconds>0) or clear (seconds=0) expiry.
#[route(POST "/sl/{board_id}/ttl")]
async fn set_ttl(Form(form): Form<TtlForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    match form.seconds {
        Some(s) if s > 0 => {
            let field = form.field.as_deref().filter(|f| !f.is_empty()).unwrap_or("expires_at");
            if let Err(e) = store.ttl_set(raw, s, field).await {
                tracing::warn!(target: "lan", "ttl set failed: {e}");
                return Err(redirect(&format!("/sl/{raw}?err=ttl")).into());
            }
        }
        Some(_) => {
            let _ = store.ttl_clear(raw).await;
        }
        None => return Err(redirect(&format!("/sl/{raw}?err=ttl")).into()),
    }
    Err(redirect(&format!("/sl/{raw}")).into())
}

/// `POST /sl/{board}/audit`: `enabled=on|off`.
#[route(POST "/sl/{board_id}/audit")]
async fn set_audit(Form(form): Form<WebEnabledForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let enabled = form.enabled.as_deref() == Some("on");
    if let Err(e) = store.audit_enable(raw, enabled).await {
        tracing::warn!(target: "lan", "audit set failed: {e}");
        return Err(redirect(&format!("/sl/{raw}?err=audit")).into());
    }
    Err(redirect(&format!("/sl/{raw}")).into())
}

/// `POST /sl/{board}/rate`: set (per_minute>0) or clear (per_minute=0).
#[route(POST "/sl/{board_id}/rate")]
async fn set_rate(Form(form): Form<RateForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    match form.per_minute {
        Some(p) if p > 0 => {
            if let Err(e) = store.rate_set(raw, p).await {
                tracing::warn!(target: "lan", "rate set failed: {e}");
                return Err(redirect(&format!("/sl/{raw}?err=rate")).into());
            }
        }
        Some(_) => {
            let _ = store.rate_set(raw, 0).await;
            store.rate_reset(raw).await;
        }
        None => return Err(redirect(&format!("/sl/{raw}?err=rate")).into()),
    }
    Err(redirect(&format!("/sl/{raw}")).into())
}

/// `GET /sl/{board}/dump`: the whole board as one JSON document.
#[route(GET "/sl/{board_id}/dump")]
async fn dump_board(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let dump = store
        .record_dump(raw)
        .await
        .map_err(|e| bad_request(format!("dump failed: {e}")))?;
    Ok(Json(dump))
}

/// `POST /sl/{board}/restore`: paste a dump document back into the board.
#[route(POST "/sl/{board_id}/restore")]
async fn restore_board(Form(form): Form<RestoreForm>, cx: &Cx) -> Result {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(current.as_ref(), &board) {
        return Err(forbidden().into());
    }
    let dump: Value = match serde_json::from_str(form.dump.trim()) {
        Ok(v) => v,
        Err(_) => return Err(redirect(&format!("/sl/{raw}?err=restore")).into()),
    };
    match store.record_restore(raw, &dump).await {
        Ok(counts) => {
            tracing::info!(target: "lan", "restored {counts} on {raw}");
            Err(redirect(&format!("/sl/{raw}")).into())
        }
        Err(e) => {
            tracing::warn!(target: "lan", "restore failed: {e}");
            Err(redirect(&format!("/sl/{raw}?err=restore")).into())
        }
    }
}

/// `POST /sl/{board}/delete`: tombstone the catalog row + drop the files.
#[route(POST "/sl/{board_id}/delete")]
async fn delete_board(cx: &Cx) -> Result<SeeOther> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);
    let store = app_context::<TursoStore>(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Ok(see_other(board_home(Some(&current))));
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    let tenant = board.tenant_id.unwrap_or(NO_TENANT);
    let _: serde_json::Value = db
        .query(models::tombstone_board(raw.to_string(), tenant)?)
        .await?;
    store
        .board_delete(NO_TENANT, raw)
        .await
        .map_err(|e| bad_request(format!("delete failed: {e}")))?;
    // Drop the space's listing media and every staged/deployed build object.
    let prefix = format!("srv/{raw}/");
    for key in storage.list(&prefix).await.unwrap_or_default() {
        storage.delete(&key).await.ok();
    }
    Ok(see_other(board_home(Some(&current))))
}

/// `POST /sl/{board}/update`: edit a space's details (title, description,
/// schema, upsert key).
#[route(POST "/sl/{board_id}/update")]
async fn update_board(Form(form): Form<UpdateForm>, cx: &Cx) -> Result<SeeOther> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Ok(see_other(board_home(Some(&current))));
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    let title = form.title.trim().to_string();
    if title.is_empty() {
        return Ok(see_other(&format!("/sl/{raw}")));
    }
    let schema_json = empty_to_none(form.schema_json.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(s) = &schema_json {
        if !serde_json::from_str::<serde_json::Value>(s).is_ok_and(|v| v.is_object()) {
            return Ok(see_other(&format!("/sl/{raw}?err=schema")));
        }
    }
    let unique_key = empty_to_none(form.unique_key.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(k) = &unique_key {
        if !k
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Ok(see_other(&format!("/sl/{raw}?err=key")));
        }
    }
    let _: serde_json::Value = db
        .query(models::update_board(
            raw.to_string(),
            title,
            form.description,
            schema_json.map(str::to_string).unwrap_or_default(),
            unique_key.map(str::to_string).unwrap_or_default(),
        )?)
        .await?;
    Ok(see_other(&format!("/sl/{raw}")))
}

/// `POST /sl/{board}/visibility`: publish / unpublish a space.
#[route(POST "/sl/{board_id}/visibility")]
async fn set_space_visibility(Form(form): Form<VisibilityForm>, cx: &Cx) -> Result<SeeOther> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Ok(see_other(board_home(Some(&current))));
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    let _: serde_json::Value = db
        .query(models::set_board_public(raw.to_string(), form.is_public.is_some())?)
        .await?;
    // Stay on the console the staff member came from (portal / owner console),
    // so the Publish toggle gives visible feedback in place.
    let back = headers(cx)
        .get("referer")
        .and_then(|v| v.to_str().ok())
        .filter(|r| r.contains("/portal/serverless") || r.contains("/admin/serverless"))
        .map(str::to_string)
        .unwrap_or_else(|| format!("/sl/{raw}"));
    Ok(see_other(&back))
}

/// `POST /sl/{board}/files`: upload a build into `staging/` (browser
/// directory picker — every part carries its relative path).
#[route(POST "/sl/{board_id}/files")]
async fn upload_build_files(mut upload: Multipart, cx: &Cx) -> Result<SeeOther> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let storage = platform_core::storage(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Ok(see_other(board_home(Some(&current))));
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    let mut count = 0u64;
    let mut errors = 0u64;
    while let Some(field) = upload.next_field().await? {
        if field.file_name().is_none() {
            continue;
        }
        let file_name = field.file_name().map(str::to_owned).unwrap_or_default();
        let Some(path) = files::normalize_upload_path(&file_name) else {
            errors += 1;
            continue;
        };
        let bytes = field.bytes().await?;
        if bytes.is_empty() {
            continue;
        }
        storage
            .put(
                &files::staging_key(raw, &path),
                bytes,
                files::content_type(&path),
            )
            .await?;
        count += 1;
    }
    let back = if errors > 0 {
        format!("/sl/{raw}?err=files")
    } else {
        format!("/sl/{raw}?uploaded={count}")
    };
    Ok(see_other(&back))
}

/// `POST /sl/{board}/media`: replace the space's icon, banner, and append
/// gallery photos (`icon`, `banner`, `photos` multipart fields).
#[route(POST "/sl/{board_id}/media")]
async fn upload_space_media(mut upload: Multipart, cx: &Cx) -> Result<SeeOther> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Ok(see_other(board_home(Some(&current))));
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }

    let media_key = |kind: &str, index: usize, file_name: &str| -> String {
        let ext = image_extension(file_name);
        if kind == "photo" {
            format!("srv/{raw}/gallery-{index}.{ext}")
        } else {
            format!("srv/{raw}/{kind}.{ext}")
        }
    };
    let existing_gallery: Vec<String> = board
        .board_gallery_keys
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let existing_icon = board.board_icon_key.clone().unwrap_or_default();
    let existing_banner = board.board_banner_key.clone().unwrap_or_default();

    let mut icon_key = existing_icon;
    let mut banner_key = existing_banner;
    let mut gallery = existing_gallery;
    while let Some(field) = upload.next_field().await? {
        let name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(str::to_owned).unwrap_or_default();
        if file_name.is_empty() || !["icon", "banner", "photos"].contains(&name.as_str()) {
            continue;
        }
        let bytes = field.bytes().await?;
        if bytes.is_empty() {
            continue;
        }
        match name.as_str() {
            "icon" => {
                let key = media_key("icon", 0, &file_name);
                if !icon_key.is_empty() && icon_key != key {
                    storage.delete(&icon_key).await.ok();
                }
                storage
                    .put(&key, bytes, infer_image_content_type(&file_name))
                    .await?;
                icon_key = key;
            }
            "banner" => {
                let key = media_key("banner", 0, &file_name);
                if !banner_key.is_empty() && banner_key != key {
                    storage.delete(&banner_key).await.ok();
                }
                storage
                    .put(&key, bytes, infer_image_content_type(&file_name))
                    .await?;
                banner_key = key;
            }
            "photos" => {
                let key = media_key("photo", gallery.len(), &file_name);
                storage
                    .put(&key, bytes, infer_image_content_type(&file_name))
                    .await?;
                gallery.push(key);
            }
            _ => {}
        }
    }
    let _: serde_json::Value = db
        .query(models::set_board_media(
            raw.to_string(),
            icon_key,
            banner_key,
            serde_json::to_string(&gallery).unwrap_or_else(|_| "[]".to_string()),
        )?)
        .await?;
    Ok(see_other(&format!("/sl/{raw}?media=1")))
}

/// `POST /sl/{board}/media/remove`: remove this space's icon, banner or one
/// gallery photo — deletes the stored object and updates the catalog row, so
/// the console's previews and the public card update immediately.
#[route(POST "/sl/{board_id}/media/remove")]
async fn remove_space_media(Form(form): Form<MediaRemoveForm>, cx: &Cx) -> Result<SeeOther> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Ok(see_other(board_home(Some(&current))));
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    let mut icon_key = board.board_icon_key.clone().unwrap_or_default();
    let mut banner_key = board.board_banner_key.clone().unwrap_or_default();
    let mut gallery: Vec<String> = board
        .board_gallery_keys
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    match form.which.as_str() {
        "icon" => {
            if !icon_key.is_empty() {
                storage.delete(&icon_key).await.ok();
                icon_key.clear();
            }
        }
        "banner" => {
            if !banner_key.is_empty() {
                storage.delete(&banner_key).await.ok();
                banner_key.clear();
            }
        }
        "photo" => {
            let seq = form
                .seq
                .as_deref()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|s| *s < gallery.len());
            if let Some(seq) = seq {
                let key = gallery.remove(seq);
                storage.delete(&key).await.ok();
            }
        }
        _ => {}
    }
    let _: serde_json::Value = db
        .query(models::set_board_media(
            raw.to_string(),
            icon_key,
            banner_key,
            serde_json::to_string(&gallery).unwrap_or_else(|_| "[]".to_string()),
        )?)
        .await?;
    Ok(see_other(&format!("/sl/{raw}?media=1")))
}

/// `POST /sl/{board}/deploy`: copy `staging/` → `dist/`, stamp
/// `boardDeployedAt`, and optionally publish (checkbox on the form).
#[route(POST "/sl/{board_id}/deploy")]
async fn deploy_space(Form(form): Form<VisibilityForm>, cx: &Cx) -> Result<SeeOther> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);
    let storage = platform_core::storage(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Ok(see_other(board_home(Some(&current))));
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    let deployed = files::deploy_staging_to_dist(storage, raw)
        .await
        .map_err(|e| bad_request(format!("deploy failed: {e}")))?;
    if deployed == 0 {
        return Ok(see_other(&format!("/sl/{raw}?err=empty")));
    }
    let _: serde_json::Value = db.query(models::set_board_deployed(raw.to_string())?).await?;
    if form.is_public.is_some() {
        let _: serde_json::Value =
            db.query(models::set_board_public(raw.to_string(), true)?).await?;
    }
    Ok(see_other(&format!("/sl/{raw}?deployed={deployed}")))
}

// ---------------------------------------------------------------------------
// public spaces (marketplace mirror)
// ---------------------------------------------------------------------------

/// `GET /spaces/{tenant_id}`: a tenant's public grid of deployed+published
/// spaces. Each card links straight to the running app at `/s/{board}`.
#[page("/spaces/{tenant_id}")]
async fn spaces_page(cx: &Cx) -> Result {
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
    let spaces: models::BoardPage = if staff {
        db.query(models::boards_by_tenant(tenant_id)?).await?
    } else {
        db.query(models::spaces_by_tenant(tenant_id)?).await?
    };

    view! {
        <a
            class="mb-6 inline-block text-sm text-sky-400 hover:text-sky-300"
            href=("/tenant/".to_owned() + &tenant_id.to_string())
        >
            "← Back to the space"
        </a>
        <div class="mb-6 flex items-baseline gap-3">
            <h1 class="text-3xl font-bold">
                (tenant.tenant_name.as_deref().unwrap_or("Spaces"))
            </h1>
            <span class="rounded-full border border-sky-700 bg-sky-900/40 px-2 py-0.5 text-xs text-sky-200">
                "App"
            </span>
        </div>
        if spaces.items.is_empty() {
            <p class="py-10 text-sm text-zinc-400">
                "No public apps here yet."
            </p>
        } else {
            <ul class="grid list-none grid-cols-1 gap-4 p-0 sm:grid-cols-2 lg:grid-cols-3">
                for space in &spaces.items {
                    let title = space.board_title.clone().unwrap_or_default();
                    let initial = title.chars().next().unwrap_or('?');
                    let icon_key = space.board_icon_key.as_deref().filter(|k| !k.is_empty());
                    let banner_key = space.board_banner_key.as_deref().filter(|k| !k.is_empty());
                    let gallery: Vec<String> = space
                        .board_gallery_keys
                        .as_deref()
                        .and_then(|json| serde_json::from_str(json).ok())
                        .unwrap_or_default();
                    let open_url = "/s/".to_owned() + space.board_id.as_deref().unwrap_or_default();
                    <li>
                        <a
                            class="block overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900 transition hover:-translate-y-0.5 hover:border-sky-400"
                            href=(open_url.clone())
                        >
                            match banner_key {
                                Some(key) => {
                                    <img class="aspect-video w-full bg-zinc-800 object-cover"
                                        src=("/blob/".to_owned() + &platform_core::encode_key(key))
                                        alt=(title.clone()) />
                                },
                                None => {
                                    <div class="flex aspect-video w-full items-center justify-center bg-zinc-800 text-sm text-zinc-600">
                                        "App"
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
                                            <img class="h-10 w-10 shrink-0 rounded-lg border border-zinc-700 bg-zinc-800 object-cover"
                                                src=("/blob/".to_owned() + &platform_core::encode_key(key))
                                                alt=(title.clone()) />
                                        },
                                        None => {
                                            <div class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg border border-zinc-700 bg-zinc-800 text-base text-zinc-500">
                                                (initial)
                                            </div>
                                        },
                                    }
                                    <div class="min-w-0 flex-1">
                                        <div class="flex items-center gap-2">
                                            <div class="truncate text-base font-semibold text-zinc-100">
                                                (title.clone())
                                            </div>
                                            if space.board_deployed_at.is_some() {
                                                <span class="shrink-0 rounded-full border border-emerald-900 bg-emerald-950/60 px-2 py-0.5 text-[10px] font-semibold text-emerald-300">
                                                    "LIVE"
                                                </span>
                                            }
                                        </div>
                                        <p class="mt-1 text-sm text-zinc-400">
                                            (space.board_description.clone().unwrap_or_default())
                                        </p>
                                    </div>
                                </div>
                                <span
                                    class="mt-3 inline-block rounded-md bg-sky-600 px-3 py-1 text-xs font-semibold text-white"
                                >
                                    "Open app"
                                </span>
                            </div>
                        </a>
                    </li>
                }
            </ul>
        }
    }
}

// ---------------------------------------------------------------------------
// serving deployed / staged builds
// ---------------------------------------------------------------------------

/// True for live spaces: published **and** deployed at least once.
fn is_live(board: &models::ServerlessBoard) -> bool {
    board.board_public_reads.unwrap_or(false) && board.board_deployed_at.is_some()
}

/// Rewrite root-absolute asset references in an HTML build file so the app
/// renders from its mount point (`/s/{board}/…` or the preview prefix).
/// Skips external URLs, protocol-relative refs, and already-mounted paths.
fn rewrite_html_assets(html: &str, mount: &str) -> String {
    const PATTERNS: [&str; 4] = ["=\"/", "='/", "(\"/", "('/"];
    // The pattern consumes the absolute path's leading `/`, so the mount is
    // joined without one.
    let mount = mount.trim_start_matches('/');
    let mut out = String::with_capacity(html.len() + 128);
    let mut rest = html;
    loop {
        let mut hit: Option<(usize, &str)> = None;
        for pattern in PATTERNS {
            if let Some(idx) = rest.find(pattern) {
                if hit.map_or(true, |(cur, _)| idx < cur) {
                    hit = Some((idx, pattern));
                }
            }
        }
        let Some((idx, pattern)) = hit else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..idx + pattern.len()]);
        let tail = &rest[idx + pattern.len()..];
        let skip = tail.starts_with('/')
            || tail.starts_with("http:")
            || tail.starts_with("https:")
            || tail.starts_with("mailto:")
            || tail.starts_with("tel:")
            || tail.starts_with(mount);
        if skip {
            out.push_str(tail);
            break;
        }
        out.push_str(mount);
        out.push('/');
        rest = tail;
    }
    out
}

/// Stream one build file with index.html fallback + SPA rewrite.
async fn serve_build_file(
    cx: &Cx,
    board_id: &str,
    root: &str,
    mount: &str,
    path: Option<&str>,
) -> Result<topcoat::router::response::Response> {
    let storage = platform_core::storage(cx);
    let Some(file) = files::get_build_file(storage, board_id, root, path).await? else {
        return Err(not_found().into());
    };
    let is_html = file.content_type.starts_with("text/html");
    let bytes = if is_html {
        let text = String::from_utf8_lossy(&file.bytes);
        Bytes::from(rewrite_html_assets(&text, mount))
    } else {
        file.bytes
    };
    let cache = if is_html {
        "no-cache"
    } else if files::is_immutable_asset(path.unwrap_or("")) {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    Ok(
        (([("content-type", file.content_type), ("cache-control", cache)], bytes),)
            .into_response(cx)?,
    )
}

/// `GET /s/{board_id}`: the deployed space's entry point.
#[route(GET "/s/{board_id}")]
async fn space_serve_root(cx: &Cx) -> Result<topcoat::router::response::Response> {
    let raw: &str = path_param::<BoardId>(cx);
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_live(&board) {
        return Err(not_found().into());
    }
    serve_build_file(cx, raw, "dist", &format!("/s/{raw}"), None).await
}

/// `GET /s/{board_id}/{encoded}`: one deployed build file (base64url path).
#[route(GET "/s/{board_id}/{encoded}")]
async fn space_serve_file(cx: &Cx) -> Result<topcoat::router::response::Response> {
    let raw: &str = path_param::<BoardId>(cx);
    let encoded: &str = path_param::<Encoded>(cx);
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_live(&board) {
        return Err(not_found().into());
    }
    let Some(path) = decode_build_path(encoded) else {
        return Err(not_found().into());
    };
    serve_build_file(cx, raw, "dist", &format!("/s/{raw}"), Some(&path)).await
}

/// `GET /sl/{board_id}/preview`: staff preview of the staged build's entry.
#[route(GET "/sl/{board_id}/preview")]
async fn preview_root(cx: &Cx) -> Result<topcoat::router::response::Response> {
    let raw: &str = path_param::<BoardId>(cx);
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    serve_build_file(cx, raw, "staging", &format!("/sl/{raw}/preview"), None).await
}

/// `GET /sl/{board_id}/preview/{encoded}`: one staged build file.
#[route(GET "/sl/{board_id}/preview/{encoded}")]
async fn preview_file(cx: &Cx) -> Result<topcoat::router::response::Response> {
    let raw: &str = path_param::<BoardId>(cx);
    let encoded: &str = path_param::<Encoded>(cx);
    let auth = platform_core::auth(cx);
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    let Some(board) = board_row(cx, raw).await? else {
        return Err(not_found().into());
    };
    if !is_staff_for(Some(&current), &board) {
        return Err(forbidden().into());
    }
    let Some(path) = decode_build_path(encoded) else {
        return Err(not_found().into());
    };
    serve_build_file(cx, raw, "staging", &format!("/sl/{raw}/preview"), Some(&path)).await
}

/// Decode a base64url-encoded build path, rejecting escapes (`..`, absolute).
fn decode_build_path(encoded: &str) -> Option<String> {
    let decoded = platform_core::decode_key(encoded)?;
    files::sanitize_build_path(&decoded)
}

// ---------------------------------------------------------------------------
// JSON API
// ---------------------------------------------------------------------------

/// A resolved key against a board: an API key token (`board:bucket:secret`)
/// or the owner key (board catalog hash).
#[derive(Clone)]
pub(crate) enum ApiAuth {
    Key(serverless_core::SrvApiKey),
    Owner(platform_auth::Role),
}

/// Resolve a bearer/`x-srv-key` secret to a board: an API key token
/// (`board:bucket:secret`) or the owner key (board catalog hash).
async fn resolve_key(cx: &Cx, board_id: &str, token: &str) -> Result<Option<ApiAuth>> {
    let owner_secret = token
        .strip_prefix(&format!("{board_id}:owner:"))
        .unwrap_or(token);
    if let Some(board) = board_row(cx, board_id).await? {
        if owner_key_matches(&board, owner_secret) {
            return Ok(Some(ApiAuth::Owner(platform_auth::Role::Owner)));
        }
    }
    let store = app_context::<TursoStore>(cx);
    match store.key_auth(token).await {
        Ok(Some((board_id_of, key))) if board_id_of == board_id => Ok(Some(ApiAuth::Key(key))),
        _ => Ok(None),
    }
}

async fn api_auth(cx: &Cx, board_id: &str) -> Result<(models::ServerlessBoard, Option<ApiAuth>)> {
    api_auth_impl(cx, board_id, true).await
}

/// Like `api_auth`, but never applies the rate limit (used by the rate
/// set/clear routes themselves, which must stay reachable when limited).
async fn api_auth_unlimited(cx: &Cx, board_id: &str) -> Result<(models::ServerlessBoard, Option<ApiAuth>)> {
    api_auth_impl(cx, board_id, false).await
}

async fn api_auth_impl(
    cx: &Cx,
    board_id: &str,
    apply_rate: bool,
) -> Result<(models::ServerlessBoard, Option<ApiAuth>)> {
    let Some(board) = board_row(cx, board_id).await? else {
        return Err(not_found().into());
    };
    let Some(token) = bearer(cx) else {
        return Ok((board, None));
    };
    let auth = resolve_key(cx, board_id, &token).await?;
    if apply_rate {
        if let Some(a) = &auth {
            let bucket = match a {
                ApiAuth::Key(k) => k.bucket.clone(),
                ApiAuth::Owner(_) => "owner".to_string(),
            };
            let store = app_context::<TursoStore>(cx);
            if !store.rate_allow(board_id, &bucket).await {
                return Err(too_many_requests(1).into());
            }
        }
    }
    Ok((board, auth))
}

pub(crate) fn requires(auth: &Option<ApiAuth>, min: KeyRole) -> Result<()> {
    let have = match auth {
        Some(ApiAuth::Key(k)) => k.role,
        Some(ApiAuth::Owner(_)) => KeyRole::Admin,
        None => KeyRole::Reader,
    };
    let ok = match min {
        KeyRole::Reader => true,
        KeyRole::Writer => have != KeyRole::Reader,
        KeyRole::Admin => have == KeyRole::Admin,
    };
    if ok { Ok(()) } else { Err(forbidden().into()) }
}

pub(crate) fn writer_of(auth: &Option<ApiAuth>) -> Option<String> {
    match auth {
        Some(ApiAuth::Key(k)) => k.writer.clone(),
        _ => None,
    }
}

/// `GET /api/srv/{board}`: board meta (public under `public_reads`).
#[route(GET "/api/srv/{board_id}")]
async fn api_board(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let store = app_context::<TursoStore>(cx);
    let count = store.record_count(NO_TENANT, raw).await.unwrap_or(0);
    Ok(Json(json!({
        "boardId": board.board_id,
        "title": board.board_title,
        "publicReads": board.board_public_reads.unwrap_or(false),
        "uniqueKey": board.board_unique_key,
        "schemaJson": board.board_schema_json,
        "recordCount": count,
        "deployedAt": board.board_deployed_at,
    })))
}

/// `POST /api/srv/{board}/records`: `{"payload": {...}, "upsert": false?}`.
#[route(POST "/api/srv/{board_id}/records")]
async fn api_record_insert(Json(req): Json<RecordInsert>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    let seq = store
        .record_insert(NO_TENANT, raw, &req.payload, writer.as_deref(), req.upsert)
        .await
        .map_err(|e| bad_request(format!("insert failed: {e}")))?;
    Ok(Json(json!({ "seq": seq, "boardId": raw })))
}

/// `GET /api/srv/{board}/records?limit&before&offset`, enriched with
/// `q` (substring search, `hl=1` → `<mark>` highlight), `filter`
/// (JSON conditions `[{"field","op","value"}]` or a single object),
/// `order`/`desc` (sort by a JSON path) — search/filter take precedence.
#[route(GET "/api/srv/{board_id}/records")]
async fn api_record_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let q = parse_query_params::<ListQuery>(cx).unwrap_or_default();
    let limit = q.limit.unwrap_or(25).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let store = app_context::<TursoStore>(cx);
    let items = if let Some(query) = q.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        store
            .record_search(raw, query, q.hl.as_deref().is_some_and(truthy), limit, offset)
            .await
            .map_err(|e| bad_request(format!("search failed: {e}")))?
    } else if let Some(path) = q.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        let eq = q
            .eq
            .as_deref()
            .filter(|e| !e.trim().is_empty())
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|_| bad_request("eq must be JSON"))?;
        store
            .record_query(raw, path, eq.as_ref(), limit, offset)
            .await
            .map_err(|e| bad_request(format!("query failed: {e}")))?
    } else if let Some(filter) = q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
        let filter_value: Value = serde_json::from_str(filter)
            .map_err(|_| bad_request("filter must be JSON"))?;
        let parsed = crate::dsl::parse_filter(&filter_value)
            .map_err(|e| bad_request(format!("bad filter: {e}")))?;
        let orders = q
            .order
            .as_deref()
            .filter(|o| !o.is_empty())
            .map(|o| vec![(o.to_string(), q.desc.as_deref().map(|d| !truthy(d)).unwrap_or(true))])
            .unwrap_or_default();
        store
            .record_filter(raw, &parsed.conds, &orders, limit, offset)
            .await
            .map_err(|e| bad_request(format!("filter failed: {e}")))?
    } else {
        store
            .record_list(NO_TENANT, raw, limit, q.before, offset)
            .await
            .map_err(|e| bad_request(format!("list failed: {e}")))?
    };
    Ok(Json(json!({ "items": items })))
}

/// `GET /api/srv/{board}/records/{seq}`.
#[route(GET "/api/srv/{board_id}/records/{seq}")]
async fn api_record_get(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    match store.record_get(NO_TENANT, raw, seq).await {
        Ok(Some(rec)) => Ok(Json(json!(rec))),
        Ok(None) => Err(not_found().into()),
        Err(e) => Err(bad_request(format!("get failed: {e}")).into()),
    }
}

/// `DELETE /api/srv/{board}/records/{seq}`.
#[route(DELETE "/api/srv/{board_id}/records/{seq}")]
async fn api_record_delete(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let deleted = store
        .record_delete(NO_TENANT, raw, seq)
        .await
        .map_err(|e| bad_request(format!("delete failed: {e}")))?;
    Ok(Json(json!({ "deleted": deleted })))
}

/// `POST /api/srv/{board}/keys`: `{"role": "reader|writer|admin", "writer"?}`.
#[route(POST "/api/srv/{board_id}/keys")]
async fn api_key_issue(Json(req): Json<KeyMint>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let role = match req.role.as_deref() {
        Some(r) if r == "reader" => KeyRole::Reader,
        Some(r) if r == "writer" => KeyRole::Writer,
        Some(r) if r == "admin" => KeyRole::Admin,
        _ => return Err(bad_request("invalid role").into()),
    };
    let store = app_context::<TursoStore>(cx);
    let (bucket, secret) = store
        .key_issue(NO_TENANT, raw, role, req.writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("key issue failed: {e}")))?;
    let token = format!("{raw}:{bucket}:{secret}");
    Ok(Json(json!({ "bucket": bucket, "token": token })))
}

/// `GET /api/srv/{board}/keys`.
#[route(GET "/api/srv/{board_id}/keys")]
async fn api_key_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let keys = store.key_list(NO_TENANT, raw).await.map_err(|e| bad_request(format!("list failed: {e}")))?;
    Ok(Json(json!({ "items": keys })))
}

/// `POST /api/srv/{board}/keys/{bucket}/revoke`.
#[route(POST "/api/srv/{board_id}/keys/{bucket}/revoke")]
async fn api_key_revoke(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let bucket: &str = path_param::<Bucket>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let revoked = store
        .key_revoke(NO_TENANT, raw, bucket)
        .await
        .map_err(|e| bad_request(format!("revoke failed: {e}")))?;
    Ok(Json(json!({ "revoked": revoked })))
}

/// `POST /api/srv/{board}/hooks`: `{"url": ..., "secret"?}`.
#[route(POST "/api/srv/{board_id}/hooks")]
async fn api_hook_register(Json(req): Json<HookMint>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Some(url) = req.url.filter(|u| u.starts_with("http")) else {
        return Err(bad_request("invalid hook url").into());
    };
    let store = app_context::<TursoStore>(cx);
    store
        .hook_register(NO_TENANT, raw, &url, req.secret.as_deref())
        .await
        .map_err(|e| bad_request(format!("hook failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/srv/{board}/hooks`.
#[route(GET "/api/srv/{board_id}/hooks")]
async fn api_hook_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let hooks = store.hook_list(NO_TENANT, raw).await.map_err(|e| bad_request(format!("list failed: {e}")))?;
    Ok(Json(json!({ "items": hooks })))
}

/// `DELETE /api/srv/{board}/hooks?url=<encoded>`.
#[route(DELETE "/api/srv/{board_id}/hooks")]
async fn api_hook_remove(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Some(url) = parse_query_params::<UrlForm>(cx).ok().and_then(|f| f.url) else {
        return Err(bad_request("missing url query param").into());
    };
    let store = app_context::<TursoStore>(cx);
    let removed = store
        .hook_remove(NO_TENANT, raw, &url)
        .await
        .map_err(|e| bad_request(format!("hook failed: {e}")))?;
    Ok(Json(json!({ "removed": removed })))
}

// ---------------------------------------------------------------------------
// Records: set / patch / filter / search / aggregate / bulk / delete-filter
// ---------------------------------------------------------------------------

/// `PUT /api/srv/{board}/records/{seq}`: replace the whole payload.
#[route(PUT "/api/srv/{board_id}/records/{seq}")]
async fn api_record_set(Json(req): Json<RecordInsert>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    store
        .record_set(NO_TENANT, raw, seq, &req.payload, writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("set failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "seq": seq })))
}

/// `PATCH /api/srv/{board}/records/{seq}`: `{"patch": {"$set": {...}, ...}}`.
#[route(PATCH "/api/srv/{board_id}/records/{seq}")]
async fn api_record_patch(Json(req): Json<PatchForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    let updated = store
        .record_patch(raw, seq, &req.patch, writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("patch failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "seq": seq, "payload": updated })))
}

/// `GET /api/srv/{board}/aggregate?op=count|sum|avg|min|max&field=&group_by=&filter=`.
#[route(GET "/api/srv/{board_id}/aggregate")]
async fn api_record_aggregate(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let q = parse_query_params::<AggregateQuery>(cx).unwrap_or_default();
    let Some(op) = q.op.as_deref().and_then(crate::dsl::Agg::parse) else {
        return Err(bad_request("op must be count|sum|avg|min|max").into());
    };
    let conds = match q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
        Some(filter) => {
            let filter_value: Value = serde_json::from_str(filter)
                .map_err(|_| bad_request("filter must be JSON"))?;
            crate::dsl::parse_filter(&filter_value)
                .map_err(|e| bad_request(format!("bad filter: {e}")))?
                .conds
        }
        None => Vec::new(),
    };
    let store = app_context::<TursoStore>(cx);
    let result = store
        .record_aggregate(raw, op, q.field.as_deref().unwrap_or("$"), q.group_by.as_deref(), &conds)
        .await
        .map_err(|e| bad_request(format!("aggregate failed: {e}")))?;
    Ok(Json(result))
}

/// `POST /api/srv/{board}/records/bulk`: `{"payloads": [{...}, ...]}` (alias `items`).
#[route(POST "/api/srv/{board_id}/records/bulk")]
async fn api_record_bulk(Json(req): Json<BulkInsert>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let store = app_context::<TursoStore>(cx);
    let writer = writer_of(&auth);
    let seqs = store
        .record_bulk_insert(raw, req.payloads, writer.as_deref())
        .await
        .map_err(|e| bad_request(format!("bulk failed: {e}")))?;
    Ok(Json(json!({ "seqs": seqs, "count": seqs.len() })))
}

/// `DELETE /api/srv/{board}/records?filter=<json>` — delete everything
/// matching the filter (no `filter` param on this route is a 400 guard).
#[route(DELETE "/api/srv/{board_id}/records")]
async fn api_record_delete_filter(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Some(filter) = parse_query_params::<FilterQuery>(cx).ok().and_then(|f| f.filter) else {
        return Err(bad_request("missing filter query param").into());
    };
    let filter_value: Value = serde_json::from_str(&filter)
        .map_err(|_| bad_request("filter must be JSON"))?;
    let parsed = crate::dsl::parse_filter(&filter_value)
        .map_err(|e| bad_request(format!("bad filter: {e}")))?;
    let store = app_context::<TursoStore>(cx);
    let deleted = store
        .record_delete_filter(raw, &parsed.conds)
        .await
        .map_err(|e| bad_request(format!("delete failed: {e}")))?;
    Ok(Json(json!({ "deleted": deleted })))
}

/// `POST /api/srv/{board}/records/{seq}/links`: `{"parent": seq, "via": "x"}`.
#[route(POST "/api/srv/{board_id}/records/{seq}/links")]
async fn api_link_set(Json(req): Json<LinkForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let (Some(parent), Some(via)) = (req.parent, req.via.as_deref().filter(|v| !v.is_empty())) else {
        return Err(bad_request("need parent seq and via").into());
    };
    let store = app_context::<TursoStore>(cx);
    store
        .link_set(raw, seq, parent, via)
        .await
        .map_err(|e| bad_request(format!("link failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/srv/{board}/records/{seq}/links` — resolve the record's parents.
#[route(GET "/api/srv/{board_id}/records/{seq}/links")]
async fn api_link_get(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let store = app_context::<TursoStore>(cx);
    let links = store.link_get(raw, seq).await.map_err(|e| bad_request(format!("links failed: {e}")))?;
    Ok(Json(json!({ "items": links })))
}

/// `DELETE /api/srv/{board}/records/{seq}/links?via=` — remove one link.
#[route(DELETE "/api/srv/{board_id}/records/{seq}/links")]
async fn api_link_clear(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let seq: &str = path_param::<Seq>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Ok(seq) = seq.parse::<i64>() else {
        return Err(bad_request("invalid seq").into());
    };
    let Some(via) = parse_query_params::<ViaQuery>(cx).ok().and_then(|v| v.via) else {
        return Err(bad_request("missing via query param").into());
    };
    let store = app_context::<TursoStore>(cx);
    let cleared = store.link_clear(raw, seq, &via).await.map_err(|e| bad_request(format!("link failed: {e}")))?;
    Ok(Json(json!({ "cleared": cleared })))
}

// ---------------------------------------------------------------------------
// Calls (SSRF-guarded outbound HTTP), jobs, deliveries, config
// ---------------------------------------------------------------------------

/// `POST /api/srv/{board}/call`: `{"url", "method"?, "headers"?, "body"?,
/// "timeout_ms"?}` — server-side HTTP with the SSRF guard; response body
/// capped at 2MB.
#[route(POST "/api/srv/{board_id}/call")]
async fn api_record_call(Json(req): Json<CallBody>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Writer)?;
    let Some(url) = req.url.filter(|u| crate::netguard::valid_url(u)) else {
        return Err(bad_request("url rejected (public http(s) only)").into());
    };
    let method = req.method.as_deref().unwrap_or("POST").to_uppercase();
    let Ok(method) = reqwest::Method::from_bytes(method.as_bytes()) else {
        return Err(bad_request("invalid method").into());
    };
    let timeout_ms = req.timeout_ms.unwrap_or(10_000).clamp(500, 60_000);
    let mut builder = reqwest::Client::new()
        .request(method, &url)
        .timeout(std::time::Duration::from_millis(timeout_ms));
    if let Some(headers) = req.headers.as_ref() {
        for (k, v) in headers {
            if let Some(v) = v.as_str() {
                builder = builder.header(k, v);
            }
        }
    }
    if let Some(body) = req.body.as_ref() {
        builder = builder.body(body.to_string());
    }
    match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let mut text = resp.text().await.unwrap_or_default();
            let truncated = text.len() > 2 * 1024 * 1024;
            text.truncate(2 * 1024 * 1024);
            Ok(Json(json!({ "status": status, "body": text, "truncated": truncated })))
        }
        Err(e) => Err(bad_request(format!("call failed: {e}")).into()),
    }
}

/// `POST /api/srv/{board}/jobs`: `{"name", "schedule", "action"}` —
/// action is `{"type":"http"|"poll","url","method"?,"headers"?,"body"?,
/// "timeout_ms"?,"path"?}` (zw app_jobs shape).
#[route(POST "/api/srv/{board_id}/jobs")]
async fn api_job_add(Json(req): Json<JobForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let (Some(name), Some(schedule), Some(action)) = (
        req.name.filter(|n| !n.is_empty()),
        req.schedule.as_deref().filter(|s| !s.is_empty()),
        req.action.as_ref(),
    ) else {
        return Err(bad_request("need name, schedule and action").into());
    };
    let store = app_context::<TursoStore>(cx);
    let id = store
        .job_add(raw, &name, schedule, action)
        .await
        .map_err(|e| bad_request(format!("job failed: {e}")))?;
    Ok(Json(json!({ "id": id, "ok": true })))
}

/// `GET /api/srv/{board}/jobs`.
#[route(GET "/api/srv/{board_id}/jobs")]
async fn api_job_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let jobs = store.job_list(raw).await.map_err(|e| bad_request(format!("jobs failed: {e}")))?;
    Ok(Json(json!({ "items": jobs })))
}

/// `DELETE /api/srv/{board}/jobs/{id}`.
#[route(DELETE "/api/srv/{board_id}/jobs/{job_id}")]
async fn api_job_remove(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let job_id: &str = path_param::<JobId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Ok(id) = job_id.parse::<i64>() else {
        return Err(bad_request("invalid job id").into());
    };
    let store = app_context::<TursoStore>(cx);
    let removed = store.job_remove(raw, id).await.map_err(|e| bad_request(format!("job failed: {e}")))?;
    Ok(Json(json!({ "removed": removed })))
}

/// `GET /api/srv/{board}/hooks/deliveries?status=&limit=` — retry queue.
#[route(GET "/api/srv/{board_id}/hooks/deliveries")]
async fn api_delivery_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let q = parse_query_params::<DeliveriesQuery>(cx).unwrap_or_default();
    let store = app_context::<TursoStore>(cx);
    let items = store
        .delivery_list(raw, q.status.as_deref().filter(|s| !s.is_empty()), q.limit.unwrap_or(50).clamp(1, 500))
        .await
        .map_err(|e| bad_request(format!("deliveries failed: {e}")))?;
    Ok(Json(json!({ "items": items })))
}

/// `POST /api/srv/{board}/hooks/replay`: `{"id"?}` — re-queue failed or
/// dead deliveries (all of them when no id).
#[route(POST "/api/srv/{board_id}/hooks/replay")]
async fn api_delivery_replay(body: Body, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let bytes = topcoat::router::body_limit(cx);
    let raw_body = topcoat::router::to_bytes(body, bytes).await.map_err(|e| bad_request(format!("body too large: {e}")))?;
    let req: ReplayForm = if raw_body.is_empty() {
        ReplayForm::default()
    } else {
        serde_json::from_slice(&raw_body).map_err(|e| bad_request(format!("replay body: {e}")))?
    };
    let store = app_context::<TursoStore>(cx);
    let requeued = store
        .delivery_replay(raw, req.id)
        .await
        .map_err(|e| bad_request(format!("replay failed: {e}")))?;
    Ok(Json(json!({ "requeued": requeued })))
}

/// `GET /api/srv/{board}/config` — title/schema/ttl/rate/audit + more.
#[route(GET "/api/srv/{board_id}/config")]
async fn api_config_get(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let config = store.board_config(raw).await.map_err(|e| bad_request(format!("config failed: {e}")))?;
    Ok(Json(config))
}

/// `PUT /api/srv/{board}/ttl`: `{"seconds": N, "field": "expires_at"}` —
/// records with `payload[field]` older than N seconds are hidden + swept.
#[route(PUT "/api/srv/{board_id}/ttl")]
async fn api_ttl_set(Json(req): Json<TtlForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let (Some(seconds), Some(field)) = (req.seconds, req.field.as_deref().filter(|f| !f.is_empty())) else {
        return Err(bad_request("need seconds and field").into());
    };
    if seconds <= 0 {
        return Err(bad_request("seconds must be positive").into());
    }
    let store = app_context::<TursoStore>(cx);
    store.ttl_set(raw, seconds, field).await.map_err(|e| bad_request(format!("ttl failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "seconds": seconds, "field": field })))
}

/// `DELETE /api/srv/{board}/ttl` — disable TTL expiry.
#[route(DELETE "/api/srv/{board_id}/ttl")]
async fn api_ttl_clear(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    store.ttl_clear(raw).await.map_err(|e| bad_request(format!("ttl failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// `PUT /api/srv/{board}/audit`: `{"enabled": true|false}`.
#[route(PUT "/api/srv/{board_id}/audit")]
async fn api_audit_set(Json(req): Json<EnabledForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    store
        .audit_enable(raw, req.enabled)
        .await
        .map_err(|e| bad_request(format!("audit failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "enabled": req.enabled })))
}

/// `GET /api/srv/{board}/audit?limit=` — the append-only audit trail.
#[route(GET "/api/srv/{board_id}/audit")]
async fn api_audit_list(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let q = parse_query_params::<LimitQuery>(cx).unwrap_or_default();
    let store = app_context::<TursoStore>(cx);
    let items = store.audit_list(raw, q.limit.unwrap_or(50).clamp(1, 1000)).await.map_err(|e| bad_request(format!("audit failed: {e}")))?;
    Ok(Json(json!({ "items": items })))
}

/// `PUT /api/srv/{board}/rate`: `{"per_minute": N}` — in-memory sliding
/// window per bucket (default bucket = "api").
#[route(PUT "/api/srv/{board_id}/rate")]
async fn api_rate_set(Json(req): Json<RateForm>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth_unlimited(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let Some(per_minute) = req.per_minute else {
        return Err(bad_request("need per_minute").into());
    };
    if per_minute <= 0 {
        return Err(bad_request("per_minute must be positive").into());
    }
    let store = app_context::<TursoStore>(cx);
    store.rate_set(raw, per_minute).await.map_err(|e| bad_request(format!("rate failed: {e}")))?;
    Ok(Json(json!({ "ok": true, "per_minute": per_minute })))
}

/// `DELETE /api/srv/{board}/rate` — disable rate limiting.
#[route(DELETE "/api/srv/{board_id}/rate")]
async fn api_rate_clear(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth_unlimited(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    store.rate_set(raw, 0).await.map_err(|e| bad_request(format!("rate failed: {e}")))?;
    store.rate_reset(raw).await;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/srv/{board}/dump` — the whole board (config + records +
/// hooks + links) as one JSON document; `POST /restore` puts it back.
#[route(GET "/api/srv/{board_id}/dump")]
async fn api_dump(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let dump = store.record_dump(raw).await.map_err(|e| bad_request(format!("dump failed: {e}")))?;
    Ok(Json(dump))
}

#[route(POST "/api/srv/{board_id}/restore")]
async fn api_restore(Json(dump): Json<serde_json::Value>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (_board, auth) = api_auth(cx, raw).await?;
    requires(&auth, KeyRole::Admin)?;
    let store = app_context::<TursoStore>(cx);
    let counts = store.record_restore(raw, &dump).await.map_err(|e| bad_request(format!("restore failed: {e}")))?;
    Ok(Json(counts))
}

// ---------------------------------------------------------------------------
// Events (SSE) — realtime record stream with `after=`/`Last-Event-ID` resume
// ---------------------------------------------------------------------------

/// The SSE stream: replayed events first, then the live broadcast.
struct EventStream {
    replay: std::vec::IntoIter<serde_json::Value>,
    rx: tokio::sync::broadcast::Receiver<String>,
    filter: Option<String>,
}

impl futures_core::Stream for EventStream {
    type Item = Result<Event>;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        if let Some(data) = self.replay.next() {
            let id = data.get("id").and_then(serde_json::Value::as_i64).unwrap_or(0).to_string();
            let kind = data.get("event").and_then(serde_json::Value::as_str).unwrap_or("message");
            return match Event::new().event(kind).id(id).json_data(&data) {
                Ok(ev) => Poll::Ready(Some(Ok(ev))),
                Err(e) => Poll::Ready(Some(Err(e))),
            };
        }
        loop {
            match self.rx.try_recv() {
                Ok(raw) => {
                    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                    if let Some(filter) = self.filter.as_deref() {
                        let kind = value.get("event").and_then(Value::as_str).unwrap_or("");
                        if kind != filter {
                            continue;
                        }
                    }
                    let id = value.get("seq").and_then(Value::as_i64).unwrap_or(0).to_string();
                    let kind = value.get("event").and_then(Value::as_str).unwrap_or("message");
                    return match Event::new().event(kind).id(id).json_data(&value) {
                        Ok(ev) => Poll::Ready(Some(Ok(ev))),
                        Err(e) => Poll::Ready(Some(Err(e))),
                    };
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    // No poll API on this tokio's broadcast receiver: re-arm
                    // a wakeup so we re-poll shortly after events land (the
                    // spawned task never touches the receiver itself).
                    let waker = cx.waker().clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        waker.wake();
                    });
                    return Poll::Pending;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Poll::Ready(None),
            }
        }
    }
}

/// `GET /api/srv/{board}/events?after=&filter=` — the board's realtime
/// event stream (`record.created/updated/deleted`). `after` (or the
/// `Last-Event-ID` header) resumes from the newest events; `filter` keeps
/// one kind only. Public on `public_reads` boards.
#[route(GET "/api/srv/{board_id}/events")]
async fn api_events(cx: &Cx) -> Result<Sse<impl Stream<Item = Result<Event>> + use<>>> {
    let raw: &str = path_param::<BoardId>(cx);
    let (board, auth) = api_auth(cx, raw).await?;
    if !board.board_public_reads.unwrap_or(false) && auth.is_none() {
        return Err(unauthorized().into());
    }
    let q = parse_query_params::<EventsQuery>(cx).unwrap_or_default();
    let last_event = topcoat::router::content::sse::last_event_id(cx)
        .and_then(|s| s.parse::<i64>().ok());
    let after = q.after.or(last_event).unwrap_or(0).max(0);
    let filter = q.filter.as_deref().filter(|f| !f.is_empty()).map(str::to_owned);
    let store = app_context::<TursoStore>(cx);
    let replay = store
        .event_replay(raw, after, filter.as_deref(), 500)
        .await
        .map_err(|e| bad_request(format!("events failed: {e}")))?;
    let replay_values: Vec<Value> = replay
        .into_iter()
        .map(|(id, kind, seq, payload)| json!({
            "id": id,
            "event": kind,
            "seq": seq,
            "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
        }))
        .collect();
    let rx = store.subscribe(raw).await;
    Ok(Sse::new(EventStream {
        replay: replay_values.into_iter(),
        rx,
        filter,
    })
    .keep_alive(KeepAlive::new()))
}

// ---------------------------------------------------------------------------
// CORS preflight
// ---------------------------------------------------------------------------

fn cors_headers(cx: &Cx) -> topcoat::router::HeaderMap {
    let mut cors = topcoat::router::HeaderMap::new();
    cors.insert(
        "access-control-allow-origin",
        headers(cx)
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("*")
            .parse()
            .unwrap(),
    );
    cors.insert("access-control-allow-methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS".parse().unwrap());
    cors.insert("access-control-allow-headers", "authorization, content-type, x-srv-key, last-event-id".parse().unwrap());
    cors.insert("access-control-max-age", "86400".parse().unwrap());
    cors
}

macro_rules! cors_routes {
    ($( $name:ident => $path:literal ),* $(,)?) => {
        $(
            #[route(OPTIONS $path)]
            async fn $name(cx: &Cx) -> Result<impl IntoResponse> {
                let (cors, body) = (cors_headers(cx), Body::empty());
                Ok((cors, body))
            }
        )*
    };
}

cors_routes! {
    cors_records => "/api/srv/{board_id}/records",
    cors_record_seq => "/api/srv/{board_id}/records/{seq}",
    cors_record_bulk => "/api/srv/{board_id}/records/bulk",
    cors_record_links => "/api/srv/{board_id}/records/{seq}/links",
    cors_aggregate => "/api/srv/{board_id}/aggregate",
    cors_call => "/api/srv/{board_id}/call",
    cors_events => "/api/srv/{board_id}/events",
    cors_keys => "/api/srv/{board_id}/keys",
    cors_key_revoke => "/api/srv/{board_id}/keys/{bucket}/revoke",
    cors_hooks => "/api/srv/{board_id}/hooks",
    cors_deliveries => "/api/srv/{board_id}/hooks/deliveries",
    cors_replay => "/api/srv/{board_id}/hooks/replay",
    cors_jobs => "/api/srv/{board_id}/jobs",
    cors_job_seq => "/api/srv/{board_id}/jobs/{job_id}",
    cors_config => "/api/srv/{board_id}/config",
    cors_ttl => "/api/srv/{board_id}/ttl",
    cors_audit => "/api/srv/{board_id}/audit",
    cors_rate => "/api/srv/{board_id}/rate",
    cors_dump => "/api/srv/{board_id}/dump",
    cors_restore => "/api/srv/{board_id}/restore",
}

// ---------------------------------------------------------------------------
// MCP tool: ONE `cli` tool, zerowrapper-style (help layers + argv parsing).
// Public: catalog, doc, guide. Mutations: `POST /api/srv/mcp` behind
// `x-srv-key` — the id in the key must match the space id in the command.
// ---------------------------------------------------------------------------

/// `GET /api/srv/mcp`: help layer 0 — the single tool's overview.
#[route(GET "/api/srv/mcp")]
async fn mcp_catalog(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(json!({
        "mcp": "serverless-spaces",
        "tool": {
            "name": "cli",
            "summary": "The space CLI: deploy and manage a space (build files, records, keys, hooks) using the space id from your x-srv-key.",
            "usage": "cli <group> <verb> [args]   (groups: space, records, keys, hooks)",
            "input": {"args": ["string", "..."]},
            "auth": "header x-srv-key: <space_id>:owner:<secret> (or <space_id>:<bucket>:<secret>); every command manages that space, --space is optional",
            "helpLayers": [
                "cli / cli --help          overview (no key needed)",
                "cli about                  what a space is + how to get your key (no key needed)",
                "cli help <group>           per-group help (no key needed)",
                "cli help <group> <verb>    per-verb help — most detail (no key needed)",
                "GET /api/srv/mcp/guide     full manual"
            ],
        },
    })))
}

/// `GET /api/srv/mcp/doc/{tool}`: the single tool's doc (help layer 2).
#[route(GET "/api/srv/mcp/doc/{tool}")]
async fn mcp_tool_docs(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let tool: &str = path_param::<Tool>(cx);
    if tool != "cli" {
        return Err(not_found().into());
    }
    Ok(Json(json!({
        "tool": "cli",
        "usage": cli::GLOBAL_USAGE,
        "about": cli::ABOUT,
        "groups": ["space", "records", "keys", "hooks"],
        "example": {"tool": "cli", "params": {"args": ["space", "deploy", "--publish", "on"]}},
        "helpIsPublic": "every help layer answers without x-srv-key: cli, cli about, cli --help, cli help <group>, cli help <group> <verb>",
        "spaceIdDefaultsToKey": "commands with no --space manage the space in your x-srv-key",
        "errors": ["401 no key (commands only)", "403 role too low", "error JSON with usage/hint layers"]
    })))
}

/// `GET /api/srv/mcp/guide`: help layer 3 — the full manual.
#[route(GET "/api/srv/mcp/guide")]
async fn mcp_guide_route(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(mcp_guide()))
}

fn mcp_guide() -> serde_json::Value {
    json!({
        "title": "Serverless Spaces — MCP guide (one `cli` tool)",
        "sections": [
            {"heading": "1. What a space is",
             "body": cli::ABOUT},
            {"heading": "2. Create the space (web)",
             "body": "Sign in, open Portal → Spaces (or /sl) and create a board. The one-time owner key is shown once in the redirect URL: /sl/<space_id>?owner_key=<secret> — copy it."},
            {"heading": "3. Configure your MCP client",
             "body": "EASY MODE (no config JSON): hand the agent the ONE key string from the space page (<space_id>:owner:<secret> or an issued <space_id>:<bucket>:<token>) — it passes it as the id: cli space show --space <space_id>:owner:<secret> (or --key …). The leading part is the space id; the rest authenticates the call itself, so the client just points at the url and the agent drives the space. HEADER MODE: register one tool with header x-srv-key: <space_id>:owner:<secret> and every command manages the space in the header. Request body {\"tool\":\"cli\",\"params\":{\"args\":[...]}}. Sanity check: curl -X POST /api/srv/mcp -H \"content-type: application/json\" -d '{\"tool\":\"cli\",\"params\":{\"args\":[\"space\",\"show\",\"--space\",\"<space_id>:owner:<key>\"]}}'."},
            {"heading": "4. First call",
             "body": "cli space show → meta + record count for your space (with the key string as --space, or plain with the header). Running `cli` with no args prints the overview; all help layers are PUBLIC (no key needed): cli --help, cli help <group>, cli help <group> <verb> and /api/srv/mcp/guide — read them before you own a space."},
            {"heading": "5. Upload a build and deploy",
             "body": "For each file of your dist/: cli space files upload <path> --content <base64>. Then cli space deploy --publish on. The staging build is copied to dist/ and the space goes live at /s/<space_id>."},
            {"heading": "6. Records + webhooks",
             "body": "cli records insert/list/get/delete/count work like the HTTP API. cli hooks register lets you receive inserts via POST."},
            {"heading": "7. Troubleshooting",
             "body": "401/403 on a command — revoked or wrong-role key (cli keys issue). \"scoped to\" — your --space must match the key's space, or just omit it. \"nothing staged\" — upload files first. \"unknown space id\" — the key has no space; create one on the web at /sl."}
        ]
    })
}

/// `POST /api/srv/mcp`: run the single `cli` tool with
/// `{"tool":"cli","params":{"args":[...]}}` authenticated by `x-srv-key`.
/// Help-layer invocations (no args, `about`, `--help`, `help …`) are
/// public — answered before auth so an agent without a space can still
/// learn the tool from every help layer.
#[route(POST "/api/srv/mcp")]
async fn mcp_run(Json(req): Json<McpInvoke>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    if req.tool != "cli" {
        return Err(not_found().into());
    }
    let args = req.args_list();
    if let Some(value) = cli::help_response(&args) {
        return Ok(Json(value));
    }
    let store = app_context::<TursoStore>(cx);
    // The call may carry its own key — `--key` or `--space <composite>`
    // (the console's one copyable string <space_id>:<bucket>:<secret>); the
    // x-srv-key header is then optional, but when both exist they must name
    // the same space.
    let embedded = cli::embedded_key(&args);
    let header_token = headers(cx)
        .get("x-srv-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let (token, board_id) = match (embedded, header_token) {
        (Some((board, token)), Some(header)) => {
            let header_board = header.split(':').next().unwrap_or_default();
            if header_board != board {
                return Ok(Json(json!({
                    "status": "error",
                    "error": format!("key mismatch: the header is for {header_board}, the passed --space/--key string is for {board}"),
                    "hint": "use one key only — either the x-srv-key header or the string passed in the call"
                })));
            }
            (token, board)
        }
        (Some((board, token)), None) => (token, board),
        (None, Some(header)) => (header.to_string(), header.split(':').next().unwrap_or_default().to_string()),
        (None, None) => return Err(unauthorized().into()),
    };
    if board_row(cx, &board_id).await?.is_none() {
        return Err(not_found().into());
    }
    let Some(auth) = resolve_key(cx, &board_id, &token).await? else {
        return Err(unauthorized().into());
    };
    let cli = cli::Cli { cx, store, key_board: board_id, auth };
    match cli.run(&args).await {
        Ok(value) => Ok(Json(value)),
        Err(value) => Ok(Json(value)),
    }
}

// ---------------------------------------------------------------------------
// Native MCP over HTTP: `POST /mcp` speaks real MCP JSON-RPC (streamable-HTTP
// style) so any MCP client (opencode `type: "remote"`, Claude, …) points
// straight at the URL — no stdio bridge. ONE `cli` tool, same CLI + help
// layers as the envelope endpoint. Commands authenticate with the
// `x-srv-key` header — `<space_id>:owner:<secret>` or
// `<space_id>:<bucket>:<token>` — whose leading id IS the space being
// managed (--space is optional and must match), so the agent manages a space
// with the key alone. All help layers are public and need no key.
// ---------------------------------------------------------------------------

/// The MCP protocol version this server speaks.
const MCP_PROTOCOL: &str = "2025-06-18";

/// One JSON-RPC request body, leniently parsed (`id` optional for
/// notifications, `params` optional for `ping`).
#[derive(Deserialize)]
struct McpRpcMessage {
    id: Option<serde_json::Value>,
    #[serde(default)]
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

fn mcp_ok(id: Option<serde_json::Value>, result: serde_json::Value) -> serde_json::Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn mcp_error(id: Option<serde_json::Value>, code: i32, message: &str) -> serde_json::Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool result: text content, flagged as an error when the CLI failed.
fn mcp_tool_result(id: Option<serde_json::Value>, text: String, is_error: bool) -> serde_json::Value {
    mcp_ok(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
    )
}

/// The single `cli` tool's MCP definition (same shape as the bridge had).
fn mcp_cli_tool() -> serde_json::Value {
    json!({
        "name": "cli",
        "description": "Serverless-spaces CLI for LANsystem: deploy and manage a space (build files, records, keys, hooks). \
Syntax: cli <group> <verb> [args] with groups space/records/keys/hooks. Every command manages the space in the \
x-srv-key header (--space is optional and must match when given). NO-HEADER MODE: pass the space's key string \
as the id and the call authenticates itself — cli space show --space <space_id>:owner:<secret> (or \
--key <composite> / an issued <space_id>:<bucket>:<token>); the leading part is the space id. \
ALL help layers are PUBLIC (no key): cli, cli about, cli --help, cli help <group>, cli help <group> <verb>.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "CLI tokens, e.g. ['space','show'] or ['space','show','--space','<space_id>:owner:<secret>'] or ['help','space']"
                }
            },
            "required": ["args"]
        }
    })
}

/// `GET /mcp`: human/agent-readable catalog; MCP clients use `POST /mcp`.
#[route(GET "/mcp")]
async fn mcp_http_catalog(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(json!({
        "mcp": "serverless-spaces",
        "transport": "JSON-RPC over HTTP (MCP protocol 2025-06-18)",
        "endpoint": "POST /mcp  (initialize | tools/list | tools/call | ping)",
        "tool": mcp_cli_tool(),
        "auth": "header x-srv-key: <space_id>:owner:<secret> or <space_id>:<bucket>:<token> — commands only; each help layer is public. NO-HEADER MODE: pass the key string as the id in the call (cli space show --space <space_id>:owner:<secret> / --key) — that string IS the credential, no config JSON needed",
        "clients": [
            "opencode: {\"mcp\":{\"lansrv-spaces\":{\"type\":\"remote\",\"url\":\"http://<host>:4321/mcp\"}}}",
            "claude: mcpServers lansrv-spaces type http url http://<host>:4321/mcp"
        ]
    })))
}

/// `POST /mcp`: the real MCP endpoint — one `cli` tool, help layers public.
#[route(POST "/mcp")]
async fn mcp_http_rpc(Json(raw): Json<serde_json::Value>, cx: &Cx) -> Result<Json<serde_json::Value>> {
    // A non-json-rpc body should still answer as a JSON-RPC parse error.
    let Ok(msg) = serde_json::from_value::<McpRpcMessage>(raw) else {
        return Ok(Json(mcp_error(None, -32700, "Parse error: expected a JSON-RPC request")));
    };
    let id = msg.id.clone();
    match msg.method.as_str() {
        "initialize" => Ok(Json(mcp_ok(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "lansrv-spaces", "version": "1.0.0" },
            }),
        ))),
        "notifications/initialized" | "notifications/cancelled" => Ok(Json(serde_json::Value::Null)),
        "ping" => Ok(Json(mcp_ok(id, json!({})))),
        "tools/list" => Ok(Json(mcp_ok(id, json!({ "tools": [mcp_cli_tool()] })))),
        "tools/call" => {
            let params = msg.params.unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            if name != "cli" {
                return Ok(Json(mcp_error(id, -32601, "Method not found: tools/call for unknown tool")));
            }
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            // Reuse the envelope's tokenizer (array of tokens or one string).
            let envelope = McpInvoke { tool: "cli".to_string(), params: arguments.as_object().cloned().unwrap_or_default() };
            let args = envelope.args_list();
            // Help layers are public — no key needed.
            if let Some(value) = cli::help_response(&args) {
                return Ok(Json(mcp_tool_result(id, serde_json::to_string(&value).unwrap_or_default(), false)));
            }
            let store = app_context::<TursoStore>(cx);
            // Commands need a key. It may be the x-srv-key header, OR the
            // key string itself passed as the id (`--key`/`--space
            // <space_id>:<bucket>:<secret>`) — the console's one copyable
            // string, so a client needs NO config JSON. When both exist
            // they must name the same space.
            let embedded = cli::embedded_key(&args);
            let header_token = headers(cx)
                .get("x-srv-key")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let (token, board_id) = match (embedded, header_token) {
                (Some((board, token)), Some(header)) => {
                    let header_board = header.split(':').next().unwrap_or_default();
                    if header_board != board {
                        let text = serde_json::to_string(&json!({
                            "status": "error",
                            "error": format!("key mismatch: the header is for {header_board}, the passed --space/--key string is for {board}"),
                            "hint": "use one key only — either the x-srv-key header or the string passed in the call"
                        })).unwrap_or_default();
                        return Ok(Json(mcp_tool_result(id, text, true)));
                    }
                    (token, board)
                }
                (Some((board, token)), None) => (token, board),
                (None, Some(header)) => (header.to_string(), header.split(':').next().unwrap_or_default().to_string()),
                (None, None) => {
                    let text = serde_json::to_string(&json!({
                        "status": "error",
                        "error": "401: no key — no x-srv-key header and no key string in the call",
                        "hint": "set the header x-srv-key: <space_id>:owner:<secret>, or pass the key string as the id: cli space show --space <space_id>:owner:<secret>; help layers need no key"
                    })).unwrap_or_default();
                    return Ok(Json(mcp_tool_result(id, text, true)));
                }
            };
            if board_row(cx, &board_id).await?.is_none() {
                let text = serde_json::to_string(&json!({
                    "status": "error",
                    "error": "404: unknown space id in key",
                    "hint": "create a space on the web at /sl (or /portal/serverless) and copy its one-time owner key"
                })).unwrap_or_default();
                return Ok(Json(mcp_tool_result(id, text, true)));
            }
            let Some(auth) = resolve_key(cx, &board_id, &token).await? else {
                let text = serde_json::to_string(&json!({
                    "status": "error",
                    "error": "401: bad or revoked key",
                    "hint": "issue a scoped key (cli keys issue) or use the owner key from the create redirect"
                })).unwrap_or_default();
                return Ok(Json(mcp_tool_result(id, text, true)));
            };
            let cli = cli::Cli { cx, store, key_board: board_id, auth };
            match cli.run(&args).await {
                Ok(value) => Ok(Json(mcp_tool_result(id, serde_json::to_string(&value).unwrap_or_default(), false))),
                Err(value) => Ok(Json(mcp_tool_result(id, serde_json::to_string(&value).unwrap_or_default(), true))),
            }
        }
        other => Ok(Json(mcp_error(id, -32601, &format!("Method not found: {other}")))),
    }
}

/// Borrow an `ApiAuth` so a checked-in value can flow through `requires()`.
pub(crate) fn borrow_auth(auth: &ApiAuth) -> ApiAuth {
    match auth {
        ApiAuth::Key(k) => ApiAuth::Key(k.clone()),
        ApiAuth::Owner(r) => ApiAuth::Owner(*r),
    }
}

// ---------------------------------------------------------------------------
// form/query types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateForm {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    schema_json: Option<String>,
    #[serde(default)]
    public_reads: bool,
    #[serde(default)]
    unique_key: Option<String>,
}

#[derive(Deserialize)]
struct UpdateForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    schema_json: Option<String>,
    #[serde(default)]
    unique_key: Option<String>,
}

#[derive(Deserialize)]
struct VisibilityForm {
    #[serde(default)]
    is_public: Option<String>,
}

/// `which` is `icon`, `banner` or `photo`; `seq` is the gallery index for
/// `photo`.
#[derive(Deserialize)]
struct MediaRemoveForm {
    #[serde(default)]
    which: String,
    #[serde(default)]
    seq: Option<String>,
}

/// The MCP invoke body: `{"tool": "cli", "params": {"args": [...]}}`.
#[derive(Deserialize)]
struct McpInvoke {
    tool: String,
    #[serde(default)]
    params: serde_json::Map<String, serde_json::Value>,
}

impl McpInvoke {
    /// `args` may be an array of tokens or a single command string.
    fn args_list(&self) -> Vec<String> {
        match self.params.get("args") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect(),
            Some(Value::String(raw)) => raw.split_whitespace().map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Deserialize)]
struct RoleForm {
    role: Option<String>,
    #[serde(default)]
    writer: Option<String>,
}

#[derive(Deserialize)]
struct BucketForm {
    bucket: Option<String>,
}

#[derive(Deserialize)]
struct HookForm {
    url: Option<String>,
    #[serde(default)]
    secret: Option<String>,
}

#[derive(Deserialize)]
struct UrlForm {
    url: Option<String>,
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    eq: Option<String>,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    order: Option<String>,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    hl: Option<String>,
}

/// `PATCH /records/{seq}` body: the JSON-patch ops object.
#[derive(Deserialize)]
struct PatchForm {
    patch: serde_json::Value,
}

/// `GET /aggregate` query params.
#[derive(Deserialize, Default)]
struct AggregateQuery {
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    field: Option<String>,
    #[serde(default)]
    group_by: Option<String>,
    #[serde(default)]
    filter: Option<String>,
}

/// `POST /records/bulk` body.
#[derive(Deserialize)]
struct BulkInsert {
    #[serde(alias = "items")]
    payloads: Vec<serde_json::Value>,
}

/// `DELETE /records?filter=` query param.
#[derive(Deserialize)]
struct FilterQuery {
    filter: Option<String>,
}

/// `POST /call` body.
#[derive(Deserialize)]
struct CallBody {
    url: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    body: Option<serde_json::Value>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// `POST /jobs` body.
#[derive(Deserialize)]
struct JobForm {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    action: Option<serde_json::Value>,
}

/// `GET /hooks/deliveries` query params.
#[derive(Deserialize, Default)]
struct DeliveriesQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

/// `POST /hooks/replay` body.
#[derive(Deserialize, Default)]
struct ReplayForm {
    #[serde(default)]
    id: Option<i64>,
}

/// `PUT /ttl` body.
#[derive(Deserialize)]
struct TtlForm {
    seconds: Option<i64>,
    #[serde(default)]
    field: Option<String>,
}

/// `PUT /audit` body.
#[derive(Deserialize)]
struct EnabledForm {
    enabled: bool,
}

/// `PUT /rate` body.
#[derive(Deserialize)]
struct RateForm {
    per_minute: Option<i64>,
}

/// `POST /sl/{board}/jobs/add` form — the action arrives as a JSON string.
#[derive(Deserialize)]
struct WebJobForm {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

/// `POST /sl/{board}/jobs/remove` form.
#[derive(Deserialize)]
struct JobIdForm {
    #[serde(default)]
    id: Option<i64>,
}

/// `POST /sl/{board}/audit` form — checkbox value is `on`/`off`.
#[derive(Deserialize)]
struct WebEnabledForm {
    #[serde(default)]
    enabled: Option<String>,
}

/// `POST /sl/{board}/restore` form — dump JSON in a textarea.
#[derive(Deserialize)]
struct RestoreForm {
    dump: String,
}

/// `GET /sl/{board}` — records view controls.
#[derive(Deserialize, Default)]
struct BoardQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    order: Option<String>,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    agg_op: Option<String>,
    #[serde(default)]
    agg_field: Option<String>,
    #[serde(default)]
    agg_group: Option<String>,
}

/// `GET /audit?limit=` query param.
#[derive(Deserialize, Default)]
struct LimitQuery {
    #[serde(default)]
    limit: Option<i64>,
}

/// `POST /records/{seq}/links` body.
#[derive(Deserialize)]
struct LinkForm {
    parent: Option<i64>,
    #[serde(default)]
    via: Option<String>,
}

/// `DELETE /records/{seq}/links?via=` query param.
#[derive(Deserialize)]
struct ViaQuery {
    via: Option<String>,
}

/// `GET /events?after=&filter=` query params.
#[derive(Deserialize, Default)]
struct EventsQuery {
    #[serde(default)]
    after: Option<i64>,
    #[serde(default)]
    filter: Option<String>,
}

#[derive(Deserialize)]
struct OwnerQuery {
    #[serde(default)]
    owner_key: Option<String>,
}

#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize)]
struct ErrorQuery {
    #[serde(default)]
    err: Option<String>,
}

#[derive(Deserialize)]
struct MediaQuery {
    #[serde(default)]
    media: Option<String>,
}

#[derive(Deserialize)]
struct RecordInsert {
    payload: serde_json::Value,
    #[serde(default)]
    upsert: bool,
}

#[derive(Deserialize)]
struct KeyMint {
    role: Option<String>,
    #[serde(default)]
    writer: Option<String>,
}

#[derive(Deserialize)]
struct HookMint {
    url: Option<String>,
    #[serde(default)]
    secret: Option<String>,
}

// ---------------------------------------------------------------------------
// ids
// ---------------------------------------------------------------------------

/// The file extension to bake into a media key so `/blob` serves a correct
/// content type (`image/jpeg` fallback for unknown sources).
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

/// Sniff a content type for an uploaded photo by extension.
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

/// Accept `1|true|on|yes` as truthy query values (curl-friendly).
fn truthy(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "on" | "yes")
}

/// Read a string key from a JSON row (jobs/deliveries/config), blank when missing.
fn jv_str(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string).unwrap_or_default()
}

/// Read an integer key from a JSON row, 0 when missing.
fn jv_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn empty_to_none(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.trim().is_empty())
}

/// `sl-{millis:x}-{counter:04x}` — locally-unique enough for a LAN (same
/// scheme as `pd-` products).
fn new_board_id() -> String {
    format!("sl-{:x}-{:04x}", now_millis(), counter())
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn counter() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}