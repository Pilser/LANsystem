#![allow(clippy::result_large_err)]

pub mod client;
pub mod config;
pub mod error;
pub mod keys;

pub use client::{GetResult, PutResult, Storage};
pub use config::StorageConfig;
pub use error::{Error, Result};
