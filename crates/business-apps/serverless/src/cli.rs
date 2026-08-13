//! The single-tool CLI: one MCP tool (`cli`) that speaks a small
//! command-line language — `cli <group> <verb> [args]` — mirroring
//! zerowrapper's `srv` CLI (layered help, `--flag value` parsing).
//! Every task targets the space the `x-srv-key` belongs to: `--space <id>`
//! is optional and must match the key when given. Every verb calls the same
//! store/db/storage paths as the web routes and REST API, so CLI bugs == web
//! bugs. The help layers are fully public: an agent can read the whole
//! documentation without owning a space or a key; only the actions need the
//! space id (which must match the id inside the `x-srv-key` token).
//!
//! No-header mode: the `--space`/`--space-id`/`--key` value may be the
//! console's ONE copyable string — `<space_id>:owner:<secret>` or
//! `<space_id>:<bucket>:<secret>` — which authenticates the call itself
//! (`embedded_key`), so a client pointed at the URL with no config JSON
//! works: `cli space show --space <the-string>`. The agent just passes the
//! id it was given.

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use topcoat::context::Cx;

use crate::routes::{
    ApiAuth, board_row, borrow_auth, requires, writer_of,
};
use crate::store::TursoStore;
use crate::{files, models};
use serverless_core::{BoardStore, KeyRole};

const NO_TENANT: i64 = 0;

// ---------------------------------------------------------------------------
// help layers
// ---------------------------------------------------------------------------

/// Layer 1: the tool overview. Running `cli` with no args prints this.
pub const GLOBAL_USAGE: &str = "\
usage: cli <group> <verb> [args]
       cli about                     what a space is + how to get your key
       cli --help                    this overview
       cli help <group>              per-group help (e.g. `cli help space`)
       cli help <group> <verb>       per-verb help (e.g. `cli help records insert`)

groups:
  space     show / update / deploy / files / media / delete /  the space itself
            jobs / ttl / audit / rate / link / config /
            recipes / secrets / dump / restore
  records   insert / set / patch / list / get / delete /      the space's records
            query / filter / search / aggregate / bulk / call / count
  keys      issue / revoke / list                            scoped API keys
  hooks     register / list / remove / deliveries / replay    webhooks

the space id is OPTIONAL — your key identifies the space:
  cli space show                      manage this key's space (no --space needed)
  cli space show --space sl-...       same, explicit (must match the key)
  cli space show --space sl-..:owner:...   pass the ONE console string — the
                                           leading part IS the space id, the
                                           rest authenticates (no header needed)
find a space's id on the space page (top right, next to the title — copy
button included), or in the create redirect /sl/<space_id>?owner_key=....
auth: pass the `x-srv-key: <space_id>:owner:<secret>` header (or a scoped
`<space_id>:<bucket>:<secret>` key), OR the key string itself as the --space
value above. The HELP layers above need NO key at all — they are public.";

/// `cli about` — what a space is and how the tool fits.
pub const ABOUT: &str = "\
A space is a deployable frontend app hosted by LANsystem: a record board
plus a built web app. The public card on your tenant's Spaces page links
straight to the running app at /s/{space_id}.

How to get started:
  1. Create the space on the web (/sl/boards/create or your portal's
     Spaces tab). The space page shows the space id (copyable) and the
     redirect URL carries the one-time owner key:
     /sl/<space_id>?owner_key=<secret>.
2. Give your agent the ONE string the space page shows — the key
       (redirect URL /sl/<space_id>?owner_key=<secret>). Either configure
       the MCP server once with header x-srv-key: <space_id>:owner:<secret>
       (or an issued scoped token), or just pass the string as the id in
       each call: cli space show --space <space_id>:owner:<secret>.
       Either way every command defaults to that space, so `cli space show`
       alone also works.
    3. Verify:  cli space show
   4. Ship a build: cli space files upload index.html --content <b64>
                  cli space deploy --publish on
   5. Mint scoped keys for other apps: cli keys issue --role writer

Help layers: cli --help (overview) | cli help <group> (group) |
cli help <group> <verb> (per-verb, most detail). They are PUBLIC — an
agent that has no space yet can read every layer without a key.";

const SPACE_USAGE: &str = "\
usage: cli space <verb> [--space <space_id>] [args]
  show    [--space <id>]                                 board meta, deploy state, record count
  update  [--space <id>] [--title \"t\"] [--description \"d\"] [--public-reads on|off]
  deploy  [--space <id>] [--publish on|off]              copy staging/ -> dist/ and go live
  files   [--space <id>] list [--root staging|dist]
          [--space <id>] upload <path> --content <base64>
          [--space <id>] remove <path>                   (staging only)
  media   [--space <id>] [--icon <b64>] [--banner <b64>] [--gallery <b64>]* [--clear-gallery]
  config  [--space <id>]                                 board config (incl. hardening rules)
  config  [--space <id>] set|clear --computed '<json>'    computed/validate/redact rules (any key)
          [--space <id>] [--validate '<json>'] [--redact '<json>']
  recipes [--space <id>] list|add|enable|disable|remove   event recipes (any key)
  secrets [--space <id>] list|set|remove                  encrypted app secrets (any key)
  jobs    [--space <id>] add|list|remove                 cron jobs (admin)
  ttl     [--space <id>] set|clear                       record expiry (admin)
  audit   [--space <id>] on|off|list                     audit trail (admin)
  rate    [--space <id>] set|clear                       in-memory rate limit (admin)
  link    [--space <id>] set|clear|show                  record parent links (admin/writer)
  dump    [--space <id>]                                 export the board as JSON (admin)
  restore [--space <id>] --dump '<json>'                 import a dump (admin)
  delete  [--space <id>]                                 permanently delete the space
--space defaults to the space your key belongs to; pass it explicitly only
to be safe. per-verb help: cli help space <verb>";

const RECORDS_USAGE: &str = "\
usage: cli records <verb> [--space <space_id>] [args]
  insert  [--space <id>] '<json>' [--writer <w>] [--upsert]
  set     [--space <id>] --seq N '<json>'               replace a record's payload
  patch   [--space <id>] --seq N '<patch>'              apply $set/$inc/$dec/$mul/$unset ops
  list    [--space <id>] [--limit N] [--offset N] [--q text] [--filter '<json>'] [--order path] [--hl]
  get     [--space <id>] --seq N
  query   [--space <id>] --path a.b [--eq <json>] [--limit N] [--offset N]
  filter  [--space <id>] --filter '<json>' [--order path [--asc|--desc]] [--limit N] [--offset N]
  search  [--space <id>] --q text [--hl] [--limit N] [--offset N]
  aggregate [--space <id>] --op count|sum|avg|min|max --field a.b [--group-by c] [--filter '<json>']
  bulk    [--space <id>] '<json>' '<json>' ...          insert many at once
  call    [--space <id>] <url> [--method M] [--body '<json>'] [--timeout-ms N]
  delete  [--space <id>] --seq N | --filter '<json>'    one record or filter match
  count   [--space <id>]
--space defaults to the space your key belongs to.
per-verb help: cli help records <verb>";

const KEYS_USAGE: &str = "\
usage: cli keys <verb> [--space <space_id>] [args]
  issue   [--space <id>] --role reader|writer|admin [--writer <label>]   returns the one-time token
  revoke  [--space <id>] --bucket <bucket>
  list    [--space <id>]
--space defaults to the space your key belongs to.
per-verb help: cli help keys <verb>";

const HOOKS_USAGE: &str = "\
usage: cli hooks <verb> [--space <space_id>] [args]
  register [--space <id>] <url> [--secret <s>]
  list     [--space <id>]
  remove   [--space <id>] --url <url>
  deliveries [--space <id>] [--status pending|ok|dead] [--limit N]   retry queue
  replay   [--space <id>] [--id N]                      re-queue failed deliveries
--space defaults to the space your key belongs to.
per-verb help: cli help hooks <verb>";

    /// Layer 4: per-verb detail (zerowrapper's `verb_help`).
    fn verb_help(group: &str, verb: &str) -> Option<&'static str> {
        match (group, verb) {
        ("space", "show") => Some("\
cli space show [--space <space_id>]
prints the space's catalog row: title, description, isPublic, deployedAt,
unique key, schema and record count. reader+ role.
--space defaults to the space your key belongs to."),
        ("space", "update") => Some("\
cli space update [--space <space_id>] [--title \"t\"] [--description \"d\"] [--public-reads on|off]
updates the space's details. Only supplied flags change; unset fields keep
their current values. admin+ role."),
        ("space", "deploy") => Some("\
cli space deploy [--space <space_id>] [--publish on|off]
copies every staged build file (srv/<id>/staging/) to dist/, stamps
boardDeployedAt, and with --publish on flips the space to public — the
card on /spaces/<tenant> and /s/<space_id> go live. Errors if nothing is
staged yet. admin+ role."),
        ("space", "files") => Some("\
cli space files [--space <space_id>] list [--root staging|dist]
cli space files [--space <space_id>] upload <path> --content <base64>
cli space files [--space <space_id>] remove <path>
manage the build: list the staged (or deployed) files, upload one file as
base64 (relative path, e.g. index.html or assets/app.js), remove a staged
file. admin+ role for upload/remove; reader+ for list."),
        ("space", "media") => Some("\
cli space media [--space <space_id>] [--icon <b64>] [--banner <b64>] [--gallery <b64>]* [--clear-gallery]
sets the space's icon, banner and/or gallery photos (base64 PNG/JPEG/WebP).
--gallery appends one photo per occurrence; --clear-gallery removes every
gallery photo first. Unsupplied flags keep current values. admin+ role."),
        ("space", "config") => Some("\
cli space config [--space <space_id>]
prints the board config: title, public_reads, unique key, ttl, rate, audit
and the computed/validate/redact rules. no role ceiling: any key for the
space can view or manage rules.

cli space config set --computed '<json>' [--validate '<json>'] [--redact '<json>']
cli space config clear --computed|--validate|--redact
writes the pre-write hardening rules: computed is an object of
{field: expr} (e.g. {\"total\": \"$.price * $.qty\"}), validate is an array
of {\"when\": \"<expr>\", \"error\": \"msg\"} rules — the first failing rule
rejects the write — and redact is an array of JSON paths ([\"$.token\"])
masked with \"***\" on public reads only. clear resets a rule to unset.
no role ceiling: any key for the space can manage rules."),
        ("space", "recipes") => Some("\
cli space recipes list [--space <space_id>]
cli space recipes add --name n --when <event> [--match '<json>'] \
       [--dedup-on <path>] [--disabled] --actions '<json>'
cli space recipes enable|disable|remove --name n [--space <space_id>]
event recipes run on board events: --when is an event name pattern such as
record.created / record.updated / record.deleted / inbound / cron:<job.name>. --match filters
the payload (JSON matching), --dedup-on dedupes by a payload path (a
sha-256 fingerprint column in srv_recipe_runs), --actions is the action
list, e.g. [{\"$secrets\":{\"url\":\"https://api.example.com?k={{secrets.KEY}}\"}}, {\"http\":{\"url\":\"https://...\"}}].
recipes dispatch after every write and after each cron run; http actions
are SSRF-guarded (private/loopback refused). no role ceiling: any key for
the space can manage recipes."),
        ("space", "secrets") => Some("\
cli space secrets list [--space <space_id>]
cli space secrets set --name n --value <secret> [--space <space_id>]
cli space secrets remove --name n [--space <space_id>]
encrypted app secrets for recipe actions: values are AES-256-GCM
encrypted at rest (LAN_SRV_SECRET_KEY) and injected into action templates
as {{secrets.NAME}}; list shows names + fingerprints only, never values.
no role ceiling: any key for the space can manage secrets."),
        ("space", "jobs") => Some("\
cli space jobs add --name n --schedule '*/10 * * * *' --action '{\"type\":\"poll\",\"url\":\"https://...\",\"path\":\"$.a.b\"}'
cli space jobs list
cli space jobs remove --id N
cron jobs: 5-field schedules plus '@every 30s'. action is
{\"type\":\"http\"|\"poll\",\"url\",\"method\"?,\"headers\"?,\"body\"?,
\"timeout_ms\"?,\"path\"?} - poll extracts a JSON path from the response
and stores it as a record. The scheduler runs every 15s. admin+ role."),
        ("space", "ttl") => Some("\
cli space ttl set --seconds N --field expires_at
cli space ttl clear
records whose payload[field] (unix seconds) is older than N are hidden
from reads and swept every 30s. admin+ role."),
        ("space", "audit") => Some("\
cli space audit on | off  |  list [--limit N]
append-only audit trail (action, seq, payload sha256) when enabled.
admin+ role."),
        ("space", "rate") => Some("\
cli space rate set --per-minute N | clear
in-memory sliding-window request cap per minute for the board's API
(default bucket). admin+ role."),
        ("space", "link") => Some("\
cli space link set --seq N --parent M --via x
cli space link clear --seq N --via x
cli space link show --seq N
record parent links: record N points at record M through 'via'.
writer+ for set/clear, reader+ for show."),
        ("space", "dump") => Some("\
cli space dump [--space <space_id>]
exports the whole board (config + records + hooks + links) as JSON.
admin+ role."),
        ("space", "restore") => Some("\
cli space restore [--space <space_id>] --dump '<json>'
imports a dump (explicit seqs preserved; hooks and config restored).
admin+ role."),
        ("space", "delete") => Some("\
cli space delete [--space <space_id>]
permanently deletes the space: catalog tombstone, records, keys, hooks and
every stored object. Irreversible. admin+ role."),
        ("records", "insert") => Some("\
cli records insert [--space <space_id>] '<json>' [--writer <w>] [--upsert]
appends a record; with --upsert it replaces the row holding the board's
unique key value instead of appending. writer+ role."),
        ("records", "list") => Some("\
cli records list [--space <space_id>] [--limit N] [--offset N]
newest-first page of records (limit 1..500, default 25). reader+ role."),
        ("records", "get") => Some("\
cli records get [--space <space_id>] --seq N
fetches one record by its seq. reader+ role."),
        ("records", "delete") => Some("\
cli records delete [--space <space_id>] --seq N
deletes one record by seq. writer+ role."),
        ("records", "set") => Some("cli records set [--space <space_id>] --seq N '<json>'
replaces the whole payload of record N. writer+ role."),
        ("records", "patch") => Some("cli records patch [--space <space_id>] --seq N '<patch>'
applies Mongo-ish ops in place: {\"$set\":{\"a\":1,\"b.c\":2},
\"$inc\":{\"n\":1},\"$dec\":{\"n\":1},\"$mul\":{\"n\":2},
\"$unset\":[\"x\"]}. Returns the updated payload. writer+ role."),
        ("records", "query") => Some("cli records query [--space <space_id>] --path a.b[2].c [--eq <json>] [--limit N] [--offset N]
returns every record where the JSON path exists (optionally equal to --eq).
reader+ role."),
        ("records", "filter") => Some("cli records filter [--space <space_id>] --filter '<json>' [--order path [--asc|--desc]] [--limit N] [--offset N]
filter is a condition or an and-list: [{\"field\":\"a.b\",\"op\":\"gt\",
\"value\":100}] with ops eq|neq|gt|gte|lt|lte|contains|not_contains|in.
reader+ role."),
        ("records", "search") => Some("cli records search [--space <space_id>] --q text [--hl] [--limit N] [--offset N]
full-text-ish substring search over the raw payload; --hl wraps matches
in <mark>. reader+ role."),
        ("records", "aggregate") => Some("cli records aggregate [--space <space_id>] --op count|sum|avg|min|max --field a.b [--group-by c] [--filter '<json>']
aggregates the JSON path's numbers across records, optionally grouped by
another path. reader+ role."),
        ("records", "bulk") => Some("cli records bulk [--space <space_id>] '<json>' '<json>' ...
inserts many records at once (each gets its own seq). writer+ role."),
        ("records", "call") => Some("cli records call [--space <space_id>] <url> [--method M] [--body '<json>'] [--timeout-ms N]
server-side HTTP call (SSRF-guarded: public http(s) only) returning the
status and body. writer+ role."),
        ("records", "count") => Some("\
cli records count [--space <space_id>]
prints the record row count. reader+ role."),
        ("keys", "issue") => Some("\
cli keys issue [--space <space_id>] --role reader|writer|admin [--writer <label>]
mints a scoped API key; the returned token (<space_id>:<bucket>:<secret>)
is shown once. admin+ role."),
        ("keys", "revoke") => Some("\
cli keys revoke [--space <space_id>] --bucket <bucket>
revokes a scoped key by its bucket id. admin+ role."),
        ("keys", "list") => Some("\
cli keys list [--space <space_id>]
lists every scoped key (bucket, role, writer label, created_at). admin+ role."),
        ("hooks", "list") => Some("cli hooks list [--space <space_id>]
lists registered webhook URLs. admin+ role."),
        ("hooks", "remove") => Some("cli hooks remove [--space <space_id>] --url <url>
unregisters a webhook (pending deliveries are not dropped). admin+ role."),
        ("hooks", "deliveries") => Some("cli hooks deliveries [--space <space_id>] [--status pending|ok|dead] [--limit N]
lists the webhook delivery queue: id, url, event, seq, attempt, status,
last_message. pending retries with backoff 2^n s (dead after 5 tries).
admin+ role."),
        ("hooks", "replay") => Some("cli hooks replay [--space <space_id>] [--id N]
re-queues failed or dead deliveries (all of them without --id). admin+ role."),
        ("hooks", "register") => Some("\
cli hooks register [--space <space_id>] <url> [--secret <s>]
registers a webhook URL (signed with an optional secret). admin+ role."),
        _ => None,
    }
}

fn group_usage(group: &str) -> Option<&'static str> {
    match group {
        "space" => Some(SPACE_USAGE),
        "records" => Some(RECORDS_USAGE),
        "keys" => Some(KEYS_USAGE),
        "hooks" => Some(HOOKS_USAGE),
        _ => None,
    }
}

/// Answers any pure help-layer invocation: no args, `about`, `--help`,
/// `help <group>` / `help <group> <verb>` and the `--help` variants. This is
/// fully public — no space id, no key, no auth — so an agent can discover
/// the whole tool before it owns a space. Returns `None` when `tokens` is a
/// real command (which then needs a key and a space).
pub fn help_response(tokens: &[String]) -> Option<Value> {
    let command = tokens.join(" ");
    if tokens.is_empty() {
        return Some(json!({ "ok": true, "tool": "cli", "help": GLOBAL_USAGE }));
    }
    if tokens[0] == "about" {
        return Some(json!({ "ok": true, "about": ABOUT }));
    }
    if tokens[0] == "--help" || tokens[0] == "-h" {
        return Some(json!({ "ok": true, "help": GLOBAL_USAGE }));
    }
    if tokens[0] == "help" {
        let (group, verb) = match (tokens.get(1), tokens.get(2)) {
            (Some(g), Some(v)) => (g.as_str(), Some(v.as_str())),
            (Some(g), None) => (g.as_str(), None),
            (None, _) => return Some(json!({ "ok": true, "help": GLOBAL_USAGE })),
        };
        return Some(if let Some(v) = verb {
            if let Some(doc) = verb_help(group, v) {
                json!({ "ok": true, "help": doc })
            } else {
                err_json("unknown verb", &command, group_usage(group).unwrap_or(GLOBAL_USAGE), "run `cli help <group>` for the verb list")
            }
        } else if let Some(u) = group_usage(group) {
            json!({ "ok": true, "help": u })
        } else {
            err_json("unknown group", &command, GLOBAL_USAGE, "groups: space, records, keys, hooks")
        });
    }
    if tokens.len() >= 2 && (tokens[1] == "--help" || tokens[1] == "-h") {
        return Some(if let Some(u) = group_usage(&tokens[0]) {
            json!({ "ok": true, "help": u })
        } else {
            err_json("unknown group", &command, GLOBAL_USAGE, "groups: space, records, keys, hooks")
        });
    }
    if tokens.len() >= 3 && (tokens[2] == "--help" || tokens[2] == "-h") {
        return Some(if let Some(doc) = verb_help(&tokens[0], &tokens[1]) {
            json!({ "ok": true, "help": doc })
        } else {
            err_json("unknown verb", &command, group_usage(&tokens[0]).unwrap_or(GLOBAL_USAGE), "run `cli help <group>` for the verb list")
        });
    }
    None
}

// ---------------------------------------------------------------------------
// tiny flag/positional parser (zerowrapper-style)
// ---------------------------------------------------------------------------

struct Args<'a> {
    tokens: &'a [String],
    pos: usize,
}

impl<'a> Args<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let t = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(t)
    }

    /// Next positional (skips `--flag value` pairs).
    fn positional(&mut self) -> Option<String> {
        while let Some(t) = self.next() {
            if !t.starts_with("--") {
                return Some(t.to_string());
            }
            if t.contains('=') {
                continue;
            }
            if self.tokens.get(self.pos).is_some_and(|n| !n.starts_with("--")) {
                self.pos += 1;
            }
        }
        None
    }

    fn val_after(&self, flag: &str) -> Option<String> {
        for (i, t) in self.tokens.iter().enumerate() {
            if t == flag {
                return self.tokens.get(i + 1).map(|v| v.trim().to_string());
            }
            if let Some(rest) = t.strip_prefix(flag) {
                if let Some(v) = rest.strip_prefix('=') {
                    return Some(v.trim().to_string());
                }
            }
        }
        None
    }

    fn has(&self, flag: &str) -> bool {
        self.tokens.iter().any(|t| t == flag)
    }

    /// Every occurrence of a repeatable flag's value (zerowrapper's
    /// `all_flags`) — e.g. `--gallery <b64> --gallery <b64>`.
    fn all_flags(&self, flag: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, t) in self.tokens.iter().enumerate() {
            if t == flag {
                if let Some(v) = self.tokens.get(i + 1) {
                    out.push(v.trim().to_string());
                }
            } else if let Some(rest) = t.strip_prefix(flag) {
                if let Some(v) = rest.strip_prefix('=') {
                    out.push(v.trim().to_string());
                }
            }
        }
        out
    }

    fn flag_on(&self, flag: &str) -> Option<bool> {
        self.val_after(flag).and_then(|v| match v.as_str() {
            "on" | "true" | "1" | "yes" => Some(true),
            "off" | "false" | "0" | "no" => Some(false),
            _ => None,
        })
    }
}

/// Extract an embedded key from tool args: `--key <composite>` or
/// `--space` / `--space-id <composite>` where the value is a full key string
/// `<space_id>:<bucket>:<secret>` (the console's one copyable string).
/// Returns the space id it names and the whole token, so an endpoint can
/// authenticate a call with NO header at all — that single string IS the
/// credential, and the agent only ever "passes the id".
pub fn embedded_key(tokens: &[String]) -> Option<(String, String)> {
    let args = Args { tokens, pos: 0 };
    let raw = args
        .val_after("--key")
        .or_else(|| args.val_after("--space"))
        .or_else(|| args.val_after("--space-id"))?;
    let parts: Vec<String> = raw.split(':').map(str::to_string).collect();
    (parts.len() == 3 && parts.iter().all(|p| !p.is_empty()))
        .then(|| (parts[0].clone(), raw))
}

fn err_json(error: &str, command: &str, usage: &str, hint: &str) -> Value {
    json!({
        "status": "error",
        "error": error,
        "command": command,
        "usage": usage,
        "hint": hint,
    })
}

// ---------------------------------------------------------------------------
// the CLI
// ---------------------------------------------------------------------------

pub struct Cli<'a> {
    pub cx: &'a Cx,
    pub store: &'a TursoStore,
    pub key_board: String,
    pub auth: ApiAuth,
}

type CmdResult = Result<Value, Value>;

impl<'a> Cli<'a> {
    fn err(&self, error: &str, command: &str, usage: &str, hint: &str) -> CmdResult {
        Err(err_json(error, command, usage, hint))
    }

    /// The space id is passed as a flag on every task — but it defaults to
    /// the space the key belongs to, so `cli space show` alone manages that
    /// space fully. An explicit `--space` must match the key's space
    /// (enforced by `board`). The value may be the ONE-STRING key copied
    /// from the console — `<space_id>:owner:<secret>` or
    /// `<space_id>:<bucket>:<token>` — in which case only the leading id is
    /// the space (the endpoint already authenticated with the whole string).
    fn space_arg(&self, args: &mut Args<'_>, _command: &str, _usage: &str) -> Result<String, Value> {
        Ok(args
            .val_after("--space")
            .or_else(|| args.val_after("--space-id"))
            .filter(|s| !s.is_empty())
            .map(|s| s.split(':').next().unwrap_or_default().to_string())
            .unwrap_or_else(|| self.key_board.clone()))
    }

    /// The space id must match the key's board.
    async fn board(&self, id: &str, command: &str, usage: &str) -> Result<models::ServerlessBoard, Value> {
        if id != self.key_board {
            return Err(err_json(
                &format!("this key is scoped to {0}; you asked for {1}", self.key_board, id),
                command,
                usage,
                &format!("omit --space to manage {0} (this key's space)", self.key_board),
            ));
        }
        match board_row(self.cx, id).await {
            Ok(Some(board)) => Ok(board),
            Ok(None) => Err(err_json("unknown space id", command, usage, "this key has no space — create one on the web at /sl, or check the key's id; ids look like sl-<hex>-0000")),
            Err(e) => Err(err_json(&format!("space lookup failed: {e}"), command, usage, "")),
        }
    }

    fn guard(&self, min: KeyRole) -> Result<(), Value> {
        match requires(&Some(borrow_auth(&self.auth)), min) {
            Ok(()) => Ok(()),
            Err(_) => Err(err_json("forbidden: your key's role is too low for this verb", "", "", "")),
        }
    }

    pub async fn run(&self, tokens: &[String]) -> CmdResult {
        let command = tokens.join(" ");
        // help layers (fully public — no space, no key needed)
        if let Some(value) = help_response(tokens) {
            return Ok(value);
        }
        let (group, verb) = match (tokens.first(), tokens.get(1)) {
            (Some(g), Some(v)) => (g.as_str(), v.as_str()),
            _ => return Ok(json!({ "ok": true, "help": GLOBAL_USAGE })),
        };
        let rest: Vec<String> = tokens[2..].to_vec();
        let mut args = Args { tokens: &rest, pos: 0 };
        match (group, verb) {
            ("space", "show") => self.space_show(&mut args, &command).await,
            ("space", "update") => self.space_update(&mut args, &command).await,
            ("space", "deploy") => self.space_deploy(&mut args, &command).await,
            ("space", "files") => self.space_files(&mut args, &command).await,
            ("space", "media") => self.space_media(&mut args, &command).await,
            ("space", "delete") => self.space_delete(&mut args, &command).await,
            ("space", "config") => self.space_config(&mut args, &command).await,
            ("space", "jobs") => self.space_jobs(&mut args, &command).await,
            ("space", "ttl") => self.space_ttl(&mut args, &command).await,
            ("space", "audit") => self.space_audit(&mut args, &command).await,
            ("space", "rate") => self.space_rate(&mut args, &command).await,
            ("space", "link") => self.space_link(&mut args, &command).await,
            ("space", "dump") => self.space_dump(&mut args, &command).await,
            ("space", "restore") => self.space_restore(&mut args, &command).await,
            ("space", "recipes") => self.space_recipes(&mut args, &command).await,
            ("space", "secrets") => self.space_secrets(&mut args, &command).await,
            ("records", "insert") => self.records_insert(&mut args, &command).await,
            ("records", "set") => self.records_set(&mut args, &command).await,
            ("records", "patch") => self.records_patch(&mut args, &command).await,
            ("records", "list") => self.records_list(&mut args, &command).await,
            ("records", "get") => self.records_get(&mut args, &command).await,
            ("records", "query") => self.records_query(&mut args, &command).await,
            ("records", "filter") => self.records_filter(&mut args, &command).await,
            ("records", "search") => self.records_search(&mut args, &command).await,
            ("records", "aggregate") => self.records_aggregate(&mut args, &command).await,
            ("records", "bulk") => self.records_bulk(&mut args, &command).await,
            ("records", "call") => self.records_call(&mut args, &command).await,
            ("records", "delete") => self.records_delete(&mut args, &command).await,
            ("records", "count") => self.records_count(&mut args, &command).await,
            ("keys", "issue") => self.keys_issue(&mut args, &command).await,
            ("keys", "revoke") => self.keys_revoke(&mut args, &command).await,
            ("keys", "list") => self.keys_list(&mut args, &command).await,
            ("hooks", "register") => self.hooks_register(&mut args, &command).await,
            ("hooks", "list") => self.hooks_list(&mut args, &command).await,
            ("hooks", "remove") => self.hooks_remove(&mut args, &command).await,
            ("hooks", "deliveries") => self.hooks_deliveries(&mut args, &command).await,
            ("hooks", "replay") => self.hooks_replay(&mut args, &command).await,
            _ => self.err(
                "unknown command",
                &command,
                GLOBAL_USAGE,
                "run `cli --help` for groups, `cli help <group>` for verbs",
            ),
        }
    }

    async fn space_show(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        let board = self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let count = self.store.record_count(NO_TENANT, &id).await.unwrap_or(0);
        Ok(json!({
            "ok": true,
            "boardId": board.board_id,
            "title": board.board_title,
            "description": board.board_description,
            "uniqueKey": board.board_unique_key,
            "isPublic": board.board_public_reads.unwrap_or(false),
            "deployedAt": board.board_deployed_at,
            "recordCount": count,
        }))
    }

    async fn space_update(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        let board = self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let title = args.val_after("--title").unwrap_or_else(|| board.board_title.clone().unwrap_or_default());
        let description = args.val_after("--description").unwrap_or_else(|| board.board_description.clone().unwrap_or_default());
        let mut schema = board.board_schema_json.clone().unwrap_or_default();
        let mut unique = board.board_unique_key.clone().unwrap_or_default();
        if let Some(s) = args.val_after("--schema") {
            if !serde_json::from_str::<Value>(&s).is_ok_and(|v| v.is_object()) {
                return self.err("--schema is not a JSON object", command, SPACE_USAGE, "e.g. --schema '{\"fields\":[...]}'");
            }
            schema = s;
        }
        if let Some(u) = args.val_after("--unique") {
            unique = u;
        }
        let db = platform_core::db(self.cx);
        if let Some(pub_on) = args.flag_on("--public-reads") {
            let query = models::set_board_public(id.clone(), pub_on).map_err(|e| err_json(&format!("update failed: {e}"), command, SPACE_USAGE, ""))?;
            let _: serde_json::Value = db.query(query).await.map_err(|e| err_json(&format!("visibility update failed: {e}"), command, SPACE_USAGE, ""))?;
        }
        let query = models::update_board(id.clone(), title, description, schema, unique).map_err(|e| err_json(&format!("update failed: {e}"), command, SPACE_USAGE, ""))?;
        let _: serde_json::Value = db.query(query).await.map_err(|e| err_json(&format!("update failed: {e}"), command, SPACE_USAGE, ""))?;
        Ok(json!({ "ok": true, "boardId": id }))
    }

    async fn space_deploy(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        let board = self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let db = platform_core::db(self.cx);
        let storage = platform_core::storage(self.cx);
        let deployed = files::deploy_staging_to_dist(storage, &id)
            .await
            .map_err(|e| err_json(&format!("deploy failed: {e}"), command, SPACE_USAGE, ""))?;
        if deployed == 0 {
            return self.err("nothing staged yet — upload files first", command, SPACE_USAGE, "cli space files upload index.html --content <b64>");
        }
        let query = models::set_board_deployed(id.clone()).map_err(|e| err_json(&format!("deploy stamp failed: {e}"), command, SPACE_USAGE, ""))?;
        let _: serde_json::Value = db.query(query).await.map_err(|e| err_json(&format!("deploy stamp failed: {e}"), command, SPACE_USAGE, ""))?;
        let publish = args.flag_on("--publish").unwrap_or(false);
        if publish {
            let query = models::set_board_public(id.clone(), true).map_err(|e| err_json(&format!("publish failed: {e}"), command, SPACE_USAGE, ""))?;
            let _: serde_json::Value = db.query(query).await.map_err(|e| err_json(&format!("publish failed: {e}"), command, SPACE_USAGE, ""))?;
        }
        let live = board.board_public_reads.unwrap_or(false) || publish;
        Ok(json!({ "ok": true, "deployed": deployed, "live": live }))
    }

    async fn space_files(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        let _ = self.board(&id, command, SPACE_USAGE).await?;
        let verb = args.positional().unwrap_or_default();
        let storage = platform_core::storage(self.cx);
        match verb.as_str() {
            "list" => {
                self.guard(KeyRole::Reader)?;
                let root = args.val_after("--root").unwrap_or_else(|| "staging".to_string());
                let root = if root == "dist" { "dist" } else { "staging" };
                let prefix = if root == "dist" { files::dist_key(&id, "") } else { files::staging_key(&id, "") };
                let keys = storage.list(&prefix).await.unwrap_or_default();
                Ok(json!({ "ok": true, "root": root, "files": keys }))
            }
            "upload" => {
                self.guard(KeyRole::Admin)?;
                let Some(path) = args.positional().and_then(|p| files::sanitize_build_path(&p)) else {
                    return self.err("missing or invalid <path>", command, SPACE_USAGE, "relative path, e.g. index.html or assets/app.js");
                };
                let Some(content) = args.val_after("--content") else {
                    return self.err("missing --content <base64>", command, SPACE_USAGE, "base64 of the file bytes");
                };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|_| err_json("--content is not valid base64", command, SPACE_USAGE, ""))?;
                storage
                    .put(&files::staging_key(&id, &path), Bytes::from(bytes), files::content_type(&path))
                    .await
                    .map_err(|e| err_json(&format!("upload failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "staged": path }))
            }
            "remove" => {
                self.guard(KeyRole::Admin)?;
                let Some(path) = args.positional().and_then(|p| files::sanitize_build_path(&p)) else {
                    return self.err("missing or invalid <path>", command, SPACE_USAGE, "relative path, e.g. assets/app.js");
                };
                storage
                    .delete(&files::staging_key(&id, &path))
                    .await
                    .map_err(|e| err_json(&format!("remove failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "removed": path }))
            }
            _ => self.err("unknown verb for `space files`", command, SPACE_USAGE, "verbs: list, upload, remove — `cli help space files`"),
        }
    }

    async fn space_media(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        let board = self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let storage = platform_core::storage(self.cx);
        let db = platform_core::db(self.cx);
        let mut icon = board.board_icon_key.clone().unwrap_or_default();
        let mut banner = board.board_banner_key.clone().unwrap_or_default();
        let mut gallery: Vec<String> = board
            .board_gallery_keys
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        if args.has("--clear-gallery") {
            for key in &gallery {
                storage.delete(key).await.ok();
            }
            gallery.clear();
        }
        let icon_b64 = args.val_after("--icon");
        let banner_b64 = args.val_after("--banner");
        if icon_b64.is_none() && banner_b64.is_none() && args.all_flags("--gallery").is_empty() {
            return self.err("nothing to set — supply --icon, --banner and/or --gallery", command, SPACE_USAGE, "e.g. `cli space media --gallery <b64>`");
        }
        if let Some(b64) = icon_b64 {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| err_json("--icon is not valid base64", command, SPACE_USAGE, ""))?;
            let key = format!("srv/{id}/icon.png");
            if !icon.is_empty() && icon != key {
                storage.delete(&icon).await.ok();
            }
            storage.put(&key, Bytes::from(bytes), "image/png").await.map_err(|e| err_json(&format!("icon upload failed: {e}"), command, SPACE_USAGE, ""))?;
            icon = key;
        }
        if let Some(b64) = banner_b64 {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| err_json("--banner is not valid base64", command, SPACE_USAGE, ""))?;
            let key = format!("srv/{id}/banner.png");
            if !banner.is_empty() && banner != key {
                storage.delete(&banner).await.ok();
            }
            storage.put(&key, Bytes::from(bytes), "image/png").await.map_err(|e| err_json(&format!("banner upload failed: {e}"), command, SPACE_USAGE, ""))?;
            banner = key;
        }
        for (index, b64) in args.all_flags("--gallery").into_iter().enumerate() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| err_json("--gallery value is not valid base64", command, SPACE_USAGE, ""))?;
            let key = format!("srv/{id}/gallery-{}.png", gallery.len() + index);
            storage.put(&key, Bytes::from(bytes), "image/png").await.map_err(|e| err_json(&format!("gallery upload failed: {e}"), command, SPACE_USAGE, ""))?;
            gallery.push(key);
        }
        let query = models::set_board_media(id.clone(), icon.clone(), banner.clone(), serde_json::to_string(&gallery).unwrap_or_else(|_| "[]".to_string()))
            .map_err(|e| err_json(&format!("media update failed: {e}"), command, SPACE_USAGE, ""))?;
        let _: serde_json::Value = db.query(query).await.map_err(|e| err_json(&format!("media update failed: {e}"), command, SPACE_USAGE, ""))?;
        Ok(json!({ "ok": true, "boardId": id, "icon": icon, "banner": banner, "gallery": gallery }))
    }

    async fn space_delete(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        let board = self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let db = platform_core::db(self.cx);
        let storage = platform_core::storage(self.cx);
        let tenant = board.tenant_id.unwrap_or(NO_TENANT);
        let query = models::tombstone_board(id.clone(), tenant).map_err(|e| err_json(&format!("delete failed: {e}"), command, SPACE_USAGE, ""))?;
        let _: serde_json::Value = db.query(query).await.map_err(|e| err_json(&format!("delete failed: {e}"), command, SPACE_USAGE, ""))?;
        self.store
            .board_delete(NO_TENANT, &id)
            .await
            .map_err(|e| err_json(&format!("delete failed: {e}"), command, SPACE_USAGE, ""))?;
        let prefix = format!("srv/{id}/");
        for key in storage.list(&prefix).await.unwrap_or_default() {
            storage.delete(&key).await.ok();
        }
        Ok(json!({ "ok": true, "deleted": id }))
    }

    async fn records_insert(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Writer)?;
        let Some(payload_raw) = args.positional() else {
            return self.err("missing '<json>' payload", command, RECORDS_USAGE, "single-quoted JSON, e.g. '{\"email\":\"a@b.c\"}'");
        };
        let payload: Value = serde_json::from_str(&payload_raw)
            .map_err(|e| err_json(&format!("payload is not valid JSON: {e}"), command, RECORDS_USAGE, ""))?;
        let writer = writer_of(&Some(borrow_auth(&self.auth)));
        let seq = self
            .store
            .record_insert(NO_TENANT, &id, &payload, writer.as_deref(), args.has("--upsert"))
            .await
            .map_err(|e| err_json(&format!("insert failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "seq": seq, "boardId": id, "upsert": args.has("--upsert") }))
    }

    async fn records_list(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let limit = args.val_after("--limit").and_then(|v| v.parse::<i64>().ok()).unwrap_or(25).clamp(1, 500);
        let offset = args.val_after("--offset").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let items = self
            .store
            .record_list(NO_TENANT, &id, limit, None, offset)
            .await
            .map_err(|e| err_json(&format!("list failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "items": items }))
    }

    async fn records_get(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let Some(seq) = args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()) else {
            return self.err("missing --seq N", command, RECORDS_USAGE, "e.g. --seq 3");
        };
        match self.store.record_get(NO_TENANT, &id, seq).await {
            Ok(Some(rec)) => Ok(json!({ "ok": true, "record": rec })),
            Ok(None) => self.err("no such record", command, RECORDS_USAGE, "run `cli records list --space <id>` to see seqs"),
            Err(e) => self.err(&format!("get failed: {e}"), command, RECORDS_USAGE, ""),
        }
    }

    async fn records_delete(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Writer)?;
        if let Some(filter_raw) = args.val_after("--filter") {
            let filter_value: Value = serde_json::from_str(&filter_raw)
                .map_err(|e| err_json(&format!("filter is not valid JSON: {e}"), command, RECORDS_USAGE, ""))?;
            let parsed = crate::dsl::parse_filter(&filter_value)
                .map_err(|e| err_json(&format!("bad filter: {e}"), command, RECORDS_USAGE, ""))?;
            let deleted = self
                .store
                .record_delete_filter(&id, &parsed.conds)
                .await
                .map_err(|e| err_json(&format!("delete failed: {e}"), command, RECORDS_USAGE, ""))?;
            return Ok(json!({ "ok": true, "deleted": deleted, "filter": true }));
        }
        let Some(seq) = args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()) else {
            return self.err("need --seq N or --filter '<json>'", command, RECORDS_USAGE, "e.g. --seq 3 or --filter '[{\"field\":\"a\",\"op\":\"eq\",\"value\":1}]'");
        };
        let deleted = self
            .store
            .record_delete(NO_TENANT, &id, seq)
            .await
            .map_err(|e| err_json(&format!("delete failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "deleted": deleted }))
    }

    async fn records_count(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let count = self.store.record_count(NO_TENANT, &id).await.unwrap_or(0);
        Ok(json!({ "ok": true, "count": count }))
    }

    async fn keys_issue(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, KEYS_USAGE)?;
        self.board(&id, command, KEYS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let role = match args.val_after("--role").as_deref() {
            Some("reader") => KeyRole::Reader,
            Some("writer") => KeyRole::Writer,
            Some("admin") => KeyRole::Admin,
            _ => return self.err("missing or invalid --role", command, KEYS_USAGE, "reader|writer|admin"),
        };
        let writer = args.val_after("--writer");
        let (bucket, secret) = self
            .store
            .key_issue(NO_TENANT, &id, role, writer.as_deref())
            .await
            .map_err(|e| err_json(&format!("key issue failed: {e}"), command, KEYS_USAGE, ""))?;
        Ok(json!({ "ok": true, "bucket": bucket, "token": format!("{id}:{bucket}:{secret}"), "note": "shown once" }))
    }

    async fn keys_revoke(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, KEYS_USAGE)?;
        self.board(&id, command, KEYS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let Some(bucket) = args.val_after("--bucket") else {
            return self.err("missing --bucket <bucket>", command, KEYS_USAGE, "run `cli keys list --space <id>` to see buckets");
        };
        let revoked = self
            .store
            .key_revoke(NO_TENANT, &id, &bucket)
            .await
            .map_err(|e| err_json(&format!("revoke failed: {e}"), command, KEYS_USAGE, ""))?;
        Ok(json!({ "ok": true, "revoked": revoked }))
    }

    async fn keys_list(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, KEYS_USAGE)?;
        self.board(&id, command, KEYS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let keys = self
            .store
            .key_list(NO_TENANT, &id)
            .await
            .map_err(|e| err_json(&format!("list failed: {e}"), command, KEYS_USAGE, ""))?;
        Ok(json!({ "ok": true, "keys": keys }))
    }

    async fn hooks_register(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, HOOKS_USAGE)?;
        self.board(&id, command, HOOKS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let Some(url) = args.positional().filter(|u| u.starts_with("http")) else {
            return self.err("missing or invalid <url>", command, HOOKS_USAGE, "http(s) URL, e.g. https://example.com/hook");
        };
        let secret = args.val_after("--secret");
        self.store
            .hook_register(NO_TENANT, &id, &url, secret.as_deref())
            .await
            .map_err(|e| err_json(&format!("hook failed: {e}"), command, HOOKS_USAGE, ""))?;
        Ok(json!({ "ok": true, "url": url }))
    }

    // ---- records: set / patch / query / filter / search / aggregate ----

    async fn records_set(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Writer)?;
        let Some(seq) = args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()) else {
            return self.err("missing --seq N", command, RECORDS_USAGE, "e.g. --seq 3");
        };
        let Some(payload_raw) = args.positional() else {
            return self.err("missing '<json>' payload", command, RECORDS_USAGE, "single-quoted JSON, e.g. '{\"a\":1}'");
        };
        let payload: Value = serde_json::from_str(&payload_raw)
            .map_err(|e| err_json(&format!("payload is not valid JSON: {e}"), command, RECORDS_USAGE, ""))?;
        self.store
            .record_set(NO_TENANT, &id, seq, &payload, None)
            .await
            .map_err(|e| err_json(&format!("set failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "seq": seq }))
    }

    async fn records_patch(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Writer)?;
        let Some(seq) = args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()) else {
            return self.err("missing --seq N", command, RECORDS_USAGE, "e.g. --seq 3");
        };
        let Some(patch_raw) = args.positional() else {
            return self.err("missing '<patch>' json", command, RECORDS_USAGE, "e.g. '{\"$set\":{\"name\":\"b\"},\"$inc\":{\"n\":1}}'");
        };
        let patch: Value = serde_json::from_str(&patch_raw)
            .map_err(|e| err_json(&format!("patch is not valid JSON: {e}"), command, RECORDS_USAGE, ""))?;
        let updated = self
            .store
            .record_patch(&id, seq, &patch, None)
            .await
            .map_err(|e| err_json(&format!("patch failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "seq": seq, "payload": updated }))
    }

    async fn records_query(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let Some(path) = args.val_after("--path").filter(|p| !p.is_empty()) else {
            return self.err("missing --path a.b", command, RECORDS_USAGE, "a JSON path, e.g. --path profile.name");
        };
        let eq = args.val_after("--eq").and_then(|v| serde_json::from_str(&v).ok());
        let limit = args.val_after("--limit").and_then(|v| v.parse::<i64>().ok()).unwrap_or(25).clamp(1, 500);
        let offset = args.val_after("--offset").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let items = self
            .store
            .record_query(&id, &path, eq.as_ref(), limit, offset)
            .await
            .map_err(|e| err_json(&format!("query failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "path": path, "items": items }))
    }

    async fn records_filter(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let Some(filter_raw) = args.val_after("--filter") else {
            return self.err("missing --filter '<json>'", command, RECORDS_USAGE, "[{\"field\":\"a.b\",\"op\":\"gt\",\"value\":100}]");
        };
        let filter_value: Value = serde_json::from_str(&filter_raw)
            .map_err(|e| err_json(&format!("filter is not valid JSON: {e}"), command, RECORDS_USAGE, ""))?;
        let parsed = crate::dsl::parse_filter(&filter_value)
            .map_err(|e| err_json(&format!("bad filter: {e}"), command, RECORDS_USAGE, ""))?;
        let desc = !args.has("--asc");
        let orders = args.val_after("--order").filter(|o| !o.is_empty()).map(|o| vec![(o, desc)]).unwrap_or_default();
        let limit = args.val_after("--limit").and_then(|v| v.parse::<i64>().ok()).unwrap_or(25).clamp(1, 500);
        let offset = args.val_after("--offset").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let items = self
            .store
            .record_filter(&id, &parsed.conds, &orders, limit, offset)
            .await
            .map_err(|e| err_json(&format!("filter failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "items": items }))
    }

    async fn records_search(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let Some(q) = args.val_after("--q").filter(|q| !q.is_empty()) else {
            return self.err("missing --q <text>", command, RECORDS_USAGE, "the text to search for");
        };
        let limit = args.val_after("--limit").and_then(|v| v.parse::<i64>().ok()).unwrap_or(25).clamp(1, 500);
        let offset = args.val_after("--offset").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let items = self
            .store
            .record_search(&id, &q, args.has("--hl"), limit, offset)
            .await
            .map_err(|e| err_json(&format!("search failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "q": q, "items": items }))
    }

    async fn records_aggregate(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Reader)?;
        let Some(op) = args.val_after("--op").as_deref().and_then(crate::dsl::Agg::parse) else {
            return self.err("missing or invalid --op", command, RECORDS_USAGE, "count|sum|avg|min|max");
        };
        let field = args.val_after("--field").unwrap_or_default();
        let group_by = args.val_after("--group-by").filter(|g| !g.is_empty());
        let conds = match args.val_after("--filter") {
            Some(raw) => {
                let value: Value = serde_json::from_str(&raw)
                    .map_err(|e| err_json(&format!("filter is not valid JSON: {e}"), command, RECORDS_USAGE, ""))?;
                crate::dsl::parse_filter(&value)
                    .map_err(|e| err_json(&format!("bad filter: {e}"), command, RECORDS_USAGE, ""))?
                    .conds
            }
            None => Vec::new(),
        };
        let result = self
            .store
            .record_aggregate(&id, op, &field, group_by.as_deref(), &conds)
            .await
            .map_err(|e| err_json(&format!("aggregate failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "result": result }))
    }

    async fn records_bulk(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Writer)?;
        let mut payloads = Vec::new();
        while let Some(raw) = args.positional() {
            let payload: Value = serde_json::from_str(&raw)
                .map_err(|e| err_json(&format!("'{raw}' is not valid JSON: {e}"), command, RECORDS_USAGE, ""))?;
            payloads.push(payload);
        }
        if payloads.is_empty() {
            return self.err("missing '<json>' payloads", command, RECORDS_USAGE, "one or more single-quoted JSON objects");
        }
        let writer = writer_of(&Some(borrow_auth(&self.auth)));
        let seqs = self
            .store
            .record_bulk_insert(&id, payloads, writer.as_deref())
            .await
            .map_err(|e| err_json(&format!("bulk failed: {e}"), command, RECORDS_USAGE, ""))?;
        Ok(json!({ "ok": true, "seqs": seqs, "count": seqs.len() }))
    }

    async fn records_call(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, RECORDS_USAGE)?;
        self.board(&id, command, RECORDS_USAGE).await?;
        self.guard(KeyRole::Writer)?;
        let Some(url) = args.positional().filter(|u| crate::netguard::valid_url(u)) else {
            return self.err("missing or unsafe <url>", command, RECORDS_USAGE, "public http(s) URL only (SSRF-guarded)");
        };
        let method = args.val_after("--method").unwrap_or_else(|| "POST".to_string()).to_uppercase();
        let Ok(method) = reqwest::Method::from_bytes(method.as_bytes()) else {
            return self.err("invalid --method", command, RECORDS_USAGE, "GET|POST|PUT|PATCH|DELETE");
        };
        let timeout_ms = args.val_after("--timeout-ms").and_then(|v| v.parse::<u64>().ok()).unwrap_or(10_000).clamp(500, 60_000);
        let mut builder = reqwest::Client::new()
            .request(method, &url)
            .timeout(std::time::Duration::from_millis(timeout_ms));
        if let Some(body) = args.val_after("--body") {
            if let Ok(value) = serde_json::from_str::<Value>(&body) {
                builder = builder.body(value.to_string());
            } else {
                builder = builder.body(body);
            }
        }
        match builder.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                Ok(json!({ "ok": true, "url": url, "status": status, "body": text }))
            }
            Err(e) => self.err(&format!("call failed: {e}"), command, RECORDS_USAGE, ""),
        }
    }

    // ---- space: config / jobs / ttl / audit / rate / link / dump / restore

    async fn space_config(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        let sub = args.positional();
        let clearing = sub.as_deref() == Some("clear");
        match sub.as_deref() {
            Some("set") | Some("clear") => {
                let mut touched: Vec<String> = Vec::new();
                for (flag, key) in [
                    ("--computed", "computed_json"),
                    ("--validate", "validate_json"),
                    ("--redact", "redact_json"),
                ] {
                    let value: Option<Result<Option<Value>, Value>> = if clearing {
                        if args.has(flag) { Some(Ok(None)) } else { None }
                    } else {
                        args.val_after(flag).map(|v| {
                            if v == "null" {
                                Ok(None)
                            } else {
                                serde_json::from_str::<Value>(&v)
                                    .map(Some)
                                    .map_err(|e| err_json(&format!("{flag} is not valid JSON: {e}"), command, SPACE_USAGE, ""))
                            }
                        })
                    };
                    let Some(v) = value else { continue };
                    let v = v?;
                    self.store
                        .board_config_set(&id, key, v.as_ref())
                        .await
                        .map_err(|e| err_json(&format!("config failed: {e}"), command, SPACE_USAGE, ""))?;
                    touched.push(flag.trim_start_matches("--").to_string());
                }
                if touched.is_empty() {
                    return self.err(
                        if clearing { "need at least one of --computed/--validate/--redact" } else { "need --computed/--validate/--redact '<json>' (or 'null' to clear)" },
                        command,
                        SPACE_USAGE,
                        "cli space config set --computed '{\"total\":\"$.price * $.qty\"}'",
                    );
                }
                Ok(json!({ "ok": true, "action": if clearing { "clear" } else { "set" }, "keys": touched }))
            }
            _ => {
                let config = self
                    .store
                    .board_config(&id)
                    .await
                    .map_err(|e| err_json(&format!("config failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "config": config }))
            }
        }
    }

    async fn space_recipes(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        match args.positional().as_deref() {
            Some("list") => {
                let recipes = self
                    .store
                    .recipe_list(&id)
                    .await
                    .map_err(|e| err_json(&format!("recipes failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "recipes": recipes }))
            }
            Some("add") => {
                let (Some(name), Some(when)) = (
                    args.val_after("--name").filter(|n| !n.is_empty()),
                    args.val_after("--when").filter(|w| !w.is_empty()),
                ) else {
                    return self.err("need --name and --when", command, SPACE_USAGE, "e.g. --when record.created or --when cron:ping");
                };
                let match_json = match args.val_after("--match") {
                    Some(m) => Some(
                        serde_json::from_str::<Value>(&m)
                            .map_err(|e| err_json(&format!("--match is not valid JSON: {e}"), command, SPACE_USAGE, ""))?,
                    ),
                    None => None,
                };
                let actions = args
                    .val_after("--actions")
                    .map(|a| {
                        serde_json::from_str::<Value>(&a)
                            .map_err(|e| err_json(&format!("--actions is not valid JSON: {e}"), command, SPACE_USAGE, ""))
                    })
                    .transpose()?
                    .unwrap_or_else(|| Value::Array(Vec::new()));
                let enabled = !args.has("--disabled");
                let id_row = self
                    .store
                    .recipe_add(
                        &id,
                        &name,
                        &when,
                        match_json.as_ref().map(Value::to_string).as_deref(),
                        enabled,
                        args.val_after("--dedup-on").as_deref(),
                        &actions.to_string(),
                    )
                    .await
                    .map_err(|e| err_json(&format!("recipe add failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "id": id_row, "name": name }))
            }
            Some(verb @ ("enable" | "disable")) => {
                let Some(name) = args.val_after("--name").filter(|n| !n.is_empty()) else {
                    return self.err("need --name", command, SPACE_USAGE, "cli space recipes enable --name n");
                };
                let found = self
                    .store
                    .recipe_set_enabled(&id, &name, verb == "enable")
                    .await
                    .map_err(|e| err_json(&format!("recipe failed: {e}"), command, SPACE_USAGE, ""))?;
                if !found {
                    return self.err("recipe not found", command, SPACE_USAGE, "cli space recipes list");
                }
                Ok(json!({ "ok": true, "name": name, "enabled": verb == "enable" }))
            }
            Some("remove") => {
                let Some(name) = args.val_after("--name").filter(|n| !n.is_empty()) else {
                    return self.err("need --name", command, SPACE_USAGE, "cli space recipes remove --name n");
                };
                let found = self
                    .store
                    .recipe_remove(&id, &name)
                    .await
                    .map_err(|e| err_json(&format!("recipe failed: {e}"), command, SPACE_USAGE, ""))?;
                if !found {
                    return self.err("recipe not found", command, SPACE_USAGE, "cli space recipes list");
                }
                Ok(json!({ "ok": true, "name": name }))
            }
            _ => self.err("recipes needs list|add|enable|disable|remove", command, SPACE_USAGE, "cli space recipes list"),
        }
    }

    async fn space_secrets(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        match args.positional().as_deref() {
            Some("list") => {
                let secrets = self
                    .store
                    .secret_list(&id)
                    .await
                    .map_err(|e| err_json(&format!("secrets failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "secrets": secrets }))
            }
            Some("set") => {
                let (Some(name), Some(value)) = (
                    args.val_after("--name").filter(|n| !n.is_empty()),
                    args.val_after("--value").or_else(|| args.next().map(str::to_owned)).filter(|v| !v.is_empty()),
                ) else {
                    return self.err("need --name and --value", command, SPACE_USAGE, "cli space secrets set --name KEY --value <secret>");
                };
                self.store
                    .secret_set(&id, &name, &value)
                    .await
                    .map_err(|e| err_json(&format!("secret failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "name": name }))
            }
            Some("remove") => {
                let Some(name) = args.val_after("--name").filter(|n| !n.is_empty()) else {
                    return self.err("need --name", command, SPACE_USAGE, "cli space secrets remove --name KEY");
                };
                let found = self
                    .store
                    .secret_remove(&id, &name)
                    .await
                    .map_err(|e| err_json(&format!("secret failed: {e}"), command, SPACE_USAGE, ""))?;
                if !found {
                    return self.err("secret not found", command, SPACE_USAGE, "cli space secrets list");
                }
                Ok(json!({ "ok": true, "name": name }))
            }
            _ => self.err("secrets needs list|set|remove", command, SPACE_USAGE, "cli space secrets list"),
        }
    }

    async fn space_jobs(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        match args.positional().as_deref() {
            Some("add") => {
                let (Some(name), Some(schedule), Some(action_raw)) = (
                    args.val_after("--name").filter(|n| !n.is_empty()),
                    args.val_after("--schedule").filter(|s| !s.is_empty()),
                    args.val_after("--action"),
                ) else {
                    return self.err("need --name, --schedule and --action", command, SPACE_USAGE, "e.g. --action '{\"type\":\"http\",\"url\":\"https://...\"}'");
                };
                let action: Value = serde_json::from_str(&action_raw)
                    .map_err(|e| err_json(&format!("action is not valid JSON: {e}"), command, SPACE_USAGE, ""))?;
                let job_id = self
                    .store
                    .job_add(&id, &name, &schedule, &action)
                    .await
                    .map_err(|e| err_json(&format!("job add failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "id": job_id }))
            }
            Some("list") => {
                let jobs = self
                    .store
                    .job_list(&id)
                    .await
                    .map_err(|e| err_json(&format!("jobs failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "jobs": jobs }))
            }
            Some("remove") => {
                let Some(job_id) = args.val_after("--id").and_then(|v| v.parse::<i64>().ok()) else {
                    return self.err("missing --id N", command, SPACE_USAGE, "run `cli space jobs list` to see ids");
                };
                let removed = self
                    .store
                    .job_remove(&id, job_id)
                    .await
                    .map_err(|e| err_json(&format!("job remove failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "removed": removed }))
            }
            _ => self.err("jobs needs add|list|remove", command, SPACE_USAGE, "cli space jobs add|list|remove"),
        }
    }

    async fn space_ttl(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        match args.positional().as_deref() {
            Some("set") => {
                let (Some(seconds), Some(field)) = (
                    args.val_after("--seconds").and_then(|v| v.parse::<i64>().ok()),
                    args.val_after("--field").filter(|f| !f.is_empty()),
                ) else {
                    return self.err("need --seconds N and --field <path>", command, SPACE_USAGE, "e.g. --seconds 3600 --field expires_at");
                };
                if seconds <= 0 {
                    return self.err("--seconds must be positive", command, SPACE_USAGE, "");
                }
                self.store
                    .ttl_set(&id, seconds, &field)
                    .await
                    .map_err(|e| err_json(&format!("ttl failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "seconds": seconds, "field": field }))
            }
            Some("clear") => {
                self.store
                    .ttl_clear(&id)
                    .await
                    .map_err(|e| err_json(&format!("ttl failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "ttl": "disabled" }))
            }
            _ => self.err("ttl needs set|clear", command, SPACE_USAGE, "cli space ttl set|clear"),
        }
    }

    async fn space_audit(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        match args.positional().as_deref() {
            Some(sub) if sub == "on" || sub == "off" => {
                let enabled = sub == "on";
                self.store
                    .audit_enable(&id, enabled)
                    .await
                    .map_err(|e| err_json(&format!("audit failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "enabled": enabled }))
            }
            Some("list") => {
                let limit = args.val_after("--limit").and_then(|v| v.parse::<i64>().ok()).unwrap_or(50).clamp(1, 1000);
                let items = self
                    .store
                    .audit_list(&id, limit)
                    .await
                    .map_err(|e| err_json(&format!("audit failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "items": items }))
            }
            _ => self.err("audit needs on|off|list", command, SPACE_USAGE, "cli space audit on|off|list"),
        }
    }

    async fn space_rate(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        match args.positional().as_deref() {
            Some("set") => {
                let Some(per_minute) = args.val_after("--per-minute").and_then(|v| v.parse::<i64>().ok()) else {
                    return self.err("missing --per-minute N", command, SPACE_USAGE, "requests allowed per minute");
                };
                if per_minute <= 0 {
                    return self.err("--per-minute must be positive", command, SPACE_USAGE, "");
                }
                self.store
                    .rate_set(&id, per_minute)
                    .await
                    .map_err(|e| err_json(&format!("rate failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "per_minute": per_minute }))
            }
            Some("clear") => {
                self.store
                    .rate_set(&id, 0)
                    .await
                    .map_err(|e| err_json(&format!("rate failed: {e}"), command, SPACE_USAGE, ""))?;
                self.store.rate_reset(&id).await;
                Ok(json!({ "ok": true, "per_minute": 0 }))
            }
            _ => self.err("rate needs set|clear", command, SPACE_USAGE, "cli space rate set --per-minute N|clear"),
        }
    }

    async fn space_link(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        match args.positional().as_deref() {
            Some("set") => {
                self.guard(KeyRole::Writer)?;
                let (Some(seq), Some(parent), Some(via)) = (
                    args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()),
                    args.val_after("--parent").and_then(|v| v.parse::<i64>().ok()),
                    args.val_after("--via").filter(|v| !v.is_empty()),
                ) else {
                    return self.err("need --seq N --parent M --via x", command, SPACE_USAGE, "");
                };
                self.store
                    .link_set(&id, seq, parent, &via)
                    .await
                    .map_err(|e| err_json(&format!("link failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "seq": seq, "parent": parent, "via": via }))
            }
            Some("clear") => {
                self.guard(KeyRole::Writer)?;
                let (Some(seq), Some(via)) = (
                    args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()),
                    args.val_after("--via").filter(|v| !v.is_empty()),
                ) else {
                    return self.err("need --seq N --via x", command, SPACE_USAGE, "");
                };
                let cleared = self
                    .store
                    .link_clear(&id, seq, &via)
                    .await
                    .map_err(|e| err_json(&format!("link failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "cleared": cleared }))
            }
            Some("show") => {
                self.guard(KeyRole::Reader)?;
                let Some(seq) = args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()) else {
                    return self.err("missing --seq N", command, SPACE_USAGE, "");
                };
                let links = self
                    .store
                    .link_get(&id, seq)
                    .await
                    .map_err(|e| err_json(&format!("links failed: {e}"), command, SPACE_USAGE, ""))?;
                Ok(json!({ "ok": true, "seq": seq, "links": links }))
            }
            _ => self.err("link needs set|clear|show", command, SPACE_USAGE, "cli space link set|clear|show"),
        }
    }

    async fn space_dump(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let dump = self
            .store
            .record_dump(&id)
            .await
            .map_err(|e| err_json(&format!("dump failed: {e}"), command, SPACE_USAGE, ""))?;
        Ok(json!({ "ok": true, "dump": dump }))
    }

    async fn space_restore(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, SPACE_USAGE)?;
        self.board(&id, command, SPACE_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let Some(dump_raw) = args.val_after("--dump") else {
            return self.err("missing --dump '<json>'", command, SPACE_USAGE, "paste the output of `cli space dump`");
        };
        let dump: Value = serde_json::from_str(&dump_raw)
            .map_err(|e| err_json(&format!("dump is not valid JSON: {e}"), command, SPACE_USAGE, ""))?;
        let counts = self
            .store
            .record_restore(&id, &dump)
            .await
            .map_err(|e| err_json(&format!("restore failed: {e}"), command, SPACE_USAGE, ""))?;
        Ok(json!({ "ok": true, "counts": counts }))
    }

    // ---- hooks: list / remove / deliveries / replay

    async fn hooks_list(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, HOOKS_USAGE)?;
        self.board(&id, command, HOOKS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let hooks = self
            .store
            .hook_list(NO_TENANT, &id)
            .await
            .map_err(|e| err_json(&format!("list failed: {e}"), command, HOOKS_USAGE, ""))?;
        Ok(json!({ "ok": true, "hooks": hooks }))
    }

    async fn hooks_remove(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, HOOKS_USAGE)?;
        self.board(&id, command, HOOKS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let Some(url) = args.val_after("--url").filter(|u| u.starts_with("http")) else {
            return self.err("missing --url <url>", command, HOOKS_USAGE, "run `cli hooks list` to see urls");
        };
        let removed = self
            .store
            .hook_remove(NO_TENANT, &id, &url)
            .await
            .map_err(|e| err_json(&format!("remove failed: {e}"), command, HOOKS_USAGE, ""))?;
        Ok(json!({ "ok": true, "removed": removed }))
    }

    async fn hooks_deliveries(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, HOOKS_USAGE)?;
        self.board(&id, command, HOOKS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let status = args.val_after("--status").filter(|s| !s.is_empty());
        let limit = args.val_after("--limit").and_then(|v| v.parse::<i64>().ok()).unwrap_or(50).clamp(1, 500);
        let items = self
            .store
            .delivery_list(&id, status.as_deref(), limit)
            .await
            .map_err(|e| err_json(&format!("deliveries failed: {e}"), command, HOOKS_USAGE, ""))?;
        Ok(json!({ "ok": true, "items": items }))
    }

    async fn hooks_replay(&self, args: &mut Args<'_>, command: &str) -> CmdResult {
        let id = self.space_arg(args, command, HOOKS_USAGE)?;
        self.board(&id, command, HOOKS_USAGE).await?;
        self.guard(KeyRole::Admin)?;
        let id_filter = args.val_after("--id").and_then(|v| v.parse::<i64>().ok());
        let requeued = self
            .store
            .delivery_replay(&id, id_filter)
            .await
            .map_err(|e| err_json(&format!("replay failed: {e}"), command, HOOKS_USAGE, ""))?;
        Ok(json!({ "ok": true, "requeued": requeued }))
    }
}
