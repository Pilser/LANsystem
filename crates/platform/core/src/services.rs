use db::Db;
use platform_auth::Auth;
use storage::Storage;
use topcoat::context::{app_context, Cx};

/// The bundled, app-wide services, registered once on the router as Topcoat
/// app context.
#[derive(Clone)]
pub struct Services {
    pub db: Db,
    pub storage: Storage,
    pub auth: Auth,
}

impl Services {
    /// Reads configs from the environment and establishes both clients.
    pub async fn connect() -> crate::Result<Self> {
        crate::bootstrap::connect().await
    }
}

/// Access the services bundle from within a page/component/shard handler.
pub fn services(cx: &Cx) -> &Services {
    app_context::<Services>(cx)
}

/// Convenience accessor for the shared database client.
pub fn db(cx: &Cx) -> &Db {
    &services(cx).db
}

/// Convenience accessor for the shared storage client.
pub fn storage(cx: &Cx) -> &Storage {
    &services(cx).storage
}

/// Convenience accessor for the identity service.
pub fn auth(cx: &Cx) -> &Auth {
    &services(cx).auth
}