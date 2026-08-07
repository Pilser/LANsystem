use app_contract::AppRegistry;
use topcoat::{
    Result,
    context::{Cx, app_context},
    htmx::hx_request,
    router::{Router, RouterBuilderDiscoverExt, layout, page},
    view::view,
};

#[tokio::main]
async fn main() -> Result<()> {
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

/// The document shell: styles, htmx, and the top bar. Pages render fragments
/// only; this layout wraps them. htmx partial requests skip the shell and get
/// the fragment back for in-place swapping.
const STYLESHEET: &str = r#"
:root {
  color-scheme: dark;
  --bg: #0d1117;
  --panel: #161b22;
  --panel-2: #1c2330;
  --text: #e6edf3;
  --muted: #8b949e;
  --accent: #58a6ff;
  --border: #21262d;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  font-family: 'Inter', system-ui, sans-serif;
  background: var(--bg);
  color: var(--text);
  line-height: 1.6;
}
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
.chrome { max-width: 1024px; margin: 0 auto; padding: 0 20px 60px; }
.topbar {
  display: flex; align-items: center; gap: 16px;
  padding: 14px 0; border-bottom: 1px solid var(--border); margin-bottom: 24px;
}
.topbar .brand { font-weight: 700; font-size: 18px; color: var(--text); }
.topbar .brand:hover { text-decoration: none; }
.topbar nav { display: flex; gap: 14px; font-size: 14px; }
h1 { font-size: 26px; margin: 0 0 6px; }
p { margin: 4px 0; }
.lead { color: var(--muted); }
.app-grid, .media-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(210px, 1fr));
  gap: 14px; list-style: none; padding: 0; margin: 24px 0 0;
}
.app-card, .media-card {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 16px;
  transition: border-color .15s, transform .15s;
}
.app-card:hover, .media-card:hover {
  border-color: var(--accent);
  transform: translateY(-2px);
  text-decoration: none;
}
.app-card .name { font-weight: 600; font-size: 16px; color: var(--text); }
.app-card .desc { font-size: 13px; color: var(--muted); margin-top: 4px; }
.media-title { font-size: 15px; color: var(--text); margin-bottom: 8px; }
.media-meta { font-size: 13px; color: var(--muted); }
.media-empty { color: var(--muted); font-size: 14px; padding: 32px 0; }
.media-header { display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap; }
.media-header h1 { margin: 0; }
.media-header p { color: var(--muted); }
.refresh {
  margin-left: auto;
  background: var(--panel-2); color: var(--text);
  border: 1px solid var(--border); border-radius: 6px;
  padding: 6px 14px; font-size: 13px; cursor: pointer;
}
.refresh:hover { border-color: var(--accent); }
.player-wrap { margin-top: 20px; }
.player-wrap video { width: 100%; max-width: 720px; border-radius: 10px; background: #000; display: block; }
.back { display: inline-block; margin-bottom: 12px; font-size: 14px; }
footer {
  margin-top: 48px; padding-top: 16px;
  border-top: 1px solid var(--border);
  color: var(--muted); font-size: 12px;
}
"#;

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
                <link rel="preconnect" href="https://fonts.googleapis.com">
                <link
                    rel="stylesheet"
                    href="https://fonts.googleapis.com/css2?family=Inter:wght@400;600;700&amp;display=swap"
                >
                <style>(STYLESHEET)</style>
                <script src="https://cdn.jsdelivr.net/npm/htmx.org@2.0.10/dist/htmx.min.js"></script>
            </head>
            <body hx-boost="true">
                <div class="chrome">
                    <header class="topbar">
                        <a class="brand" href="/">"LANsystem"</a>
                        <nav>
                            <a href="/">"Launcher"</a>
                            <a href="/media-library">"Media Library"</a>
                        </nav>
                    </header>
                    <main id="main-content">(slot?)</main>
                    <footer>"LANsystem · a private server on your LAN."</footer>
                </div>
            </body>
        </html>
    }
}

#[page("/")]
async fn home(cx: &Cx) -> Result {
    let registry = app_context::<AppRegistry>(cx);

    view! {
        <h1>"Welcome to your LAN"</h1>
        <p class="lead">"One place for the apps running on this server."</p>
        <ul class="app-grid">
            for app in registry.all() {
                <li>
                    <a class="app-card" href=(app.home)>
                        <div class="name">(app.name)</div>
                        <div class="desc">(app.description)</div>
                    </a>
                </li>
            }
        </ul>
    }
}