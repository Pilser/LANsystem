//! The staff console shipped as a business app: owners and tenant admins
//! manage users, tenants and access from `/admin`. The app is registered in
//! the launcher like any other; its routes are collected by link-time
//! discovery.

use app_contract::BusinessApp;

pub mod routes;

/// The console app.
pub struct Console;

impl BusinessApp for Console {
    const ID: &'static str = "console";
    const NAME: &'static str = "Console";
    const ICON: &'static str = "lucide:settings";
    const GROUP: &'static str = "Platform";
    const HOME: &'static str = "/admin";
    const DESCRIPTION: &'static str =
        "Users, tenants and access settings for this LAN server.";
}