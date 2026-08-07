# LANsystem — Architecture & Workspace Plan

> Foundational doc. Status: **decided**. Apps are compile-time feature plugins
> (Option A below).

## 1. Vision

A privately-hosted, full-stack system that lives on a local WiFi network. It runs several
apps off one server process and one database:

- **Media server** — films, songs, downloads
- **Marketplace** (virtual)
- **News**
- **Messaging** between users
- *(expandable)* ordering, delivery, and other domain apps

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
│   └── business-apps/               # ★ plugins (the open decision — see §5)
│       ├── app-contract/            #   the "BusinessApp" plugin trait + registry
│       ├── media/                   #   media server: films, songs, downloads
│       ├── marketplace/             #   virtual marketplace
│       ├── news/                    #   news feed
│       ├── ordering/                #   (future) ordering service
│       └── delivery/                #   (future) delivery service
│
└── apps/
    └── lan/                         # the only binary — wiring only, no business logic
```

Cargo supports nested workspace members, so `crates/business-apps/*` are independent
crates that can be enabled/disabled individually via features.

## 4. Layered dependency rules (keep it manageable)

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

## 5. Decision — apps are compile-time feature plugins

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
captures `installed_apps()` at the top of the `home` page.

Each user-visible app therefore becomes a one-`crate` "plugin": add the `features =
["media"]` line (or drop a new crate under `business-apps/`) and it appears in the
launcher with its own nav entry — nothing else changes.

## 6. Standard file layout of one business-app crate

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

## 7. Bootstrap (apps/lan)

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

## 8. Storage keys

`storage` mirrors object keys:

```text
media/<user>/<album>/<track>.mp3
chat/<sender>/<msg_id>.jpg
news/<year>/<slug>.pdf
```
object keys binary blobs; everything about the objects themselves lives in the
graph in HelixDB.

## 9. Licensing edge

- `db` crate wraps: `helix-db` (AGPL) — keep **every other crate** free of it so the
  app-level code keeps the MIT/Apache hygiene.
- `storage` crate wraps: `aws-sdk-s3` + RustFS (Apache-2.0).

## 10. Shared schema conventions (HelixDB glossary)

HelixDB is a labeled property graph. Every crate writes to the **same one instance**,
so all crates must agree on labels, edge names, and property names. This glossary is the
single source of truth — baked into `crates/db/src/schema.rs` as constants so queries
cannot drift by typos. **Always reuse these names; never invent new ones.**

### Node labels (PascalCase, nouns)

| Label | Meaning |
|---|---|
| `User` | a person on the LAN |
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
| `BELONGS_TO` | MediaItem/Album → User | ownership |
| `RELEASED` | Album → MediaItem | album contains media |
| `SENT_TO` | Message → Message | chat thread (message linking) |
| `AUTHORED_BY` | Article/MediaItem → User | creator |
| `POSTED_BY` | Message/Listing/Article → User | creator |
| `PLACED_FOR` | Listing → User | marketplace listing |

### Node property conventions (camelCase)

| Property | Applies to | Convention |
|---|---|---|
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

1. Use `userId`/`mediaItemId`-style unique ID keys as primary anchor — never broad label scan.
2. Indexed properties stay at top level (nested `config.metadata.score` cannot be indexed).
3. Binary bytes never go into Helix; store only object keys; bytes live in RustFS.
4. One `User node` unique index on `userId`.
5. Soft-delete instead of hard delete; filter `deletedAt` = null first.

### Where this lives

- `crates/db/src/schema.rs` — the constants (single source of truth).
- Reference at `docs/ARCHITECTURE.md` §10-§11 and `.agents/skills/helix-query-rust/SKILL.md`.

## 11. DB & storage interaction (query ownership)

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
