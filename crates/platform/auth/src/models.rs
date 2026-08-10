//! Row types and enums persisted in the shared graph (`db::schema`).



pub const USER_STATUS_ACTIVE: &str = "active";
pub const USER_STATUS_SUSPENDED: &str = "suspended";
pub const TENANT_STATUS_ACTIVE: &str = "active";
pub const TENANT_STATUS_SUSPENDED: &str = "suspended";
pub const TENANT_STATUS_PENDING: &str = "pending";
pub const TENANT_STATUS_REJECTED: &str = "rejected";
/// Onboarding payment gate outcomes.
pub const PAYMENT_STATUS_NONE: &str = "none";
pub const PAYMENT_STATUS_PAID: &str = "paid";
pub const PAYMENT_STATUS_DECLINED: &str = "declined";
/// The domains a business can apply for (mirrors the app registry in `apps/lan`).
pub const DOMAINS: &[(&str, &str)] = &[
    ("media", "Media Library"),
    ("marketplace", "Marketplace"),
];

pub const ROLE_OWNER: &str = "owner";
pub const ROLE_TENANT_ADMIN: &str = "tenant_admin";
pub const ROLE_MEMBER: &str = "member";

/// Platform roles, in ascending privilege order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Member,
    TenantAdmin,
    Owner,
}

impl Role {
    /// The on-wire value stored on the `User.role` property.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => ROLE_MEMBER,
            Self::TenantAdmin => ROLE_TENANT_ADMIN,
            Self::Owner => ROLE_OWNER,
        }
    }

    /// Parse a role from the graph's stored string. Unknown values map to
    /// `Member` (least privilege is the safe default).
    pub fn parse(value: &str) -> Role {
        match value {
            ROLE_OWNER => Self::Owner,
            ROLE_TENANT_ADMIN => Self::TenantAdmin,
            _ => Self::Member,
        }
    }

    /// Whether `self` holds at least `required` privilege.
    pub fn is_at_least(self, required: Role) -> bool {
        self >= required
    }
}

/// A `User` node row as projected by `queries::*`. All properties optional:
/// Helix projection returns the fields that exist.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct User {
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub password_hash: Option<String>,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default = "default_status")]
    pub status: String,
    pub tenant_id: Option<i64>,
    #[serde(deserialize_with = "datetime::optional")]
    pub created_at: Option<i64>,
    #[serde(deserialize_with = "datetime::optional")]
    pub updated_at: Option<i64>,
}

fn default_role() -> String {
    ROLE_MEMBER.to_string()
}

fn default_status() -> String {
    USER_STATUS_ACTIVE.to_string()
}

/// Helix returns `DateTime`-typed properties as RFC3339 strings but may also
/// return raw epoch millis; accept either and expose epoch millis i64.
pub mod datetime {
    use serde::Deserializer;

    pub fn optional<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        use serde::Deserialize;

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum EpochOrIso {
            Millis(i64),
            Iso(String),
        }

        Ok(match Option::<EpochOrIso>::deserialize(deserializer)? {
            None => None,
            Some(EpochOrIso::Millis(millis)) => Some(millis),
            Some(EpochOrIso::Iso(iso)) => Some(
                db::DateTime::parse_rfc3339(&iso)
                    .map(|date| date.millis())
                    .map_err(|_| D::Error::custom(format!("invalid datetime: {iso:?}")))?,
            ),
        })
    }
}

impl User {
    pub fn role(&self) -> Role {
        Role::parse(&self.role)
    }

    pub fn is_active(&self) -> bool {
        self.status == USER_STATUS_ACTIVE
    }
}

/// A `Tenant` node as projected by `#[query]`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Tenant {
    pub tenant_id: Option<i64>,
    pub tenant_name: Option<String>,
    pub status: String,
    pub apps_enabled: Option<Vec<String>>,
    pub storage_quota_bytes: Option<i64>,
    pub storage_used_bytes: Option<i64>,
    pub billing_enabled: Option<bool>,
    pub auth_webhook_url: Option<String>,
    /// Shows in the public service directory (tenant-admin controlled).
    #[serde(default)]
    pub is_public: Option<bool>,
    /// Tenant tagline shown on directory cards.
    #[serde(default)]
    pub description: Option<String>,
    /// Tenant profile: the wide banner shown on the directory card.
    #[serde(default, rename = "tenantBannerKey")]
    pub banner_key: Option<String>,
    /// Tenant profile: the circular profile icon on the directory card.
    #[serde(default, rename = "tenantAvatarKey")]
    pub avatar_key: Option<String>,
    /// Tenant profile: JSON array of gallery image keys for the directory
    /// card slideshow.
    #[serde(default, rename = "tenantGalleryKeys")]
    pub gallery_keys: Option<String>,
    /// The applicant user who signed this business up (`pending` tenants only).
    #[serde(default)]
    pub applicant_id: Option<i64>,
    /// Onboarding payment gate: `none`/`paid`/`declined`.
    #[serde(default)]
    pub payment_status: Option<String>,
    #[serde(deserialize_with = "datetime::optional")]
    pub created_at: Option<i64>,
}

impl Tenant {
    pub fn is_active(&self) -> bool {
        self.status == TENANT_STATUS_ACTIVE
    }

    pub fn is_pending(&self) -> bool {
        self.status == TENANT_STATUS_PENDING
    }

    pub fn is_rejected(&self) -> bool {
        self.status == TENANT_STATUS_REJECTED
    }

    pub fn is_suspended(&self) -> bool {
        self.status == TENANT_STATUS_SUSPENDED
    }

    /// Whether the onboarding payment gate passed for this application.
    pub fn is_paid(&self) -> bool {
        self.payment_status.as_deref() == Some(PAYMENT_STATUS_PAID)
    }

    /// Whether this tenant may use `app_id` (via `appsEnabled`).
    pub fn has_app(&self, app_id: &str) -> bool {
        self.apps_enabled
            .as_deref()
            .is_some_and(|apps| apps.iter().any(|enabled| enabled == app_id))
    }

    /// The stored JSON gallery array parsed as keys (empty when unset).
    pub fn gallery(&self) -> Vec<String> {
        self.gallery_keys
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default()
    }
}

/// A `Domain` policy node: the owner's per-domain onboarding rules.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DomainPolicy {
    /// The domain this policy governs (`media`, `marketplace`, …).
    #[serde(default)]
    pub domain: Option<String>,
    /// When true, applying to this domain must clear the payment webhook.
    #[serde(default)]
    pub payment_required: Option<bool>,
    /// The owner's billing webhook; `POST`ed at apply time.
    #[serde(default)]
    pub payment_webhook_url: Option<String>,
}

/// Envelope for `queries::domain_policies`.
#[derive(Debug, serde::Deserialize)]
pub struct DomainPolicyRows {
    pub policies: Vec<DomainPolicy>,
}

/// One `Session` node row: the persisted token hash + expiry bound to a user.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub session_id: Option<String>,
    pub user_id: Option<i64>,
    #[serde(deserialize_with = "datetime::optional")]
    pub expires_at: Option<i64>,
}

/// V2 read responses wrap each batch variable in its own key; these are the
/// per-query envelope shapes (same convention as `media::models`).
#[derive(Debug, serde::Deserialize)]
pub struct UserRows {
    pub user: Vec<User>,
}

#[derive(Debug, serde::Deserialize)]
pub struct UsersRows {
    pub users: Vec<User>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TenantRows {
    pub tenant: Vec<Tenant>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TenantsRows {
    pub tenants: Vec<Tenant>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SessionRows {
    pub session: Vec<SessionRecord>,
}