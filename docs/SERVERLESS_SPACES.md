# Serverless Spaces — Implementation Brief

> Status: **current spec**. Mirrors `marketplace` exactly, with ONE difference:
> instead of a buyable product, a **space** is a deployable frontend app. The
> user uploads a built frontend (`dist/`) plus media (icon, banner, gallery
> photos) during development, then **deploys** — after which the public
> gallery card of that space links directly to the running app, which renders
> because it's a real frontend served by LANsystem.
>
> Follow this file until the feature is finished. Second spec:
> `SERVERLESS_MCP.md` (the public MCP tool over the same spaces).

## 1. What a space is

A space is the existing serverless **board** (`ServerlessApp` graph node +
turso DB file on `/data/dbs/`) extended with marketplace-style listing media
and a **deployed frontend**:

| Data | Where | Like marketplace |
|---|---|---|
| `boardId`, title, schema, unique key, owner key hash | HelixDB `ServerlessApp` | `productId`, title… |
| `boardDescription` | HelixDB | `description` |
| `boardIconKey` (square icon) | MinIO `srv/{board}/icon.{ext}` | `productImageKey` |
| `boardBannerKey` (wide banner) | MinIO `srv/{board}/banner.{ext}` | cover |
| `boardGalleryKeys` (JSON array) | MinIO `srv/{board}/gallery-N.{ext}` | `galleryKeys` |
| `boardPublicReads` → becomes `isPublic` (bool!) | HelixDB | `isPublic` |
| Dev/staging build files | MinIO `srv/{board}/staging/…` | (new) |
| Deployed build files | MinIO `srv/{board}/dist/…` | (new) |
| `boardDeployedAt` (rev timestamp) | HelixDB | (new) |

**Lesson from the smoke test:** the graph stores a real JSON boolean when the
`#[query]` param is a Rust `bool` (marketplace's `set_product_public` proves
it — `Predicate::eq(IS_PUBLIC, true)` filters fine, `Option<bool>` rows
deserialize fine). Do NOT round-trip bools as `i64` like the original
serverless code did. Revert `boardPublicReads` to a bool param/field.

## 2. Object store layout (MinIO, prefix `srv/`)

```
srv/{board_id}/icon.{ext}          ← icon, upload replaces in place
srv/{board_id}/banner.{ext}        ← banner, replaces in place
srv/{board_id}/gallery-N.{ext}     ← gallery photos (0,1,2…)
srv/{board_id}/staging/<path>      ← dev uploads (staff-only preview)
srv/{board_id}/dist/<path>         ← deployed build (public serve)
```

`/blob/{encoded}` gains `srv/` in its prefix whitelist
(`crates/platform/core/src/blob.rs`) so media renders like `shop/` photos do.

## 3. Serving the deployed app

- `GET /s/{board_id}/{path?}` — public. Serves `srv/{board_id}/dist/{path}`,
  `index.html` when `path` is empty or missing on MinIO (SPA fallback → the
  stored `index.html`). Content type by extension (html, js, mjs, css, json,
  map, png, jpg, svg, webp, gif, ico, txt, woff2…). `cache-control` per asset.
  Only for spaces with `isPublic` AND `boardDeployedAt` set, else 404.
- `GET /sl/{board_id}/preview/{path?}` — staff (owner / that board's tenant
  admin), serves `staging/` with the same resolver so a builder can test the
  build **during development** before deploying.
- `POST /sl/{board_id}/deploy` — staff: copy `staging/` → `dist/` (list +
  read + put, no S3 copy in the storage client), set `boardDeployedAt`, and
  optionally publish (`isPublic`) — the deploy form has a checkbox.

## 4. Uploads (during development)

- `POST /sl/{board_id}/files` — multipart, field `file`, browser directory
  picker (`webkitdirectory`) or many files; every part's **relative path** is
  rebuilt from the tab-separated filename trailing segments and stored under
  `staging/` (sanitize each segment; reject `..`, separators, NUL).
- `POST /sl/{board_id}/media` — multipart fields `icon`, `banner`, `photos`
  (many) → `icon.`, `banner.`, `gallery-N.` keys; updates the graph props and
  replaces prior objects (delete old key when the extension changes).
- Both routes are the marketplace staff-guard shape: owner, or tenant admin
  whose `tenantId` matches the board's row.

## 5. Public gallery (marketplace mirror)

- `GET /spaces/{tenant_id}` — public grid of that tenant's deployed+public
  spaces, rendered exactly like the shop card grid: banner on top, gallery
  strip below, title + description, and the whole card links to
  `/s/{board_id}/` (the deployed app — **no intermediate page**).
- The tenant landing page and the top nav gain a "Spaces" entry when the
  tenant `has_app("serverless")` (same pattern as `has_marketplace`).
- Card rendering: if deployed → banner + "Open" affordance; if not deployed
  the space is hidden from `public_reads`-less guests (isPublic && deployedAt
  only).

## 6. Tenant dashboard / admin portal

- `space_manage_card` is a **space overview card** (NOT a marketplace mirror):
  status pill (Draft / Offline / LIVE), per-space identity — icon, title,
  copyable space id, banner + gallery preview via `/blob` — plus quick
  actions Publish/Unpublish (`set_board_public`), Preview build, Open app,
  Console. All build/media/records/keys/hooks management lives in the
  per-space Console at `/sl/{board_id}`; the card is an overview only.
- Tenant portal: tab **Serverless** (`/portal/serverless`, gated by
  `has_app("serverless")`) — the dedicated Serverless console: create form +
  the tenant's spaces as overview cards. `/portal` overview shows a
  "Serverless spaces" count card. `GET /sl` hard-redirects tenants to
  `/portal/serverless` and the owner to `/admin/serverless` (one console per
  audience; `/sl/{board_id}` stays as the deep-manage surface).
- Space console (`/sl/{board_id}`): has its own Publish/Unpublish button
  (card publishes stay on the originating console via the Referer), and the
  media manager shows **live previews** of the current banner, icon and
  gallery photos with per-item remove (`POST /sl/{board}/media/remove`,
  `which=icon|banner|photo` + `seq`) — uploads replace/append and the
  preview + public card update immediately.
- Owner console: `/admin/serverless` feature page lists spaces across
  tenants with the same overview cards.
- Public feed: `/public` lists every live space **directly at the top** in a
  "Spaces · Deployed apps" section — one card per space with its OWN icon,
  banner, gallery strip and LIVE badge; the whole card links straight to the
  running app `/s/{board_id}` (no tenant hop). The tenant directory
  ("Businesses") follows below, and `/tenant/{id}` keeps its per-tenant
  Spaces section + "View all" link to the standalone grid
  `/spaces/{tenant_id}`.

## 7. Apply + approval flow (UX)

- `DOMAINS` in `crates/platform/auth/src/models.rs` gains
  `("serverless", "Serverless Space")` → applicants can pick the service in
  the `/apply` dropdown; `apps_enabled` is seeded from the domain exactly
  like marketplace/media.
- `/apply/status` shows a violet **Serverless** domain badge for serverless
  applicants (was falling back to "Media"), and the approved copy says
  "open your Serverless console" instead of generic portal text.
- Owner approves via the existing queue (quota form unchanged). After
  approval the tenant admin lands in the portal with the Serverless tab.
  First space creation happens from the console (or later via MCP) — cards
  explain the flow.
- Do not overload the apply form: one new dropdown option, the domain pills
  line already handles it.

## 8. MCP tool (see `SERVERLESS_MCP.md`)

Public documentation endpoint with **3 layers of help** + key-gated command
endpoint. Creating the space (web) auto-generates the owner key / API keys —
the user copies the token into their MCP client and can then manage the whole
space **including deploying** from the MCP tool.

The app-runner surface (records math + substring search, realtime SSE,
outbound calls, cron jobs, webhook retry queue, config/export, console
panels) is **implemented** — see `SERVERLESS_MCP.md` §6 for the full
feature/status table and the E2E verification notes.

Hardening (write rules computed/validate/redact, event recipes with an
SSRF-guarded engine, AES-256-GCM encrypted secrets, expression DSL) is also
**implemented** — same doc §6.7, plus `docs/SERVERLESS_HARDENING.md` for the
port plan, schema and session log. CLI verbs: `space config set|clear`,
`space recipes …`, `space secrets …`; REST: `PUT /config`,
`/recipes{/…}`, `/secrets{/…}`, `POST /events`; console panels on
`/sl/{board}` (Write rules / Recipes / Secrets).

## 9. Implementation order

1. `crates/db/src/schema.rs` — new props (`BOARD_DESCRIPTION`, `BOARD_ICON_KEY`,
   `BOARD_BANNER_KEY`, `BOARD_GALLERY_KEYS`, `BOARD_DEPLOYED_AT`) + `Property`
   enum both directions; switch `BOARD_PUBLIC_READS` handling to bool.
2. `blob.rs` — whitelist `srv/`.
3. `platform/auth/src/models.rs` — `DOMAINS` += serverless.
4. `crates/business-apps/serverless/src/models.rs` — bool `board_public_reads`,
   new fields, `set_board_public`, `set_board_media`, `set_board_deployed`,
   `update_board` (title/description).
5. `store.rs` — key scheme helpers for `srv/{board}/…` keys, staging→dist
   copy, dist/index/index.html resolver.
6. `routes.rs` — `/spaces/{tenant}`, `/s/{board}/*`, preview, files/media
   upload, deploy, manage card wiring, portal/console pages, MCP endpoints.
7. `components.rs` — `space_manage_card` (+ reuse in admin console).
8. `apps/lan` nav `has_serverless`, `admin` portal_tabs + console Spaces
   page, tenant page link.
9. Build, restart, smoke test (create → upload dist → preview → deploy →
   public card → `/s/{board}` renders (topcoat paths cannot end in `/`)).