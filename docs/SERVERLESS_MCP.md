# Serverless Spaces — MCP Tool (one `cli` tool)

> Public, documentation-first MCP tool over the serverless spaces API — the
> same shape as the `zerowrapper` MCP server used by `whatsapp4agents`
> (reference: **`manage-whiteboard`** and its embedded `srv` CLI in
> `zw-mcp/src/srv.rs`). ONE tool named `cli` speaks a small command-line
> language — `cli <group> <verb> <space_id> [args]` — with the help layered
> 4 deep. Any MCP client can load the documentation and the help layers
> anonymously; **making commands** only work once the user has created a
> space and copied its auto-generated key into their MCP client config.
>
> Note: this is a *spec* captured before implementation, so the agent follows
> it to the end. See `SERVERLESS_SPACES.md` for the spaces feature itself.

## 1. Surface

One HTTP JSON endpoint, served by the serverless app (no SSE/stdio
transport; MCP clients that speak plain JSON-RPC-over-HTTP dial it directly —
same philosophy as the `agent-app`/`/api/agent` pattern in this repo):

| Endpoint | Auth | Purpose |
|---|---|---|
| `GET /api/srv/mcp` | none | Catalog: the single tool + the 4 help layers (layer 0) |
| `GET /api/srv/mcp/doc/cli` | none | The tool's doc: usage, groups, example (layer 1) |
| `GET /api/srv/mcp/guide` | none | Full manual: setup, key flow, deploy guide (layer 3) |
| `POST /api/srv/mcp` | `x-srv-key: <token>` | Invoke `{"tool":"cli","params":{"args":[...]}}` → `{"ok":true,...}` or `{"status":"error","usage":...,"hint":...}` |

The public help endpoints are **always readable** (that's the "public for
documentation" part) — four graduated layers, like zerowrapper:

1. **Layer 0 (overview)** — `cli` with no args / `cli --help`: usage line,
   groups, the "every task takes the space id" rule, auth note.
2. **Layer 1 (about)** — `cli about`: what a space is, how to create one and
   get the owner key, the four help layers.
3. **Layer 2 (group)** — `cli help <group>` (space/records/keys/hooks):
   every verb + flags of the group. Also `cli <group> --help`.
4. **Layer 3 (verb)** — `cli help <group> <verb>`: full per-verb detail
   (flags, role required, examples). Also `cli <group> <verb> --help`.

Errors echo the relevant layer: `{"status":"error","error":…,
"command":…,"usage":<layer>,"hint":…}` (zerowrapper's `err_json` shape).

## 2. Command gating (the key flow)

- A user with **no space** can use the help layers freely; `POST` without a
  key returns 401. Commands do NOT work in public.
- Creating a space (web or via an owner/admin session) **auto-generates**
  the existing artifacts: the one-time **owner key** and, once issued,
  scoped API keys (`reader|writer|admin`). The create redirect shows the
  one-time token (`/sl/<space_id>?owner_key=<secret>` — already
  implemented).
- The user pastes that token into their MCP client config
  (`x-srv-key: <space_id>:owner:<secret>` or
  `<space_id>:<bucket>:<secret>`), and the same `BoardStore` + `Auth` code
  paths the web uses now answer the tool (MCP bugs == web bugs, like the
  agent rule).
- **The key's leading id IS the space being managed** — `--space` is
  optional and must match it. The server re-derives the key's board and
  never trusts the URL/params; a mismatch returns a scoped-error with the
  usage layer attached.

## 2b. Native MCP over HTTP (`POST /mcp`)

The app also speaks the real MCP protocol (JSON-RPC over HTTP, streamable-
HTTP style, protocol `2025-06-18`) at **`POST /mcp`** — no stdio bridge, no
Node: any MCP client points straight at the URL.

| Method | Purpose |
|---|---|
| `POST /mcp` `initialize` | handshake → `protocolVersion`, `tools` capability |
| `POST /mcp` `tools/list` | the single `cli` tool definition |
| `POST /mcp` `tools/call` | runs `cli <group> <verb> [args]` |
| `POST /mcp` `ping` | keep-alive |
| `GET /mcp` | catalog (non-MCP, human/agent readable) |

Same rules as the envelope endpoint: help layers answer with **no key**;
commands need a key — the `x-srv-key` header
(`<space_id>:owner:<secret>` or a scoped `<space_id>:<bucket>:<token>`),
whose leading id is the space. Tool failures come back as
`isError: true` text results (zerowrapper-style), never JSON-RPC errors.

### 2b-1. No-header mode — the ONE string

For "just pass the id" clients, the key string itself can be passed as the
space in the call; the leading part IS the space id, the rest
authenticates — no config JSON, the client only needs the url:

```
cli space show --space <space_id>:owner:<secret>      (or --key <string>)
cli records count --key <space_id>:<bucket>:<token>
```

When both the header and an embedded string are given, their spaces must
match (clear `key mismatch` error otherwise).

Client config — opencode (`~/.config/opencode/opencode.json`):

```json
"lansrv-spaces": {
  "type": "remote",
  "url": "http://127.0.0.1:4321/mcp",
  "enabled": true,
  "headers": { "x-srv-key": "<space_id>:owner:<secret>" }
}
```

(`headers` optional: without it the help layers still work, and commands
answer a tool error explaining the missing key — or use no-header mode
above. `headers` and no-header mode can coexist; spaces must agree.)
Claude Code: same entry under `mcpServers` with `"type": "http"`.

## 3. The `cli` tool (groups + verbs)

Current verbs mirror existing web/API capabilities. The roadmap (§6) adds
the app-runner store ops (search, math, realtime, calls, cron) — each lands
with its public `verb_help` entry first, so `cli help …` documents a verb
before the verb ships.

```
usage: cli <group> <verb> <space_id> [args]
```

| Group | Verb | Params | Requires |
|---|---|---|---|
| `space` | `show` | `<id>` | reader+ |
| | `update` | `<id> [--title "t"] [--description "d"] [--public-reads on|off]` | admin+ |
| | `deploy` | `<id> [--publish on|off]` | admin+ |
| | `files` | `<id> list [--root staging\|dist]` | reader+ |
| | | `<id> upload <path> --content <b64>` | admin+ |
| | | `<id> remove <path>` | admin+ |
| | `media` | `<id> [--icon <b64>] [--banner <b64>]` | admin+ |
| | `delete` | `<id>` | admin+ |
| `records` | `insert` | `<id> '<json>' [--writer <w>] [--upsert]` | writer+ |
| | `list` | `<id> [--limit N] [--offset N]` | reader+ |
| | `get` | `<id> --seq N` | reader+ |
| | `delete` | `<id> --seq N` | writer+ |
| | `count` | `<id>` | reader+ |
| `keys` | `issue` | `<id> --role reader\|writer\|admin [--writer <label>]` | admin+ |
| | `revoke` | `<id> --bucket <bucket>` | admin+ |
| | `list` | `<id>` | admin+ |
| `hooks` | `register` | `<id> <url> [--secret <s>]` | admin+ |

`deploy` is the headline "manage entire space including deploying from MCP"
verb: after `space files upload` (once per file of a small `dist/`) the
client calls `deploy --publish on` and the public gallery card goes live.

## 4. Public docs shape

Layer 3 guide answers, in order:
1. What a space is (one paragraph + link to `/sl` docs page).
2. Create the space in the tenant's Serverless console (`/portal/serverless`
   — owner: `/admin/serverless`) — where the owner key shows.
3. Copy the key into the MCP client.
4. First call — `cli space show <space_id>`.
5. Upload a build (`space files upload` per file) and `deploy --publish on`.
6. Records basics + webhooks (`records insert`/`list`, `hooks register`).
7. Troubleshooting: 401/403 (revoked/wrong-role key — `keys issue`),
   "scoped to" (space id must match the key's board), "nothing staged"
   (upload files first), "unknown space id" (create it in the Serverless
   console at `/portal/serverless`).

## 5. Acceptance

- Help layers load for anonymous GETs (layers 0–3) and via the tool itself
  (`cli --help`, `cli about`, `cli help <group>`, `cli help <group> <verb>`).
- `POST` without a key → 401 `unauthorized`; with a key of a board that does
  not exist → 404; with a valid key → the action runs exactly like the web
  route (same store calls).
- Unknown groups/verbs and wrong space ids return the layered error JSON
  (`status:error` + `usage` + `hint`), never a bare 500.
- No public `POST` can mutate anything (key required; `requires()` gates
  roles like the API).

## 6. Full app-runner coverage (implemented)

> Parity with zerowrapper's serverless at commit `cb66222`
> (`zw-db/src/{cron,jobs,filters,calls,keys,hook_deliveries}.rs`,
> `ch-web-ui/src/{scheduler,serverless_sse}.rs`, `zw-mcp/src/srv.rs`) is
> **implemented** — see the completion status columns below and the E2E
> verification notes in `SERVERLESS_SPACES.md` §10. Every feature ships as
> a store method (`store.rs`) + same-path web/REST route + `verb_help`
> entry, and keeps the key flow: `--space` optional (defaults to the key's
> space), header or no-header mode, roles gate like the current verbs.

### 6.1 Records: full CRUD, math, pagination, search — implemented

| Verb | Shape | Role | Status |
|---|---|---|---|
| `insert` | `'<json>' [--writer <w>] [--upsert]` | writer+ | done |
| `set` | `--seq N '<json>' [--writer <w>]` | writer+ | done |
| `patch` | `--seq N '{"$set":{…},"$inc":{…}}' [--writer <w>]` | writer+ | done — ops apply `$inc`/`$dec`/`$mul` **before** `$set`, one SQLite write |
| `query` | `<json-path> [--eq <v>]` | reader+ | done — `$.a.b[2]`, `$` = root |
| `filter` | `--filter '<json>' [--order <path> [--asc\|--desc]] [--limit N] [--offset N]` | reader+ | done — ops `eq neq gt gte lt lte contains not_contains in`, and-lists |
| `search` | `--q <text> [--hl] [--limit N] [--offset N]` | reader+ | done — case-insensitive substring via `LIKE` on the payload (no FTS5); `--hl` wraps matches in `<mark>` |
| `aggregate` | `--op count\|sum\|avg\|min\|max --field <path> [--group-by <path>] [--filter '<json>']` | reader+ | done — count is `COUNT(*)`, others aggregate `json_extract`; grouped returns one row per key |
| `bulk` | `'[<json>,…]'` (or varargs) `[--upsert] [--writer <w>]` | writer+ | done |
| `delete` | `--seq N` **or** `--filter '<json>'` | writer+ | done (filter cascade is a single event) |
| `list` | `[--limit N] [--offset N]` (REST: `before`) | reader+ | done — newest-first |

REST mirrors for app JS (all under `/api/srv/{board_id}`, bearer-key auth,
reads public when `public_reads`): `POST /records`, `PUT|PATCH|GET|DELETE
/records/{seq}`, `GET /records?limit&offset&before`, `GET /records?q=&hl=1`,
`GET /records?path=&eq=`, `GET /records?filter=&order=&desc=1`,
`GET /aggregate?op=&field=&group_by=&filter=`, `DELETE /records?filter=`,
`POST /records/bulk` (body `{"payloads":[…]}`).

### 6.2 Realtime events (SSE) — implemented

- `GET /api/srv/{board_id}/events?after=<event id>&filter=<kind>` — SSE:
  replays events newer than the `after` id cursor (also resumed from the
  `Last-Event-ID` header), then streams the live broadcast. `filter`
  keeps a single event kind (`record.created|updated|deleted`). Capped at
  500 replayed events (the `srv_events` table prunes old rows). Periodic
  keep-alive comments. Reader role, or public when `public_reads`.
- The event id doubles as the cursor; `data:` carries
  `{"event", "id", "seq", "payload"}` for replayed rows and
  `{"event", "seq", "payload", "boardId"}` live — clients should key off
  the `id:` line either way.
- A live broadcast channel (256 slots, lag-tolerant) fans out every
  mutation; replay is DB-backed, so client reconnects never miss events.
- WebSocket is deliberately not added — SSE covers polling apps and agents.

### 6.3 Outbound API calls (app JS → external APIs) — implemented

| Verb | Shape | Role | Status |
|---|---|---|---|
| `records call` | `<url> [--method M] [--body '<json>'] [--timeout-ms N]` | writer+ | done |

SSRF guard: loopback/private/link-local/metadata URLs rejected (verified
E2E: `127.0.0.1` and `169.254.169.254` blocked); body cap 2 MB; timeout
0.5–60 s. Result `{"status":N,"headers":{…},"body":"…"}`.
REST: `POST /api/srv/{board_id}/call`.

### 6.4 Cron jobs (scheduler) — implemented

| Verb | Shape | Role | Status |
|---|---|---|---|
| `space jobs add` | `--name <n> --schedule <cron\|@every> --action '<json>'` | admin+ | done |
| `space jobs list` | | admin+ | done |
| `space jobs remove` | `--id N` | admin+ | done |

Schedule: 5-field cron (`min hour dom month dow` with `*`, `*/n`, `a-b`,
`a,b`) or `@every 30s|5m|2h|1d`. Action JSON:
`{"type":"http"|"poll","url","method","headers","body","timeout_ms","path":"$.a.b"}`.
Background worker wakes every 15 s: `http` fires the request (SSRF-guarded),
`poll` extracts the JSON path from the response and **inserts it as a
record**. Every run records `last_run_at / last_status (ok|error) /
last_message`; `next_run_at` reschedules automatically (verified E2E with
a `@every 30s` job). Table `srv_jobs` per board (Turso), parsed with the
cron engine in `src/cron.rs`.

### 6.5 Webhook delivery queue (durable) — implemented

| Verb | Shape | Role | Status |
|---|---|---|---|
| `hooks register` | `<url> [--secret <s>]` | admin+ | done |
| `hooks list` | | admin+ | done |
| `hooks remove` | `--url <url>` | admin+ | done |
| `hooks deliveries` | `[--status pending\|ok\|dead] [--limit N]` | admin+ | done |
| `hooks replay` | `[--id N]` (all when omitted) | admin+ | done |

Delivery: HMAC-SHA256 `x-srv-signature` header (`sha256(secret + body)`),
retries with exponential backoff `2^attempt` seconds (cap 300 s), dead-lettered
after 5 failed attempts (visible in `deliveries`, re-runnable via `replay`).
Triggered on `record.created/updated/deleted`; delivery is enqueued
(`srv_hook_deliveries`) on every mutation, so queued events survive
restarts. Verified E2E: a dead endpoint backed off 0→1→2→…→dead with the
HTTP error in `last_message`, and `replay` re-queued it immediately.

### 6.6 Space config, export, CORS — implemented

- `space show` returns the full config (title, public_reads, unique key,
  record count, deployed_at) — plus new verbs:
  - `space config` — prints ttl/rate/audit/unique-key settings (reader+)
  - `space ttl set --seconds N --field <path>` / `space ttl clear` —
    records whose `payload[field]` (unix seconds) is past
    `now + N` are **hidden from all reads** (list/get/search/filter/
    aggregate) and swept every 30 s
  - `space rate set --per-minute N` / `space rate clear` — in-memory
    sliding 60 s window on the API, enforced with 429s
  - `space audit on|off|list` — append-only trail (action, seq, payload
    sha256) when enabled
  - `space link set|clear|show` — record parent links (`via` labels)
  - `space dump` / `space restore` — export/import the whole board
    (config + records + hooks + links; restore preserves explicit seqs)
- REST for app JS: `GET /api/srv/{board_id}/config`,
  `PUT|DELETE /ttl|rate`, `PUT|GET /audit`, `POST /links`. CORS preflight
  (`OPTIONS`) is answered on the whole `/api/srv/{board_id}/*` family.
- Console (`/sl/{board}`): records search/filter/aggregate controls, jobs
  add/list/remove, delivery queue + replay, and the config panel (ttl,
  audit, rate, dump download, restore) — all server-rendered, no JS needed.

### 6.7 Hardening (computed/validate/redact, recipes, secrets) — implemented

Write-rule + recipe + secret surface, no role ceiling (any valid space key;
only `/events` needs writer+):

- `space config` now also prints the computed/validate/redact rules; new verbs:
  - `space config set --computed '<json>' [--validate '<json>'] [--redact '<json>']`
    — `computed` is an object `{"total": "$.price * $.qty"}` evaluated on every
    write (later fields may reference earlier, alphabetical order);
    `validate` is `[{"when": "<expr>", "error": "msg"}]` — first failing rule
    aborts the write with its message; `redact` is `["$.ssn"]` — paths masked
    with `"***"` on **public reads** only (stored payload untouched)
  - `space config clear --computed|--validate|--redact` — unset a rule
  - `space recipes list|add|enable|disable|remove` — `add` takes `--name`,
    `--when record.created|record.updated|record.deleted|inbound|cron:<job.name>`,
    optional `--match '<json>'` / `--dedup-on <path>` / `--disabled`, and
    `--actions '[{"http":{"url":"..."}}]'` (dedup fingerprints sha-256 of the
    payload path into `srv_recipe_runs`)
  - `space secrets list|set --name N --value v|remove --name N` — AES-256-GCM
    encrypted at rest (`LAN_SRV_SECRET_KEY`; dev fallback key + warn),
    injected into http action templates as `{{secrets.NAME}}`; list shows
    names + fingerprints only
- Recipes dispatch after every insert/set/delete and after each cron job run
  (`cron:<job.name>`, payload = polled record or null for http jobs) and on
  `POST /api/srv/{board_id}/events` (body = raw JSON or `{"payload": {...}}`).
  http actions are SSRF-guarded (loopback/private refused), timeout 0.5–60 s,
  2 MB cap.
- REST for app JS: `PUT /api/srv/{board_id}/config` (computed/validate/redact,
  null clears), `GET|POST /recipes`, `PUT /recipes/{name}/enabled`,
  `DELETE /recipes/{name}`, `GET /secrets`, `PUT|DELETE /secrets/{name}`,
  `POST /events`.
- Console (`/sl/{board}`): "Write rules", "Recipes" and "Secrets" panels with
  set/add/enable/remove forms (server-rendered).

### 6.8 Milestones — all landed

1. **M1 — records ops** (6.1) — done (search uses LIKE, not FTS5; `--hl`
   wraps matches in `<mark>`).
2. **M2 — realtime SSE** (6.2) — done (event-id cursor + `Last-Event-ID`
   resume + kind filter + keep-alive).
3. **M3 — outbound calls + webhook queue** (6.3, 6.5) — done (SSRF guard
   verified; durable retry queue with backoff + replay).
4. **M4 — cron jobs** (6.4) — done (`srv_jobs`, 15 s worker, http/poll
   actions, verified with a live `@every 30s` run).
5. **M5 — config/export/CORS** (6.6) — done, plus the console panels.
