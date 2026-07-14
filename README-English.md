# open-runo

**GraphQL Federation platform built with Rust** (Poem/Tauri/Cosmo are never
direct dependencies — their functionality is hand-implemented for
compatibility on tokio+hyper) — WunderGraph Cosmo's paid-plan features,
delivered as OSS. Ships with its own self-learning AI (no external LLM
contract required). Developed in parallel with its sibling repo
[poem-cosmo-tauri](https://github.com/aon-co-jp/poem-cosmo-tauri) — either
repo may lead a given change; whichever side notices a divergence mirrors
it to the other. **Both Poem and Tauri — including in-browser execution
capability — are developed from scratch and reproduced with full
compatibility.**

📖 Other languages: [日本語](README-Japan.md) / [中文](README-Chinese.md) /
[한국어](README-Korea.md) / [Español](README-Spain.md) / [Français](README-France.md) /
[Deutsch](README-Germany.md) / [Italiano](README-Italy.md) / [Русский](README-Russia.md) /
[العربية](README-Arabic.md) ·
To integrate open-runo into another project, see **[PORTING.md](PORTING.md)**.

## What is open-runo?

As microservices multiply, REST APIs sprawl out of control (BFF hell, `/v1 /v2`
version explosion, unmanageable endpoint growth). open-runo solves this at the
root with **GraphQL Federation + VersionlessAPI**. Features that WunderGraph
Cosmo (written in Go) only offers on paid plans (Launch / Scale / Enterprise)
are implemented here in pure Rust — **entirely free, as OSS**.

## Feature comparison

| Feature | Cosmo free | Cosmo paid | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| GraphQL Subscriptions (WebSocket) | ✅ | ✅ | ✅ |
| Persisted Queries / Trusted Documents | — | ✅ | ✅ **free** |
| Fine-grained RBAC (per route) | — | ✅ | ✅ **free** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **free** |
| SCIM 2.0 provisioning (Users/Groups) | — | ✅ | ✅ **free** |
| Audit log (stored via Git-on-SQL) | — | ✅ | ✅ **free** |
| Fine-grained rate limiting (token bucket) | — | ✅ | ✅ **free** |
| Response caching | — | ✅ | ✅ **free** |
| Multi-graph / namespaces | — | ✅ | ✅ **free** |
| Request / team-size / retention limits | Yes | Relaxed | **None at all** |

### Features unique to open-runo

- 🧠 **Self-learning AI** (zero external LLM cost) — automatic HTML page cache
  decisions (cold-start prediction via URL pattern generalization), rendering-cost
  learning, adaptive TTL, stale-while-revalidate pre-generation (users never see a MISS)
- 🔑 **KeyGuardian** — fully automated API key lifecycle: SCIM-linked auto issuance
  and revocation, automatic quarantine and recovery for anomalous key usage
- 🗄️ **DUAL DATABASE** — mirrored PostgreSQL + aruaru-db writes, with automatic
  consistency verification and self-healing
- 📦 **One-step move / one-step restore** — export all data + AI learning state
  to a single portable JSON, auto-backed up to two locations (local + a Google
  Drive sync folder), with a one-call `restore-latest`
- 🔀 **Engine conversion & distributed integration** — convert
  MySQL→PostgreSQL→CockroachDB with a single function call (with automatic
  verification), export SQL/CSV for Snowflake, unify internal distributed DBs
  via FederatedBackend (which can also be declared entirely from a single
  TOML file — members, table routing, broadcast tables, default member)
- ⚡ **VersionlessAPI** — a compatibility rule engine that avoids ever creating `/v1 /v2`
- 🖥️ **Desktop management app** compiled from Rust to WebAssembly (no Tauri,
  no Node.js, no TypeScript build chain)
- ⌨️ **CLI (`open-runo-cli`)** — a `wgc`-equivalent CLI for schema
  register/get/history, federation status, and OpenAPI spec retrieval;
  auto self-issues an API key if `--api-key` is omitted

## Quick start

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 337 tests (356 with --all-features)
cargo run -p open-runo-gateway  # start the combined REST + GraphQL server (poem-free)
```

```bash
# GraphQL (GraphiQL is served on GET /graphql)
curl -X POST http://localhost:8080/graphql \
     -H 'content-type: application/json' \
     -d '{"query":"{ health }"}'

# Schema registration (REST admin surface)
curl -X POST http://localhost:8080/api/schemas \
     -H 'x-api-key: dev-key' \
     -d '{"service_name":"users","sdl":"type User { id: ID! }"}'
```

### Using the management UI (WASM frontend)

`cargo run` alone starts the API server; the management UI it serves at
`GET /` (`apps/desktop-wasm`) needs a one-time build:

```bash
rustup target add wasm32-unknown-unknown         # once
cargo install wasm-bindgen-cli --version 0.2.126  # once (must match Cargo.lock's version)
make wasm-frontend                                # generates apps/desktop-wasm/www/pkg
cargo run -p open-runo-gateway                    # now serves the built UI too
```

Open `http://localhost:8080/` for a 10-page admin UI: Dashboard, Schema
Registry, Federation, AI Routing, DUAL DATABASE, SCIM, Persisted Queries,
Feature Flags, Cache & Backup, and Analytics (monthly request-count
metering + per-operation latency/error-rate, `docs/cosmo-parity.md` 4a) —
Rust compiled to WebAssembly, no Tauri/Node.js/TypeScript.

See **[PORTING.md](PORTING.md)** for enabling the AI HTML cache in your own app,
plus the full list of environment variables and endpoints.

### Registering a new service with Federation (CLI)

Registering a GraphQL schema via `open-runo-cli` makes `open-runo-gateway`
route that service through Federation.

```bash
# 1. Write the SDL for the service you want to register
cat > users.graphql << 'EOF'
type User { id: ID! name: String! }
type Query { user(id: ID!): User }
EOF

# 2. With the server running, register it (stage defaults to "local")
cargo run -p open-runo-cli -- schema register \
  --service users \
  --sdl-file users.graphql \
  --stage local

# 3. Check the version history for that service
cargo run -p open-runo-cli -- schema history --service users

# 4. Check overall federation status (contributing services, type/field counts)
cargo run -p open-runo-cli -- federation status
```

If you don't pass `--api-key`, one is auto self-issued, so local development
works with no extra setup.

## Workspace structure (18 crates)

Composed of `open-runo-router` (REST gateway / auth / audit / AI HTML cache /
self-maintenance), `open-runo-gateway` (GraphQL endpoint: Federation /
Subscriptions / Persisted Queries / response cache), `open-runo-federation`
(schema composition / breaking-change detection), `open-runo-db` (`DbBackend`
abstraction over 9 engines, DUAL / Federated — including TOML-file config —
/ migrate), `open-runo-security` (API keys / JWT / OIDC / RBAC / rate
limiting), `open-runo-scim` (SCIM 2.0), `open-runo-cache`,
`open-runo-persisted-queries`, `open-runo-ai-routing`,
`open-runo-versionless-api`, `open-runo-cli` (a `wgc`-equivalent CLI),
`open-runo-api-types` (shared REST/CLI types), `open-runo-feature-flags`
(deterministic-bucketing canary rollouts), and `open-runo-history` /
`-backup` / `-observability` (the latter with OTLP trace export). See
[docs/architecture.md](docs/architecture.md) for details.

## Deployment

The same binary runs unmodified on a self-hosted server, a VPS, AWS, or Docker.
Scale from a minimal single-SQLite setup up to `--features full` (DUAL + Redis +
ClickHouse) via feature flags. There is no functionality gated behind a
"managed-only" tier.

## Documentation

- [docs/architecture.md](docs/architecture.md) — overall design
- [docs/cosmo-parity.md](docs/cosmo-parity.md) — Cosmo feature-parity matrix
- [docs/poem-parity.md](docs/poem-parity.md) — Poem feature-parity matrix
- [docs/tauri-parity.md](docs/tauri-parity.md) — Tauri feature-parity matrix
- [docs/migration.md](docs/migration.md) — move / convert / consolidate
- [docs/api-spec.md](docs/api-spec.md) — API spec
- [docs/security.md](docs/security.md) — security
- [docs/HANDOFF.md](docs/HANDOFF.md) — development history

## Related Projects

There is a target architecture combining `open-web-server` with this
repository, `poem-cosmo-tauri`, PostgreSQL, `aruaru-db`, and `open-raid-z`,
designed to prevent network loss of paid-item and financial/securities
data in 3D online games (quadruple-redundant transport and quadruple-
redundant DB writes, revised 2026-07-11). open-runo can participate as a
Federation Gateway / backend within that architecture (see
[open-web-server](https://github.com/aon-co-jp/open-web-server)'s
`README.md`/`CLAUDE.md` for the full picture).

## License

Apache-2.0 OR MIT (your choice). See [CONTRIBUTING.md](CONTRIBUTING.md) to contribute.
