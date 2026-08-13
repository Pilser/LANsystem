# SERVERLESS HARDENING — features, plan and session memory

This doc is the cross-session memory for the serverless hardening work
(computed/validate/redact config, recipes engine, encrypted secrets,
expression DSL). It mirrors the `whatsapp4agents` commit `b2b88e5`
("serverless hardening": redact read-path, recipes engine, secrets,
computed/validate setters, `zw-expr` DSL) into LANsystem **with modularity**:
new crates + module splits instead of growing `routes.rs`/`store.rs`.

The vendored reference lives at `whatsapp4agents/` (git remote
Pilser/Whatsapp4Agents, own history). Reference files:
`zerowrapper/zw-expr/src/*`, `zerowrapper/zw-recipe/src/*`,
`zerowrapper/zw-db/src/{logic,recipes,secrets}.rs`,
`docs/serverless-execution-model.md` (extract: `/tmp/opencode/exec-model.md`).

---

## 1. Feature catalogue (what we are porting)

### 1.1 Expression DSL (`serverless-expr` — NEW leaf crate)
Pure, side-effect-free expression engine over a JSON payload. Grammar:
- Literals: numbers, strings (`"…"` / `'…'`), booleans, `null`
- Paths: `$.price`, `$.items[1].qty`, `$` (whole payload), `$.meta.*` (wildcard)
- Arithmetic `+ - * / %`, comparisons `== != < <= > >=`, logic `&& || !`, ternary `c ? a : b`
- Calls (~28): lower upper trim len num str concat round floor ceil abs min max
  if coalesce contains startswith endswith join daysago nowunix dateadd
  year month day hour minute second
- Hard caps: recursion depth + container node counts bounded; no panics on hostile input
API: `parse(src) -> Result<Expr, ExprError>`, `apply(&Expr, &data) -> Result<Value>`,
`evaluate(src, data)`, `truthy(src, data) -> Result<bool>`, `get_path(data, path) -> Value`
(missing → Null), `path::parse_path`.

### 1.2 Recipe engine (`serverless-recipe` — NEW crate, DB-free via trait)
Declarative event → condition → actions engine.
- `Recipe { name, when, match_json: Option<Value>, enabled (default true), dedup_on: Option<String>, actions: Vec<Value> }`
- `EventKind`: Created | Updated | Deleted | Inbound | Cron; `when` strings:
  `record.created|updated|deleted`, `inbound`, `cron:<job_name>`
- `EventContext { kind, board_id, seq: Option<i64>, payload: Value, writer: Option<String>, cron_name: Option<String> }`
- `Engine<'a> { db: &'a dyn RecipeDb }`; `run_recipes(recipes, secrets, ctx, dry_run) -> Vec<(name, RunOutcome)>`,
  `run_recipe(...) -> RunOutcome { matched, report: RunReport }`
- `MAX_ACTIONS = 50`; per-action optional `when` condition (expr string)
- After actions: if `report.changed` and payload differs and kind is Created/Updated/Deleted with a seq
  → persist via `RecipeDb::record_set` (engine writes bypass re-dispatch)
- Actions (15): `$compute {field: expr}` `$copy {from,to}` `$move {from,to}`
  `$format {field, pattern:{$.x}}` `$merge {into, <obj>}` `$push {path,value}`
  `$pull {path, <cond>}` `$sort {path, asc?}` `$slice {path, start, end?}`
  `$upsert_other {board, payload}` `$notify {url,method?,headers?,body?,secret?,timeout_ms?}`
  `$call {same}` `$schedule {name?, at|<schedule>, action}` `$set_state {state}`
  `$log {message}` — external ones are queued in `report.external` (secret-substituted)
  and executed by the ENGINE via `RecipeDb` (`execute_external`)
- `Secrets` (pure): uppercase names, `{X}`/`${X}`/`{{X}}` substitution (longest pattern
  wins), `masked()` = names only (`"secret:"` values), `substitute_value` recurses JSON
- `match_public(payload, filter)` — filter object `{"$.path": {"eq": v}}` with ops
  eq neq gt gte lt lte contains not_contains in
- `RunReport { actions_run: Vec<String>, logs: Vec<String>, external: Vec<Value>, changed: bool }`

### 1.3 `RecipeDb` trait (LAN modularity seam — better than zw's concrete DbPool)
```rust
#[async_trait]
pub trait RecipeDb: Send + Sync {
    async fn board_owner(&self, board_id: &str) -> anyhow::Result<String>;
    async fn record_insert(&self, board_id: &str, payload: &Value, writer: Option<&str>, upsert: bool) -> anyhow::Result<i64>;
    async fn record_set(&self, board_id: &str, seq: i64, payload: &Value, writer: Option<&str>) -> anyhow::Result<()>;
    async fn http_call(&self, board_id: &str, owner: &str, method: &str, url: &str, headers: &[(String,String)], body: Option<&str>, timeout_ms: u64) -> anyhow::Result<CallOutcome>; // {status: u16, duration_ms: u64}
    async fn job_add(&self, board_id: &str, name: &str, schedule: &str, action: &Value) -> anyhow::Result<i64>;
    async fn recipe_ran(&self, board_id: &str, recipe: &str, key: &Value) -> anyhow::Result<bool>;
    async fn recipe_mark_ran(&self, board_id: &str, recipe: &str, key: &Value) -> anyhow::Result<()>;
}
```
`TursoStore` implements it in `store/recipes.rs`. `http_call` reuses `netguard::valid_url`
+ reqwest (timeout clamp 500..60000). Engine code is DB-free; zw-recipe's `Engine { db: &DbPool }`
becomes `Engine { db: &'a dyn RecipeDb }`.

### 1.4 Store logic (pure + write-path integration)
- `computed`: meta.toml `computed_json` = `{"total": "$.price * $.qty"}`; on write,
  evaluate each field expr into the payload IN ORDER (later fields may reference earlier)
- `validate`: meta.toml `validate_json` = `[{"when": "<expr>", "error": "msg"}]`; first
  failing rule aborts the write with its message
- `redact`: meta.toml `redact_json` = `["$.ssn"]`; masked on **every public read** by
  replacing the path's value with `"***"` — but NOT on writes or internal consumers
- `prepare_payload(board, payload) -> payload` = computed → validate-rules → JSON-schema
  (applied in `record_insert` + `record_set`; engine persist path skips it)
- Read redaction: `record_get`, `record_list` (mod.rs) and `record_query`/`record_filter`/
  `record_search` (records.rs) mask via `redact_payload`/`redact_records`; `files.rs` does
  NOT use `record_query` — the public REST `record_query` is at routes.rs:2170; raw
  variants `record_insert_raw`/`record_set_raw` (skip ALL effects incl. dispatch) keep
  the engine loop-free
- Dispatch: `dispatch_recipes(board_id, kind, seq, payload, writer, cron_name)` called AFTER
  the effects on record insert/set/delete (never while holding the board lock); `inbound`
  via `POST /api/srv/{board}/events`; cron recipes dispatched from
  `workers::scheduler_worker` right after a job fires (`cron:<job.name>`, payload = polled
  record or Null for http jobs — matches zw)

### 1.5 Schema (per-board turso db, `board_conn` migration batch)
```sql
CREATE TABLE IF NOT EXISTS srv_recipes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    board_id TEXT NOT NULL,
    name TEXT NOT NULL,
    when_json TEXT NOT NULL,
    match_json TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    dedup_on TEXT,
    actions_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (board_id, name)
);
CREATE TABLE IF NOT EXISTS srv_recipe_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recipe TEXT NOT NULL,
    dedup_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (recipe, dedup_key)
);
CREATE TABLE IF NOT EXISTS srv_app_secrets (
    name TEXT PRIMARY KEY,          -- always UPPERCASE
    cipher TEXT NOT NULL,           -- base64(nonce||ct), AES-256-GCM
    fingerprint TEXT NOT NULL,      -- sha256 of plaintext
    created_at TEXT NOT NULL
);
```
`recipe_add`/`recipe_mark_ran` INSERTs set `created_at = now_millis()` (NOT NULL).
computed/validate/redact live in **meta.toml** (LAN style, like ttl/audit/rate):
keys `computed_json`, `validate_json`, `redact_json` (empty string = unset).

### 1.6 Secrets at rest
- Master key: env `LAN_SRV_SECRET_KEY` (any-length bytes, AES-256-GCM 32-byte key via
  SHA-256 digest) → `master_key()`; dev fallback `lan-dev-key-{pid}-{fingerprint("x")[..16]}`
  + `tracing::warn!` once (Rust 2024: `gen` is reserved → variable named `generated`)
- `aes-gcm` 0.10; 12-byte nonce; cipher = base64(nonce||ct) — decrypt fails on tamper
- `secret_set(board, name, value)`: name uppercased, `INSERT ... ON CONFLICT DO UPDATE`
- `secret_list(board)`: names + fingerprint (sha256 hex, shown truncated in console)
- `secrets_map(board)`: decrypt-all into `HashMap<String,String>` (engine runtime)
- remove: DELETE by name (uppercase); fingerprint() = sha256 hex (64 chars)

### 1.7 HTTP surface (admin-only except where noted)
```
GET    /api/srv/{board}/config      computed/validate/redact (board_config, extended)
PUT    /api/srv/{board}/config      body {computed?, validate?, redact?} (null clears a key)
GET    /api/srv/{board}/recipes                 → {ok, recipes:[...]} (rows incl. actions)
POST   /api/srv/{board}/recipes     {name, when, match_json?, enabled?, dedup_on?, actions}
PUT    /api/srv/{board}/recipes/{name}/enabled  {enabled: bool}
DELETE /api/srv/{board}/recipes/{name}
GET    /api/srv/{board}/secrets                → {ok, secrets:[{name, fingerprint, created_at}]}
PUT    /api/srv/{board}/secrets/{name}  {value}  (create or rotate)
DELETE /api/srv/{board}/secrets/{name}
POST   /api/srv/{board}/events      raw JSON body (or {payload: {...}}) → when=inbound (writer+)
```
All `requires(Admin)` (events: Writer). `when` values: `record.created|record.updated|
record.deleted|inbound|cron:<job.name>` — anything else is skipped at dispatch. Config GET
(extend `board_config` in ops.rs) already returns computed/validate/redact.

### 1.8 CLI verbs (`cli` single MCP tool, admin+)
```
cli space config set --computed '<json>' [--validate '<json>'] [--redact '<json>']
cli space config clear --computed|--validate|--redact
cli space recipes list | add --name n --when w [--match '<json>'] [--dedup-on <path>] [--disabled] --actions '<json>' | enable|disable --name n | remove --name n
cli space secrets list | set --name N --value v | remove --name N
```
Help layers: GLOBAL_USAGE/SPACE_USAGE group lines + verb_help entries (all public).

### 1.9 Console (`/sl/{board}`)
- "Write rules" panel: computed/validate/redact textareas (set + clear-all buttons)
- Recipes panel: list (name/when/match/enabled) + add form + enable/disable + remove buttons
- Secrets panel: list (names + truncated fingerprint) + set form (name+value) + remove
- New session POST routes: `/sl/{board}/rules`, `/recipes/add`, `/recipes/toggle`,
  `/recipes/remove`, `/secrets/set`, `/secrets/remove`

---

## 2. Implementation order (the 10 steps)

1. `crates/serverless-expr` — port zw-expr verbatim (rename `zw_expr` → `serverless_expr`), port tests
2. `crates/serverless-recipe` — port zw-recipe with `RecipeDb` trait seam, port tests
3. store split: `store.rs` → `store/{mod,records,ops,logic,recipes,secrets}.rs`. E0119 → the
   full `impl BoardStore for TursoStore` stays in `store/mod.rs` (trait impl cannot span files);
   inherent impls in records.rs/ops.rs; shared privates → pub(crate)
4. store features: migration batch (1.5), meta config fns, `prepare_payload`, read redaction +
   `_raw` write variants, dispatch_recipes, secrets crypto (1.6), RecipeDb impl
5. cron dispatch: `workers::scheduler_worker` runs `cron:<job.name>` recipes after a job fires
6. routes: split `routes.rs` → `routes/{mod,api}.rs` (console stays in mod.rs); add 1.7 routes incl.
   `POST /events`; extend `board_config` (+ `board_config_set`, `board_config_extended`)
7. cli: add 1.8 verbs + help strings (help layers are public — doc tool auto-lists via verb_help)
8. console: 1.9 panels on board_page (reuse `jv_str`/`jv_i64` helpers)
9. tests: logic/secrets/recipes pure tests + store rules-pipeline integration tests
   (computed+validate on write, redact masks reads, schema reject, config clear) — tmp data dir
   pinned once per test binary via `pin_test_data_dir()` (OnceLock; `set_var` is unsafe in 2024)
10. docs: this file finalize + SERVERLESS_MCP.md (new verbs, /events, computed/validate/redact) +
    SERVERLESS_SPACES.md §8 table

## 3. Verification loop (always)

```bash
cargo build --release -p lan                 # ~1 min
cargo test -p serverless-app --lib -p serverless-expr -p serverless-recipe
docker compose -f deploy/docker-compose.yml restart lan
sleep 6 && docker logs deploy-lan-1 --since 30s   # wait for "connected: db + storage + auth ready"
# live E2E via curl against 127.0.0.1:4321 (board + key below)
```

## 4. Live environment / test board

- LAN host `IICO` 192.168.88.101; compose publishes only `127.0.0.1:4321:3000`.
- Test space `sl-19ff0182674-0000` (DELIVERY), 14 records, ping job id 1, dead hook deliveries.
- Owner key (admin, writer label "e2e-test"):
  `sl-19ff0182674-0000:k-1e76b822a3af7d7d:097ad558ebb20d12c5e184ad2900a96d93ccdc48d31e6955`
- REST: `Authorization: Bearer <full key>`; MCP: `x-srv-key: <full key>` header or
  `--space <full key>` (embedded). Envelope: `POST /api/srv/mcp {"tool":"cli","params":{"args":[...]}}`;
  native: `POST /mcp` JSON-RPC `tools/call` name=cli arguments.args.
- Board DB on disk: `docker cp deploy-lan-1:/data/dbs/<id>/board.db /tmp/x.db` + python3 sqlite3
  (also meta.toml next to it). Container `deploy-lan-1`.
- EnvFilter shows only `target:"lan"` logs — use `tracing::info!(target: "lan", ...)` for debug.

## 5. Session log

### Session 1 (parity): app-runner parity shipped — records math/search/aggregate, SSE
events, outbound calls, cron jobs, webhook retry queue, ttl/rate/audit/links config,
dump/restore, console panels. Committed `ffcc0c9`, `d2c98bf`, `1297b5a` + pushed.

### Session 2 (this one): ported end-to-end
- Read all zw sources; produced this plan. Commit `b2b88e5` pulled into whatsapp4agents.
- Steps 1-5 done: crates `serverless-expr` + `serverless-recipe` (RecipeDb seam), store split,
  store features, cron dispatch. `ServerlessError::Internal` added; serverless-app deps:
  jsonschema 0.49 (default-features=false), aes-gcm, anyhow, both new crates.
- Step 6: routes split to `routes/{mod,api}.rs` — shared types (web forms, `borrow_auth`,
  RateForm/BoardQuery/OwnerQuery/TokenQuery/MediaQuery) live pub(crate) in mod.rs; api.rs holds
  the API section with corrected imports (split from git HEAD, marker-verified). New routes:
  `PUT /config`, recipes CRUD + enable, secrets CRUD, `POST /events`.
- Step 7: cli verbs `space config set|clear`, `space recipes`, `space secrets` + help layers.
- Step 8: console panels (Write rules / Recipes / Secrets) + session POST routes.
- Step 9: 28 tests green (22 + 3 rules-pipeline + 3 recipe pure). Fixed latent meta.toml bug:
  `write_meta_kv` used `{:?}` escaping — JSON values with embedded quotes corrupted on
  round-trip; now JSON-encoded on write, JSON-decoded on read.
- Step 10 (this file): finalize. Release build, deploy restart, live E2E, commit + push pending.