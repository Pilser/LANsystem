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