//! Topcoat glue: builds the shared `Db` + `Storage` backends and the
//! identity service (`Auth`), registering them once on the router as app
//! context, so business-app crates pull them out of `&Cx` via [`db`],
//! [`storage`], [`auth`], and [`services`].

mod bootstrap;
pub mod error;
mod services;

pub use error::{Error, Result};
pub use platform_auth::authorizer::AuthDecision;
pub use platform_auth::{Auth, Role};
pub use services::{auth, db, services, storage, Services};

/// Object keys surfacing in browser URLs are transported base64url (the keys
/// contain `/`, which route params cannot carry). `/blob/<encoded>` proxy
/// routes in the apps decode with this.
pub fn encode_key(key: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
}

/// Decode a key previously encoded with [`encode_key`].
pub fn decode_key(encoded: &str) -> Option<String> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()
        .map(|bytes| String::from_utf8(bytes).ok())
        .flatten()
}