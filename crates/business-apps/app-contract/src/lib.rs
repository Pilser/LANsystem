//! The `BusinessApp` plugin contract and the installed-app registry.
//!
//! Every user-facing app (media, marketplace, news, ...) implements
//! [`BusinessApp`] for an empty marker type and is pushed into an
//! [`AppRegistry`] at link time by the `apps/lan` binary. The home-screen
//! launcher reads the [`AppMeta`] snapshots to build the app grid and nav.

pub mod app;

pub use app::{AppMeta, AppRegistry, BusinessApp};