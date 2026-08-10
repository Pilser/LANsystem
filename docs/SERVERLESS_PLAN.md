# Serverless Apps — Plan

> Status: **proposed** — written before any code. Reference: the `whatsapp4agents/`
> repo's `zerowrapper/` (serverless boards on turso) and the `turso/` engine
> (SQLite re-implementation in Rust; the only shipped storage backend is a local
> file, behind the `DatabaseStorage` trait).

## 1. Goal

Give every tenant the ability to **spawn serverless apps ("boards")**: a per-tenant,
SQL-backed record store exposed over an HTTP API with scoped API keys, webhooks and
a portal management UI — deployed and managed from inside LANsystem like any other
business app. The board's *rows* live in a turso DB file on the container's volume;
the board's *materials* (photos, uploads, backups) live in the existing MinIO object
storage.

## 2. Data ownership split

| What | Where | Why |
|---|---|---|
| Board catalog, owner keys, key metadata | **HelixDB** (`ServerlessApp` label, scoped by `tenantId`) | Portal lists boards like products; graph = single source of truth for *what exists*; append-only audit |
| Records, sessions, hook rows, filters, audit rows | **turso file per tenant** on the container volume | Real SQL (`UPDATE`/`DELETE`/JSON path/`GROUP BY`) is honestly better than the graph for a row store |
| Board photos/docs/uploads, DB snapshots | **MinIO** under `sl/<tenant_id>/…` and `db-backups/<tenant_id>/…` | Object storage for materials (consistent with `shop/`, `media/`, `profile/`) |
| API keys (reader/writer/admin, salted, revocable) | turso `srv_keys` table inside the tenant's DB | Mirrors `zerowrapper/zw-db/src/keys.rs` exactly |

## 3. Filesystem layout on the container

```
/data/dbs/                            ← new named volume, mounted in deploy-lan-1
├── <tenant_id>/                      ← one dir per tenant (sanitized id, 0700)
│   ├── boards/
│   │   ├── <board_id>/board.db       ← turso file for the board
│   │   └── <board_id>/meta.toml      ← board id, owner key, created_at
└── …
```

- Files created **lazily** on first access (open → migration `CREATE TABLE IF NOT EXISTS`).
- `deploy/docker-compose.yml` gains a named volume (e.g. `lan-data:/data`) so DB files
  survive `docker compose down/up`. DB files must NEVER live in the container layer.
- `zerowrapper`-style `sanitize_key` on every path segment; no `..` traversal.

## 4. Isolation (4 layers)

1. **Filesystem**: per-tenant directory, `0700`, sanitized ids.
2. **DB**: one turso file per tenant → cross-tenant SQL is structurally impossible.
3. **Connections**: `BoardStore` keyed by `(tenant_id, board_id)`; one async
   connection per key (`turso::Builder::new_local(path).build().await?.connect()`);
   LRU eviction + idle sweeper closes idle connections (files persist).
4. **API**: board ids live under the tenant in HelixDB; the portal/agent guards are
   the same owner/tenant-admin checks as products. Record access never crosses the
   tenant boundary because the *board id* itself is tenant-scoped.

## 5. API surface (mirrors zerowrapper `srv_*`)

### Records
- `POST /api/srv/<board>/records` — insert JSON payload (schema-validated; upsert by `unique_key`; tags `writer` from the key)
- `POST /api/srv/<board>/records/<seq>/set` — replace payload
- `POST /api/srv/<board>/records/<seq>/patch` — JSON-path patch ops
- `GET /api/srv/<board>/records` — list (limit/before/offset)
- `GET /api/srv/<board>/records/<seq>` — one
- `GET /api/srv/<board>/records/after/<seq>` — streaming/pagination
- `DELETE /api/srv/<board>/records/<seq>` — delete one (turso: real DELETE, unlike the graph)
- `POST /api/srv/<board>/query` — filter DSL (reuse zerowrapper `filters.rs`: `Agg/FilterCond/Op`)

### Keys (host middleware, shared by all routes)
- `POST /api/srv/<board>/keys` — issue `reader | writer | admin` key (owner/admin key required); secret shown once, stored salted
- `POST /api/srv/<board>/keys/<bucket>/revoke`
- Requests send `Authorization: Bearer <app_key>`; `board_id` is derived from the key, never trusted from the URL
- Legacy fallback: the board's `owner_key` acts as an unscoped `admin` key

### Hooks
- `POST /api/srv/<board>/hooks` / `DELETE` / `GET` — webhooks with optional secret; deliveries from a background task (the one piece needing a worker — see §7)

## 6. Portal & agent

- **Portal** (`/portal/serverless`): board list + create board (title, schema, `public_reads`,
  `unique_key`) with `product_manage_card`-style cards; per-board: keys table (issue/revoke),
  records table, hooks list, snapshot/backup buttons.
- **Agent actions** (`agent-app`): `srv.board.list/create/update/delete`,
  `srv.record.list/insert/set/patch/delete`, `srv.key.issue/revoke`, `srv.hook.register/remove/list`
  — all calling the same `BoardStore` + `Auth` methods as the routes (agent bugs == human bugs).

## 7. Lifecycle & operations

- **Spawn**: first access → `BoardStore::open` (mkdir, migrate, ready).
- **Quota**: `PRAGMA max_page_count` caps the DB file per tenant quota.
- **Backup**: periodic snapshot of `board.db` → MinIO `db-backups/<tenant>/<board>/<ts>.db`,
  keep N versions (uploaded via the existing storage client).
- **Eviction**: idle-close connections after N minutes (files remain on disk).
- **Teardown**: delete board → Helix tombstone + remove file + delete MinIO objects
  (same pattern as product delete).
- **Background worker**: hook delivery + backup sweeper run as tokio tasks in the single
  process (there is no scheduler in HelixDB).

## 8. Build steps (in order)

1. `deploy/docker-compose.yml`: named volume `lan-data:/data`.
2. `crates/serverless-core/` — traits + types only (`SrvApp`, `SrvRecord`, `SrvApiKey`,
   `SrvHook`, `BoardStore` trait). Plugins depend only on this registry crate
   (zerowrapper's `zw-core` lesson: no plugin→plugin deps).
3. `crates/business-apps/serverless/` — turso-backed `BoardStore`, `/api/srv/*` routes,
   portal page, agent actions.
4. Wire turso as a path dependency (`../turso/bindings/rust`) in the workspace.
5. Backup sweeper + `sl/` blob upload routes.
6. Register `serverless_app::Serverless` in `apps/lan/src/main.rs`.

## 9. Open questions

- One file per **tenant** vs per **board**: per-board = more file handles but a hot
  board never blocks another tenant's queries; per-tenant = fewer handles, coarser lock
  contention. Default: per-board (matches zerowrapper), revisit if FD count matters.
- Where key hashes live: HelixDB (portal-readable, cross-process) vs turso `srv_keys`
  (same file as records). Default: turso (keys are board data), owner-key metadata in Helix.
- Whether `schema_json` validation stays JSON-path based (zerowrapper) or becomes
  Helix-style property constraints (stricter, more work). Default: JSON-path.
