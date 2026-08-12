//! Serverless business app: per-tenant record boards backed by a local turso
//! SQL engine.
//!
//! Each board has a `ServerlessApp` catalog row in HelixDB (id/title/schema/
//! owner-key-hash) and a turso file on the container volume holding its
//! records, keys and webhooks. This crate is the only one allowed to depend
//! on the `turso` engine — plugins talk to boards through `serverless-core`'s
//! [`BoardStore`] trait.

pub mod cli;
pub mod components;
pub mod cron;
pub mod dsl;
pub mod files;
pub mod models;
pub mod netguard;
pub mod routes;
pub mod store;
pub mod workers;

use app_contract::BusinessApp;

/// Marker type for the installed serverless app plugin.
pub struct Serverless;

impl BusinessApp for Serverless {
    const ID: &'static str = "serverless";
    const NAME: &'static str = "Serverless";
    const ICON: &'static str = "lucide:database-zap";
    const GROUP: &'static str = "Developer";
    const HOME: &'static str = "/sl";
    const DESCRIPTION: &'static str = "Per-tenant record boards backed by a local SQL engine.";
}