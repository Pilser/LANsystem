//! The staff console: two faces of one app.
//!
//! `/admin` is the **owner console** — the application queue (approve/reject
//! with quota), tenant management, users and the per-domain payment policies.
//! The owner never creates tenants: every business arrives through the public
//! application in `onboarding`. `/portal` is the **tenant admin portal** — a
//! tenant admin's own business: its users, its directory entry, its billing.
//! Mutations are plain POST forms that bounce back to the caller's home.

use std::collections::HashMap;

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, see_other},
        page, parse_query_params, route,
    },
    view::view,
};

use platform_auth::{Auth, CurrentUser, Role, Tenant};

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// Signed-in staff (`>= TenantAdmin`), else login.
pub async fn staff(cx: &Cx, auth: &Auth) -> Result<CurrentUser> {
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    Ok((current.user.role().is_at_least(Role::TenantAdmin))
        .then_some(current)
        .ok_or_redirect("/")?)
}

/// The owner, else their own home.
pub async fn console_user(cx: &Cx, auth: &Auth) -> Result<CurrentUser> {
    let current = staff(cx, auth).await?;
    Ok((current.user.role() == Role::Owner)
        .then_some(current)
        .ok_or_redirect("/")?)
}

/// A tenant admin bound to a tenant, else their own home.
async fn portal_user(cx: &Cx, auth: &Auth) -> Result<CurrentUser> {
    let current = staff(cx, auth).await?;
    Ok((current.user.role() == Role::TenantAdmin
        && current.user.tenant_id.map_or(false, |id| id > 0))
    .then_some(current)
    .ok_or_redirect("/")?)
}

/// Where a staff principal bounces after a mutation.
fn home_for(current: &CurrentUser) -> &'static str {
    if current.user.role() == Role::Owner {
        "/admin"
    } else {
        "/portal"
    }
}

// ---------------------------------------------------------------------------
// Form models
// ---------------------------------------------------------------------------

/// Create or update a user (`user_id` absent = create).
#[derive(Deserialize)]
pub struct SaveUserForm {
    pub user_id: Option<String>,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub tenant_id: Option<String>,
    pub password: String,
}

/// One user's id, for toggle/drop actions.
#[derive(Deserialize)]
pub struct UserIdForm {
    pub user_id: String,
}

/// Update an established tenant's settings (owner console card).
#[derive(Deserialize)]
pub struct SaveTenantForm {
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub tenant_name: String,
    #[serde(default)]
    pub storage_quota_gib: Option<i64>,
    #[serde(default)]
    pub tenant_domain: Option<String>,
    #[serde(default)]
    pub billing_enabled: Option<String>,
    #[serde(default)]
    pub auth_webhook_url: String,
    #[serde(default)]
    pub is_public: Option<String>,
    #[serde(default)]
    pub tagline: String,
}

/// One tenant's id, for the suspend/activate toggle.
#[derive(Deserialize)]
pub struct TenantIdForm {
    pub tenant_id: String,
}

/// Owner reviewing an application: tenant id + the storage it gets.
#[derive(Deserialize)]
pub struct ApproveForm {
    pub tenant_id: String,
    pub storage_quota_gib: i64,
}

/// Owner editing a domain payment policy.
#[derive(Deserialize)]
pub struct PolicyForm {
    pub domain: String,
    #[serde(default)]
    pub payment_required: Option<String>,
    #[serde(default)]
    pub payment_webhook_url: String,
}

/// `?domain=` / `?status=` filters on the owner's tenant list.
#[derive(Deserialize)]
struct AdminQuery {
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

// ---------------------------------------------------------------------------
// The owner console
// ---------------------------------------------------------------------------

/// `GET /admin`: applications to approve, established tenants, and the
/// per-domain payment policies. Owner only.
#[page("/admin")]
async fn console(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let _current = console_user(cx, &auth).await?;

    let query = parse_query_params::<AdminQuery>(cx).ok();
    let want_domain = query.as_ref().and_then(|q| q.domain.as_deref());
    let want_status = query.as_ref().and_then(|q| q.status.as_deref());

    let all_tenants = auth.tenants().await?;
    let users = auth.users().await?;
    let policies = auth.domain_policies().await?;

    let applicant_names: HashMap<i64, String> = users
        .iter()
        .filter_map(|view| {
            view.user
                .user_id
                .map(|id| (id, view.user.username.clone().unwrap_or_default()))
        })
        .collect();

    let (mut pending, rest): (Vec<_>, Vec<_>) =
        all_tenants.into_iter().partition(|tenant| tenant.is_pending());
    let (mut rejected, mut tenants): (Vec<_>, Vec<_>) =
        rest.into_iter().partition(|tenant| tenant.is_rejected());
    tenants.retain(|tenant| {
        let domain_ok = match want_domain {
            Some("media") => tenant.has_app("media"),
            Some("marketplace") => tenant.has_app("marketplace"),
            _ => true,
        };
        let status_ok = match want_status {
            Some("active") => tenant.is_active(),
            Some("suspended") => tenant.is_suspended(),
            _ => true,
        };
        domain_ok && status_ok
    });
    pending.sort_by_key(|tenant| tenant.created_at.unwrap_or(0));
    rejected.sort_by_key(|tenant| tenant.created_at.unwrap_or(0));

    view! {
        <div class="mb-8">
            <h1 class="text-3xl font-bold">"Console"</h1>
            <p class="mt-1 text-sm text-zinc-400">
                "The platform. Businesses arrive here from the public
                application form."
            </p>
        </div>

        // -- Applications -----------------------------------------------------
        <section class="mb-10">
            <div class="mb-3 flex items-baseline gap-3">
                <h2 class="text-xl font-semibold">"Applications"</h2>
                <span class="text-sm text-zinc-500">(pending.len())" waiting"</span>
            </div>
            if pending.is_empty() && rejected.is_empty() {
                <p class="rounded-xl border border-zinc-800 bg-zinc-900 p-4 text-sm text-zinc-400">
                    "The queue is empty. Businesses apply from the directory."
                </p>
            }
            if !pending.is_empty() {
                <ul class="mb-6 grid list-none grid-cols-1 gap-3 p-0 md:grid-cols-2">
                    for tenant in &pending {
                        <li class="rounded-xl border border-zinc-800 bg-zinc-900 p-4">
                            <div class="flex items-start justify-between gap-3">
                                <div class="min-w-0">
                                    <div class="flex flex-wrap items-center gap-2">
                                        <div class="font-semibold">
                                            (tenant.tenant_name.as_deref().unwrap_or("—"))
                                        </div>
                                        let (domain_label, domain_class) = domain_pill(tenant);
                                        <span class=("rounded-full border px-2 py-0.5 text-xs ".to_owned() + domain_class)>
                                            (domain_label)
                                        </span>
                                        if tenant.is_paid() {
                                            <span class="rounded-full border border-emerald-900 bg-emerald-950/60 px-2 py-0.5 text-xs text-emerald-300">
                                                "paid"
                                            </span>
                                        }
                                    </div>
                                    <div class="mt-1 text-xs text-zinc-400">
                                        <span>"applied " (tenant.created_at.map_or_else(|| "—".to_string(), created_display))</span>
                                    </div>
                                    <div class="mt-1 text-xs text-zinc-500">
                                        "Applicant: "
                                        (tenant.applicant_id.and_then(|id| applicant_names.get(&id)).map(String::as_str).unwrap_or("—"))
                                        if tenant.payment_status.as_deref() == Some("declined") {
                                            <span class="text-amber-300">"· payment declined"</span>
                                        }
                                    </div>
                                </div>
                                <form method="post" action="/admin/applications/reject" class="flex shrink-0 items-center gap-2">
                                    <input type="hidden" name="tenant_id" value=(id_text(tenant.tenant_id)) />
                                    <button class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-red-400">
                                        "Reject"
                                    </button>
                                </form>
                            </div>
                            <form method="post" action="/admin/applications/approve"
                                class="mt-4 flex flex-wrap items-end gap-3 border-t border-zinc-800 pt-3"
                            >
                                <input type="hidden" name="tenant_id" value=(id_text(tenant.tenant_id)) />
                                <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                    "Storage quota (GiB)"
                                    <input type="number" name="storage_quota_gib" min="1" value="2"
                                        class="w-24 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400" />
                                </label>
                                <button
                                    class="cursor-pointer rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-emerald-500"
                                >
                                    "Approve & give quota"
                                </button>
                            </form>
                        </li>
                    }
                </ul>
            }
            if !rejected.is_empty() {
                <div class="mb-4 rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
                    <h3 class="mb-2 text-sm font-semibold text-zinc-400">"Rejected"</h3>
                    <ul class="grid list-none grid-cols-1 gap-2 p-0 text-sm md:grid-cols-2">
                        for tenant in &rejected {
                            <li class="flex items-center gap-2 rounded-md border border-zinc-800 px-3 py-2">
                                <span class="h-2 w-2 shrink-0 rounded-full bg-red-500" />
                                <span class="truncate">
                                    (tenant.tenant_name.as_deref().unwrap_or("—"))
                                </span>
                                <span class="ml-auto text-xs text-zinc-500">
                                    "rejected"
                                </span>
                            </li>
                        }
                    </ul>
                </div>
            }
        </section>

        // -- Established tenants ------------------------------------------------
        <section class="mb-10">
            <div class="mb-3 flex flex-wrap items-center justify-between gap-3">
                <h2 class="text-xl font-semibold">"Tenants"</h2>
                <form method="get" action="/admin"
                    class="flex flex-wrap items-end gap-3 text-xs text-zinc-400"
                >
                    <label class="flex flex-col gap-1">
                        "Domain"
                        <select name="domain"
                            class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400"
                        >
                            <option value=("")>"All domains"</option>
                            for (value, label) in DOMAINS {
                                <option value=(value) selected=(want_domain == Some(value))>
                                    (label)
                                </option>
                            }
                        </select>
                    </label>
                    <label class="flex flex-col gap-1">
                        "Status"
                        <select name="status"
                            class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400"
                        >
                            <option value=("")>"All statuses"</option>
                            <option value="active" selected=(want_status == Some("active"))>"Active"</option>
                            <option value="suspended" selected=(want_status == Some("suspended"))>"Suspended"</option>
                        </select>
                    </label>
                    <button
                        class="cursor-pointer rounded-md bg-zinc-800 px-3 py-1.5 text-sm font-semibold text-zinc-100 transition hover:bg-zinc-700"
                    >
                        "Filter"
                    </button>
                    if want_domain.is_some() || want_status.is_some() {
                        <a href="/admin"
                            class="self-end rounded-md border border-zinc-700 px-3 py-1.5 text-sm text-zinc-300 transition hover:border-sky-400"
                        >
                            "Reset"
                        </a>
                    }
                </form>
            </div>
            if tenants.is_empty() {
                <p class="mb-4 rounded-xl border border-zinc-800 bg-zinc-900 p-4 text-sm text-zinc-400">
                    "No tenants with this filter — approved businesses appear here."
                </p>
            } else {
                <ul class="mb-6 grid list-none grid-cols-1 gap-3 p-0 md:grid-cols-2">
                    for tenant in &tenants {
                        <li class="rounded-xl border border-zinc-800 bg-zinc-900 p-4">
                            <div class="flex items-start justify-between gap-3">
                                <div class="min-w-0">
                                    <div class="flex items-center gap-2">
                                        <div class="font-semibold">
                                            (tenant.tenant_name.as_deref().unwrap_or("—"))
                                        </div>
                                        let (domain_label, domain_class) = domain_pill(tenant);
                                        <span class=("rounded-full border px-2 py-0.5 text-xs ".to_owned() + domain_class)>
                                            (domain_label)
                                        </span>
                                    </div>
                                    <div class="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-zinc-400">
                                        <span class=(status_class(tenant.is_active()))>
                                            if tenant.is_active() { "active" } else { "suspended" }
                                        </span>
                                        <span>"quota " (quota_text(tenant.storage_quota_bytes))</span>
                                        <span>(usage_text(tenant.storage_used_bytes))</span>
                                        <span>(billing_text(tenant.billing_enabled, tenant.auth_webhook_url.as_deref()))</span>
                                        <span class=(public_class(tenant.is_public))>
                                            if tenant.is_public == Some(true) { "in directory" } else { "not listed" }
                                        </span>
                                    </div>
                                    <p class="mt-1 text-xs text-zinc-500">
                                        (tenant.description.as_deref().unwrap_or("No tagline yet"))
                                    </p>
                                </div>
                                <form method="post" action="/admin/tenants/toggle">
                                    <input type="hidden" name="tenant_id" value=(id_text(tenant.tenant_id)) />
                                    <button
                                        class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-sky-400"
                                    >
                                        if tenant.is_active() { "Suspend" } else { "Activate" }
                                    </button>
                                </form>
                            </div>
                            <form
                                method="post"
                                action="/admin/tenants/save"
                                class="mt-4 flex flex-wrap items-end gap-3 border-t border-zinc-800 pt-3"
                            >
                                <input type="hidden" name="tenant_id" value=(id_text(tenant.tenant_id)) />
                                <input type="hidden" name="tenant_name" value=(tenant.tenant_name.as_deref().unwrap_or_default()) />
                                <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                    "Quota (GiB)"
                                    <input type="number" name="storage_quota_gib" min="1"
                                        value=(quota_gib(tenant.storage_quota_bytes))
                                        class="w-24 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400" />
                                </label>
                                <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                    "Domain"
                                    <select name="tenant_domain"
                                        class="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400"
                                    >
                                        for (value, label) in DOMAINS {
                                            <option value=(value) selected=(tenant.apps_enabled.as_deref().and_then(|a| a.first()).map(String::as_str) == Some(value))>
                                                (label)
                                            </option>
                                        }
                                    </select>
                                </label>
                                <label class="flex h-8 items-center gap-2 text-xs text-zinc-300">
                                    <input type="checkbox" name="billing_enabled" value="on" checked=(tenant.billing_enabled == Some(true))
                                        class="h-4 w-4 accent-sky-500" />
                                    "Billing webhook"
                                </label>
                                <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                    "AAA webhook URL"
                                    <input name="auth_webhook_url" value=(tenant.auth_webhook_url.as_deref().unwrap_or_default())
                                        class="w-56 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400" />
                                </label>
                                <label class="flex h-8 items-center gap-2 text-xs text-zinc-300">
                                    <input type="checkbox" name="is_public" value="on" checked=(tenant.is_public == Some(true))
                                        class="h-4 w-4 accent-emerald-500" />
                                    "List in public directory"
                                </label>
                                <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                    "Tagline (shown on directory card)"
                                    <input name="tagline" value=(tenant.description.as_deref().unwrap_or_default())
                                        class="w-64 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400" />
                                </label>
                                <button
                                    class="cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-sky-400"
                                >
                                    "Save settings"
                                </button>
                            </form>
                        </li>
                    }
                </ul>
            }
        </section>

        // -- Domain payment policies -------------------------------------------
        <section class="mb-10">
            <h2 class="mb-3 text-xl font-semibold">"Domain onboarding"</h2>
            <p class="mb-3 max-w-2xl text-sm text-zinc-400">
                "Per domain you can require that new businesses pay first: the
                application then calls this webhook, and only a `paid` answer
                moves the application into the queue."
            </p>
            <ul class="grid list-none grid-cols-1 gap-3 p-0 md:grid-cols-2">
                for (value, label) in DOMAINS {
                    let policy = policies.iter().find(|p| p.domain.as_deref() == Some(*value));
                    <li class="rounded-xl border border-zinc-800 bg-zinc-900 p-4">
                        <div class="mb-3 font-semibold">(label)</div>
                        <form method="post" action="/admin/policies/save" class="flex flex-col gap-3">
                            <input type="hidden" name="domain" value=(*value) />
                            <label class="flex items-center gap-2 text-sm text-zinc-300">
                                <input type="checkbox" name="payment_required" value="on"
                                    checked=(policy.and_then(|p| p.payment_required).unwrap_or(false))
                                    class="h-4 w-4 accent-amber-500" />
                                "Also require payment at apply time"
                            </label>
                            <label class="flex flex-col gap-1 text-xs text-zinc-400">
                                "Billing webhook URL (answers `paid: true|false`)"
                                <input name="payment_webhook_url"
                                    placeholder="http://billing.example/pay"
                                    value=(policy.and_then(|p| p.payment_webhook_url.clone()).unwrap_or_default())
                                    class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-400" />
                            </label>
                            <button class="self-start cursor-pointer rounded-md border border-zinc-700 px-3 py-1 text-xs transition hover:border-sky-400">
                                "Save policy"
                            </button>
                        </form>
                    </li>
                }
            </ul>
        </section>

        // -- Users -------------------------------------------------------------
        <section class="mb-10">
            <h2 class="mb-3 text-xl font-semibold">"Users"</h2>
            <div class="overflow-x-auto rounded-xl border border-zinc-800">
                <table class="w-full text-left text-sm">
                    <thead>
                        <tr class="border-b border-zinc-800 text-xs uppercase tracking-wide text-zinc-500">
                            <th class="px-4 py-2">"Username"</th>
                            <th class="px-4 py-2">"Name"</th>
                            <th class="px-4 py-2">"Role"</th>
                            <th class="px-4 py-2">"Tenant"</th>
                            <th class="px-4 py-2">"Status"</th>
                            <th class="px-4 py-2">"Actions"</th>
                        </tr>
                    </thead>
                    <tbody>
                        for user in &users {
                            <tr class="border-b border-zinc-800/60 last:border-0">
                                <td class="px-4 py-2 font-mono text-xs">
                                    (user.user.username.as_deref().unwrap_or("—"))
                                </td>
                                <td class="px-4 py-2">
                                    (user.user.display_name.as_deref().unwrap_or("—"))
                                </td>
                                <td class="px-4 py-2">
                                    <span class=(role_badge_class(user.role))>
                                        (role_badge(user.role))
                                    </span>
                                </td>
                                <td class="px-4 py-2 text-xs text-zinc-400">
                                    (user.tenant_name.as_deref().unwrap_or("—"))
                                </td>
                                <td class="px-4 py-2">
                                    <span class=(status_class(user.is_active))>
                                        if user.is_active { "active" } else { "suspended" }
                                    </span>
                                </td>
                                <td class="px-4 py-2">
                                    <div class="flex flex-wrap gap-2">
                                        <form method="post" action="/admin/users/toggle">
                                            <input type="hidden" name="user_id" value=(id_text(user.user.user_id)) />
                                            <button
                                                class="cursor-pointer rounded-md border border-zinc-700 px-2 py-1 text-xs transition hover:border-sky-400"
                                            >
                                                if user.is_active { "Suspend" } else { "Activate" }
                                            </button>
                                        </form>
                                        <form method="post" action="/admin/users/drop">
                                            <input type="hidden" name="user_id" value=(id_text(user.user.user_id)) />
                                            <button
                                                class="cursor-pointer rounded-md border border-zinc-700 px-2 py-1 text-xs transition hover:border-red-400"
                                            >
                                                "Drop sessions"
                                            </button>
                                        </form>
                                    </div>
                                </td>
                            </tr>
                        }
                    </tbody>
                </table>
            </div>
        </section>
    }
}

// ---------------------------------------------------------------------------
// The tenant admin portal
// ---------------------------------------------------------------------------

/// `GET /portal`: a tenant admin's own business — members, directory entry,
/// billing hook.
#[page("/portal")]
async fn portal(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let current = portal_user(cx, &auth).await?;
    let tenant_id = current.user.tenant_id.unwrap_or(0);
    let members = auth.users_by_tenant(tenant_id).await?;
    let tenant = auth
        .tenant_by_id(tenant_id)
        .await?
        .unwrap_or_default();
    let (domain_label, domain_class) = domain_pill(&tenant);

    view! {
        <div class="mb-8">
            <h1 class="text-3xl font-bold">"Tenant portal"</h1>
            <p class="mt-1 text-sm text-zinc-400">
                "Business: " (tenant.tenant_name.as_deref().unwrap_or("—"))
                <span class=("ml-2 rounded-full border px-2 py-0.5 text-xs ".to_string() + domain_class)>
                    (domain_label)
                </span>
            </p>
        </div>

        <section class="mb-10">
            <h2 class="mb-3 text-xl font-semibold">"Directory entry"</h2>
            <form method="post" action="/admin/tenant/visibility"
                class="rounded-xl border border-zinc-800 bg-zinc-900 p-4"
            >
                <input type="hidden" name="tenant_id" value=(id_text(tenant.tenant_id)) />
                <div class="flex flex-wrap items-end gap-4">
                    <label class="flex h-9 items-center gap-2 text-sm text-zinc-300">
                        <input type="checkbox" name="is_public" value="on" checked=(tenant.is_public == Some(true))
                            class="h-4 w-4 accent-emerald-500" />
                        "List in the public directory"
                    </label>
                    <label class="flex flex-col gap-1 text-sm">
                        "Tagline (shown on the directory card)"
                        <input name="tagline" value=(tenant.description.as_deref().unwrap_or_default())
                            class="w-80 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                    </label>
                    <button
                        class="cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500"
                    >
                        "Save"
                    </button>
                </div>
            </form>
        </section>

        <section class="mb-10">
            <h2 class="mb-3 text-xl font-semibold">"Billing"</h2>
            <form method="post" action="/admin/tenant/billing"
                class="rounded-xl border border-zinc-800 bg-zinc-900 p-4"
            >
                <p class="mb-3 max-w-2xl text-sm text-zinc-400">
                    "Turn on a billing webhook if this business bills by
                    subscription or usage: every access to the tenant's
                    content asks the webhook to allow or deny."
                </p>
                <input type="hidden" name="tenant_id" value=(id_text(tenant.tenant_id)) />
                <div class="flex flex-wrap items-end gap-4">
                    <label class="flex h-9 items-center gap-2 text-sm text-zinc-300">
                        <input type="checkbox" name="billing_enabled" value="on" checked=(tenant.billing_enabled == Some(true))
                            class="h-4 w-4 accent-sky-500" />
                        "Billing webhook active"
                    </label>
                    <label class="flex flex-col gap-1 text-sm">
                        "Webhook URL"
                        <input name="auth_webhook_url" value=(tenant.auth_webhook_url.as_deref().unwrap_or_default())
                            class="w-80 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                    </label>
                    <button
                        class="cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500"
                    >
                        "Save"
                    </button>
                </div>
            </form>
        </section>

        <section class="mb-10">
            <h2 class="mb-3 text-xl font-semibold">"Members"</h2>
            <div class="overflow-x-auto rounded-xl border border-zinc-800">
                <table class="w-full text-left text-sm">
                    <thead>
                        <tr class="border-b border-zinc-800 text-xs uppercase tracking-wide text-zinc-500">
                            <th class="px-4 py-2">"Username"</th>
                            <th class="px-4 py-2">"Name"</th>
                            <th class="px-4 py-2">"Role"</th>
                            <th class="px-4 py-2">"Status"</th>
                            <th class="px-4 py-2">"Actions"</th>
                        </tr>
                    </thead>
                    <tbody>
                        for user in &members {
                            <tr class="border-b border-zinc-800/60 last:border-0">
                                <td class="px-4 py-2 font-mono text-xs">
                                    (user.user.username.as_deref().unwrap_or("—"))
                                </td>
                                <td class="px-4 py-2">
                                    (user.user.display_name.as_deref().unwrap_or("—"))
                                </td>
                                <td class="px-4 py-2">
                                    <span class=(role_badge_class(user.role))>
                                        (role_badge(user.role))
                                    </span>
                                </td>
                                <td class="px-4 py-2">
                                    <span class=(status_class(user.is_active))>
                                        if user.is_active { "active" } else { "suspended" }
                                    </span>
                                </td>
                                <td class="px-4 py-2">
                                    <div class="flex flex-wrap gap-2">
                                        <form method="post" action="/admin/users/toggle">
                                            <input type="hidden" name="user_id" value=(id_text(user.user.user_id)) />
                                            <button
                                                class="cursor-pointer rounded-md border border-zinc-700 px-2 py-1 text-xs transition hover:border-sky-400"
                                            >
                                                if user.is_active { "Suspend" } else { "Activate" }
                                            </button>
                                        </form>
                                        <form method="post" action="/admin/users/drop">
                                            <input type="hidden" name="user_id" value=(id_text(user.user.user_id)) />
                                            <button
                                                class="cursor-pointer rounded-md border border-zinc-700 px-2 py-1 text-xs transition hover:border-red-400"
                                            >
                                                "Drop sessions"
                                            </button>
                                        </form>
                                    </div>
                                </td>
                            </tr>
                        }
                    </tbody>
                </table>
            </div>
            <form
                method="post"
                action="/admin/users/save"
                class="mt-6 rounded-xl border border-zinc-800 bg-zinc-900 p-4"
            >
                <h3 class="mb-3 text-sm font-semibold text-zinc-300">"Add a member"</h3>
                <div class="flex flex-wrap items-end gap-4">
                    <label class="flex flex-col gap-1 text-sm">
                        "Username"
                        <input name="username" required=""
                            class="w-40 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                    </label>
                    <label class="flex flex-col gap-1 text-sm">
                        "Display name"
                        <input name="display_name"
                            class="w-44 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                    </label>
                    <label class="flex flex-col gap-1 text-sm">
                        "Password"
                        <input type="password" name="password" required="" autocomplete="new-password"
                            class="w-44 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                    </label>
                    <label class="flex flex-col gap-1 text-sm">
                        "Role"
                        <select name="role"
                            class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400">
                            <option value="member">"Member"</option>
                            <option value="tenant_admin">"Tenant admin"</option>
                        </select>
                    </label>
                    <button
                        class="cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500"
                    >
                        "Save"
                    </button>
                </div>
            </form>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/// `POST /admin/applications/approve`: activate a pending application with
/// the given quota; the applicant becomes the tenant admin.
#[route(POST "/admin/applications/approve")]
pub async fn applications_approve(
    Form(form): Form<ApproveForm>,
    cx: &Cx,
) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = console_user(cx, &auth).await?;
    let Ok(tenant_id) = form.tenant_id.parse() else {
        return Ok(see_other(home_for(&current)));
    };
    let quota = form.storage_quota_gib.max(1).saturating_mul(1 << 30);
    let _ = auth.approve_application(tenant_id, quota).await;
    Ok(see_other("/admin"))
}

/// `POST /admin/applications/reject`: refuse a pending application; the
/// applicant stays signed in and sees the rejection.
#[route(POST "/admin/applications/reject")]
pub async fn applications_reject(
    Form(form): Form<TenantIdForm>,
    cx: &Cx,
) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = console_user(cx, &auth).await?;
    let Ok(tenant_id) = form.tenant_id.parse() else {
        return Ok(see_other(home_for(&current)));
    };
    let _ = auth.reject_application(tenant_id).await;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/policies/save`: the owner's per-domain onboarding policy.
#[route(POST "/admin/policies/save")]
pub async fn policies_save(
    Form(form): Form<PolicyForm>,
    cx: &Cx,
) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = console_user(cx, &auth).await?;
    if !is_known_domain(&form.domain) {
        return Ok(see_other(home_for(&current)));
    }
    auth.save_domain_policy(
        form.domain.clone(),
        form.payment_required.is_some(),
        form.payment_webhook_url.clone(),
    )
    .await?;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/users/save`: create a user (tenant admins may only create
/// users in their own tenant; only the owner may mint owners).
#[route(POST "/admin/users/save")]
pub async fn users_save(Form(form): Form<SaveUserForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;

    let role = Role::parse(&form.role);
    if role == Role::Owner && current.user.role() != Role::Owner {
        return Ok(see_other(home_for(&current)));
    }
    let tenant_id = match (current.user.role() == Role::Owner, form.tenant_id) {
        (true, Some(raw)) => optional_i64(Some(raw)),
        (true, None) => None,
        (false, _) => current.user.tenant_id,
    };
    let password = if form.password.trim().is_empty() {
        None
    } else {
        Some(form.password.clone())
    };

    auth.save_user(
        None,
        form.username.clone(),
        form.display_name.clone(),
        role,
        tenant_id,
        password,
    )
    .await?;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/users/toggle`: suspend / re-activate a user.
#[route(POST "/admin/users/toggle")]
pub async fn users_toggle(Form(form): Form<UserIdForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;

    let Some(user_id) = form.user_id.parse().ok() else {
        return Ok(see_other(home_for(&current)));
    };
    let active = auth
        .users()
        .await?
        .iter()
        .find(|view| view.user.user_id == Some(user_id))
        .is_some_and(|view| view.is_active);
    auth.set_user_active(user_id, !active).await?;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/users/drop`: kill every session of a user (MikroTik-style).
#[route(POST "/admin/users/drop")]
pub async fn users_drop(Form(form): Form<UserIdForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    let Some(user_id) = form.user_id.parse().ok() else {
        return Ok(see_other(home_for(&current)));
    };
    auth.force_drop_user(user_id).await?;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/tenants/save`: update an established tenant (owner only).
#[route(POST "/admin/tenants/save")]
pub async fn tenants_save(Form(form): Form<SaveTenantForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    if current.user.role() != Role::Owner {
        return Ok(see_other(home_for(&current)));
    }
    let Some(tenant_id) = optional_i64(form.tenant_id) else {
        // The owner never creates tenants any more — businesses apply (§4a).
        return Ok(see_other(home_for(&current)));
    };
    let quota = form.storage_quota_gib.unwrap_or(0).saturating_mul(1 << 30);
    let apps_enabled = form
        .tenant_domain
        .filter(|d| !d.is_empty() && is_known_domain(d))
        .into_iter()
        .map(|d| d.to_owned())
        .collect::<Vec<_>>();
    auth.set_tenant_apps(tenant_id, apps_enabled).await?;
    auth.set_tenant_storage(tenant_id, quota, 0).await?;
    auth.set_tenant_billing(
        tenant_id,
        form.billing_enabled.is_some(),
        form.auth_webhook_url.clone(),
    )
    .await?;
    auth.set_tenant_visibility(tenant_id, form.is_public.is_some(), form.tagline.clone())
        .await?;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/tenant/billing`: the tenant admin's own billing hook.
#[route(POST "/admin/tenant/billing")]
pub async fn tenant_billing(
    Form(form): Form<SaveTenantForm>,
    cx: &Cx,
) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    let tenant_id = match (current.user.role() == Role::Owner, current.user.tenant_id) {
        (true, _) => optional_i64(form.tenant_id),
        (false, Some(tid)) => Some(tid),
        (false, None) => None,
    };
    let Some(tenant_id) = tenant_id else {
        return Ok(see_other(home_for(&current)));
    };
    auth.set_tenant_billing(
        tenant_id,
        form.billing_enabled.is_some(),
        form.auth_webhook_url.clone(),
    )
    .await?;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/tenant/visibility`: a tenant admin (or the owner) toggles
/// their own tenant's entry in the public directory + its tagline.
#[route(POST "/admin/tenant/visibility")]
pub async fn tenant_visibility(
    Form(form): Form<SaveTenantForm>,
    cx: &Cx,
) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    let tenant_id = match (current.user.role() == Role::Owner, current.user.tenant_id) {
        (true, _) => optional_i64(form.tenant_id),
        (false, Some(tid)) => Some(tid),
        (false, None) => None,
    };
    let Some(tenant_id) = tenant_id else {
        return Ok(see_other(home_for(&current)));
    };
    auth.set_tenant_visibility(tenant_id, form.is_public.is_some(), form.tagline.clone())
        .await?;
    Ok(see_other(home_for(&current)))
}

/// `POST /admin/tenants/toggle`: suspend / re-activate a tenant (owner only).
#[route(POST "/admin/tenants/toggle")]
pub async fn tenants_toggle(
    Form(form): Form<TenantIdForm>,
    cx: &Cx,
) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = console_user(cx, &auth).await?;
    let Some(tenant_id) = form.tenant_id.parse().ok() else {
        return Ok(see_other(home_for(&current)));
    };
    let active = auth
        .tenants()
        .await?
        .iter()
        .find(|tenant| tenant.tenant_id == Some(tenant_id))
        .is_some_and(|tenant| tenant.is_active());
    auth.set_tenant_active(tenant_id, !active).await?;
    Ok(see_other(home_for(&current)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const DOMAINS: &[(&str, &str)] = &[
    ("media", "Media Library"),
    ("marketplace", "Marketplace"),
];

fn is_known_domain(domain: &str) -> bool {
    DOMAINS.iter().any(|(value, _)| *value == domain)
}

/// The label + badge class for a tenant's domain, for list cards.
fn domain_pill(tenant: &Tenant) -> (&'static str, &'static str) {
    match tenant.apps_enabled.as_deref().and_then(|apps| apps.first().map(String::as_str)) {
        Some("marketplace") => ("Marketplace", "border-sky-700 bg-sky-900/40 text-sky-200"),
        Some("media") => ("Media", "border-emerald-900 bg-emerald-950/60 text-emerald-300"),
        _ => ("No domain", "border-zinc-700 bg-zinc-800 text-zinc-400"),
    }
}

fn created_display(millis: i64) -> String {
    let days = millis / 86_400_000;
    let secs = (millis / 1000) % 86_400;
    format!("{}d {}h ago", days, secs / 3600)
}

/// Parse an optional id from a form string (`""` or `None` → `None`).
fn optional_i64(raw: Option<String>) -> Option<i64> {
    raw.and_then(|s| if s.is_empty() { None } else { s.parse().ok() })
}

/// Render an id as a form value (`None` → empty string).
fn id_text(id: Option<i64>) -> String {
    id.map_or_else(String::new, |v| v.to_string())
}

fn role_badge(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::TenantAdmin => "tenant admin",
        Role::Member => "member",
    }
}

fn role_badge_class(role: Role) -> &'static str {
    match role {
        Role::Owner => "text-amber-300",
        Role::TenantAdmin => "text-sky-300",
        Role::Member => "text-zinc-400",
    }
}

fn status_class(active: bool) -> &'static str {
    if active { "text-emerald-400" } else { "text-red-400" }
}

fn public_class(is_public: Option<bool>) -> &'static str {
    if is_public == Some(true) { "text-emerald-400" } else { "text-zinc-500" }
}

fn quota_text(bytes: Option<i64>) -> String {
    bytes.map_or_else(|| "—".into(), |bytes| format!("{} MiB", bytes / 1024 / 1024))
}

fn quota_gib(bytes: Option<i64>) -> i64 {
    bytes.map_or(2, |bytes| (bytes >> 30).max(1))
}

fn usage_text(bytes: Option<i64>) -> String {
    bytes
        .filter(|bytes| *bytes > 0)
        .map_or_else(|| "unused".into(), |bytes| format!("{} MiB used", bytes / 1024 / 1024))
}

fn billing_text(billing: Option<bool>, webhook: Option<&str>) -> String {
    match (billing, webhook) {
        (Some(true), Some(url)) if !url.is_empty() => format!("billing via {url}"),
        (Some(true), _) => "billing on (no webhook)".into(),
        _ => "no billing".into(),
    }
}