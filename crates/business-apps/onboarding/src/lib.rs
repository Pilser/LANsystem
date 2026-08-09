//! The public onboarding app: anyone on the LAN can apply for a business
//! tenant ("Create a business"), optionally pay through the domain's policy
//! webhook, and follow the application until the owner approves it.

use app_contract::BusinessApp;

pub mod routes;

/// The onboarding app.
pub struct Onboarding;

impl BusinessApp for Onboarding {
    const ID: &'static str = "onboarding";
    const NAME: &'static str = "Apply for a business";
    const ICON: &'static str = "lucide:store-plus";
    const GROUP: &'static str = "Platform";
    const HOME: &'static str = "/apply";
    const DESCRIPTION: &'static str = "Sign your business up for a domain on this LAN.";
}
