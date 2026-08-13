# Serverless gaps (e-commerce & general) — cross-repo tracking

Portable gap list for the serverless board engine (records + rules + recipes +
secrets + hooks + keys). Used to track which repo covers what: **LANsystem**
(this repo) and **zerowrapper / whatsapp4agents** (which share the design).
Each gap: what it is, why it matters, how it's implemented where done, and
per-repo status.

Status legend: `done` (implemented + tested), `plan` (designed, not built),
`missing` (not present), `n/a` (doesn't apply to this repo).

---

## Gap 1 — Action payload templating (`{{payload.*}}` / context vars)

**What.** Recipe actions (`$call`, `$notify`, `$upsert_other`, `$patch_other`,
`$compute` …) can currently only inject **secrets** (`{{secrets.NAME}}`,
`${NAME}`, `{NAME}`) into their payload/URL/header strings. They cannot refer
to the triggering event: the record payload, seq, event kind, board id,
writer, or cron job name.

**Why it matters (e-commerce).** A payment webhook recipe needs to forward the
order's id/amount to the provider; a fulfillment recipe needs the order's
customer/items in the outbound call; a cross-board upsert needs the order data
carried into the other board. Without templating these payloads must be static
JSON or assembled client-side — dynamic flows are impossible.

**Design.** Extend the substitution pass to also resolve, in order:
- `{{secrets.NAME}}` — existing (case-insensitive)
- `{{payload.a.b}}` — path into the triggering event payload
- `{{seq}}`, `{{kind}}` (record.created/updated/deleted/inbound/cron:<job>),
  `{{board}}`, `{{writer}}`, `{{cron}}` — context scalars

Missing keys stay literal (so URLs with braces don't break) or fail the
action? — decide: **fail the action** with a clear log entry (a typo'd
`{{paylod.total}}` silently POSTing garbage is worse than a hard failure).

**Tests.** Unit: every syntax against a canned EventContext; integration:
`$call`/`$upsert_other` with templated payload, verified in the other board.

**Status.**
- LANsystem: done (v2 — see Gap 7 history below)
- zerowrapper: missing — same `substitute_value` seam, same fix applies

---

## Gap 2 — Same-board writes from inbound / cron recipes

**What.** The recipe persist step ("write the mutated payload back to the
record") only runs for `record.created|updated|deleted` events. `inbound`
(`POST /events`) and `cron:<job>` recipes can mutate their payload copy
(`$compute`, `$merge` …) but the result is dropped — and `$upsert_other`
can only target an explicitly named board.

**Why it matters.** A Stripe webhook (`inbound`) can't create the paid-order
record or flip `status:"paid"` server-side; the storefront must poll. An
abandoned-cart cron job can't write a reminder record. Every flow needs a
round trip through the client or a second board.

**Design.**
- Allow `$upsert_other` (and the new `$patch_other`, Gap 3) to omit `board` —
  omitting it targets the **triggering board**.
- Persist path for `inbound`/`cron` when the recipe payload mutated AND there
  is a target: for inbound, `$upsert_other` without `board` creates a record.
  No seq exists for inbound, so the "persist same seq" path stays
  `record.*`-only.
- Loop safety: the store's raw write paths (`record_insert_raw`/
  `record_set_raw`/patch-raw) never re-dispatch recipes, so same-board writes
  cannot loop.

**Tests.** inbound recipe with `$upsert_other` (no board) creates a record;
cron recipe creates a record; no infinite dispatch.

**Status.**
- LANsystem: done
- zerowrapper: check — same persist-gate and `$upsert_other` semantics

---

## Gap 3 — `$patch_other` (patch another record by filter)

**What.** No recipe action can modify an *existing* record other than the
triggering one. `$upsert_other` inserts/upserts into another board; there is
no "find by filter and apply `$set/$inc/$dec/$mul/$unset`".

**Why it matters.** Stock decrement on order creation (the canonical e-commerce
write) is impossible server-side: `order.created` can't touch the product
record. Same for reserved-payment holds, loyalty counters, coupon redemption
counters.

**Design.** New action:
```json
{"$patch_other": {
  "board": "store-products",          // optional: same board when omitted
  "filter": [{"field": "sku", "op": "eq", "value": "A1"}],
  "patch": {"$inc": {"stock": -1}}    // $set / $inc / $dec / $mul / $unset
}}
```
- Match: first record matching the board's filter syntax.
- Applied via the same patch engine as `records patch` (raw path — no
  re-dispatch, no recipes for the patched record; the *triggering* recipe
  already ran).
- Patch actions may template (Gap 1): `{"$inc": {"stock": "{{payload.qty}}"}}`.

**Tests.** Unit (filter → patch math, unset, no-match no-op) + integration:
order.created recipe decrements the product record, verified in DB.

**Status.**
- LANsystem: done
- zerowrapper: missing — new seam method + action kind

---

## Gap 4 — Inbound webhook authentication (HMAC on `POST /events`)

**What.** `POST /api/srv/{board}/events` (inbound recipes) is unauthenticated
beyond the optional `requires(Writer)` — and after the role-ceiling removal it
accepts any key. No signature verification: a forged request can trigger
recipes, hit external endpoints, or mint records.

**Why it matters.** Real payment providers (Stripe: `t=`/`v1=` HMAC-SHA256,
GitHub: `sha256=`, generic: `X-Hub-Signature-256`) sign webhook bodies; the
board must verify or any attacker can fake "payment received".

**Design.**
- New config key `webhook_secret` (set via `PUT /config {"webhook_secret":
  "<secret>"}` / `cli space config set --webhook-secret <s>`, `null` clears).
- When set, `POST /events` requires `X-Hub-Signature-256: sha256=<hex>` where
  `hex = HMAC-SHA256(webhook_secret, raw_body_bytes)` (GitHub-compatible);
  verify constant-time. `X-Signature: <hex>` (bare HMAC) is also accepted.
- `401` otherwise (unsigned/forged/wrong secret); `GET /config` exposes
  `webhook_secret_set: true` only (never the value).
- The route still requires a writer+ key (customer keys are always refused),
  and the raw body is captured before signature checks (works regardless of
  body-size limits, which cap at the same 2 MB as actions).

**Tests.** Unit (sign/verify vectors: known HMAC-SHA256 digest, tampered body,
wrong secret) + integration (signed POST → recipe runs; unsigned/forged/wrong
secret → 401).

**Status.**
- LANsystem: done
- zerowrapper: missing — same route, same design

---

## Gap 5 — Customer identity (scoped keys)

**What.** Keys are shared roles (`reader|writer|admin`) with no per-customer
identity. Storefronts use `public_reads` (all-or-nothing, fields masked by
redact) — there is no "this key may only read/create its own customer's
records", no customer signup, no per-customer write attribution beyond a free-
text `--writer` label.

**Why it matters.** "My orders" queries, order status tracking, and secure
checkout all need a customer-scoped credential; otherwise every customer
shares one reader key and sees every order.

**Design (phase 1 — API-scoped keys).**
- `srv_keys.scope` column (TEXT, nullable) — the customer id.
- New role `customer` (read + write records, nothing else — no recipes/secrets/
  config/keys/events/hooks/aggregate/links/call/delete-filter).
- Issue: `POST /api/srv/{board}/keys {"role":"customer","scope":"<id>"}` (admin)
  → customer token; CLI: `cli keys issue --role customer --scope <id>`.
- Read scoping: `records list/get/search/query/filter` with a customer key only
  return records whose `payload.customer_id == scope id`.
- Write enforcement: `record_insert/bulk/set/patch` with a customer key force
  `payload.customer_id = scope id` (client-supplied value overwritten) and
  `set/patch/delete` 404 on any record that does not carry the scope id, so no
  cross-customer tampering.
- The CLI itself refuses customer keys (storefront REST only), so the agent
  tool cannot bypass the scope rules. The SSE realtime stream
  (`GET /events`) is staff-only too (customer keys get 403), since live
  cross-customer events cannot be scope-filtered per-subscriber.

**Design (phase 2 — sessions, not built).** Storefront login UI, signed
session cookies / short-lived JWTs, password/OTP auth, order lookup by token —
platform-level; out of the board engine. Doc only.

**Tests.** Customer key: list/get scoped, insert forces customer_id, admin key
unscoped, role guards (customer cannot call recipes/secrets/config/keys/
events/aggregate/links/call/delete-filter), set/patch/delete on another
customer's record 404s, CLI refuses customer keys.

**Design (phase 2 — sessions, not built).** Storefront login UI, signed
session cookies / short-lived JWTs, password/OTP auth, order lookup by token —
platform-level; out of the board engine. Doc only.

**Tests.** Customer key: list/get scoped, insert forces customer_id, admin key
unscoped, role guards (customer cannot call recipes/secrets/config).

**Status.**
- LANsystem: phase 1 done
- zerowrapper: missing (phase 1 design applies as-is)

---

## Gap 6 — Atomic multi-record transactions

**What.** Every write is a single record op. Order + stock decrement, or
transfer debits/credits, are separate writes with a race window; a crash
between them leaves inconsistent state. Recipes fire *after* the triggering
write, so a recipe failure cannot roll the trigger back.

**Why it matters.** Inventory oversell under concurrency; payment-ledger
pairs; coupon redemption races.

**Design (options).**
- A) Same-board transaction action: `{"$transaction": [{"patch": {...}},
  {"patch": {...}}]}` executed under the board write lock (SQLite is
  single-writer, so the lock already serializes) with all-or-nothing
  semantics — recipe-level compensation is out of scope.
- B) Two-phase: trigger write in `pending` state + recipe confirms — requires
  a reconciler (cron) for failures.
- (A) is far simpler given the per-board lock; defer (B).

**Tests.** Concurrent order flood × product stock: final stock non-negative
and equals orders sum; crash-injection test for the transaction path.

**Status.**
- LANsystem: plan (A) — not built
- zerowrapper: missing

---

## Gap 7 — Misc / inventory of what's already covered (context for zw)

Covered today in both repos unless noted:
- computed/validate/redact rules; recipes with `--match`, `--dedup-on`
  (sha-256 fingerprint in `srv_recipe_runs`), enable/disable;
- secrets AES-256-GCM at rest, `{{secrets.*}}` injection, fingerprints only
  on list;
- hooks with delivery retry queue + replay; cron jobs (`@every`, cron
  expressions) dispatching `cron:<job>` recipes; outbound `$call/$notify`
  with SSRF guard (private/loopback refused);
- keys reader/writer/admin, salted+revocable; per-board rate limit, audit
  trail, TTL; dump/restore; media + static deploy;
- REST surface + `cli` MCP tool with public help layers.

## Bug catalogue (found while implementing + live-testing gaps 1–5)

Apply these to any repo porting the gaps (zerowrapper / whatsapp4agents) —
every one of these was found by live E2E, not by unit tests:

1. **Double-encoded string config in meta.toml.** `board_config_set` writes
   `serde_json::to_string(value)`; for a JSON *string* value the stored line
   ends up `key = "\"value\""` and `read_meta`'s single `from_str::<String>`
   decode leaves the quotes ON — so the HMAC secret was `"s3cr3t-w00k"` (with
   quotes) and every signed `POST /events` got 401. Fix: decode one layer on
   read (`webhook_secret()` does `from_str::<String>(&s).unwrap_or(s)`).
   Object/array values are unaffected (no surrounding quotes). **Check any
   config key that stores a plain string.**
2. **Route guards can silently vanish in a refactor — and the CLI bypasses
   REST.** The scope checks on `record set/patch/delete` were written but never
   landed on disk (tooling error); a live customer key immediately deleted
   another customer's record. Worse: the CLI calls the store directly, so
   scope enforcement in routes is not enough — a customer key through
   `cli records list` would see everything. Fixes: (a) verify each write
   surface with a real scoped key before trusting it, (b) refuse customer
   keys at the CLI layer entirely (`staff_guard`: "storefront REST only").
   **Check every read/write path, not just the ones you think are public.**
3. **SSE realtime stream leaks across customers.** `GET /events` replays 500
   events and live-broadcasts every payload to subscribers; there is no
   per-subscriber scope filter, so a customer key could watch all records.
   Fix: `requires_staff(Reader)` on the stream route — customer keys get 403
   (blocking, not filtering — live cross-customer events can't be scoped
   cheaply). **Check your websocket/SSE surface for the same hole.**
4. **`config clear` double-consumed a positional.** The CLI verb called
   `args.positional()` twice; the second call returned the next token, so
   `clear` fell through to the `set` path (`--computed is not valid JSON`).
   Fix: bind `let sub = args.positional();` once. **Check any CLI subcommand
   that reads its verb twice.**
5. **Migrations: guard `ALTER TABLE` and don't trust a copy.** `ADD COLUMN`
   must tolerate "already exists" (it runs on every board open). Debugging
   note: copying only `board.db` without the `-wal` file shows the pre-
   migration schema and sends you hunting a phantom bug — checkpoint the WAL
   or copy both.

## Session log

- 2026-08-13 (LANsystem): gaps 1–5 phase-1 implemented and live-verified on
  the DELIVERY board; gap 6 left as plan (A). See commit log for details.
- 2026-08-13 (LANsystem): gap 5 hardening round — customer keys blocked on
  aggregate/links/call/delete-filter, the CLI and the SSE stream; bulk inserts
  force `customer_id`; set/patch/delete on non-owned records 404; `webhook_secret`
  set/clear surface added (PUT /config + CLI flag) and its double-encoded
  meta.toml storage fixed (HMAC secret now round-trips); HMAC + scope tests
  added (30 serverless-app tests).
- 2026-08-13 (LANsystem): gap 5 hardening round — customer keys blocked on
  aggregate/links/call/delete-filter and the CLI; bulk inserts force
  `customer_id`; set/patch/delete on non-owned records 404; `webhook_secret`
  set/clear surface added (PUT /config + CLI flag) and its double-encoded
  meta.toml storage fixed (HMAC secret now round-trips); HMAC + scope tests
  added (30 serverless-app tests).
