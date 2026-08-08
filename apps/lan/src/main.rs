use std::time::Duration;

use app_contract::AppRegistry;
use platform_auth::Role;
use topcoat::{
    Result,
    context::{app_context, Cx},
    cookie::RouterBuilderCookieExt,
    htmx::hx_request,
    router::{Router, RouterBuilderDiscoverExt, layout, page},
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
    registry.push(admin_app::Console);

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
                        <a class="text-lg font-bold text-zinc-100" href="/">
                            "LANsystem"
                        </a>
                        <nav class="flex gap-4 text-sm text-zinc-400">
                            <a class="hover:text-zinc-100" href="/">"Launcher"</a>
                            <a class="hover:text-zinc-100" href="/media-library">"Media Library"</a>
                            match &current {
                                Some(user) if user.user.role().is_at_least(Role::TenantAdmin) => {
                                    <a class="hover:text-zinc-100" href="/admin">"Console"</a>
                                },
                                _ => {},
                            }
                        </nav>
                        <div class="ml-auto flex items-center gap-3 text-sm">
                            match &current {
                                Some(user) => {
                                    <span class="hidden text-zinc-400 sm:inline">
                                        (user.user.display_name.as_deref().unwrap_or("user"))
                                    </span>
                                    <form method="post" action="/logout">
                                        <button
                                            class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-sm text-zinc-300 transition hover:border-red-400 hover:text-red-300"
                                        >
                                            "Sign out"
                                        </button>
                                    </form>
                                },
                                None => {
                                    <a
                                        class="rounded-md border border-sky-700 bg-sky-900/40 px-3 py-1 text-sky-200 transition hover:bg-sky-800/40"
                                        href="/login"
                                    >
                                        "Sign in"
                                    </a>
                                },
                            }
                        </div>
                    </header>
                    <main id="main-content">(slot?)</main>
                    <footer
                        class="mt-14 border-t border-zinc-800 pt-4 text-xs text-zinc-500"
                    >
                        "LANsystem · a private server on your LAN."
                    </footer>
                </div>
            </body>
        </html>
    }
}

#[page("/")]
async fn home(cx: &Cx) -> Result {
    let registry = app_context::<AppRegistry>(cx);

    view! {
        <h1 class="mb-2 text-3xl font-bold">"Welcome to your LAN"</h1>
        <p class="mb-6 text-zinc-400">
            "One place for the apps running on this server."
        </p>
        <ul class="grid list-none grid-cols-1 gap-4 p-0 sm:grid-cols-2 lg:grid-cols-3">
            for app in registry.all() {
                <li>
                    <a
                        class="block rounded-xl border border-zinc-800 bg-zinc-900 p-5 transition hover:-translate-y-0.5 hover:border-sky-400"
                        href=(app.home)
                    >
                        <div class="text-base font-semibold text-zinc-100">
                            (app.name)
                        </div>
                        <div class="mt-1 text-sm text-zinc-400">
                            (app.description)
                        </div>
                    </a>
                </li>
            }
        </ul>
    }
}