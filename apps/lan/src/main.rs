use app_contract::AppRegistry;
use topcoat::{
    Result,
    context::{Cx, app_context},
    router::{Router, RouterBuilderDiscoverExt, page},
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

#[page("/")]
async fn home(cx: &Cx) -> Result {
    let registry = app_context::<AppRegistry>(cx);
    view! {
        <!DOCTYPE html>
        <html>
            <head>
                <title>"LANsystem"</title>
            </head>
            <body>
                <h1>"LANsystem"</h1>
                <ul>
                    for app in registry.all() {
                        <li><a href=(app.home)>(app.name)</a></li>
                    }
                </ul>
            </body>
        </html>
    }
}