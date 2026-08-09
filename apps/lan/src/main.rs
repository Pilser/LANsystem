use std::time::Duration;

use app_contract::AppRegistry;
use platform_auth::Role;
use topcoat::{
    Result,
    context::Cx,
    cookie::RouterBuilderCookieExt,
    htmx::hx_request,
    router::{BodyLimit, Router, RouterBuilderDiscoverExt, error::redirect, layout, page, request::uri},
    session::RouterBuilderSessionExt,
    view::view,
};

/// Tailwind CSS compiled at build time by `build.rs` (standalone CLI, no
/// Node). Inlined so the runtime needs no asset bundler.
const TAILWIND_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/tailwind.css"));

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lan=info,platform_core=info,platform_auth=info,db=info,storage=info,tower_http=info".into()),
        )
        .init();

    let services = match platform_core::Services::connect().await {
        Ok(services) => services,
        Err(error) => {
            eprintln!(
                "lan: failed to connect to backend services: {error}\n\
                 hint: point HELIX_* at a running helix db and RUSTFS_* at a \
                 running rustfs gateway, then re-run"
            );
            return Err(error.into());
        }
    };
    tracing::info!("connected: db + storage + auth ready");

    let auth = platform_core::Auth::new(services.db.clone());

    let mut registry = AppRegistry::new();
    registry.push(media_app::Media);
    registry.push(marketplace_app::Marketplace);
    registry.push(onboarding_app::Onboarding);
    registry.push(admin_app::Console);
    registry.push(agent_app::Agent);

    topcoat::start(
        Router::builder()
            .discover()
            .app_context(services)
            .app_context(auth)
            .app_context(registry)
            .cookies()
            .sessions(
                topcoat::session::SessionConfig::builder()
                    .lifetime(Duration::from_secs(7 * 24 * 60 * 60))
                    .build(),
            )
            .layer(BodyLimit::max(8 * 1024 * 1024 * 1024).at("/media-library/upload"))
            .layer(BodyLimit::max(64 * 1024 * 1024).at("/shop-manage"))
            .build(),
    )
    .await?;

    Ok(())
}

/// The document shell: Tailwind styles, htmx, and the top bar. Pages render
/// fragments only; this layout wraps them. htmx partial requests skip the
/// shell and get the fragment back for in-place swapping.
#[layout("/")]
async fn root(cx: &Cx, slot: Result) -> Result {
    if hx_request(cx) {
        return slot;
    }

    let current = platform_core::auth(cx).current_user(cx).await?;
    let on_console = uri(cx).path().starts_with("/admin");

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>"LANsystem"</title>
                <style>(TAILWIND_CSS)</style>
                <script src="https://cdn.jsdelivr.net/npm/htmx.org@2.0.10/dist/htmx.min.js"></script>
            </head>
            <body
                class="min-h-screen bg-zinc-950 text-zinc-100 antialiased"
                hx-boost="true"
            >
                <div class="mx-auto max-w-5xl px-5 pb-16">
                    <header
                        class="mb-8 flex items-center gap-4 border-b border-zinc-800 py-4"
                    >
                        match &current {
                            None => {
                                <div class="ml-auto flex items-center gap-3 text-sm">
                                    <a
                                        class="rounded-md border border-sky-700 bg-sky-900/40 px-3 py-1 text-sm text-sky-200 transition hover:bg-sky-800/40"
                                        href="/admin"
                                    >
                                        "Console"
                                    </a>
                                </div>
                            },
Some(_) if on_console => {
                                <div class="ml-auto flex items-center gap-4 text-sm">
                                    <a
                                        class="text-sky-400 hover:text-sky-300"
                                        href="/public"
                                    >
                                        "← Back to the directory"
                                    </a>
                                    <form method="post" action="/logout">
                                        <button
                                            class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-sm text-zinc-300 transition hover:border-red-400 hover:text-red-300"
                                        >
                                            "Sign out"
                                        </button>
                                    </form>
                                </div>
                            },
                            Some(_) => {
                                <a class="text-lg font-bold text-zinc-100" href="/">
                                    "LANsystem"
                                </a>
                                <nav class="flex gap-4 text-sm text-zinc-400">
                                    <a class="hover:text-zinc-100" href="/public">"Directory"</a>
                                    <a class="hover:text-zinc-100" href="/media-library">"Media Library"</a>
                                    match current.as_ref().map(|c| c.user.role()) {
                                        Some(Role::Owner) => {
                                            <a class="hover:text-zinc-100" href="/admin">"Console"</a>
                                        },
                                        Some(Role::TenantAdmin) => {
                                            <a class="hover:text-zinc-100" href="/portal">"Portal"</a>
                                        },
                                        _ => {},
                                    }
                                </nav>
                                <div class="ml-auto flex items-center gap-3 text-sm">
                                    <span class="hidden text-zinc-400 sm:inline">
                                        (current.as_ref().and_then(|u| u.user.display_name.as_deref()).unwrap_or("user"))
                                    </span>
                                    <form method="post" action="/logout">
                                        <button
                                            class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-sm text-zinc-300 transition hover:border-red-400 hover:text-red-300"
                                        >
                                            "Sign out"
                                        </button>
                                    </form>
                                </div>
                            },
                        }
                    </header>
                    <main id="main-content">(slot?)</main>
                    match &current {
                        Some(_) => {
                            <footer
                                class="mt-14 border-t border-zinc-800 pt-4 text-xs text-zinc-500"
                            >
                                "LANsystem · a private server on your LAN."
                            </footer>
                        },
                        None => {},
                    }
                </div>
            </body>
        </html>
    }
}

/// `GET /`: no longer a launcher page — signed-out visitors see the public
/// directory, owners land in the console, tenant admins land in their portal,
/// members land in their app (or the application status page).
#[page("/")]
async fn home(cx: &Cx) -> Result {
    let Some(current) = platform_core::auth(cx).current_user(cx).await? else {
        return Err(redirect("/public").into());
    };
    match current.user.role() {
        Role::Owner => Err(redirect("/admin").into()),
        Role::TenantAdmin => Err(redirect("/portal").into()),
        Role::Member => {
            if current.user.tenant_id.is_some_and(|id| id > 0) {
                Err(redirect("/media-library").into())
            } else {
                Err(redirect("/apply/status").into())
            }
        }
    }
}