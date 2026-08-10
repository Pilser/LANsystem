//! Serverless boards: shared types + the [`BoardStore`] trait.
//!
//! This crate is the registry boundary (the `zw-core` lesson from the
//! `whatsapp4agents` reference): plugins depend only on this crate, never on
//! each other. The turso-backed implementation of [`BoardStore`] lives in the
//! serverless business app (`crates/business-apps/serverless`), which is the
//! only crate that may depend on the `turso` engine.

mod error;
mod store;
mod types;

pub use error::ServerlessError;
pub use store::BoardStore;
pub use types::{
    KeyRole, SrvApp, SrvApiKey, SrvHook, SrvRecord, app_meta_path, sanitize_key,
};

/// Result alias used by every [`BoardStore`] method.
pub type Result<T> = std::result::Result<T, ServerlessError>;