# LANsystem

Local-area-network app platform: a Topcoat (Rust web) launcher that hosts apps
(e.g. the Media Library) backed by a HelixDB graph + S3 object storage, all in
Docker on one bridge network.

## Layout

- `apps/lan/` — entry binary (launcher + app registry)
- `crates/db/` — HelixDB client (query builder, HTTP transport)
- `crates/storage/` — S3 (MinIO) client for app files
- `crates/platform/core/` — service wiring (`Services::connect`)
- `crates/business-apps/` — apps (media) registered via `AppRegistry`
- `deploy/docker-compose.yml` — runs ONLY the app binary in Docker
- `helix.toml` — helix CLI config (DB instance)
- `docs/ARCHITECTURE.md`, `docs/DEPLOY.md`
- `helix-db/`, `topcoat/` — vendored reference repos (own git history, not tracked)

## Run the stack

```bash
helix start LANsystem                  # DB + MinIO containers (host :6969)
cargo build --release -p lan
docker compose -f deploy/docker-compose.yml up -d   # app on 127.0.0.1:4321
```

After a rebuild only the binary changed, and it's bind-mounted — **never
`--force-recreate`** the app container; just restart it:

```bash
docker compose -f deploy/docker-compose.yml restart lan
```

Recreate only churns apt-get in the entrypoint (`ca-certificates` reinstall,
flaky container DNS) and stalls the app longer. The entrypoint's apt step is
the cause of slow boots; wait for "connected: db + storage + auth ready" in
logs before probing.

Env the app needs: `HELIX_ENDPOINT=http://helix-LANsystem-LANsystem:8080`,
`HELIX_QUERY_PATH=v2/query`, `RUSTFS_ENDPOINT/REGION/ACCESS_KEY/SECRET_KEY/BUCKET`
(creds `minioadmin`/`minioadmin`, bucket `lan`).

## DB facts (learned the hard way)

- The DB runs the standalone image `ghcr.io/helixdb/helixdb:v0.0.3` (`helix.toml`),
  which serves the **v2 contract at `/v2/query`**. The old `enterprise-dev` image
  serves v1 (`/v1/query`) and breaks the app (500). Don't revert.
- The helix CLI's readiness probe can time out on this image (`did not become
  ready`) — the gateway is still up; check `curl :6969/readyz`.
- No `helix compile`/`helix check`. Queries are validated by the running instance:
  `helix query LANsystem -e '…'` or `--file examples/request.json`
  (JSON body in `examples/request.json` is v1-style — for v2 use `query.read.entries`).

## Git hygiene

`.helix/`, `target/`, `helix-db/`, `topcoat/` are gitignored. Never `git add -f`
them; the vendored repos keep their own history.

## Notes

- Topcoat CLI is optional (`topcoat dev`, `topcoat fmt`); not needed to build/run.
- Rust 1.97+ stable per `rust-toolchain.toml`; workspace lints forbid `unsafe`,
  warn on `unwrap`/`expect`/`todo`/`dbg!`.
