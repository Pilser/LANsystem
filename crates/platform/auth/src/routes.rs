//! The login page and the login/logout form handlers.
//!
//! These are platform routes (registered through link-time discovery like any
//! `#[page]`/`#[route]`), rendering fragments inside the shell layout from
//! `apps/lan`. Successful login bounces to `/`; the layout then sees the
//! session and swaps the nav for the signed-in state.

use serde::Deserialize;
use topcoat::{
    Result,
    context::{app_context, Cx},
    router::{content::Form, error::{see_other, SeeOther}, parse_query_params, route},
    view::view,
};

use crate::Auth;

/// The credentials form.
#[derive(Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

/// `?error=1` marks a failed attempt on the page.
#[derive(Deserialize)]
pub struct LoginQuery {
    #[serde(default)]
    pub error: bool,
}

/// `GET /login`: the sign-in page (fragment; shell comes from the layout).
#[route(GET "/login")]
pub async fn login_page(cx: &Cx) -> Result {
    let failed = parse_query_params::<LoginQuery>(cx)
        .map(|query| query.error)
        .unwrap_or(false);

    view! {
        <div class="mx-auto mt-10 max-w-sm">
            <a
                class="mb-4 inline-block text-sm text-sky-400 hover:text-sky-300"
                href="/public"
            >
                "← Back to the directory"
            </a>
            <h1 class="mb-6 text-2xl font-bold">"Sign in"</h1>
            if failed {
                <p class="mb-4 rounded-md border border-red-800 bg-red-950/40 px-3 py-2 text-sm text-red-300">
                    "That username or password did not match."
                </p>
            }
            <form method="post" action="/login" class="flex flex-col gap-4">
                <label class="flex flex-col gap-1 text-sm">
                    "Username"
                    <input
                        name="username" required=""
                       
                        autocomplete="username"
                        class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-sky-400"
                    />
                </label>
                <label class="flex flex-col gap-1 text-sm">
                    "Password"
                    <input
                        type="password" required=""
                        name="password"
                       
                        autocomplete="current-password"
                        class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-sky-400"
                    />
                </label>
                <button
                    class="mt-2 cursor-pointer rounded-md bg-sky-600 px-4 py-2 text-sm font-semibold text-white transition hover:bg-sky-500"
                >
                    "Sign in"
                </button>
            </form>
            <p class="mt-4 text-xs text-zinc-500">
                "Don't have an account? Ask the LAN owner to create one."
            </p>
        </div>
    }
}

/// `POST /login`: verify credentials and start a session. Staff (the owner
/// or a tenant admin) land straight into their console; everyone else goes
/// to the launcher.
#[route(POST "/login")]
pub async fn login(Form(form): Form<LoginForm>, cx: &Cx) -> Result<SeeOther> {
    let auth = app_context::<Auth>(cx);
    if auth.login(cx, &form.username, &form.password).await? {
        let is_staff = auth
            .authenticate(&form.username, &form.password)
            .await?
            .map(|user| user.role().is_at_least(crate::models::Role::TenantAdmin))
            .unwrap_or(false);
        Ok(see_other(if is_staff { "/admin" } else { "/" }))
    } else {
        Ok(see_other("/login?error=1"))
    }
}

/// `POST /logout`: kill the session and its record, bounce home.
#[route(POST "/logout")]
pub async fn logout(cx: &Cx) -> Result<SeeOther> {
    app_context::<Auth>(cx).logout(cx).await?;
    Ok(see_other("/"))
}