//! The centralized AI-agent protocol: JSON RPC over HTTP.
//!
//! Two endpoints, both JSON:
//!
//! - `GET /api/agent` -- the action catalog (names, parameter shapes,
//!   descriptions). Agents read this first to discover what the platform can
//!   do; it is also the live test harness for the human surface.
//! - `POST /api/agent` -- execute one action:
//!   `{"action": "...", "params": {...}}` and get `{"ok":true,"result":{...}}`
//!   back.
//!
//! Every action runs against the **same** `db` models and `platform_auth`
//! services the human routes use -- never a reimplementation -- so any bug a
//! human hits on the web surface is reachable through the protocol too, and
//! the protocol doubles as an end-to-end test harness for fixes.
//!
//! # Authentication
//!
//! - With a signed-in session cookie the action executes as that user, with
//!   exactly the role guards the human routes apply (tenant admins may only
//!   touch their own tenant, members nothing, owners everything).
//! - Headless: send `x-agent-key: <LAN_AGENT_KEY>` (env var on the app
//!   container). Key mode acts as the platform owner -- full privileges, for
//!   automation and tests.

pub mod actions;
pub mod routes;

use app_contract::BusinessApp;

/// The agent protocol as an installable "app": it only registers its routes;
/// it is never listed for tenant enablement and has no home screen.
pub struct Agent;

impl BusinessApp for Agent {
    const ID: &'static str = "agent";
    const NAME: &'static str = "Agent protocol";
    const ICON: &'static str = "lucide:bot";
    const GROUP: &'static str = "platform";
    const HOME: &'static str = "/agent";
    const DESCRIPTION: &'static str = "JSON RPC over HTTP for AI agents and test automation";
}