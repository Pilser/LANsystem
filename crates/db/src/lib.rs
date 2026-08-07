//! HelixDB client wrapper + shared schema conventions.
//!
//! This is the only crate in the workspace allowed to depend on `helix-db`.
//! Other crates reach Helix through here: connect a [`Db`], then build
//! [`QueryRequest`]s with the re-exported DSL prelude and send them via
//! [`Db::query`].

pub mod client;
pub mod error;
pub mod helpers;
pub mod schema;

pub use client::{Db, DbConfig};
pub use error::{Error, Result};
pub use helix_db::dsl::prelude::*;
pub use helix_db::Client;