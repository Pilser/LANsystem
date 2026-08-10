# LANsystem

Local-area-network app platform: a Topcoat (Rust web) launcher that hosts apps
(e.g. the Media Library) backed by a HelixDB graph + S3 object storage, all in
Docker on one bridge network.

## Layout

- `apps/lan/` — entry binary (launcher + app registry)
- `crates/db/` — HelixDB client (query builder, HTTP transport)
- `crates/storage/` — S3 (MinIO) client for app files
- `crates/platform/core/` — service wiring (`Services::connect`)
- `crates/business-apps/` — apps (media) registered via `AppRegistry`
- `deploy/docker-compose.yml` — runs ONLY the app binary in Docker
- `helix.toml` — helix CLI config (DB instance)
- `docs/ARCHITECTURE.md`, `docs/DEPLOY.md`
- `helix-db/`, `topcoat/` — vendored reference repos (own git history, not tracked)

## Run the stack

```bash
helix start LANsystem                  # DB + MinIO containers (host :6969)
cargo build --release -p lan
docker compose -f deploy/docker-compose.yml up -d   # app on 127.0.0.1:4321
```

After a rebuild only the binary changed, and it's bind-mounted — **never
`--force-recreate`** the app container; just restart it:

```bash
docker compose -f deploy/docker-compose.yml restart lan
```

Recreate only churns apt-get in the entrypoint (`ca-certificates` reinstall,
flaky container DNS) and stalls the app longer. The entrypoint's apt step is
the cause of slow boots; wait for "connected: db + storage + auth ready" in
logs before probing.

Env the app needs: `HELIX_ENDPOINT=http://helix-LANsystem-LANsystem:8080`,
`HELIX_QUERY_PATH=v2/query`, `RUSTFS_ENDPOINT/REGION/ACCESS_KEY/SECRET_KEY/BUCKET`
(creds `minioadmin`/`minioadmin`, bucket `lan`).

## DB facts (learned the hard way)

- The DB runs the standalone image `ghcr.io/helixdb/helixdb:v0.0.3` (`helix.toml`),
  which serves the **v2 contract at `/v2/query`**. The old `enterprise-dev` image
  serves v1 (`/v1/query`) and breaks the app (500). Don't revert.
- The helix CLI's readiness probe can time out on this image (`did not become
  ready`) — the gateway is still up; check `curl :6969/readyz`.
- No `helix compile`/`helix check`. Queries are validated by the running instance:
  `helix query LANsystem -e '…'` or `--file examples/request.json`
  (JSON body in `examples/request.json` is v1-style — for v2 use `query.read.entries`).
- **The v2 write/read AST has no delete mutation — data is append-only.** The only
  clean wipe is `helix stop LANsystem` → disconnect anything off the helix network
  (`docker network disconnect helix-LANsystem-LANsystem-net <name>`) →
  `helix prune LANsystem` → `helix start LANsystem`. `billing-hook` (and the app
  container) pin the network, so prune fails silently partway and leaves the data
  volume if you skip the disconnect. After prune, `docker compose down` before
  `up -d` (compose caches the old network ID); re-attach `billing-hook`.
- Ad-hoc graph reads/writes: `POST :6969/v2/query`. **`returns` lives inside
  `read`/`write` as a sibling of `entries`** (the entry `query` takes no
  `returns`; the old envelope with `returns` inside the entry 400s). Working
  read: `{"request_type":"read","query_name":null,"query":{"read":{"returns":
  ["t"],"entries":[{"query":{"name":"t","root":{"project":{"input":
  {"nodes":{"reference":"all"}},"projections":[{"property":{"source":
  "tenantId","alias":"tenantId"}}]}}}}]}},"parameters":{}}`. Label-scoped
  node reads/writes use `nodes_where` with an `and` predicate on `$label` +
  a property; writes `set` on the matched node **in place** (versions append
  at the storage layer, but a label+id-filtered read returns only the newest
  version). Property names are prefixed (e.g. `tenantBannerKey` not
  `bannerKey`); `$id` is the node id.
- Known gotcha: `NO_TENANT = 0` is stored as real `Some(0)` — always test
  `tenant_id.is_some_and(|id| id > 0)`, not `is_some()`, when deciding whether a
  user belongs to a tenant (login redirects, `/apply/*` guards).

## Agent protocol (AI/test harness)

- `GET /api/agent` — JSON catalog of every action; `POST /api/agent` with
  `{"action":"...","params":{...}}` → `{"ok":true,"result":...}`.
- Auth: session cookie (uses the human routes' role guards) or
  `x-agent-key: $LAN_AGENT_KEY` (acts as platform owner; set in
  deploy/docker-compose.yml, default `local-dev-agent-key`).
- Actions call the same `db` models and `Auth` service methods as routes —
  never ad-hoc — so agent bugs == human bugs; use it to reproduce/fix.
- Shop actions mirror the web CRUD: `shop.product.list/create/update/
  setVisibility/delete` (delete also removes stored photos like the web route).

## Cross-app wiring (separation facts)

- One binary, one router (`apps/lan`), apps discovered at link time via
  topcoat's `inventory` — `#[route]`/`#[page]` work from ANY crate in the
  binary, so shared platform routes can live outside business apps.
- **`GET /blob/{encoded}` lives in `platform-core` (`src/blob.rs`)** and serves
  every object prefix (`shop/`, `media/`, `profile/`). It is NOT a business
  app's route; if a page 404s on `/blob/…`, the platform-core mod is missing.
- Cross-app deps today: `media-app` → `marketplace-app` (space page embeds
  products + buy flow posts to marketplace's route), `admin-app` →
  marketplace+media (portal pages render their models/`product_manage_card`).
  `Tenant` profile-photo keys live in `platform-auth` (not an app); upload/
  remove routes in admin-app; display in media/admin.

## Git hygiene

`.helix/`, `target/`, `helix-db/`, `topcoat/` are gitignored. Never `git add -f`
them; the vendored repos keep their own history.

## Notes

- Topcoat CLI is optional (`topcoat dev`, `topcoat fmt`); not needed to build/run.
- Rust 1.97+ stable per `rust-toolchain.toml`; workspace lints forbid `unsafe`,
  warn on `unwrap`/`expect`/`todo`/`dbg!`.
