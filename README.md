# LANsystem

Local-area-network app platform: a [Topcoat](https://github.com/anomalyco/topcoat)
(Rust web) launcher that hosts apps (e.g. the Media Library) backed by a
[HelixDB](https://github.com/helix-db/helix-db) graph + S3 object storage,
all in Docker on one bridge network.

## Layout

- `apps/lan/` — entry binary (launcher + app registry)
- `crates/db/` — HelixDB client (query builder, HTTP transport)
- `crates/storage/` — S3 (MinIO) client for app files
- `crates/platform/core/` — service wiring (`Services::connect`)
- `crates/business-apps/` — apps (media) registered via `AppRegistry`
- `deploy/docker-compose.yml` — runs the app binary in Docker
- `helix.toml` — helix CLI config (DB instance)
- `docs/ARCHITECTURE.md`, `docs/DEPLOY.md`

## Run the stack

```bash
helix start LANsystem                  # DB + MinIO containers (host :6969)
cargo build --release -p lan
docker compose -f deploy/docker-compose.yml up -d   # app on 127.0.0.1:4321
```

See `docs/DEPLOY.md` for the full walkthrough (env vars, seeding the graph,
teardown) and `AGENTS.md` for operational notes.

## License

MIT OR Apache-2.0
