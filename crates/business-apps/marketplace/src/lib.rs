//! Marketplace business app: shops per tenant.
//!
//! Each tenant runs its own catalog (`Product` nodes) and receives order
//! tickets (`MarketplaceOrder` nodes) from signed-in buyers. Guests browse a
//! shop through the tenant's space page; tenant admins manage products and
//! orders from the shop management page.

pub mod models;
pub mod routes;

use app_contract::BusinessApp;

/// Marker type for the installed marketplace app plugin.
pub struct Marketplace;

impl BusinessApp for Marketplace {
    const ID: &'static str = "marketplace";
    const NAME: &'static str = "Marketplace";
    const ICON: &'static str = "lucide:store";
    const GROUP: &'static str = "Commerce";
    const HOME: &'static str = "/shop";
    const DESCRIPTION: &'static str = "Shops run by tenants on your LAN.";
}
