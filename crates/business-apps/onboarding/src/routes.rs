//! Public application flow: `GET /apply` form → `POST /apply` creates the
//! applicant's account plus a `pending` tenant (running the domain's payment
//! gate when the owner requires it) → `/apply/status` follows the
//! application until approved or rejected. `/pay/echo` is a tiny demo
//! billing webhook to point domain policies at while testing.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        content::{Form, Json},
        error::{SeeOther, redirect, see_other},
        page, parse_query_params, route,
    },
    view::view,
};

use platform_auth::{Auth, CurrentUser, DOMAINS, Role, Tenant};

/// The public application form.
#[derive(Deserialize)]
struct ApplyForm {
    business_name: String,
    domain: String,
    display_name: String,
    username: String,
    password: String,
}

/// `?error=` flash on the application form.
#[derive(Deserialize)]
struct ApplyQuery {
    #[serde(default)]
    error: Option<String>,
}

/// `?paid=` / `?declined=` flash state after `POST /apply/pay`.
#[derive(Deserialize)]
struct StatusQuery {
    #[serde(default)]
    paid: Option<String>,
    #[serde(default)]
    declined: Option<String>,
}

#[derive(Deserialize)]
struct PayForm {
    tenant_id: String,
}

/// `GET /apply`: the application form. Guests fill it in; signed-in
/// applicants go to their status page; staff go to their own portal.
#[page("/apply")]
async fn apply_page(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let Some(current) = auth.current_user(cx).await? else {
        let policies = auth.domain_policies().await?;
        let error = parse_query_params::<ApplyQuery>(cx)
            .ok()
            .and_then(|q| q.error);
        return view! {
            <div class="mx-auto max-w-xl">
                <h1 class="mb-1 text-3xl font-bold">"Create a business"</h1>
                <p class="mb-6 text-zinc-400">
                    "One business, one corner of this LAN. Apply, and once the
                    server owner approves you'll get your own portal."
                </p>
                if let Some(error) = error {
                    <div class="mb-4 rounded-md border border-red-800 bg-red-950/40 px-3 py-2 text-sm text-red-300">
                        (apply_error_text(&error))
                    </div>
                }
                <form method="post" action="/apply"
                    class="rounded-xl border border-zinc-800 bg-zinc-900 p-5"
                >
                    <div class="flex flex-col gap-3">
                        <label class="flex flex-col gap-1 text-sm">
                            "Business name"
                            <input name="business_name" required=""
                                class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                        </label>
                        <label class="flex flex-col gap-1 text-sm">
                            "Which service does your business need?"
                            <select name="domain"
                                class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400"
                            >
                                for (value, label) in DOMAINS {
                                    <option value=(value)>(label)</option>
                                }
                            </select>
                        </label>
                        <div class="flex flex-wrap gap-2 text-xs">
                            for (value, label) in DOMAINS {
                                let required = policies.iter().any(|p| {
                                    p.domain.as_deref() == Some(value)
                                        && p.payment_required == Some(true)
                                });
                                <span class=("rounded-full border px-2 py-0.5 ".to_owned()
                                    + if required {
                                        "border-amber-800 bg-amber-950/40 text-amber-300"
                                    } else {
                                        "border-zinc-700 bg-zinc-800 text-zinc-400"
                                    })>
                                    (label)
                                    if required { " · payment required" }
                                </span>
                            }
                        </div>
                        <label class="flex flex-col gap-1 text-sm">
                            "Your full name"
                            <input name="display_name" required=""
                                class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                        </label>
                        <label class="flex flex-col gap-1 text-sm">
                            "Username"
                            <input name="username" required=""
                                class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                        </label>
                        <label class="flex flex-col gap-1 text-sm">
                            "Password"
                            <input type="password" name="password" minlength="4" required=""
                                class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm outline-none focus:border-sky-400" />
                        </label>
                    </div>
                    <button class="mt-5 cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500">
                        "Apply"
                    </button>
                </form>
            </div>
        };
    };
    Err(redirect(principal_target(&current)).into())
}

/// `POST /apply`: validate, create the pending tenant + applicant account,
/// sign the applicant in and land on the status page.
#[route(POST "/apply")]
async fn apply_submit(Form(form): Form<ApplyForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    if let Some(error) = validate_apply(&auth, &form).await {
        return Ok(see_other(&format!("/apply?error={error}")));
    }
    match auth
        .submit_application(
            form.business_name.clone(),
            form.domain.clone(),
            form.username.clone(),
            form.display_name.clone(),
            form.password.clone(),
        )
        .await
    {
        Ok(_) => {
            let _ = auth.login(cx, &form.username, &form.password).await;
            Ok(see_other("/apply/status"))
        }
        Err(_) => Ok(see_other("/apply?error=internal")),
    }
}

/// `GET /apply/status`: a signed-in applicant's window on their business.
#[page("/apply/status")]
async fn apply_status(cx: &Cx) -> Result {
    let auth = platform_core::auth(cx);
    let Some(current) = auth.current_user(cx).await? else {
        return Err(redirect("/apply").into());
    };
    if current.user.role().is_at_least(Role::TenantAdmin) {
        return Err(redirect("/portal").into());
    }
    let Some(user_id) = current.user.user_id else {
        return Err(redirect("/apply").into());
    };
    let Some(app) = auth.application_for(user_id).await? else {
        return Err(redirect("/apply").into());
    };

    let payment_required = auth_needs_payment(&auth, &app).await;
    let flash = parse_query_params::<StatusQuery>(cx)
        .ok()
        .map(|q| (q.paid.is_some(), q.declined.is_some()));

    let (title, accent, note) = status_tone(&app.status);
    let (domain_label, domain_class) = domain_badge(&app);
    let (pay_label, pay_class, pay_button) = payment_state(&app, payment_required);
    let (meta, action) = status_meta(&app, payment_required, &current);

    view! {
        <div class="mx-auto max-w-xl">
            <h1 class=("mb-1 text-3xl font-bold ".to_string() + accent)>(title)</h1>
            <p class="mb-6 text-zinc-400">(note)</p>
            if let Some((paid, declined)) = flash {
                if paid {
                    <div class="mb-4 rounded-md border border-emerald-700 bg-emerald-950/40 px-3 py-2 text-sm text-emerald-300">
                        "Payment recorded — the owner will review your application."
                    </div>
                } else if declined {
                    <div class="mb-4 rounded-md border border-red-800 bg-red-950/40 px-3 py-2 text-sm text-red-300">
                        "Payment was declined. You can try again below."
                    </div>
                }
            }
            <div class="rounded-xl border border-zinc-800 bg-zinc-900 p-5">
                <div class="flex flex-wrap items-center gap-2">
                    <div class="text-lg font-semibold text-zinc-100">
                        (app.tenant_name.as_deref().unwrap_or("—"))
                    </div>
                    <span class=("rounded-full border px-2 py-0.5 text-xs ".to_string() + domain_class)>
                        (domain_label)
                    </span>
                    <span class=("rounded-full border px-2 py-0.5 text-xs ".to_string() + pay_class)>
                        (pay_label)
                    </span>
                </div>
                <p class="mt-3 text-sm text-zinc-400">(meta)</p>
                if pay_button {
                    <form method="post" action="/apply/pay" class="mt-5">
                        <input type="hidden" name="tenant_id"
                            value=(app.tenant_id.map(|id| id.to_string()).unwrap_or_default()) />
                        <button class="cursor-pointer rounded-md bg-amber-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-amber-500">
                            "Pay now and continue"
                        </button>
                    </form>
                }
                if let Some((action_label, action_path)) = action {
                    <a class="mt-5 inline-block cursor-pointer rounded-md bg-sky-600 px-4 py-1.5 text-sm font-semibold text-white transition hover:bg-sky-500"
                        href=(action_path)>
                        (action_label)
                    </a>
                }
            </div>
        </div>
    }
}

/// `POST /apply/pay`: run (or rerun) the domain's payment webhook for the
/// applicant's own application.
#[route(POST "/apply/pay")]
async fn pay_retry(Form(form): Form<PayForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = platform_core::auth(cx);
    let Ok(tenant_id) = form.tenant_id.parse::<i64>() else {
        return Ok(see_other("/apply/status"));
    };
    let Some(current) = auth.current_user(cx).await? else {
        return Ok(see_other("/apply"));
    };
    let Some(application) = auth
        .application_for(current.user.user_id.unwrap_or(0))
        .await
        .ok()
        .flatten()
    else {
        return Ok(see_other("/apply/status"));
    };
    if application.tenant_id != Some(tenant_id) {
        return Ok(see_other("/apply/status"));
    }
    let passed = auth.run_payment_gate(tenant_id).await.unwrap_or(Some(false));
    Ok(see_other(if passed == Some(true) {
        "/apply/status?paid=1"
    } else {
        "/apply/status?declined=1"
    }))
}

/// `POST /pay/echo`: demo billing webhook — answers `{"paid": true}` unless
/// the caller submits `paid=false`. Point a domain policy at
/// `http://<host>:4321/pay/echo` to exercise the paid onboarding path.
#[route(POST "/pay/echo")]
async fn pay_echo(Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>> {
    let paid = body.get("paid").and_then(|v| v.as_bool()).unwrap_or(true);
    Ok(Json(serde_json::json!({ "paid": paid })))
}

/// Same, reachable from a browser tab for a smoke test.
#[route(GET "/pay/echo")]
async fn pay_echo_get() -> Result<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({ "paid": true })))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn principal_target(current: &CurrentUser) -> &'static str {
    match current.user.role() {
        Role::Owner => "/admin",
        Role::TenantAdmin => "/portal",
        _ => {
            if current.user.tenant_id.map_or(false, |id| id > 0) {
                "/media-library"
            } else {
                "/apply/status"
            }
        }
    }
}

/// Whether the business's domain demands payment at onboarding.
async fn auth_needs_payment(auth: &Auth, app: &Tenant) -> bool {
    let Some(domain) = app
        .apps_enabled
        .as_deref()
        .and_then(|items| items.first().map(String::as_str))
    else {
        return false;
    };
    auth.domain_policies()
        .await
        .unwrap_or_default()
        .iter()
        .any(|p| p.domain.as_deref() == Some(domain) && p.payment_required == Some(true))
}

/// Persist-time validation, returning an `?error=` code on failure.
async fn validate_apply(auth: &Auth, form: &ApplyForm) -> Option<&'static str> {
    if form.business_name.trim().len() < 2 {
        return Some("name");
    }
    if !DOMAINS.iter().any(|(value, _)| *value == form.domain) {
        return Some("domain");
    }
    if form.username.trim().len() < 3 || form.password.len() < 4 {
        return Some("credentials");
    }
    if auth.user_exists(&form.username).await.unwrap_or(false) {
        return Some("username");
    }
    if auth.tenant_exists(&form.business_name).await.unwrap_or(false) {
        return Some("taken");
    }
    None
}

fn apply_error_text(code: &str) -> &'static str {
    match code {
        "name" => "Give the business a name (at least two characters).",
        "domain" => "Pick a service from the list.",
        "credentials" => "Username needs 3+ characters and the password 4+.",
        "username" => "That username is already taken.",
        "taken" => "A business with that name is already on the LAN.",
        _ => "Something went wrong while applying — please try again.",
    }
}

fn domain_badge(app: &Tenant) -> (&'static str, &'static str) {
    match app
        .apps_enabled
        .as_deref()
        .and_then(|items| items.first().map(String::as_str))
    {
        Some("marketplace") => {
            ("Marketplace", "border-sky-700 bg-sky-900/40 text-sky-200")
        }
        Some("serverless") => {
            ("Serverless", "border-violet-800 bg-violet-950/50 text-violet-300")
        }
        _ => ("Media", "border-emerald-900 bg-emerald-950/60 text-emerald-300"),
    }
}

/// Payment pill label + class for the status card.
fn payment_state(
    app: &Tenant,
    payment_required: bool,
) -> (&'static str, &'static str, bool) {
    if !payment_required {
        return ("no payment required", "border-zinc-700 bg-zinc-800 text-zinc-400", false);
    }
    if app.is_paid() {
        return ("paid", "border-emerald-900 bg-emerald-950/60 text-emerald-300", false);
    }
    ("payment pending", "border-amber-800 bg-amber-950/40 text-amber-300", true)
}

/// Status-page title, accent and supporting copy.
fn status_tone(status: &str) -> (&'static str, &'static str, &'static str) {
    match status {
        "active" => (
            "Your business is live",
            "text-emerald-300",
            "The owner approved your application — run it from your portal.",
        ),
        "rejected" => (
            "Application was not approved",
            "text-red-300",
            "The owner declined this application. You can apply again with a new business.",
        ),
        "suspended" => (
            "Business is suspended",
            "text-red-300",
            "The owner suspended this tenant. Contact the platform owner.",
        ),
        _ => (
            "Application is in review",
            "text-sky-300",
            "Businesses queue for the owner's approval before going live.",
        ),
    }
}

/// The card body text and the main action link, if any.
fn status_meta(
    app: &Tenant,
    payment_required: bool,
    current: &CurrentUser,
) -> (String, Option<(&'static str, &'static str)>) {
    if app.status == "active" {
        if current.user.role() == Role::TenantAdmin {
            let console = if app
                .apps_enabled
                .as_deref()
                .is_some_and(|items| items.iter().any(|s| s == "serverless"))
            {
                "Approved — open your Serverless console."
            } else {
                "Approved — your portal is ready."
            };
            return (String::from(console), Some(("/portal", "Open your portal")));
        }
        return (String::from("Approved — your business is live."), None);
    }
    if app.status == "rejected" {
        return (
            String::from("This application was declined by the owner."),
            Some(("/apply", "Apply again")),
        );
    }
    if payment_required && !app.is_paid() {
        return (
            String::from("This domain needs a payment before the owner reviews the application."),
            None,
        );
    }
    if app.is_paid() {
        return (String::from("Payment received — waiting for the owner's review."), None);
    }
    (String::from("Waiting for the owner's review."), None)
}

/// Do-nothing helper naming the tenant for the status card title.
fn _keep(_: &Tenant) {}