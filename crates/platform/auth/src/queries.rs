//! HelixDB queries owned by the auth domain (per `docs/ARCHITECTURE.md` §12).
//!
//! Anchor narrow, filter tenant first, prefer `write_batch()` for mutations.

use crate::models::{
    PAYMENT_STATUS_NONE, TENANT_STATUS_ACTIVE, TENANT_STATUS_PENDING,
};
use db::query;
use db::schema::{labels, props};
use db::{
    g, read_batch, write_batch, BatchCondition, DateTime, Expr, NodeRef, Order, Predicate,
    PropertyInput, PropertyProjection,
};

/// Equality lookup on the unique `username` index.
#[query]
pub fn user_by_username(username: String) -> ReadBatch {
    read_batch()
        .var_as(
            "user",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USERNAME, username))
                .project(vec![
                    PropertyProjection::new(props::USER_ID),
                    PropertyProjection::new(props::USERNAME),
                    PropertyProjection::new(props::DISPLAY_NAME),
                    PropertyProjection::new(props::EMAIL),
                    PropertyProjection::new(props::PASSWORD_HASH),
                    PropertyProjection::new(props::ROLE),
                    PropertyProjection::new(props::STATUS),
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::CREATED_AT),
                    PropertyProjection::new(props::UPDATED_AT),
                ]),
        )
        .returning(["user"])
}

/// Equality lookup on the `userId` property.
#[query]
pub fn user_by_id(user_id: i64) -> ReadBatch {
    read_batch()
        .var_as(
            "user",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .project(vec![
                    PropertyProjection::new(props::USER_ID),
                    PropertyProjection::new(props::USERNAME),
                    PropertyProjection::new(props::DISPLAY_NAME),
                    PropertyProjection::new(props::EMAIL),
                    PropertyProjection::new(props::PASSWORD_HASH),
                    PropertyProjection::new(props::ROLE),
                    PropertyProjection::new(props::STATUS),
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::CREATED_AT),
                    PropertyProjection::new(props::UPDATED_AT),
                ]),
        )
        .returning(["user"])
}

/// All non-deleted users, ordered by creation date (owner console).
#[query]
pub fn list_users() -> ReadBatch {
    read_batch()
        .var_as(
            "users",
            g().n_with_label(labels::USER)
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::CREATED_AT, Order::Asc)
                .limit(1000usize)
                .project(vec![
                    PropertyProjection::new(props::USER_ID),
                    PropertyProjection::new(props::USERNAME),
                    PropertyProjection::new(props::DISPLAY_NAME),
                    PropertyProjection::new(props::EMAIL),
                    PropertyProjection::new(props::ROLE),
                    PropertyProjection::new(props::STATUS),
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::CREATED_AT),
                ]),
        )
        .returning(["users"])
}

/// Users belonging to one tenant.
#[query]
pub fn users_by_tenant(tenant_id: i64) -> ReadBatch {
    read_batch()
        .var_as(
            "users",
            g().n_with_label(labels::USER)
                .where_(Predicate::and(vec![
                    Predicate::is_null(props::DELETED_AT),
                    Predicate::eq(props::TENANT_ID, tenant_id),
                ]))
                .order_by(props::CREATED_AT, Order::Asc)
                .limit(1000usize)
                .project(vec![
                    PropertyProjection::new(props::USER_ID),
                    PropertyProjection::new(props::USERNAME),
                    PropertyProjection::new(props::DISPLAY_NAME),
                    PropertyProjection::new(props::EMAIL),
                    PropertyProjection::new(props::ROLE),
                    PropertyProjection::new(props::STATUS),
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::CREATED_AT),
                ]),
        )
        .returning(["users"])
}

/// Sessions of one user (used when force-dropping).
#[query]
pub fn sessions_by_user(user_id: i64) -> ReadBatch {
    read_batch()
        .var_as(
            "sessions",
            g().n_with_label(labels::SESSION)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .project(vec![
                    PropertyProjection::new(props::SESSION_ID),
                    PropertyProjection::new(props::USER_ID),
                    PropertyProjection::new(props::EXPIRES_AT),
                ]),
        )
        .returning(["sessions"])
}

/// One session by its token hash (`sessionId`).
#[query]
pub fn session_by_id(session_id: String) -> ReadBatch {
    read_batch()
        .var_as(
            "session",
            g().n_with_label(labels::SESSION)
                .where_(Predicate::eq(props::SESSION_ID, session_id))
                .project(vec![
                    PropertyProjection::new(props::SESSION_ID),
                    PropertyProjection::new(props::USER_ID),
                    PropertyProjection::new(props::EXPIRES_AT),
                ]),
        )
        .returning(["session"])
}

/// All non-deleted tenants.
#[query]
pub fn all_tenants() -> ReadBatch {
    read_batch()
        .var_as(
            "tenants",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::CREATED_AT, Order::Asc)
                .limit(500usize)
                .project(vec![
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::TENANT_NAME),
                    PropertyProjection::new(props::STATUS),
                    PropertyProjection::new(props::APPS_ENABLED),
                    PropertyProjection::new(props::STORAGE_QUOTA_BYTES),
                    PropertyProjection::new(props::STORAGE_USED_BYTES),
                    PropertyProjection::new(props::BILLING_ENABLED),
                    PropertyProjection::new(props::AUTH_WEBHOOK_URL),
                    PropertyProjection::new(props::IS_PUBLIC),
                    PropertyProjection::new(props::DESCRIPTION),
                    PropertyProjection::new(props::TENANT_BANNER_KEY),
                    PropertyProjection::new(props::TENANT_AVATAR_KEY),
                    PropertyProjection::new(props::TENANT_GALLERY),
                    PropertyProjection::new(props::APPLICANT_ID),
                    PropertyProjection::new(props::PAYMENT_STATUS),
                    PropertyProjection::new(props::CREATED_AT),
                ]),
        )
        .returning(["tenants"])
}

/// One tenant by its `tenant_id`.
#[query]
pub fn tenant_by_id(tenant_id: i64) -> ReadBatch {
    read_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .project(vec![
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::TENANT_NAME),
                    PropertyProjection::new(props::STATUS),
                    PropertyProjection::new(props::APPS_ENABLED),
                    PropertyProjection::new(props::STORAGE_QUOTA_BYTES),
                    PropertyProjection::new(props::STORAGE_USED_BYTES),
                    PropertyProjection::new(props::BILLING_ENABLED),
                    PropertyProjection::new(props::AUTH_WEBHOOK_URL),
                    PropertyProjection::new(props::IS_PUBLIC),
                    PropertyProjection::new(props::DESCRIPTION),
                    PropertyProjection::new(props::TENANT_BANNER_KEY),
                    PropertyProjection::new(props::TENANT_AVATAR_KEY),
                    PropertyProjection::new(props::TENANT_GALLERY),
                    PropertyProjection::new(props::APPLICANT_ID),
                    PropertyProjection::new(props::PAYMENT_STATUS),
                    PropertyProjection::new(props::CREATED_AT),
                ]),
        )
        .returning(["tenant"])
}

/// One tenant by unique `tenantName` (used by the seed/upsert).
#[query]
pub fn tenant_by_name(tenant_name: String) -> ReadBatch {
    read_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_NAME, tenant_name))
                .project(vec![
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::TENANT_NAME),
                    PropertyProjection::new(props::STATUS),
                ]),
        )
        .returning(["tenant"])
}

/// The application of one applicant user (first row wins).
#[query]
pub fn tenant_by_applicant(applicant_id: i64) -> ReadBatch {
    read_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::APPLICANT_ID, applicant_id))
                .limit(1usize)
                .project(vec![
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::TENANT_NAME),
                    PropertyProjection::new(props::STATUS),
                    PropertyProjection::new(props::APPS_ENABLED),
                    PropertyProjection::new(props::STORAGE_QUOTA_BYTES),
                    PropertyProjection::new(props::BILLING_ENABLED),
                    PropertyProjection::new(props::AUTH_WEBHOOK_URL),
                    PropertyProjection::new(props::IS_PUBLIC),
                    PropertyProjection::new(props::DESCRIPTION),
                    PropertyProjection::new(props::TENANT_BANNER_KEY),
                    PropertyProjection::new(props::TENANT_AVATAR_KEY),
                    PropertyProjection::new(props::TENANT_GALLERY),
                    PropertyProjection::new(props::APPLICANT_ID),
                    PropertyProjection::new(props::PAYMENT_STATUS),
                    PropertyProjection::new(props::CREATED_AT),
                ]),
        )
        .returning(["tenant"])
}

/// Count of users by tenant (quota-facing).
#[query]
pub fn tenant_users_count(tenant_id: i64) -> ReadBatch {
    read_batch()
        .var_as(
            "count",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .count(),
        )
        .returning(["count"])
}

/// Upsert a tenant by unique name: create when missing, else touch nothing.
#[query]
pub fn upsert_tenant_by_name(
    tenant_name: String,
    tenant_id: i64,
    quota_bytes: i64,
    apps_enabled: Vec<String>,
) -> WriteBatch {
    write_batch()
        .var_as(
            "existing",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_NAME, tenant_name)),
        )
        .var_as_if(
            "created",
            BatchCondition::VarEmpty("existing".to_string()),
            g().add_n(
                labels::TENANT,
                vec![
                    (props::TENANT_NAME, PropertyInput::param("tenant_name")),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                    (props::STORAGE_QUOTA_BYTES, PropertyInput::param("quota_bytes")),
                    (props::APPS_ENABLED, PropertyInput::param("apps_enabled")),
                    (props::STATUS, PropertyInput::from("active")),
                    (props::CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                ],
            ),
        )
        .var_as_if(
            "updated",
            BatchCondition::VarNotEmpty("existing".to_string()),
            g().n(NodeRef::var("existing"))
                .set_property(props::TENANT_ID, PropertyInput::param("tenant_id"))
                .set_property(props::STORAGE_QUOTA_BYTES, PropertyInput::param("quota_bytes"))
                .set_property(props::APPS_ENABLED, PropertyInput::param("apps_enabled"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["created", "updated"])
}

/// Birth of an application: a `pending` tenant tied to its applicant.
#[query]
pub fn create_application(
    tenant_id: i64,
    tenant_name: String,
    apps_enabled: Vec<String>,
    applicant_id: i64,
) -> WriteBatch {
    write_batch()
        .var_as(
            "created",
            g().add_n(
                labels::TENANT,
                vec![
                    (props::TENANT_NAME, PropertyInput::param("tenant_name")),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                    (props::STATUS, PropertyInput::from(TENANT_STATUS_PENDING)),
                    (props::APPS_ENABLED, PropertyInput::param("apps_enabled")),
                    (props::APPLICANT_ID, PropertyInput::param("applicant_id")),
                    (props::PAYMENT_STATUS, PropertyInput::from(PAYMENT_STATUS_NONE)),
                    (props::CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                ],
            ),
        )
        .returning(["created"])
}

/// The onboarding payment gate outcome on a tenant.
#[query]
pub fn set_tenant_payment(tenant_id: i64, payment_status: String) -> WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::PAYMENT_STATUS, PropertyInput::param("payment_status"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["tenant"])
}

/// All domain payment policies (`Domain` label nodes).
#[query]
pub fn domain_policies() -> ReadBatch {
    read_batch()
        .var_as(
            "policies",
            g().n_with_label(labels::DOMAIN_POLICY)
                .project(vec![
                    PropertyProjection::new(props::DOMAIN),
                    PropertyProjection::new(props::PAYMENT_REQUIRED),
                    PropertyProjection::new(props::PAYMENT_WEBHOOK_URL),
                ]),
        )
        .returning(["policies"])
}

/// Upsert a domain's payment policy by unique domain name.
#[query]
pub fn upsert_domain_policy(
    domain: String,
    payment_required: bool,
    payment_webhook_url: String,
) -> WriteBatch {
    write_batch()
        .var_as(
            "existing",
            g().n_with_label(labels::DOMAIN_POLICY)
                .where_(Predicate::eq(props::DOMAIN, domain.clone())),
        )
        .var_as_if(
            "created",
            BatchCondition::VarEmpty("existing".to_string()),
            g().add_n(
                labels::DOMAIN_POLICY,
                vec![
                    (props::DOMAIN, PropertyInput::param("domain")),
                    (props::PAYMENT_REQUIRED, PropertyInput::param("payment_required")),
                    (props::PAYMENT_WEBHOOK_URL, PropertyInput::param("payment_webhook_url")),
                ],
            ),
        )
        .var_as_if(
            "updated",
            BatchCondition::VarNotEmpty("existing".to_string()),
            g().n(NodeRef::var("existing"))
                .set_property(props::PAYMENT_REQUIRED, PropertyInput::param("payment_required"))
                .set_property(props::PAYMENT_WEBHOOK_URL, PropertyInput::param("payment_webhook_url")),
        )
        .returning(["created", "updated"])
}

/// Set a tenant's `status` (active / suspended).
#[query]
pub fn set_tenant_status(tenant_id: i64, status: String) -> WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::STATUS, PropertyInput::param("status"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["tenant"])
}

/// Set a tenant's storage quota and used counters.
#[query]
pub fn set_tenant_storage(tenant_id: i64, quota_bytes: i64, used_bytes: i64) -> WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::STORAGE_QUOTA_BYTES, PropertyInput::param("quota_bytes"))
                .set_property(props::STORAGE_USED_BYTES, PropertyInput::param("used_bytes")),
        )
        .returning(["tenant"])
}

/// Rewrite a tenant's enabled-app list (callers compute the set).
#[query]
pub fn set_tenant_apps(tenant_id: i64, apps: Vec<String>) -> WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::APPS_ENABLED, PropertyInput::param("apps")),
        )
        .returning(["tenant"])
}

/// Tenants listed in the public service directory: visible + active, with
/// just the fields the directory cards render.
#[query]
pub fn public_tenants() -> ReadBatch {
    read_batch()
        .var_as(
            "tenants",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::IS_PUBLIC, true))
                .where_(Predicate::eq(props::STATUS, TENANT_STATUS_ACTIVE))
                .order_by(props::CREATED_AT, Order::Asc)
                .limit(200usize)
                .project(vec![
                    PropertyProjection::new(props::TENANT_ID),
                    PropertyProjection::new(props::TENANT_NAME),
                    PropertyProjection::new(props::APPS_ENABLED),
                    PropertyProjection::new(props::DESCRIPTION),
                    PropertyProjection::new(props::IS_PUBLIC),
                    PropertyProjection::new(props::TENANT_BANNER_KEY),
                    PropertyProjection::new(props::TENANT_AVATAR_KEY),
                    PropertyProjection::new(props::TENANT_GALLERY),
                ]),
        )
        .returning(["tenants"])
}

/// Toggle visibility and tagline used by the directory page.
#[query]
pub fn set_tenant_visibility(tenant_id: i64, is_public: bool, description: String) -> WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::IS_PUBLIC, PropertyInput::param("is_public"))
                .set_property(props::DESCRIPTION, PropertyInput::param("description"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["tenant"])
}

/// Toggle a tenant's billing hook (flag + webhook url).
#[query]
pub fn set_tenant_billing(tenant_id: i64, billing_enabled: bool, webhook_url: String) -> WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::BILLING_ENABLED, PropertyInput::param("billing_enabled"))
                .set_property(props::AUTH_WEBHOOK_URL, PropertyInput::param("webhook_url")),
        )
        .returning(["tenant"])
}

/// Rewrite a tenant's profile media keys (banner / avatar / gallery JSON).
/// Empty strings clear the field.
#[query]
pub fn set_tenant_profile_media(
    tenant_id: i64,
    banner_key: String,
    avatar_key: String,
    gallery_keys: String,
) -> WriteBatch {
    write_batch()
        .var_as(
            "tenant",
            g().n_with_label(labels::TENANT)
                .where_(Predicate::eq(props::TENANT_ID, tenant_id))
                .set_property(props::TENANT_BANNER_KEY, PropertyInput::param("banner_key"))
                .set_property(props::TENANT_AVATAR_KEY, PropertyInput::param("avatar_key"))
                .set_property(props::TENANT_GALLERY, PropertyInput::param("gallery_keys"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["tenant"])
}

/// Upsert a user; `existing` when the username is taken. `tenantId` 0 = no
/// tenant (platform-level owner).
#[query]
pub fn upsert_user(
    user_id: i64,
    username: String,
    display_name: String,
    password_hash: String,
    role: String,
    status: String,
    tenant_id: i64,
) -> WriteBatch {
    write_batch()
        .var_as(
            "existing",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USERNAME, username)),
        )
        .var_as_if(
            "created",
            BatchCondition::VarEmpty("existing".to_string()),
            g().add_n(
                labels::USER,
                vec![
                    (props::USER_ID, PropertyInput::param("user_id")),
                    (props::USERNAME, PropertyInput::param("username")),
                    (props::DISPLAY_NAME, PropertyInput::param("display_name")),
                    (props::PASSWORD_HASH, PropertyInput::param("password_hash")),
                    (props::ROLE, PropertyInput::param("role")),
                    (props::STATUS, PropertyInput::param("status")),
                    (props::TENANT_ID, PropertyInput::param("tenant_id")),
                    (props::CREATED_AT, PropertyInput::Expr(Expr::timestamp())),
                ],
            ),
        )
        .var_as_if(
            "updated",
            BatchCondition::VarNotEmpty("existing".to_string()),
            g().n(NodeRef::var("existing"))
                .set_property(props::PASSWORD_HASH, PropertyInput::param("password_hash"))
                .set_property(props::ROLE, PropertyInput::param("role"))
                .set_property(props::STATUS, PropertyInput::param("status"))
                .set_property(props::TENANT_ID, PropertyInput::param("tenant_id"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["created", "updated"])
}

/// Force-drop a user's sessions — the RouterOS-style kill switch.
#[query]
pub fn drop_user_sessions(user_id: i64) -> WriteBatch {
    write_batch()
        .var_as(
            "sessions",
            g().n_with_label(labels::SESSION)
                .where_(Predicate::eq(props::USER_ID, user_id.clone()))
                .count(),
        )
        .var_as_if(
            "dropped",
            BatchCondition::VarNotEmpty("sessions".to_string()),
            g().n_with_label(labels::SESSION)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .drop(),
        )
        .returning(["dropped"])
}

/// Drop a single session by its token id (logout).
#[query]
pub fn drop_session_token(session_id: String) -> WriteBatch {
    write_batch()
        .var_as(
            "existing",
            g().n_with_label(labels::SESSION)
                .where_(Predicate::eq(props::SESSION_ID, session_id.clone()))
                .count(),
        )
        .var_as_if(
            "dropped",
            BatchCondition::VarNotEmpty("existing".to_string()),
            g().n_with_label(labels::SESSION)
                .where_(Predicate::eq(props::SESSION_ID, session_id))
                .drop(),
        )
        .returning(["dropped"])
}

/// Create a session node (unique `sessionId`).
#[query]
pub fn upsert_session(session_id: String, user_id: i64, expires_at: DateTime) -> WriteBatch {
    write_batch()
        .var_as(
            "created",
            g().add_n(
                labels::SESSION,
                vec![
                    (props::SESSION_ID, PropertyInput::param("session_id")),
                    (props::USER_ID, PropertyInput::param("user_id")),
                    (props::EXPIRES_AT, PropertyInput::param("expires_at")),
                ],
            ),
        )
        .returning(["created"])
}

/// Set a user's password hash.
#[query]
pub fn set_user_password(user_id: i64, password_hash: String) -> WriteBatch {
    write_batch()
        .var_as(
            "user",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .set_property(props::PASSWORD_HASH, PropertyInput::param("password_hash"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["user"])
}

/// Set a user's profile fields (display name, role, status, tenant).
#[query]
pub fn set_user_profile(
    user_id: i64,
    display_name: String,
    role: String,
    status: String,
    tenant_id: i64,
) -> WriteBatch {
    write_batch()
        .var_as(
            "user",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .set_property(props::DISPLAY_NAME, PropertyInput::param("display_name"))
                .set_property(props::ROLE, PropertyInput::param("role"))
                .set_property(props::STATUS, PropertyInput::param("status"))
                .set_property(props::TENANT_ID, PropertyInput::param("tenant_id"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["user"])
}

/// Set a user's status (active / suspended).
#[query]
pub fn set_user_status(user_id: i64, status: String) -> WriteBatch {
    write_batch()
        .var_as(
            "user",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .set_property(props::STATUS, PropertyInput::param("status"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["user"])
}

/// Set a user's role.
#[query]
pub fn set_user_role(user_id: i64, role: String) -> WriteBatch {
    write_batch()
        .var_as(
            "user",
            g().n_with_label(labels::USER)
                .where_(Predicate::eq(props::USER_ID, user_id))
                .set_property(props::ROLE, PropertyInput::param("role"))
                .set_property(props::UPDATED_AT, Expr::timestamp()),
        )
        .returning(["user"])
}

/// Create the unique indexes the auth domain depends on. Idempotent.
#[query]
pub fn ensure_auth_indexes() -> WriteBatch {
    write_batch()
        .var_as(
            "idx_user_id",
            g().create_index_if_not_exists(
                db::IndexSpec::node_unique_equality(labels::USER, props::USER_ID),
            ),
        )
        .var_as(
            "idx_username",
            g().create_index_if_not_exists(
                db::IndexSpec::node_unique_equality(labels::USER, props::USERNAME),
            ),
        )
        .var_as(
            "idx_tenant_id",
            g().create_index_if_not_exists(
                db::IndexSpec::node_unique_equality(labels::TENANT, props::TENANT_ID),
            ),
        )
        .var_as(
            "idx_tenant_name",
            g().create_index_if_not_exists(
                db::IndexSpec::node_unique_equality(labels::TENANT, props::TENANT_NAME),
            ),
        )
        .var_as(
            "idx_session_id",
            g().create_index_if_not_exists(
                db::IndexSpec::node_unique_equality(labels::SESSION, props::SESSION_ID),
            ),
        )
        .var_as(
            "idx_user_tenant",
            g().create_index_if_not_exists(
                db::IndexSpec::node_equality(labels::USER, props::TENANT_ID),
            ),
        )
        .var_as(
            "idx_session_user",
            g().create_index_if_not_exists(
                db::IndexSpec::node_equality(labels::SESSION, props::USER_ID),
            ),
        )
        .returning([
            "idx_user_id",
            "idx_username",
            "idx_tenant_id",
            "idx_tenant_name",
            "idx_session_id",
            "idx_user_tenant",
            "idx_session_user",
        ])
}