//! The staff console: users, tenants and access settings.
//!
//! Renders fragments inside the shell layout (style, nav and htmx come from
//! `apps/lan`). "Staff" means a signed-in user with role `>= TenantAdmin`:
//! the platform owner manages every tenant and user; a tenant admin manages
//! only their own tenant and its users. Mutations are plain POST forms that
//! bounce back to `/console` with a 303 See-Other.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, see_other},
        page, route,
    },
    view::view,
};

use platform_auth::{Auth, CurrentUser, Role};

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// The request's principal, redirected to `/login` when signed out and to `/`
/// when signed in but not staff.
pub async fn staff(cx: &Cx, auth: &Auth) -> Result<CurrentUser> {
    let current = auth.current_user(cx).await?.ok_or_redirect("/login")?;
    Ok((current.user.role().is_at_least(Role::TenantAdmin))
        .then_some(current)
        .ok_or_redirect("/")?)
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

/// Create or update a tenant (`tenant_id` absent = create).
#[derive(Deserialize)]
pub struct SaveTenantForm {
    #[serde(default)]
    pub tenant_id: Option<String>,
    pub tenant_name: String,
    pub storage_quota_gib: i64,
    /// One checkbox per app. Distinct field names: the form codec rejects
    /// repeated keys, so the apps to enable are literals instead.
    #[serde(default)]
    pub app_media: Option<String>,
    #[serde(default)]
    pub app_marketplace: Option<String>,
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

// ---------------------------------------------------------------------------
// The console page
// ---------------------------------------------------------------------------

/// `GET /admin`: the staff dashboard — tenants (owner) + users, with the
/// create/edit forms.
#[page("/admin")]
async fn console(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    let is_owner = current.user.role() == Role::Owner;

    let (tenants, users) = if is_owner {
        (auth.tenants().await?, auth.users().await?)
    } else if let Some(tenant_id) = current.user.tenant_id {
        (Vec::new(), auth.users_by_tenant(tenant_id).await?)
    } else {
        (Vec::new(), Vec::new())
    };

    view! {
        <div class="mb-8 flex flex-wrap items-baseline gap-4">
            <h1 class="text-3xl font-bold">"Console"</h1>
            <p class="text-sm text-zinc-400">
                if is_owner {
                    "Platform view — tenants and everyone on them."
                } else {
                    "Tenant view — the members of your tenant."
                }
            </p>
        </div>

        // -- Tenants (owner only) --------------------------------------------
        if is_owner {
            <section class="mb-10">
                <h2 class="mb-3 text-xl font-semibold">"Tenants"</h2>
                if tenants.is_empty() {
                    <p class="mb-4 rounded-xl border border-zinc-800 bg-zinc-900 p-4 text-sm text-zinc-400">
                        "No tenants yet — create one below and it gets its own
                        isolated library."
                    </p>
                } else {
                    <ul class="mb-6 grid list-none grid-cols-1 gap-3 p-0 md:grid-cols-2">
                        for tenant in &tenants {
                            <li class="rounded-xl border border-zinc-800 bg-zinc-900 p-4">
                                <div class="flex items-start justify-between gap-3">
                                    <div class="min-w-0">
                                        <div class="font-semibold">
                                            (tenant.tenant_name.as_deref().unwrap_or("—"))
                                        </div>
                                        <div class="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-zinc-400">
                                            <span class=(status_class(tenant.is_active()))>
                                                if tenant.is_active() { "active" } else { "suspended" }
                                            </span>
                                            <span>"quota " (quota_text(tenant.storage_quota_bytes))</span>
                                            <span>(usage_text(tenant.storage_used_bytes))</span>
                                            <span>"apps: " (apps_text(tenant.apps_enabled.as_deref()))</span>
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
                                    <label class="flex h-8 items-center gap-2 text-xs text-zinc-300">
                                        <input type="checkbox" name="app_media" value="media" checked=(tenant.has_app("media"))
                                            class="h-4 w-4 accent-sky-500" />
                                        "Media Library"
                                    </label>
                                    <label class="flex h-8 items-center gap-2 text-xs text-zinc-300">
                                        <input type="checkbox" name="app_marketplace" value="marketplace" checked=(tenant.has_app("marketplace"))
                                            class="h-4 w-4 accent-sky-500" />
                                        "Marketplace"
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

                <form
                    method="post"
                    action="/admin/tenants/save"
                    class="rounded-xl border border-zinc-800 bg-zinc-900 p-4"
                >
                    <h3 class="mb-3 text-sm font-semibold text-zinc-300">"Create a tenant"</h3>
                    <div class="flex flex-wrap items-end gap-4">
                        <label class="flex flex-col gap-1 text-sm">
                            "Name"
                            <input name="tenant_name" required=""
                                class="w-44 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                        </label>
                        <label class="flex flex-col gap-1 text-sm">
                            "Storage quota (GiB)"
                            <input type="number" name="storage_quota_gib" min="1" value="2" required=""
                                class="w-28 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                        </label>
                        <label class="flex h-9 items-center gap-2 text-sm">
                            <input type="checkbox" name="app_media" value="media"
                                class="h-4 w-4 accent-sky-500" />
                            "Media Library"
                        </label>
                        <label class="flex h-9 items-center gap-2 text-sm">
                            <input type="checkbox" name="app_marketplace" value="marketplace"
                                class="h-4 w-4 accent-sky-500" />
                            "Marketplace"
                        </label>
                        <label class="flex h-9 items-center gap-2 text-sm">
                            <input type="checkbox" name="billing_enabled" value="on"
                                class="h-4 w-4 accent-sky-500" />
                            "Billing / webhook"
                        </label>
                        <label class="flex flex-col gap-1 text-sm">
                            "AAA webhook URL"
                            <input name="auth_webhook_url"
                                class="w-56 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                        </label>
                        <button
                            class="cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500"
                        >
                            "Create tenant"
                        </button>
                    </div>
                </form>
            </section>
        }

        // -- Users -------------------------------------------------------------
        <section>
            <h2 class="mb-3 text-xl font-semibold">"Users"</h2>
            <div class="overflow-x-auto rounded-xl border border-zinc-800">
                <table class="w-full text-left text-sm">
                    <thead>
                        <tr class="border-b border-zinc-800 text-xs uppercase tracking-wide text-zinc-500">
                            <th class="px-4 py-2">"Username"</th>
                            <th class="px-4 py-2">"Name"</th>
                            <th class="px-4 py-2">"Role"</th>
                            if is_owner {
                                <th class="px-4 py-2">"Tenant"</th>
                            }
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
                                if is_owner {
                                    <td class="px-4 py-2 text-xs text-zinc-400">
                                        (user.tenant_name.as_deref().unwrap_or("—"))
                                    </td>
                                }
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
                <h3 class="mb-3 text-sm font-semibold text-zinc-300">"Add a user"</h3>
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
                            for (value, label) in ROLE_OPTIONS {
                                <option value=(*value)> (label) </option>
                            }
                        </select>
                    </label>
                    if is_owner {
                        <label class="flex flex-col gap-1 text-sm">
                            "Tenant"
                            <select name="tenant_id"
                                class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400">
                                <option value="">"— platform (no tenant) —"</option>
                                for tenant in &tenants {
                                    <option value=(id_text(tenant.tenant_id))>
                                        (tenant.tenant_name.as_deref().unwrap_or("—"))
                                    </option>
                                }
                            </select>
                        </label>
                    }
                    <button
                        class="cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500"
                    >
                        "Save user"
                    </button>
                </div>
            </form>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/// `POST /admin/users/save`: create a user (tenant admins may only create
/// users in their own tenant; only the owner may mint owners).
#[route(POST "/admin/users/save")]
pub async fn users_save(Form(form): Form<SaveUserForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;

    let role = Role::parse(&form.role);
    if role == Role::Owner && current.user.role() != Role::Owner {
        return Ok(see_other("/admin"));
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
    Ok(see_other("/admin"))
}

/// `POST /admin/users/toggle`: suspend / re-activate a user.
#[route(POST "/admin/users/toggle")]
pub async fn users_toggle(Form(form): Form<UserIdForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    staff(cx, &auth).await?;

    let Some(user_id) = form.user_id.parse().ok() else {
        return Ok(see_other("/admin"));
    };
    let active = auth
        .users()
        .await?
        .iter()
        .find(|view| view.user.user_id == Some(user_id))
        .is_some_and(|view| view.is_active);
    auth.set_user_active(user_id, !active).await?;
    Ok(see_other("/admin"))
}

/// `POST /admin/users/drop`: kill every session of a user (MikroTik-style).
#[route(POST "/admin/users/drop")]
pub async fn users_drop(
    Form(form): Form<UserIdForm>,
    cx: &Cx,
) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    staff(cx, &auth).await?;
    let Some(user_id) = form.user_id.parse().ok() else {
        return Ok(see_other("/admin"));
    };
    auth.force_drop_user(user_id).await?;
    Ok(see_other("/admin"))
}

/// `POST /admin/tenants/save`: create a tenant, or update its name/settings
/// when `tenant_id` is present (owner only).
#[route(POST "/admin/tenants/save")]
pub async fn tenants_save(Form(form): Form<SaveTenantForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    if current.user.role() != Role::Owner {
        return Ok(see_other("/"));
    }
    let quota = form.storage_quota_gib.saturating_mul(1 << 30);
    let tenant_id = optional_i64(form.tenant_id);
    let mut apps_enabled = Vec::new();
    if form.app_media.is_some() {
        apps_enabled.push("media".to_string());
    }
    if form.app_marketplace.is_some() {
        apps_enabled.push("marketplace".to_string());
    }
    auth.save_tenant(
        tenant_id,
        form.tenant_name.clone(),
        quota,
        apps_enabled,
        form.billing_enabled.is_some(),
        form.auth_webhook_url.clone(),
    )
    .await?;
    if let Some(tenant_id) = tenant_id {
        auth.set_tenant_visibility(tenant_id, form.is_public.is_some(), form.tagline.clone())
            .await?;
    }
    Ok(see_other("/admin"))
}

/// `POST /admin/tenant/visibility`: a tenant admin (or the owner) toggles
/// their own tenant's entry in the public directory + its tagline.
#[route(POST "/admin/tenant/visibility")]
pub async fn tenant_visibility(Form(form): Form<SaveTenantForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    // Only the tenant that admin belongs to may be touched.
    let tenant_id = match (current.user.role(), current.user.tenant_id) {
        (Role::Owner, _) => optional_i64(form.tenant_id),
        (_, Some(tid)) => Some(tid),
        (_, None) => None,
    };
    let Some(tenant_id) = tenant_id else {
        return Ok(see_other("/admin"));
    };
    auth.set_tenant_visibility(tenant_id, form.is_public.is_some(), form.tagline.clone())
        .await?;
    Ok(see_other("/admin"))
}

/// `POST /admin/tenants/toggle`: suspend / re-activate a tenant (owner only).
#[route(POST "/admin/tenants/toggle")]
pub async fn tenants_toggle(Form(form): Form<TenantIdForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let current = staff(cx, &auth).await?;
    if current.user.role() != Role::Owner {
        return Ok(see_other("/"));
    }
    let Some(tenant_id) = form.tenant_id.parse().ok() else {
        return Ok(see_other("/admin"));
    };
    let active = auth
        .tenants()
        .await?
        .iter()
        .find(|tenant| tenant.tenant_id == Some(tenant_id))
        .is_some_and(|tenant| tenant.is_active());
    auth.set_tenant_active(tenant_id, !active).await?;
    Ok(see_other("/admin"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const ROLE_OPTIONS: &[(&str, &str)] = &[
    ("owner", "Owner"),
    ("tenant_admin", "Tenant admin"),
    ("member", "Member"),
];

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

fn apps_text(apps: Option<&[String]>) -> String {
    match apps {
        Some(apps) if !apps.is_empty() => apps.join(", "),
        _ => "none".into(),
    }
}

fn billing_text(billing: Option<bool>, webhook: Option<&str>) -> String {
    match (billing, webhook) {
        (Some(true), Some(url)) if !url.is_empty() => format!("billing via {url}"),
        (Some(true), _) => "billing on (no webhook)".into(),
        _ => "no billing".into(),
    }
}