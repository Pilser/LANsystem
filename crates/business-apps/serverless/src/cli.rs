//! The single-tool CLI: one MCP tool (`cli`) that speaks a small
//! command-line language — `cli <group> <verb> [args]` — mirroring
//! zerowrapper's `srv` CLI (layered help, `--flag value` parsing).
//! Every task targets the space the `x-srv-key` belongs to: `--space <id>`
//! is optional and must match the key when given. Every verb calls the same
//! store/db/storage paths as the web routes and REST API, so CLI bugs == web
//! bugs. The help layers are fully public: an agent can read the whole
//! documentation without owning a space or a key; only the actions need the
//! space id (which must match the id inside the `x-srv-key` token).

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
  space     show / update / deploy / files / media / delete  the space itself
  records   insert / list / get / delete / count             the space's records
  keys      issue / revoke / list                            scoped API keys
  hooks     register                                          webhooks

the space id is OPTIONAL — your key identifies the space:
  cli space show                      manage this key's space (no --space needed)
  cli space show --space sl-...       same, explicit (must match the key)
find a space's id on the space page (top right, next to the title — copy
button included), or in the create redirect /sl/<space_id>?owner_key=....
auth: pass the `x-srv-key: <space_id>:owner:<secret>` header (or a scoped
`<space_id>:<bucket>:<secret>` key). The HELP layers above need NO key at
all — they are public.";

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
   2. Give your agent two things: the space id and the key. Configure the
      MCP server once (see the guide): url /api/srv/mcp, header
      x-srv-key: <space_id>:owner:<secret> (or an issued scoped token).
      With that header the agent can manage the whole space — every command
      defaults to the key's space, so `cli space show` already works.
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
  delete  [--space <id>]                                 permanently delete the space
--space defaults to the space your key belongs to; pass it explicitly only
to be safe. per-verb help: cli help space <verb>";

const RECORDS_USAGE: &str = "\
usage: cli records <verb> [--space <space_id>] [args]
  insert  [--space <id>] '<json>' [--writer <w>] [--upsert]
  list    [--space <id>] [--limit N] [--offset N]
  get     [--space <id>] --seq N
  delete  [--space <id>] --seq N
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
    /// (enforced by `board`).
    fn space_arg(&self, args: &mut Args<'_>, _command: &str, _usage: &str) -> Result<String, Value> {
        Ok(args
            .val_after("--space")
            .or_else(|| args.val_after("--space-id"))
            .filter(|s| !s.is_empty())
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
            ("records", "insert") => self.records_insert(&mut args, &command).await,
            ("records", "list") => self.records_list(&mut args, &command).await,
            ("records", "get") => self.records_get(&mut args, &command).await,
            ("records", "delete") => self.records_delete(&mut args, &command).await,
            ("records", "count") => self.records_count(&mut args, &command).await,
            ("keys", "issue") => self.keys_issue(&mut args, &command).await,
            ("keys", "revoke") => self.keys_revoke(&mut args, &command).await,
            ("keys", "list") => self.keys_list(&mut args, &command).await,
            ("hooks", "register") => self.hooks_register(&mut args, &command).await,
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
        let Some(seq) = args.val_after("--seq").and_then(|v| v.parse::<i64>().ok()) else {
            return self.err("missing --seq N", command, RECORDS_USAGE, "e.g. --seq 3");
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
}
