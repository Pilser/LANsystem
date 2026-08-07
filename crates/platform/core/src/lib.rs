//! Topcoat glue: builds the shared `Db` + `Storage` backends and registers
//! them once on the router as app context, so business-app crates pull them out
//! of `&Cx` via [`db`], [`storage`], and [`services`].

mod bootstrap;
pub mod error;
mod services;

pub use error::{Error, Result};
pub use services::{db, services, storage, Services};