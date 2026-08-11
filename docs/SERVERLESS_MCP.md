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
- **Every task asks for the space id as an argument** — and it must match
  the id inside the key. The server re-derives the key's board and never
  trusts the URL/params; a mismatch returns a scoped-error with the usage
  layer attached.

## 3. The `cli` tool (groups + verbs)

All mirror existing web/API capabilities; no new backend logic.

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
