# LANsystem — Architecture & Workspace Plan

> Foundational doc. Status: **decided**. Apps are compile-time feature plugins
> (Option A below); tenancy + the control plane are decided in §4.

## 1. Vision

A privately-hosted, full-stack system that lives on a local WiFi network. It runs several
apps off one server process and one database:

- **Media server** — films, songs, downloads
- **Marketplace** (virtual)
- **News**
- **Messaging** between users
- *(expandable)* ordering, delivery, and other domain apps

Each app is a **domain**: one server and one database, yet shared by many **tenants**
(businesses) that each run their own users with isolated data — all governed by the
owner through a built-in **Control console** (see §4).

Principle: **one feature = one crate**, files inside each crate stay small and
single-purpose, so the system stays manageable no matter how many apps are added.

## 2. Stack

| Layer | Choice | Why |
|---|---|---|
| Runtime | `tokio` | async foundation |
| HTTP | `hyper` | fast, correct HTTP (used by Topcoat under the hood) |
| Web framework | `topcoat` (tokio-rs) | full-stack: `view!` HTML, `$(...)` Rust→JS reactivity, `#[page]`/`#[shard]`/`#[procedure]`, no WASM |
| DB | `helixdb` | graph/vector store; local instance via `helix-db` SDK |
| Object store | `rustfs` | S3-compatible, Apache-2.0; serves media/binary blobs |
| Storage client | `aws-sdk-s3` | official AWS SDK, documented RustFS client; presigned URLs |
| Auth | `topcoat-session` + argon2 | local-first accounts/sessions in HelixDB; OIDC later behind one `platform/auth` trait |

Notes:
- Topcoat is v0.x and explicitly experimental — expect breaking changes. Acceptable for a
  private LAN system.
- HelixDB core is AGPL — fine for a private self-hosted server; not for redistributing a
  product that embeds it.
- RustFS Apache-2.0, Topcoat MIT — clean.

## 3. Workspace topology

```text
LANsystem/
├── Cargo.toml                      # [workspace] members + [workspace.dependencies] pins
├── rust-toolchain.toml
├── Topcoat.toml
├── README.md
├── docs/
│   └── ARCHITECTURE.md             # this file; future guides per crate live next to code
│
├── crates/
│   ├── db/                         # standalone HelixDB client — usable by anyone
│   ├── storage /                   # standalone RustFS/S3 client — usable by anyone
│   ├── platform/                    # shared underpinning
│   │   ├── core/                    #   glue: AppContext bootstrap, errors (topcoat-aware)
│   │   ├── auth/                    #   login/session (cross-cutting)
│   │   └── messaging/               #   chat (cross-cutting comm layer)
│   ├── ui/                          # shared Tailwind components
│   └── business-apps/               # ★ domain apps (see §6) + §4 console
│       ├── app-contract/            #   the "BusinessApp" plugin trait + registry
│       ├── admin/                   #   ★ owner console: applications, tenants, users, domain policies
│       ├── media/                   #   media server: films, songs, downloads
│       ├── marketplace/             #   virtual marketplace
│       ├── onboarding/              #   ★ public application flow: apply, (pay), status
│       ├── news/                    #   news feed
│       ├── ordering/                #   (future) ordering service
│       └── delivery/                #   (future) delivery service
│
└── apps/
    └── lan/                         # the only binary — wiring only, no business logic
```

Cargo supports nested workspace members, so `crates/business-apps/*` are independent
crates that can be enabled/disabled individually via features.

## 4. Tenancy & control plane (decided)

The system is multi-user and multi-tenant. Each app is a **domain** shared by
**tenants** (businesses), and each tenant runs its **own users**:

```text
owner — the platform admin (you)
├── platform console (/admin): applications, tenants, users, domain policies
├── media-library domain (one server, one database)
│    └── tenant "Rashid's Cinema"   → first business admin + members/clients
└── marketplace domain
     └── tenant "Harbor Blue Moon"  → first business admin
```

### Roles

| Role | Scope | Power |
|---|---|---|
| `owner` | platform | approve/reject applications, set quotas, suspend tenants, configure domain policies |
| `tenant_admin` | one tenant | manage that tenant's users (activate/deactivate, roles) and its content, via the **tenant portal** |
| `member` | one tenant | uses the tenant's services |
| `applicant` | — | a signed-up account with no tenant yet; sees only its **application status page** until approved |

The owner never creates a tenant on behalf of a business. Every tenant is born
from a **public application** (§4a): the first person of the business signs up,
applies for a domain, and the owner approves. On approval the applicant becomes
`tenant_admin` of the new tenant. Neither owner nor tenant admin runs the other's
UI: `/admin` is the owner console only, `/portal` is the tenant admin portal.

### Decisions

- **Auth is local-first**: argon2 password hashes + sessions in HelixDB (`topcoat-session`),
  behind one `platform/auth` trait so OIDC can be plugged in later. The first account is
  a seeded `owner` (from env).
- **Enablement is data, not build config.** Apps stay compile-time crates (§6), but the
  per-tenant on/off list, quotas, and status live on the `Tenant` node
  (`appsEnabled`, `storageQuotaBytes`, `storageUsedBytes`, `status`).
- **Tenant lifecycle drives itself through statuses.** `pending` → `active` /
  `rejected` (later `suspended`). The public feed only lists `active && isPublic`,
  so `pending` businesses are invisible until approved and published by their admin.
- **The owner console is a normal business app** — `crates/business-apps/admin`,
  display name "Console", owner role only: applications to approve, tenants
  (quota/suspend), users, and **domain policies** (see §4a). The tenant portal
  (`/portal`) is the same app's tenant-admin view without the owner controls.
- **Isolation is query-level.** Every tenant-owned node carries `tenantId`; every domain
  query filters it first (§11). One server, one database, hard per-tenant scoping.
- **Billing is a plug-in, never core (the MikroTik pattern).** The engine is mechanism;
  policy (subscriptions, pay-per-view) lives **outside** it. Core defines an
  `Authorizer` seam — `authorize(user, tenant, app, resource) -> allowed?` — and ships
  two providers: `Allow` (local policy only) and a `Webhook` provider that calls an
  external billing/AAA endpoint (MikroTik's RADIUS). A tenant enabling `billingEnabled`
  sets its own `authWebhookUrl`; the external system decides allow/deny + limits
  (e.g. bitrate), so Stripe/crypto/whatever attaches *next to* the server, never *in*
  it.
- **Session control is exposed like RouterOS API**: the platform can force-drop a
  user's sessions (and short-lived presigns) mid-watch — a subscription expiry stops
  the stream instantly with no new code in the app.
- **Phase 5 (later):** host → tenant mapping, so a tenant can get its own subdomain.

### 4a. Domain onboarding: how businesses enter (decided)

Businesses self-sign from the **public directory** (`/public` → "Create a
business"), not through the owner. The flow lives in `crates/business-apps/onboarding`:

```text
/public "Create a business" button
        │  POST /apply  (name, domain, applicant's email + password)
        ▼
payment gate? (domain policy: paymentRequired)
        ├─ yes ─► POST domain.paymentWebhookUrl  →  { "paid": true|false }
        │              paid:false ──► application status "rejected (payment)", applicant may retry
        ▼
Tenant{ status:"pending" } + applicant User (role member, no tenant)
        ▼
/app (status page)  ←─ applicant's only page until approved
        ▼
owner console: Approve (set quota by using existing quota field) / Reject
        ▼
status:"active"  + applicant becomes tenant_admin of the tenant
```

- **A business is born only by approval.** Pending tenants cost nothing, are
  invisible on `/public`, and enforce no domain app rights, so quotas only
  matter when the owner resources the tenant.
- **`TenantApplication`-free model:** an application *is* a `Tenant` node with
  `status: "pending"` (plus `paymentStatus: "none"|"paid"|"declined"` and the
  applicant's `userId`). No second node type; approval flips a fixture into a
  working tenant.
- **Payment gate is per-domain policy.** The owner configures on the console:
  `DomainPolicy{ domain, paymentRequired, paymentWebhookUrl }`. Only *selected*
  domains force payment at apply time. Webhook contract details in §13.
  Approval is deliberately decoupled from money: "pay first, we approve after" —
  the webhook says *paid*, the owner still decides *approved*.
- **Portal split.** `tenant_admin` lands on `/portal` (their tenant's users and
  content + billing toggle), never on `/admin`. `owner` only sees `/admin`.
  Members without an approved tenant land on their application status page.
- **Existing demo businesses (Rashid's Cinema, Harbor Blue Moon, …) were wiped**
  when this flow landed and re-created through the apply → approve path, so the
  public feed always traces back to a real application.

### Enforcement order (in `platform/core`)

session → user active? → tenant active? → app enabled for tenant? → role? →
authorizer (webhook or allow) → quota (on writes). Applied by router middleware and
reusable by domain queries; the launcher grid renders only the apps the caller's
tenant has enabled. Every check is a function on the auth seam, so a business app
never sees a payment library.

### Build order (phases)

1. **Identity** — sessions/roles schema, seed `owner`; `User` gains `role`/`status`.
2. **Tenants + policy** — `Tenant` node, per-tenant enable, suspend blocks everything.
3. **Control console UI (owner + tenant views)** — ships with phases 1–3 (decided).
4. **Media Library tenant-scoped** — `tenantId` on `MediaItem`, quota checks at upload.
5. **To-many domains** — host header → tenant.

## 5. Layered dependency rules (keep it manageable)

1. **Infrastructure crates (`db`, `storage`)** depend on nothing local. Any code – or a
   future tool outside this repo – can consume them directly.
2. **`platform/core`** is the only crate aware of Topcoat's `AppContext` plus both
   infra clients. It constructs the clients once and registers them as Topcoat
   app-context.
3. **Dependency direction is one-way:** `apps/lan → business-apps → platform → crates`.
   No cycles. A business app may reach `db`/`auth`/`messaging`/`ui` directly through
   `core` seams.
4. **Business apps never import each other.** Cross-app behavior moves down to `platform`.
5. **Version pinning lives only in root `Cargo.toml` ([workspace.dependencies])`**, so
   every crate's manifest stays a few lines.
6. Everything that should be discoverable (`#[page]`, `#[shard]`, `#[procedure]`, fonts,
   assets) is registered via Topcoat link-time `discover()` — nothing is wired by hand.

## 6. Decision — apps are compile-time feature plugins

**Chosen: Option A — compile-time plugins.** Each business app is a crate under
`crates/business-apps/`; it is installed/uninstalled by editing the `[features]` list in
`apps/lan/Cargo.toml`. Topcoat's link-time `discover()` registers everything
(`#[page]`, `#[shard]`, `#[procedure]`) with no manual wiring, and the home-screen
launcher enumerates installed apps via the `BusinessApp` contract.

Target: add a new domain (ordering, delivery, maps of crafts?) without touching existing
code, and let the system enumerate installed apps automatically.

Three realistic options in Rust:

| Option | Mechanism | Runtime action | Pros | Cons |
|---|---|---|---|---|
| **A. Compile-time plugins** | each app is a crate; enable/disable via Cargo `features`; just `topcoat`'s linktime `discover()` registers its routes/components | rebuild the server | type-safe, zero runtime cost, simple, no unstable ABI | adding/removing an app = edit `Cargo.toml` + recompile; binaries re-linked |
| **B. Umbrella bundle crate** | one `business-apps` crate that owns all app subcrates as one public API | none | fewest crates | less granular, weaker isolation |
| **C. Runtime plugins (dlopen)** | load dynamic libs (`.so`) at runtime via `abi_stable` | drop a file on the disk | "true" plugins, hot-swap | no stable Rust ABI, heavy, brittle, overkill for a LAN server hosting one HTTP process |

**Recommended: A** — it gives a real "install an app" feel (a line in the features list),
with full type safety. The headline still reads as "plugins" from the outside because a
`BusinessApp` trait (`app-contract`) gives every plugin the same surface: id, name,
   icon, homepage route, nav entry, installer metadata.

### The `BusinessApp` contract

```rust
// crates/business-apps/app-contract/src/lib.rs
pub trait BusinessApp {
    const ID: &'static str;         // "media-library"
    const NAME: &'static str;       // "Media Library"
    const ICON: &'static str;       // iconify set + glyph
    const HOME: &'static str;       // "/media-library"
    const GROUP: &'static str;      // "Entertainment"

    fn register(router: RouterBuilder) -> RouterBuilder;
    fn nav_item() -> impl Into<Attribute>;
}

// launcher page reads this to build the home screen / app grid
pub fn installed_apps() -> Vec<InstalledApp>;
```

Every business app implements `BusinessApp` for an empty marker type in its crate and
registers its true pages via standard `#[page]`/`#[route]` attributes. The launcher
captures `installed_apps()` at the top of the `home` page, filtered by the caller's
tenant enablement and role (§4).

Each user-visible app therefore becomes a one-`crate` "plugin": add the `features =
["media"]` line (or drop a new crate under `business-apps/`) and it appears in the
launcher with its own nav entry — nothing else changes.

## 7. Standard file layout (of one business-app crate)

Small, one-concern files:

```text
crates/business-apps/media/
├── Cargo.toml          # deps: topcoat, platform/core, db, storage, ui, app-contract
└── src/
    ├── lib.rs          # `pub mod`s + BusinessApp impl (~20 lines)
    ├── routes.rs       # #[page("/media-library")] #[page("/media-library/{id}")] — explicit paths
    ├── procedures.rs   # #[procedure] upload (presign to RustFS), stream-control
    ├── components.rs   # view! components: player, grid, upload-zone
    ├── shards.rs       # #[shard] live library filters
    ├── models.rs       # HelixDB types + query builders
    └── tests.rs
```

Adding a feature ←→ adding a crate:

1. create the crate (route/procedure/component files)
2. add a workspace member + a `[features]` entry in `apps/lan`
3. implement `BusinessApp` for auto launcher entry

That's the whole "install".

## 8. Bootstrap (apps/lan)

The binary is pure wiring:

```rust
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let router = topcoat::router::module_router!()
        .discover()                                   // pages, shards, procedures, fonts
        .app_context(platform::bootstrap::bootstrap().await?)   // Db + Storage + Auth
        .assets(AssetBundle::load()?)
        .build();
    topcoat::start(router).await
}
```

## 9. Storage keys

`storage` mirrors object keys:

```text
media/<tenantId>/<user>/<album>/<track>.mp3
chat/<tenantId>/<sender>/<msg_id>.jpg
news/<tenantId>/<year>/<slug>.pdf
```
object keys binary blobs; everything about the objects themselves lives in the
graph in HelixDB.

## 10. Licensing edge

- `db` crate wraps: `helix-db` (AGPL) — keep **every other crate** free of it so the
  app-level code keeps the MIT/Apache hygiene.
- `storage` crate wraps: `aws-sdk-s3` + RustFS (Apache-2.0).

## 11. Shared schema conventions (HelixDB glossary)

HelixDB is a labeled property graph. Every crate writes to the **same one instance**,
so all crates must agree on labels, edge names, and property names. This glossary is the
single source of truth — baked into `crates/db/src/schema.rs` as constants so queries
cannot drift by typos. **Always reuse these names; never invent new ones.**

### Node labels (PascalCase, nouns)

| Label | Meaning |
|---|---|
| `User` | a person on the LAN |
| `Tenant` | a business on a domain; carries app/quotas/status (§4) |
| `MediaItem` | a film, song, or downloadable file |
| `Album` | a group of MediaItem nodes |
| `Listing` | a marketplace listing |
| `ListingReview` | review of a listing |
| `Article` | a news story |
| `Message` | a chat message |

### Edge labels (SCREAMING_SNAKE, verbs)

| Edge | From → To | Meaning |
|---|---|---|
| `FOLLOWS` | User → User | relationship |
| `MANAGES` | Tenant → User | tenant admins (edge `role` = "admin") |
| `MEMBER_OF` | User → Tenant | tenant membership |
| `BELONGS_TO` | MediaItem/Album → User | ownership |
| `RELEASED` | Album → MediaItem | album contains media |
| `SENT_TO` | Message → Message | chat thread (message linking) |
| `AUTHORED_BY` | Article/MediaItem → User | creator |
| `POSTED_BY` | Message/Listing/Article → User | creator |
| `PLACED_FOR` | Listing → User | marketplace listing |

### Node property conventions (camelCase)

| Property | Applies to | Convention |
|---|---|---|
| `tenantId` | Tenant + all tenant-owned nodes | the tenant scope; domain queries filter on it first |
| `role` | User | `owner` / `tenant_admin` / `member` |
| `status` | Tenant, User | `active` / `suspended` (blanket stop) |
| `appsEnabled` | Tenant | list of app IDs this tenant may use |
| `billingEnabled` | Tenant | when true, the tenant's `authWebhookUrl` is consulted for access |
| `authWebhookUrl` | Tenant | external AAA/billing endpoint (MikroTik-style RADIUS) |
| `storageQuotaBytes` / `storageUsedBytes` | Tenant | quota accounting |
| `userId` | User | unique ID generated once, used as query anchor |
| `name`, `displayName` | User/MediaItem | display name/title |
| `email` | User | unique |
| `avatarKey` | User / MediaItem | object key in RustFS |
| `createdAt`, `updatedAt` | all | DateTime UTC |
| `deletedAt` | all | nullable soft-delete marker |
| `title`, `durationSec`, `streamKey` | MediaItem | media metadata |
| `releaseYear`, `description` | MediaItem/Album | description |
| `price`, `currency` | Listing | sale info |
| `status` | Listing/Article | draft/published/sold, etc |
| `body`, `text` | Message/Article | content |
| `sentAt`, `readAt` | Message | message state |

### Edge property conventions

Relationship-specific data goes on the edge: `since`, `quantity`, `role`, `sentAt`.
Otherwise leave edge properties minimal.

### Key rules

1. Use `userId`/`mediaItemId`-style unique IDs as the primary anchor — never broad label scan.
2. Indexed properties stay at top level (nested `config.metadata.score` cannot be indexed).
3. Binary bytes never go into Helix; store only object keys; the bytes live in RustFS.
4. One `User` node unique index on `userId`.
5. Soft-delete instead of hard delete; filter `deletedAt` = null first.
6. Every tenant-owned node carries `tenantId`; queries anchor on it before anything else (§4).

### Where this lives

- `crates/db/src/schema.rs` — the constants (single source of truth).
- Reference at `docs/ARCHITECTURE.md` §11–§12 and `.agents/skills/helix-query-rust/SKILL.md`.

## 12. DB & storage interaction (query ownership)

**Rule: the crate that owns a domain owns its queries.** Feature/app crates define and
keep their own HelixDB queries and storage access — nothing is centralized in a big
keep their own HelixDB queries and storage access — nothing is centralized in a big
"repository" layer. `db` and `storage` only provide transport + shared conventions.

### What each layer provides

- **`crates/db`** — a thin wrapper around `helix_db::Client` (connection, base URL,
  API key, error types) + shared schema conventions (label/property casing), plus
  re-exported DSL prelude. It does **not** know about media/marketplace/news.
- **`crates/storage`** — the `Storage` trait (put/get/delete/presign) over `aws-sdk-s3`.
  It does **not** know about buckets' business meaning — key conventions live in
  `keys.rs` but are data, not app code.
- **Each feature crate** — its own `models.rs` (HelixDB query builders with `#[query]`),
  and uses `db` + `storage` clients (retrieved via Topcoat app-context) to execute them.

### Why per-crate queries are the right call here

- HelixDB queries are **pure Rust values** (DSL builders like `read_batch()` /
  `write_batch()`), not SQL strings. A query is a value that a function returns; the
  function can live anywhere, and execution goes through one `Client`.
- Query code is schema-coupled — keeping it next to the domain model means one file,
  one owner, no cross-crate "query modules" to keep in sync.
- New features add a crate with its own `models.rs`; existing crates are never edited.

### Shared conventions every crate follows (per `helix-query-rust` skill)

1. Anchor narrow: node ID → unique property → indexed equality → scoped label scan.
   Never start from a broad label scan when an indexed key exists.
2. Reuse existing label/edge/property casing — check the repo's existing queries
   first (e.g. `userId`, `createdAt`, `FOLLOWS`, `BELONGS_TO`).
3. Filter early (tenant/scope/status before traversal).
4. Keep output shape intentional: `project(...)` for service-facing shapes.
5. Use `write_batch()` for any mutation, `read_batch()` otherwise; prefer
   `.should_await_durability(true)` on writes.
6. Vector/text search: keep tenant scope, return embeddings only when asked.

### Client execution path

```rust
// apps/lan bootstrap registers the client once as Topcoat app-context:
let client = db::connect(&config)?;                 // helix_db::Client
router.app_context(client).app_context(storage_handle);

// media crate (owned query):
#[query]
pub fn list_songs_by_artist(artist_id: u64) -> helix_db::ReadBatch {
    read_batch()
        .var_as(
            "songs",
            g().n(artist_id).out(Some("RELEASED")).project(vec![
                PropertyProjection::new("$id"),
                PropertyProjection::new("title"),
            ]),
        )
        .returning(["songs"])
}

// media page handler:
let db = app_context::<db::Client>(cx)?;
let songs = db.query(list_songs_by_artist(id)).send().await?;
```

### Storage interaction path

- Browser uploads/downloads use **presigned URLs** from `storage::presign` — media
  bytes never pass through the Topcoat process.
- Only metadata (object key, size, sha256, mimetype) goes into HelixDB as node
  properties, written by the owning crate's query.

### Skills vendored into this repo

The official HelixDB agent skills are checked into `.agents/skills/` at the project
root (from `github.com/HelixDB/skills`):

- `helix-query-rust` — authoring rules, builder catalog, canonical examples
- `helix-query-json-dynamic` — inline dynamic JSON payloads for `POST /v1/query`
- `helix-query-optimize` — index planning and query optimization
- `helix-cli` — managing local instances (`helix start dev`, queries bundle)
- `helix-memory-system` — long-term memory graph patterns (optional)

Plus the upstream repo is vendored at `helix-db/` (shallow clone) for reference.
