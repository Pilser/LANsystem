use app_contract::AppRegistry;
use topcoat::{
    Result,
    context::{Cx, app_context},
    htmx::hx_request,
    router::{Router, RouterBuilderDiscoverExt, layout, page},
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
                .unwrap_or_else(|_| "lan=info,platform_core=info,db=info,storage=info,tower_http=info".into()),
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
    tracing::info!("connected: db + storage ready");

    let mut registry = AppRegistry::new();
    registry.push(media_app::Media);

    topcoat::start(
        Router::builder()
            .discover()
            .app_context(services)
            .app_context(registry)
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
                        </nav>
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