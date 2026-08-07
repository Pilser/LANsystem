# Deploying LANsystem — local stack in Docker

The HelixDB graph engine and its MinIO object store are provisioned by the
**helix CLI** (`helix start LANsystem`) — no host installs, containers only.
The LAN application binary runs **inside Docker too**, attached to the same
network, so it reaches the DB and MinIO over the private network (MinIO is not
published on the host).

```
                        docker network: helix-LANsystem-LANsystem-net
┌────────────────┐      ┌──────────────────────────────┐
│  deploy-lan-1  │ ───► │  helix-LANsystem-LANsystem   │  (host :6969)
│  LAN (topcoat) │      │  HelixDB graph engine :8080  │
│  ubuntu:26.04  │      └──────────────┬───────────────┘
│  binary        │                     │ S3
│  mounted from  │      ┌──────────────▼───────────────┐
│  host          │ ───► │ helix-LANsystem-LANsystem-   │
└────────────────┘      │ minio :9000 (buckets: helix- │
                        │ db, lan)                     │
                        └──────────────────────────────┘
```

## 0. Prerequisites

- Docker daemon running.
- Rust 1.97+ (`cargo`), toolchain pinned by `rust-toolchain.toml`.
- The `helix` CLI (v3) — `curl -sSL "https://install.helix-db.com" | bash`.

## 1. Start HelixDB and its MinIO (helix CLI)

`helix.toml` pins the instance to the current standalone image
(`ghcr.io/helixdb/helixdb:v0.0.3`) with disk persistence. That image serves the
**v2 query contract** (`POST /v2/query`) the SDK produces; the older
`enterprise-dev` image speaks v1 and will 500 the app.

```bash
helix start LANsystem          # pulls helixdb + a minio companion container
helix status                   # LANsystem (local): http://localhost:6969 - storage: disk
curl -fsS http://127.0.0.1:6969/readyz   # "ok"
```

The CLI's own readiness probe may time out with a `did not become ready`
warning on this image — the gateway still comes up fine (see `helix logs
LANsystem` and the `readyz` check above).

## 2. Build the binary and run the app container

```bash
cargo build --release -p lan
docker compose -f deploy/docker-compose.yml up -d
```

`deploy/docker-compose.yml` runs **only the project binary**:

- image `ubuntu:26.04` (glibc 2.43 — matches the host build; `debian:bookworm`
  is too old for the release binary),
- the binary is bind-mounted from `../target/release/lan` (rebuild on host,
  then `docker compose restart`),
- joins the external network `helix-LANsystem-LANsystem-net` created by
  `helix start`,
- env: `HELIX_ENDPOINT=http://helix-LANsystem-LANsystem:8080`,
  `HELIX_QUERY_PATH=v2/query`, and S3 creds from the CLI-launched MinIO
  (`minioadmin`/`minioadmin`, bucket `lan`),
- publishes `127.0.0.1:4321` → app port 3000.

## 3. Check it

```bash
curl -fsS http://127.0.0.1:4321/              # launcher (lists apps)
curl -fsS http://127.0.0.1:4321/media-library # Media Library app
```

Seed media items by POSTing a dynamic write query to the gateway:

```bash
curl -fsS http://127.0.0.1:6969/v2/query \
  -H 'content-type: application/json' \
  -d '{"request_type":"write","steps":[...]}'   # AST per docs/ARCHITECTURE.md
```

A friendlier way is the vendored CLI in this workspace's HelixDB checkout:
`helix query LANsystem -e '…'` or `helix query LANsystem --file examples/request.json`
send the same dynamic queries the app uses.

## 4. Tear down

```bash
docker compose -f deploy/docker-compose.yml down   # stop the app container
helix stop LANsystem                               # stop DB + MinIO
```

## Do I need the topcoat CLI?

**No — not for building or running.** The `#[page]` routes are wired at
link-time via `Router::builder().discover()`; `cargo run -p lan` is enough.
The `topcoat` CLI (`crates/topcoat-cli`) is optional tooling for:

- `topcoat dev` — hot-reload dev server with the app open.
- `topcoat fmt` — formatter for `view!`-macro bodies (rustfmt alone does not
  touch them).
- `topcoat ui` — vendoring showcase UI components into an app.
