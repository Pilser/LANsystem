//! The action catalog and dispatcher.
//!
//! Each entry mirrors one human-route capability: it reads/writes through the
//! same [`db`] models and [`Auth`] service methods (never ad-hoc queries) and
//! enforces the same role guards, so the protocol can reproduce any bug that
//! a human would hit on the web surface.

use std::env;

use media_app::models as media_models;
use marketplace_app::models as market_models;
use platform_core::Role;
use serde_json::{Value, json};
use topcoat::{Result, context::Cx};

const NO_TENANT: i64 = 0;
const SHOP_STATUSES: [&str; 3] = ["accepted", "cancelled", "done"];
const MEDIA_STATUSES: [&str; 3] = ["accepted", "cancelled", "done"];

/// A parameter declared for the catalog.
pub struct ParamSpec {
    pub name: &'static str,
    pub kind: &'static str,
    pub required: bool,
    pub desc: &'static str,
}

/// One callable action, fully described for discovery.
pub struct ActionSpec {
    pub name: &'static str,
    pub group: &'static str,
    pub summary: &'static str,
    pub params: &'static [ParamSpec],
}

const CATALOG: &[ActionSpec] = &[
    ActionSpec {
        name: "auth.whoami",
        group: "auth",
        summary: "Who is executing this request (mode + session user when signed in).",
        params: &[],
    },
    ActionSpec {
        name: "tenant.list",
        group: "tenants",
        summary: "All tenants and their status/apps/quota (owner only).",
        params: &[],
    },
    ActionSpec {
        name: "tenant.get",
        group: "tenants",
        summary: "One tenant by id.",
        params: &[ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." }],
    },
    ActionSpec {
        name: "tenant.setApps",
        group: "tenants",
        summary: "Enable/disable apps for a tenant (owner only).",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." },
            ParamSpec { name: "apps", kind: "string[]", required: true, desc: "App ids, e.g. [\"media\",\"marketplace\"]." },
        ],
    },
    ActionSpec {
        name: "tenant.setQuota",
        group: "tenants",
        summary: "Set a tenant's storage quota in GiB (owner only).",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." },
            ParamSpec { name: "quota_gib", kind: "int", required: true, desc: "Quota in GiB." },
        ],
    },
    ActionSpec {
        name: "tenant.setActive",
        group: "tenants",
        summary: "Activate / suspend a tenant (owner only).",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." },
            ParamSpec { name: "active", kind: "bool", required: true, desc: "True activates." },
        ],
    },
    ActionSpec {
        name: "tenant.setVisibility",
        group: "tenants",
        summary: "Show/hide the tenant in the public directory, with tagline (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." },
            ParamSpec { name: "is_public", kind: "bool", required: true, desc: "List in directory." },
            ParamSpec { name: "tagline", kind: "string", required: false, desc: "Directory tagline." },
        ],
    },
    ActionSpec {
        name: "tenant.setBilling",
        group: "tenants",
        summary: "Toggle the billing webhook for a tenant (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." },
            ParamSpec { name: "enabled", kind: "bool", required: true, desc: "Webhook active." },
            ParamSpec { name: "webhook_url", kind: "string", required: false, desc: "Auth webhook URL." },
        ],
    },
    ActionSpec {
        name: "tenant.approve",
        group: "tenants",
        summary: "Approve a pending application (owner only).",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Pending tenant id." },
            ParamSpec { name: "quota_gib", kind: "int", required: true, desc: "Storage quota in GiB." },
        ],
    },
    ActionSpec {
        name: "tenant.reject",
        group: "tenants",
        summary: "Reject a pending application (owner only).",
        params: &[ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Pending tenant id." }],
    },
    ActionSpec {
        name: "policy.list",
        group: "tenants",
        summary: "Per-domain onboarding payment policies (owner only).",
        params: &[],
    },
    ActionSpec {
        name: "policy.save",
        group: "tenants",
        summary: "Save the onboarding policy for a domain (owner only).",
        params: &[
            ParamSpec { name: "domain", kind: "string", required: true, desc: "Domain id, e.g. \"media\"." },
            ParamSpec { name: "payment_required", kind: "bool", required: true, desc: "Apply must clear payment." },
            ParamSpec { name: "webhook_url", kind: "string", required: false, desc: "Billing webhook URL." },
        ],
    },
    ActionSpec {
        name: "user.list",
        group: "users",
        summary: "Users of one tenant (owner for any, tenant admin for their own).",
        params: &[ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." }],
    },
    ActionSpec {
        name: "user.create",
        group: "users",
        summary: "Create a user (tenant admins only in their own tenant; only the owner may mint owners).",
        params: &[
            ParamSpec { name: "username", kind: "string", required: true, desc: "Login name." },
            ParamSpec { name: "password", kind: "string", required: true, desc: "Password (min 4 chars)." },
            ParamSpec { name: "display_name", kind: "string", required: false, desc: "Display name." },
            ParamSpec { name: "role", kind: "string", required: true, desc: "member | tenant_admin | owner" },
            ParamSpec { name: "tenant_id", kind: "int", required: false, desc: "Tenant id (owner only may be unset)." },
        ],
    },
    ActionSpec {
        name: "shop.product.list",
        group: "shop",
        summary: "A tenant's products; public_only hides drafts (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." },
            ParamSpec { name: "public_only", kind: "bool", required: false, desc: "Only shelf products." },
        ],
    },
    ActionSpec {
        name: "shop.product.create",
        group: "shop",
        summary: "Add a product to a tenant's catalog (owner or the tenant's admin). Images are added via the web UI.",
        params: &[
            ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." },
            ParamSpec { name: "title", kind: "string", required: true, desc: "Product title." },
            ParamSpec { name: "description", kind: "string", required: false, desc: "Description." },
            ParamSpec { name: "price_ugx", kind: "int", required: true, desc: "Price in UGX, >= 0." },
            ParamSpec { name: "stock", kind: "int", required: false, desc: "Stock (default 0)." },
        ],
    },
    ActionSpec {
        name: "shop.product.update",
        group: "shop",
        summary: "Edit title/description/price/stock of a product (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "product_id", kind: "string", required: true, desc: "Product id." },
            ParamSpec { name: "title", kind: "string", required: false, desc: "Title." },
            ParamSpec { name: "description", kind: "string", required: false, desc: "Description." },
            ParamSpec { name: "price_ugx", kind: "int", required: false, desc: "Price in UGX." },
            ParamSpec { name: "stock", kind: "int", required: false, desc: "Stock." },
        ],
    },
    ActionSpec {
        name: "shop.product.setVisibility",
        group: "shop",
        summary: "Publish / unpublish a product on the shop page (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "product_id", kind: "string", required: true, desc: "Product id." },
            ParamSpec { name: "is_public", kind: "bool", required: true, desc: "On the shelf." },
        ],
    },
    ActionSpec {
        name: "shop.product.delete",
        group: "shop",
        summary: "Permanently remove a product: deletes its photos from storage and tombstones the row (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "product_id", kind: "string", required: true, desc: "Product id." },
            ParamSpec { name: "delete_photos", kind: "bool", required: false, desc: "Also delete the stored product photos (default true)." },
        ],
    },
    ActionSpec {
        name: "shop.order.list",
        group: "shop",
        summary: "A tenant's order tickets (owner or the tenant's admin).",
        params: &[ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." }],
    },
    ActionSpec {
        name: "shop.order.setStatus",
        group: "shop",
        summary: "Accept / cancel / complete an order ticket (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "order_id", kind: "string", required: true, desc: "Order id." },
            ParamSpec { name: "status", kind: "string", required: true, desc: "accepted | cancelled | done" },
        ],
    },
    ActionSpec {
        name: "media.item.list",
        group: "media",
        summary: "Media items (owner: everything; tenant admin: their tenant).",
        params: &[ParamSpec { name: "tenant_id", kind: "int", required: false, desc: "Restrict to a tenant." }],
    },
    ActionSpec {
        name: "media.item.get",
        group: "media",
        summary: "One media item by id.",
        params: &[ParamSpec { name: "media_item_id", kind: "string", required: true, desc: "Item id." }],
    },
    ActionSpec {
        name: "media.item.setPrice",
        group: "media",
        summary: "Set an item's sale price in UGX, 0 = free (owner or the item's tenant admin).",
        params: &[
            ParamSpec { name: "media_item_id", kind: "string", required: true, desc: "Item id." },
            ParamSpec { name: "price_ugx", kind: "int", required: true, desc: "Price in UGX, >= 0." },
        ],
    },
    ActionSpec {
        name: "media.item.setVisibility",
        group: "media",
        summary: "Show/hide an item on the tenant's public listings (owner or the item's tenant admin).",
        params: &[
            ParamSpec { name: "media_item_id", kind: "string", required: true, desc: "Item id." },
            ParamSpec { name: "is_public", kind: "bool", required: true, desc: "Public." },
        ],
    },
    ActionSpec {
        name: "media.order.list",
        group: "media",
        summary: "A tenant's media sales orders (owner or the tenant's admin).",
        params: &[ParamSpec { name: "tenant_id", kind: "int", required: true, desc: "Tenant id." }],
    },
    ActionSpec {
        name: "media.order.setStatus",
        group: "media",
        summary: "Accept / cancel / complete a media sale (owner or the tenant's admin).",
        params: &[
            ParamSpec { name: "order_id", kind: "string", required: true, desc: "Order id." },
            ParamSpec { name: "status", kind: "string", required: true, desc: "accepted | cancelled | done" },
        ],
    },
];

/// `GET /api/agent` body: the discoverable catalog.
pub fn catalog_json() -> Value {
    json!({
        "service": "LANsystem agent protocol",
        "version": "1",
        "actions": CATALOG.iter().map(|a| json!({
            "name": a.name,
            "group": a.group,
            "summary": a.summary,
            "params": a.params.iter().map(|p| json!({
                "name": p.name,
                "type": p.kind,
                "required": p.required,
                "desc": p.desc,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "auth": "session cookie, or header x-agent-key: LAN_AGENT_KEY (acts as platform owner)",
        "call": { "method": "POST", "path": "/api/agent",
                  "body": { "action": "tenant.list", "params": {} } },
    })
}

/// Who the request runs as. Key mode is the platform owner; session mode is
/// the signed-in user with the human routes' guards.
enum Scope {
    Key,
    Session(platform_auth::CurrentUser),
}

fn is_owner(scope: &Scope) -> bool {
    match scope {
        Scope::Key => true,
        Scope::Session(current) => current.user.role() == Role::Owner,
    }
}

fn is_own_admin(scope: &Scope, tenant_id: i64) -> bool {
    if tenant_id <= NO_TENANT {
        return false;
    }
    match scope {
        Scope::Key => true,
        Scope::Session(current) => {
            current.user.role().is_at_least(Role::TenantAdmin)
                && current.user.tenant_id == Some(tenant_id)
        }
    }
}

/// Unknown action or params of the wrong shape come back as `ok:false` JSON;
/// server failures too, so agents get one stable envelope.
pub async fn dispatch(cx: &Cx, key: Option<String>, action: &str, params: &Value) -> Value {
    let scope = match resolve_scope(cx, key).await {
        Ok(scope) => scope,
        Err(message) => return json!({ "ok": false, "error": message }),
    };
    match run(cx, &scope, action, params).await {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(error) => json!({ "ok": false, "error": error.to_string() }),
    }
}

async fn resolve_scope(cx: &Cx, key: Option<String>) -> std::result::Result<Scope, String> {
    if let Some(key) = key {
        let expected = env::var("LAN_AGENT_KEY").unwrap_or_default();
        if !expected.is_empty() && key == expected {
            return Ok(Scope::Key);
        }
        return Err("invalid x-agent-key".to_string());
    }
    let auth = platform_core::auth(cx);
    match auth.current_user(cx).await {
        Ok(Some(current)) => Ok(Scope::Session(current)),
        Ok(None) => Err("not signed in; send x-agent-key or log in on the web".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Params helpers
// ---------------------------------------------------------------------------

fn want_i64(params: &Value, key: &str) -> std::result::Result<i64, String> {
    params.get(key).and_then(Value::as_i64).ok_or_else(|| format!("missing or non-int param `{key}`"))
}

fn want_str(params: &Value, key: &str) -> std::result::Result<String, String> {
    params.get(key).and_then(Value::as_str).map(str::to_owned)
        .ok_or_else(|| format!("missing or non-string param `{key}`"))
}

fn want_bool(params: &Value, key: &str) -> std::result::Result<bool, String> {
    params.get(key).and_then(Value::as_bool).ok_or_else(|| format!("missing or non-bool param `{key}`"))
}

fn opt_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn opt_i64(params: &Value, key: &str) -> Option<i64> {
    params.get(key).and_then(Value::as_i64)
}

fn opt_bool(params: &Value, key: &str) -> Option<bool> {
    params.get(key).and_then(Value::as_bool)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

async fn run(cx: &Cx, scope: &Scope, action: &str, params: &Value) -> Result<Value> {
    let auth = platform_core::auth(cx);
    let db = platform_core::db(cx);

    match action {
        "auth.whoami" => {
            match scope {
                Scope::Key => Ok(json!({ "mode": "key", "identity": "platform owner" })),
                Scope::Session(current) => Ok(json!({
                    "mode": "session",
                    "username": current.user.username,
                    "display_name": current.user.display_name,
                    "role": current.user.role().as_str(),
                    "tenant_id": current.user.tenant_id.unwrap_or(0),
                })),
            }
        }

        "tenant.list" => {
            require_owner(scope).await?;
            let tenants = auth.tenants().await?;
            Ok(Value::Array(tenants.iter().map(tenant_json).collect()))
        }

        "tenant.get" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let tenant = auth.tenant_by_id(tenant_id).await?;
            Ok(tenant.as_ref().map_or(Value::Null, tenant_json))
        }

        "tenant.setApps" => {
            require_owner(scope).await?;
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            let apps = params.get("apps").and_then(Value::as_array)
                .ok_or_else(|| bad("missing or non-array param `apps`".to_string()))?;
            let apps = apps.iter().filter_map(Value::as_str).map(str::to_owned).collect();
            auth.set_tenant_apps(tenant_id, apps).await?;
            Ok(json!({ "updated": tenant_id }))
        }

        "tenant.setQuota" => {
            require_owner(scope).await?;
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            let quota_gib = want_i64(params, "quota_gib").map_err(bad)?;
            let bytes = quota_gib.max(1).saturating_mul(1 << 30);
            let used = auth.tenant_by_id(tenant_id).await?
                .and_then(|t| t.storage_used_bytes)
                .unwrap_or(0);
            auth.set_tenant_storage(tenant_id, bytes, used).await?;
            Ok(json!({ "updated": tenant_id, "quota_bytes": bytes }))
        }

        "tenant.setActive" => {
            require_owner(scope).await?;
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            let active = want_bool(params, "active").map_err(bad)?;
            auth.set_tenant_active(tenant_id, active).await?;
            Ok(json!({ "updated": tenant_id, "active": active }))
        }

        "tenant.setVisibility" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let is_public = want_bool(params, "is_public").map_err(bad)?;
            let tagline = opt_str(params, "tagline").unwrap_or_default().to_string();
            auth.set_tenant_visibility(tenant_id, is_public, tagline).await?;
            Ok(json!({ "updated": tenant_id, "is_public": is_public }))
        }

        "tenant.setBilling" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let enabled = want_bool(params, "enabled").map_err(bad)?;
            let webhook = opt_str(params, "webhook_url").unwrap_or_default().to_string();
            auth.set_tenant_billing(tenant_id, enabled, webhook).await?;
            Ok(json!({ "updated": tenant_id, "billing_enabled": enabled }))
        }

        "tenant.approve" => {
            require_owner(scope).await?;
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            let quota_gib = want_i64(params, "quota_gib").map_err(bad)?;
            let quota = quota_gib.max(1).saturating_mul(1 << 30);
            auth.approve_application(tenant_id, quota).await?;
            Ok(json!({ "approved": tenant_id, "quota_bytes": quota }))
        }

        "tenant.reject" => {
            require_owner(scope).await?;
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            auth.reject_application(tenant_id).await?;
            Ok(json!({ "rejected": tenant_id }))
        }

        "policy.list" => {
            require_owner(scope).await?;
            let policies = auth.domain_policies().await?;
            Ok(Value::Array(policies.iter().map(policy_json).collect()))
        }

        "policy.save" => {
            require_owner(scope).await?;
            let domain = want_str(params, "domain").map_err(bad)?;
            let required = want_bool(params, "payment_required").map_err(bad)?;
            let webhook = opt_str(params, "webhook_url").unwrap_or_default().to_string();
            auth.save_domain_policy(domain.clone(), required, webhook).await?;
            Ok(json!({ "saved": domain }))
        }

        "user.list" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let users = auth.users_by_tenant(tenant_id).await?;
            Ok(Value::Array(users.iter().map(user_view_json).collect()))
        }

        "user.create" => {
            let username = want_str(params, "username").map_err(bad)?;
            let password = want_str(params, "password").map_err(bad)?;
            let display_name = opt_str(params, "display_name").unwrap_or_default().to_string();
            let role = platform_auth::Role::parse(opt_str(params, "role").unwrap_or("member"));
            let tenant_id = opt_i64(params, "tenant_id");

            if role == Role::Owner && !is_owner(scope) {
                return Err(bad("only the platform owner may mint owners".to_string()).into());
            }
            let tenant_id = match (is_owner(scope), tenant_id) {
                (true, Some(raw)) => Some(raw),
                (true, None) => None,
                (false, Some(raw)) => {
                    if !is_own_admin(scope, raw) {
                        return Err(bad("tenant admins may only create users in their own tenant".to_string()).into());
                    }
                    Some(raw)
                }
                (false, None) => {
                    return Err(bad("tenant_id is required for non-owner roles".to_string()).into());
                }
            };
            if auth.user_exists(&username).await? {
                return Err(bad("username already taken".to_string()).into());
            }
            auth.save_user(
                None,
                username,
                display_name,
                role,
                tenant_id,
                Some(password),
            ).await?;
            Ok(json!({ "created": true }))
        }

        // ---- shop ---------------------------------------------------------

        "shop.product.list" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let public_only = opt_bool(params, "public_only").unwrap_or(false);
            let page: market_models::ProductPage = if public_only {
                db.query(market_models::shop_products_by_tenant(tenant_id)?).await?
            } else {
                db.query(market_models::products_by_tenant(tenant_id)?).await?
            };
            Ok(serde_json::to_value(page)?)
        }

        "shop.product.create" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let title = want_str(params, "title").map_err(bad)?;
            let description = opt_str(params, "description").unwrap_or_default().to_string();
            let price = want_i64(params, "price_ugx").map_err(bad)?.max(0);
            let stock = opt_i64(params, "stock").unwrap_or(0).max(0);
            let id = product_id();
            let _: serde_json::Value = db.query(market_models::create_product(
                id.clone(),
                title,
                description,
                price,
                "UGX".to_string(),
                String::new(),
                "[]".to_string(),
                stock,
                tenant_id,
            )?).await?;
            Ok(json!({ "product_id": id }))
        }

        "shop.product.update" => {
            let product_id = want_str(params, "product_id").map_err(bad)?;
            let page: market_models::ProductPage =
                db.query(market_models::product_by_id(product_id.clone())?).await?;
            let Some(product) = page.items.into_iter().next() else {
                return Err(bad("product not found".to_string()).into());
            };
            require_tenant(scope, product.tenant_id.unwrap_or(NO_TENANT)).await?;
            let title = opt_str(params, "title").unwrap_or("").to_string();
            let description = opt_str(params, "description").unwrap_or("").to_string();
            let price = opt_i64(params, "price_ugx").unwrap_or(product.price.unwrap_or(0));
            let stock = opt_i64(params, "stock").unwrap_or(product.stock.unwrap_or(0));
            let _: serde_json::Value = db.query(market_models::update_product(
                product_id,
                title,
                description,
                price.max(0),
                stock.max(0),
            )?).await?;
            Ok(json!({ "updated": true }))
        }

        "shop.product.setVisibility" => {
            let product_id = want_str(params, "product_id").map_err(bad)?;
            let is_public = want_bool(params, "is_public").map_err(bad)?;
            let page: market_models::ProductPage =
                db.query(market_models::product_by_id(product_id.clone())?).await?;
            let Some(product) = page.items.into_iter().next() else {
                return Err(bad("product not found".to_string()).into());
            };
            require_tenant(scope, product.tenant_id.unwrap_or(NO_TENANT)).await?;
            let _: serde_json::Value =
                db.query(market_models::set_product_public(product_id, is_public)?).await?;
            Ok(json!({ "is_public": is_public }))
        }

        "shop.product.delete" => {
            let product_id = want_str(params, "product_id").map_err(bad)?;
            let page: market_models::ProductPage =
                db.query(market_models::product_by_id(product_id.clone())?).await?;
            let Some(product) = page.items.into_iter().next() else {
                return Err(bad("product not found".to_string()).into());
            };
            require_tenant(scope, product.tenant_id.unwrap_or(NO_TENANT)).await?;
            if opt_bool(params, "delete_photos").unwrap_or(true) {
                let storage = platform_core::storage(cx);
                if let Some(key) = product.product_image_key.as_deref().filter(|k| !k.is_empty()) {
                    storage.delete(key).await.ok();
                }
                let gallery: Vec<String> = product
                    .gallery_keys
                    .as_deref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                for key in gallery.iter().filter(|k| !k.is_empty()) {
                    storage.delete(key).await.ok();
                }
            }
            let _: serde_json::Value =
                db.query(market_models::delete_product(product_id.clone())?).await?;
            Ok(json!({ "deleted": true, "product_id": product_id }))
        }

        "shop.order.list" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let page: market_models::OrderPage =
                db.query(market_models::orders_by_tenant(tenant_id)?).await?;
            Ok(serde_json::to_value(page)?)
        }

        "shop.order.setStatus" => {
            let order_id = want_str(params, "order_id").map_err(bad)?;
            let status = want_str(params, "status").map_err(bad)?;
            if !SHOP_STATUSES.contains(&status.as_str()) {
                return Err(bad("status must be one of accepted/cancelled/done".to_string()).into());
            }
            let page: market_models::OrderPage =
                db.query(market_models::order_by_id(order_id.clone())?).await?;
            let Some(order) = page.items.into_iter().next() else {
                return Err(bad("order not found".to_string()).into());
            };
            let tenant = order.tenant_id.unwrap_or(NO_TENANT);
            if !is_owner(scope) && session_tenant(scope) != Some(tenant) {
                return Err(bad("out of scope".to_string()).into());
            }
            let _: serde_json::Value =
                db.query(market_models::set_order_status(order_id.clone(), status.clone())?).await?;
            Ok(json!({ "status": status, "order_id": order_id }))
        }

        // ---- media --------------------------------------------------------

        "media.item.list" => {
            match opt_i64(params, "tenant_id") {
                Some(tenant_id) => {
                    require_tenant(scope, tenant_id).await?;
                    let page: media_models::MediaLibrary =
                        db.query(media_models::list_library_by_tenant(tenant_id)?).await?;
                    Ok(serde_json::to_value(page)?)
                }
                None => {
                    require_owner(scope).await?;
                    let page: media_models::MediaLibrary =
                        db.query(media_models::list_library()?).await?;
                    Ok(serde_json::to_value(page)?)
                }
            }
        }

        "media.item.get" => {
            let id = want_str(params, "media_item_id").map_err(bad)?;
            let page: media_models::MediaDetail =
                db.query(media_models::media_item_by_id(id.clone())?).await?;
            match page.item.first() {
                Some(item) => Ok(serde_json::to_value(item)?),
                None => Err(bad("item not found".to_string()).into()),
            }
        }

        "media.item.setPrice" => {
            let id = want_str(params, "media_item_id").map_err(bad)?;
            let price = want_i64(params, "price_ugx").map_err(bad)?.max(0);
            let item = media_item(&db, &id).await?;
            require_item_manage(scope, item.tenant_id).await?;
            let _: serde_json::Value =
                db.query(media_models::set_item_price(id.clone(), price)?).await?;
            Ok(json!({ "media_item_id": id, "price_ugx": price }))
        }

        "media.item.setVisibility" => {
            let id = want_str(params, "media_item_id").map_err(bad)?;
            let is_public = want_bool(params, "is_public").map_err(bad)?;
            let item = media_item(&db, &id).await?;
            require_item_manage(scope, item.tenant_id).await?;
            let _: serde_json::Value =
                db.query(media_models::set_item_public(id.clone(), is_public)?).await?;
            Ok(json!({ "media_item_id": id, "is_public": is_public }))
        }

        "media.order.list" => {
            let tenant_id = want_i64(params, "tenant_id").map_err(bad)?;
            require_tenant(scope, tenant_id).await?;
            let page: media_models::MediaOrderPage =
                db.query(media_models::media_orders_by_tenant(tenant_id)?).await?;
            Ok(serde_json::to_value(page)?)
        }

        "media.order.setStatus" => {
            let order_id = want_str(params, "order_id").map_err(bad)?;
            let status = want_str(params, "status").map_err(bad)?;
            if !MEDIA_STATUSES.contains(&status.as_str()) {
                return Err(bad("status must be one of accepted/cancelled/done".to_string()).into());
            }
            let page: media_models::MediaOrderPage =
                db.query(media_models::media_order_by_id(order_id.clone())?).await?;
            let Some(order) = page.items.into_iter().next() else {
                return Err(bad("order not found".to_string()).into());
            };
            let tenant = order.tenant_id.unwrap_or(NO_TENANT);
            if !is_owner(scope) && session_tenant(scope) != Some(tenant) {
                return Err(bad("out of scope".to_string()).into());
            }
            let _: serde_json::Value =
                db.query(media_models::set_media_order_status(order_id.clone(), status.clone())?).await?;
            Ok(json!({ "order_id": order_id, "status": status }))
        }

        _ => Err(bad(format!("unknown action `{action}`; GET /api/agent lists the catalog")).into()),
    }
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

async fn require_owner(scope: &Scope) -> Result<()> {
    if is_owner(scope) {
        Ok(())
    } else {
        Err(bad("owner role required".to_string()).into())
    }
}

/// Owner, or a tenant admin of this tenant.
async fn require_tenant(scope: &Scope, tenant_id: i64) -> Result<()> {
    if is_owner(scope) || is_own_admin(scope, tenant_id) {
        Ok(())
    } else {
        Err(bad("not authorized for this tenant".to_string()).into())
    }
}

/// Owner, or a tenant admin of the item's owning tenant.
async fn require_item_manage(scope: &Scope, item_tenant: Option<i64>) -> Result<()> {
    match item_tenant {
        Some(t) if t > NO_TENANT => require_tenant(scope, t).await,
        _ => require_owner(scope).await,
    }
}

fn session_tenant(scope: &Scope) -> Option<i64> {
    match scope {
        Scope::Key => None,
        Scope::Session(current) => current.user.tenant_id.filter(|t| *t > NO_TENANT),
    }
}

fn bad(message: String) -> topcoat::Error {
    platform_core::Error::Service(message).into()
}

// ---------------------------------------------------------------------------
// JSON mappers for the auth crate's deserialize-only rows
// ---------------------------------------------------------------------------

fn tenant_json(tenant: &platform_auth::Tenant) -> Value {
    json!({
        "tenant_id": tenant.tenant_id,
        "tenant_name": tenant.tenant_name,
        "status": tenant.status,
        "apps_enabled": tenant.apps_enabled,
        "storage_quota_bytes": tenant.storage_quota_bytes,
        "storage_used_bytes": tenant.storage_used_bytes,
        "billing_enabled": tenant.billing_enabled,
        "auth_webhook_url": tenant.auth_webhook_url,
        "is_public": tenant.is_public,
        "description": tenant.description,
        "applicant_id": tenant.applicant_id,
        "payment_status": tenant.payment_status,
    })
}

fn user_view_json(view: &platform_auth::UserView) -> Value {
    let user = &view.user;
    json!({
        "user_id": user.user_id,
        "username": user.username,
        "display_name": user.display_name,
        "role": view.role.as_str(),
        "active": view.is_active,
        "tenant_id": user.tenant_id,
        "tenant_name": view.tenant_name,
    })
}

fn policy_json(policy: &platform_auth::DomainPolicy) -> Value {
    json!({
        "domain": policy.domain,
        "payment_required": policy.payment_required,
        "payment_webhook_url": policy.payment_webhook_url,
    })
}

// ---------------------------------------------------------------------------
// Row helpers
// ---------------------------------------------------------------------------

async fn media_item(db: &db::Db, id: &str) -> Result<media_models::MediaItem> {
    let page: media_models::MediaDetail =
        db.query(media_models::media_item_by_id(id.to_string())?).await?;
    page.item.into_iter().next().ok_or_else(|| bad("item not found".to_string()).into())
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

fn next_id(prefix: &str) -> String {
    let now = std::time::SystemTime::now();
    let millis = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let mix = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos() as u16;
    format!("{prefix}-{millis:x}-{mix:04x}")
}

fn product_id() -> String {
    next_id("pd")
}