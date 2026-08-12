# Spaces MCP — quick connect guide (for agents + curl)

The LANsystem serverless spaces MCP exposes **one tool**: `cli` (groups:
`space`, `records`, `keys`, `hooks`). Every command takes
`<group> <verb> [args]` and manages the space in your `x-srv-key`.

## 1. What you need (one string)

`<space_id>:<bucket>:<secret>` — e.g. `sl-19ff0182674-0000:owner:abc…` or a
scoped key `sl-19ff0182674-0000:k-1e76b…:097ad5…`.

- No space yet? Open `http://127.0.0.1:4321/sl`, create one, copy the
  one-time key from the redirect URL. Scoped keys: `cli keys issue` or the
  Keys panel.
- All help layers are **public** (no key): `cli`, `cli about`,
  `cli help space`, `cli help records search`, `GET /api/srv/mcp/guide`.

## 2. Endpoints

| URL | What it is |
|---|---|
| `http://127.0.0.1:4321/mcp` | native MCP over HTTP (opencode, Claude Code, curl) |
| `http://127.0.0.1:4321/api/srv/mcp` | REST envelope (`{"tool":"cli","params":{"args":[…]}}`) |
| `http://192.168.88.101:4321/mcp` | same, from other LAN machines (host: `IICO`) |
| `http://<your-domain>/mcp` | same, behind a reverse proxy |

Note: `deploy/docker-compose.yml` publishes `127.0.0.1:4321` only — for the
LAN IP/domain to work, expose the port or proxy it.

## 3. curl (native MCP, JSON-RPC)

```bash
KEY='sl-19ff0182674-0000:k-1e76b…:097ad5…'        # your space key string
URL='http://127.0.0.1:4321/mcp'                    # or http://192.168.88.101:4321/mcp

# handshake + list tools (public, no key needed)
curl -s $URL -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}'

curl -s $URL -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

# run a command (key required)
curl -s $URL -H 'content-type: application/json' -H "x-srv-key: $KEY" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cli","arguments":{"args":["space","show"]}}}'

# records search with highlight, from the CLI tool:
# ... "arguments":{"args":["records","search","--q","alice","--hl"]}
```

## 4. curl (REST envelope)

```bash
curl -s $URL -H 'content-type: application/json' -H "x-srv-key: $KEY" \
  -d '{"tool":"cli","params":{"args":["space","show"]}}'
```

## 5. opencode config

Already in the repo (`opencode.json`): a `lansrv-spaces` remote server at
`http://127.0.0.1:4321/mcp` with `x-srv-key` from `LAN_SRV_KEY`. To use it:

```bash
export LAN_SRV_KEY='sl-19ff0182674-0000:k-1e76b…:097ad5…'   # in your shell
# then quit and restart opencode
```

## 6. Tell the agent to start using it

Say, in the chat:

> Use the `lansrv-spaces` MCP server for anything about my serverless
> spaces: first `cli about` and `cli help space`, then `cli space show`
> (my space is `sl-19ff0182674-0000`), and manage records, jobs, hooks,
> keys, ttl, audit, rate, links, dump/restore through the `cli` tool.

The agent then drives the space like any MCP tool — no curl needed.

## 7. First verbs to try

```text
cli space show                          # board meta + record count
cli space config                        # ttl / rate / audit / unique key
cli records insert '{"name":"alice","price":10}'
cli records search --q alice --hl
cli records filter --filter '[{"field":"price","op":"gt","value":5}]' --order price
cli records aggregate --op sum --field price --group-by name
cli jobs add --name ping --schedule '@every 30s' --action '{"url":"https://example.com"}'
cli hooks register https://my.app/hook --secret s3cret
cli keys issue --role writer --writer my-agent
cli space dump
```
