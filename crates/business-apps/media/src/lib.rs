//! Media Library business app: films, songs, downloads.
//!
//! Implements the [`app_contract::BusinessApp`] plugin contract and registers
//! its pages with topcoat's link-time route discovery. Queries and row types
//! live in [`models`]; the `#[page]` handlers live in [`routes`]; reusable
//! `view!` components live in [`components`].

pub mod components;
pub mod models;
pub mod routes;

use app_contract::BusinessApp;

/// Marker type for the installed media app plugin.
pub struct Media;

impl BusinessApp for Media {
    const ID: &'static str = "media-library";
    const NAME: &'static str = "Media Library";
    const ICON: &'static str = "lucide:film";
    const GROUP: &'static str = "Entertainment";
    const HOME: &'static str = "/media-library";
    const DESCRIPTION: &'static str = "Films, songs and downloads on your LAN.";
}
