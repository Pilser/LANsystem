//! Application-level identity service: password hashing, session lifecycle,
//! current-user resolution, and the policy checks business apps call.
//!
//! Sessions are persisted as `Session` nodes in HelixDB (token hash + expiry),
//! so a restart does not log anyone out, and the force-drop control is a
//! single batch delete.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use db::{DateTime, Db};
use topcoat::{
    Result,
    context::Cx,
    router::error::{RouterErrorExt, redirect, see_other},
    session::{self, TokenHash},
};

use crate::authorizer::{
    AccessResult, AuthContext, AuthDecision, Authorizer, WebhookAuthorizer,
};
use crate::models::{
    Role, SessionRecord, SessionRows, Tenant, TenantRows, TenantsRows, User, UserRows, UsersRows,
    USER_STATUS_ACTIVE,
};
use crate::queries;

const DEFAULT_OWNER_USERNAME: &str = "owner";
const DEFAULT_OWNER_PASSWORD: &str = "owner";
const NO_TENANT: i64 = 0;

/// The per-request view of the signed-in principal.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub user: User,
    pub tenant: Option<Tenant>,
}

/// A user row plus its role, for console listings.
#[derive(Debug, Clone)]
pub struct UserView {
    pub user: User,
    pub role: Role,
    pub is_active: bool,
    pub tenant_name: Option<String>,
}

/// A tenant row for the console overview.
pub type TenantView = Tenant;

/// The identity service. Registered once on the router as Topcoat app context.
#[derive(Clone)]
pub struct Auth {
    db: Db,
}

impl Auth {
    /// Build the service from a connected database client.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Create the seed `owner` account on first boot (idempotent). Credentials
    /// come from `LAN_OWNER_USERNAME` / `LAN_OWNER_PASSWORD` (safe defaults
    /// for a private LAN when unset).
    pub async fn seed_owner(&self) -> Result<()> {
        let username = std::env::var("LAN_OWNER_USERNAME")
            .unwrap_or_else(|_| DEFAULT_OWNER_USERNAME.to_string());
        let password = std::env::var("LAN_OWNER_PASSWORD")
            .unwrap_or_else(|_| DEFAULT_OWNER_PASSWORD.to_string());

        let seen: UserRows = self
            .db
            .query(queries::user_by_username(username.clone())?)
            .await?;
        if seen.user.iter().any(|u| u.username.as_deref() == Some(username.as_str())) {
            return Ok(());
        }

        let user_id = crate::ids::next();
        let hash = self.hash_password(&password)?;
        let _: serde_json::Value = self
            .db
            .query(queries::upsert_user(
                user_id,
                username,
                "Platform owner".to_string(),
                hash,
                Role::Owner.as_str().to_string(),
                USER_STATUS_ACTIVE.to_string(),
                NO_TENANT,
            )?)
            .await?;
        tracing::info!(user_id, "auth: seeded platform owner");
        Ok(())
    }

    /// Create or update a user from the console. `tenant_id = 0` means "no
    /// tenant" (platform-level account).
    pub async fn save_user(
        &self,
        user_id: Option<i64>,
        username: String,
        display_name: String,
        role: Role,
        tenant_id: Option<i64>,
        password: Option<String>,
    ) -> Result<()> {
        let tenant: i64 = tenant_id.unwrap_or(NO_TENANT);
        match user_id {
            Some(id) => {
                if let Some(password) = password {
                    let hash = self.hash_password(&password)?;
                    let _: serde_json::Value = self
                        .db
                        .query(queries::set_user_password(id, hash)?)
                        .await?;
                }
                let _: serde_json::Value = self
                    .db
                    .query(queries::set_user_profile(id, display_name, role.as_str().to_string(), USER_STATUS_ACTIVE.to_string(), tenant)?)
                    .await?;
            }
            None => {
                let hash = self.hash_password(password.as_deref().unwrap_or(""))?;
                let _: serde_json::Value = self
                    .db
                    .query(queries::upsert_user(
                        crate::ids::next(),
                        username,
                        display_name,
                        hash,
                        role.as_str().to_string(),
                        USER_STATUS_ACTIVE.to_string(),
                        tenant,
                    )?)
                    .await?;
            }
        }
        Ok(())
    }

    /// Verify a username/password pair. Returns the user on success.
    pub async fn authenticate(&self, username: &str, password: &str) -> Result<Option<User>> {
        let rows: UserRows = self
            .db
            .query(queries::user_by_username(username.to_string())?)
            .await?;
        let Some(user) = rows.user.into_iter().next() else {
            return Ok(None);
        };
        let Some(hash) = &user.password_hash else {
            return Ok(None);
        };
        Ok(self.verify_password(password, hash).then_some(user))
    }

    /// Log in: verify credentials, mint a Topcoat session, persist the record.
    pub async fn login(&self, cx: &Cx, username: &str, password: &str) -> Result<bool> {
        let Some(user) = self.authenticate(username, password).await? else {
            return Ok(false);
        };
        if !user.is_active() {
            return Ok(false);
        }
        let Some(user_id) = user.user_id else {
            return Ok(false);
        };

        let session = session::start(cx).await?;
        let expires_at = to_db_datetime(session.expires_at);
        let _: serde_json::Value = self
            .db
            .query(queries::upsert_session(
                token_hash_to_string(&session.token_hash),
                user_id,
                expires_at,
            )?)
            .await?;
        Ok(true)
    }

    /// Log out the current request's session.
    pub async fn logout(&self, cx: &Cx) -> Result<()> {
        if let Some(hash) = session::stop(cx).await? {
            let _: serde_json::Value = self
                .db
                .query(queries::drop_session_token(token_hash_to_string(&hash))?)
                .await?;
        }
        Ok(())
    }

    /// Resolve the current request's user, if any. Expired session records are
    /// dropped lazily.
    pub async fn current_user(&self, cx: &Cx) -> Result<Option<CurrentUser>> {
        let Some(hash) = session::token_hash(cx).await? else {
            return Ok(None);
        };
        let session_id = token_hash_to_string(&hash);
        let rows: SessionRows = self
            .db
            .query(queries::session_by_id(session_id.clone())?)
            .await?;
        let Some(record) = rows.session.into_iter().next() else {
            return Ok(None);
        };
        let Some(expires_at) = record.expires_at else {
            return Ok(None);
        };
        if expires_at < now_millis() {
            let _: serde_json::Value = self
                .db
                .query(queries::drop_session_token(session_id)?)
                .await?;
            return Ok(None);
        }
        let Some(user_id) = record.user_id else {
            return Ok(None);
        };
        let Some(user) = self.user_by_id(user_id).await? else {
            return Ok(None);
        };
        let tenant = match user.tenant_id {
            Some(id) if id != NO_TENANT => self.tenant_by_id(id).await.ok().flatten(),
            _ => None,
        };
        Ok(Some(CurrentUser { user, tenant }))
    }

    pub async fn user_by_id(&self, user_id: i64) -> Result<Option<User>> {
        let rows: UserRows = self
            .db
            .query(queries::user_by_id(user_id)?)
            .await?;
        Ok(rows.user.into_iter().next())
    }

    pub async fn tenant_by_id(&self, tenant_id: i64) -> Result<Option<Tenant>> {
        let rows: TenantRows = self
            .db
            .query(queries::tenant_by_id(tenant_id)?)
            .await?;
        Ok(rows.tenant.into_iter().next())
    }

    pub async fn tenants(&self) -> Result<Vec<Tenant>> {
        let rows: TenantsRows = self.db.query(queries::all_tenants()?).await?;
        Ok(rows.tenants)
    }

    /// All users, with role/tenant info, for the owner console.
    pub async fn users(&self) -> Result<Vec<UserView>> {
        let rows: UsersRows = self.db.query(queries::list_users()?).await?;
        let mut views = Vec::with_capacity(rows.users.len());
        for user in rows.users {
            let tenant_name = match user.tenant_id {
                Some(id) if id != NO_TENANT => self
                    .tenant_by_id(id)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|t| t.tenant_name),
                _ => None,
            };
            let role = user.role();
            let is_active = user.is_active();
            views.push(UserView {
                user,
                role,
                is_active,
                tenant_name,
            });
        }
        Ok(views)
    }

    /// Users of one tenant, for the tenant-admin console.
    pub async fn users_by_tenant(&self, tenant_id: i64) -> Result<Vec<UserView>> {
        let rows: UsersRows = self
            .db
            .query(queries::users_by_tenant(tenant_id)?)
            .await?;
        let views = rows.users.into_iter().map(|user| {
            let role = user.role();
            let is_active = user.is_active();
            UserView {
                user,
                role,
                is_active,
                tenant_name: None,
            }
        });
        Ok(views.collect())
    }

    /// Upsert a tenant from the console (create or update settings).
    pub async fn save_tenant(
        &self,
        tenant_id: Option<i64>,
        tenant_name: String,
        quota_bytes: i64,
        apps_enabled: Vec<String>,
        billing_enabled: bool,
        webhook_url: String,
    ) -> Result<()> {
        let tenant_id = match tenant_id {
            Some(id) => id,
            None => crate::ids::next(),
        };
        let _: serde_json::Value = self
            .db
            .query(queries::upsert_tenant_by_name(
                tenant_name,
                tenant_id,
                quota_bytes,
                apps_enabled,
            )?)
            .await?;
        let _: serde_json::Value = self
            .db
            .query(queries::set_tenant_billing(tenant_id, billing_enabled, webhook_url)?)
            .await?;
        Ok(())
    }

    pub async fn set_tenant_active(&self, tenant_id: i64, active: bool) -> Result<()> {
        let status = if active { "active" } else { "suspended" };
        let _: serde_json::Value = self
            .db
            .query(queries::set_tenant_status(tenant_id, status.to_string())?)
            .await?;
        Ok(())
    }

    /// Show/hide a tenant in the public service directory (with tagline).
    pub async fn set_tenant_visibility(
        &self,
        tenant_id: i64,
        is_public: bool,
        tagline: String,
    ) -> Result<()> {
        let _: serde_json::Value = self
            .db
            .query(queries::set_tenant_visibility(tenant_id, is_public, tagline)?)
            .await?;
        Ok(())
    }

    /// Tenants visible on the public directory page (active + `isPublic`).
    pub async fn public_tenants(&self) -> Result<Vec<Tenant>> {
        let rows: TenantsRows = self.db.query(queries::public_tenants()?).await?;
        Ok(rows.tenants)
    }

    pub async fn set_user_active(&self, user_id: i64, active: bool) -> Result<()> {
        let status = if active { "active" } else { "suspended" };
        let _: serde_json::Value = self
            .db
            .query(queries::set_user_status(user_id, status.to_string())?)
            .await?;
        Ok(())
    }

    pub async fn set_user_role(&self, user_id: i64, role: Role) -> Result<()> {
        let _: serde_json::Value = self
            .db
            .query(queries::set_user_role(user_id, role.as_str().to_string())?)
            .await?;
        Ok(())
    }

    /// MikroTik-style kill switch: drop every session of a user (and with
    /// them, short-lived presigned URLs stop resolving).
    pub async fn force_drop_user(&self, user_id: i64) -> Result<()> {
        let _: serde_json::Value = self.db
            .query(queries::drop_user_sessions(user_id)?)
            .await?;
        tracing::info!(user_id, "auth: forced drop of user sessions");
        Ok(())
    }

    /// The authorizer for a tenant: webhook when billing is on with a URL,
    /// else the local allow.
    pub fn authorizer_for(tenant: Option<&Tenant>) -> Box<dyn Authorizer> {
        if let Some(tenant) = tenant {
            if tenant.billing_enabled == Some(true) {
                if let Some(url) = tenant.auth_webhook_url.as_deref() {
                    if !url.is_empty() {
                        return Box::new(WebhookAuthorizer::new(url.to_string()));
                    }
                }
            }
        }
        Box::new(crate::authorizer::AllowAuthorizer)
    }

    /// The enforcement chain: local policy (user active, tenant active, app
    /// enabled) then the tenant's pluggable authorizer. Business apps call
    /// this before serving a resource.
    pub async fn authorize_access(
        &self,
        user: &User,
        tenant: Option<&Tenant>,
        app_id: &'static str,
        resource: String,
    ) -> AccessResult {
        if !user.is_active() {
            return Ok(AuthDecision::Deny);
        }
        if let Some(tenant) = tenant {
            if !tenant.is_active() {
                return Ok(AuthDecision::Deny);
            }
            let enabled = tenant.apps_enabled.as_deref().is_some_and(|apps| {
                apps.iter().any(|enabled| enabled == app_id)
            });
            if !enabled {
                return Ok(AuthDecision::Deny);
            }
        }
        let authorizer = Self::authorizer_for(tenant);
        authorizer
            .authorize(&AuthContext {
                user_id: user.user_id.unwrap_or(0),
                tenant_id: user.tenant_id,
                app_id,
                resource,
            })
            .await
    }

    pub fn hash_password(&self, password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()))?;
        Ok(hash.to_string())
    }

    pub fn verify_password(&self, password: &str, hash: &str) -> bool {
        PasswordHash::new(hash)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false)
    }
}

// --- helpers --------------------------------------------------------------

/// Guard: current user or redirect to `/login`.
pub async fn require_user(cx: &Cx, auth: &Auth) -> Result<CurrentUser> {
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    Ok(current)
}

/// Guard: enforce the minimum role.
pub fn require_role(current: &CurrentUser, role: crate::models::Role) -> Result<()> {
    if current.user.role().is_at_least(role) {
        Ok(())
    } else {
        Err(redirect("/").into())
    }
}

pub fn token_hash_to_string(hash: &TokenHash) -> String {
    hex_encode(&hash[..])
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn to_db_datetime(system_time: std::time::SystemTime) -> DateTime {
    let millis = system_time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        });
    DateTime::from_millis(millis)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0))
}