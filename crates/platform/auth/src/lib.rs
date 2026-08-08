//! Platform identity + access control: users, roles, tenants, sessions, and
//! the pluggable `Authorizer` seam.
//!
//! Per `docs/ARCHITECTURE.md` §4 the platform separates *mechanism* from
//! *policy*: this crate owns accounts, sessions, roles and the access checks
//! around them. Billing/payment logic is never compiled in — access decisions
//! go through [`Authorizer`], and the built-in [`WebhookAuthorizer`] leaves any
//! per-tenant AAA endpoint to answer.

mod ids;
pub mod authorizer;
pub mod models;
pub mod queries;
pub mod routes;
pub mod service;

pub use authorizer::{Authorizer, WebhookAuthorizer};
pub use models::{
    Role, SessionRecord, Tenant, User, USER_STATUS_ACTIVE, USER_STATUS_SUSPENDED,
};
pub use service::{Auth, CurrentUser, TenantView, UserView};