//! The authorization seam — `docs/ARCHITECTURE.md` §4, "mechanism vs policy".
//!
//! The platform *mechanism* (sessions, media routing, quotas) never contains
//! billing logic. Access decisions go through [`Authorizer`]; a business app
//! calls [`Authorizer::authorize`] before serving a resource and stops when
//! the answer is `deny`.

use serde::{Deserialize, Serialize};

/// What the platform asks the authorizer about.
#[derive(Debug, Clone, Serialize)]
pub struct AuthContext {
    pub user_id: i64,
    pub tenant_id: Option<i64>,
    pub app_id: &'static str,
    /// Domain-specific resource key (e.g. a media item id).
    pub resource: String,
}

/// The authorizer's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthDecision {
    /// Allow the access.
    Allow,
    /// Deny; callers render an "access denied" state.
    Deny,
}

/// Result of an authorizer call.
pub type AccessResult = std::result::Result<AuthDecision, std::io::Error>;

/// MikroTik-style RADIUS seam: an external AAA system answers allow/deny.
/// `Send + Sync` so implementations live in the app context and are called
/// from any request.
pub trait Authorizer: Send + Sync {
    /// Decide whether `ctx` may proceed.
    fn authorize<'a>(
        &'a self,
        ctx: &'a AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AccessResult> + Send + 'a>>;
}

/// Default: an effective user within an active tenant and enabled app is
/// allowed. Billing layers in by per-tenant [`WebhookAuthorizer`] swaps.
pub struct AllowAuthorizer;

impl Authorizer for AllowAuthorizer {
    fn authorize<'a>(
        &'a self,
        _ctx: &'a AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AccessResult> + Send + 'a>> {
        Box::pin(async move { Ok(AuthDecision::Allow) })
    }
}

/// Webhook-backed authorizer — the RADIUS equivalent. POSTs the access context
/// to the tenant's billing endpoint and trusts the JSON response:
///
/// ```json
/// {"allowed": true}
/// ```
///
/// Unreachable endpoint or bad body = fail *closed* (deny): it is better to
/// stop playing than to open the tenant's content after a billing outage.
pub struct WebhookAuthorizer {
    url: String,
}

impl WebhookAuthorizer {
    /// Build for a tenant-configured webhook URL.
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

/// JSON body sent to the webhook.
#[derive(Debug, Serialize)]
pub struct WebhookRequest<'a> {
    pub user_id: i64,
    pub tenant_id: Option<i64>,
    pub app_id: &'static str,
    pub resource: &'a str,
}

/// Minimal expected webhook response.
#[derive(Debug, Deserialize)]
pub struct WebhookResponse {
    #[serde(default)]
    pub allowed: bool,
}

impl Authorizer for WebhookAuthorizer {
    fn authorize<'a>(
        &'a self,
        ctx: &'a AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AccessResult> + Send + 'a>> {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let body = WebhookRequest {
                user_id: ctx.user_id,
                tenant_id: ctx.tenant_id,
                app_id: ctx.app_id,
                resource: &ctx.resource,
            };
            let response = client
                .post(&self.url)
                .json(&body)
                .send()
                .await
                .map_err(|error| std::io::Error::other(format!("webhook authorizer: {error}")))?;
            let parsed: WebhookResponse = response.json().await.map_err(|error| {
                std::io::Error::other(format!("webhook authorizer body: {error}"))
            })?;
            Ok(if parsed.allowed {
                AuthDecision::Allow
            } else {
                AuthDecision::Deny
            })
        })
    }
}