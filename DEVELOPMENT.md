# RCosmo Development Guide

This document is the practical, day-to-day companion to `CONTRIBUTING.md`.
`CONTRIBUTING.md` covers process (branching, commits, PRs); this file covers
the mechanics of building, running, and navigating the codebase.

---

## 1. Prerequisites

- Rust 1.75+ via [rustup](https://rustup.rs/) (`rustup show` to confirm the
  active toolchain)
- `rustfmt` and `clippy` components: `rustup component add rustfmt clippy`
- PostgreSQL 14+ if you're working on `open-runo-db`'s `postgres` feature
  (optional for most crates — see §5)
- Optional: `cargo install cargo-audit cargo-deny` for the full quality gate

No Node.js, Go, or Python toolchain is required — RCosmo is a pure Rust
workspace.

## 2. Getting the code building

```bash
git clone https://github.com/aon-co-jp/RCosmo.git
cd RCosmo
cargo build --workspace --all-features
```

The workspace has no root binary; each crate under `crates/` builds
independently and `open-runo-router` additionally produces a runnable binary:

```bash
cargo run -p open-runo-router
# or:
make run-router
```

By default this starts the Gateway Router on `0.0.0.0:8080` with a
`/health` and `/healthz` endpoint. Override via environment variables (see
`open-runo-core::Config::from_env`):

| Variable                            | Default        | Purpose                                             |
|-------------------------------------|----------------|------------------------------------------------------|
| `OPEN_RUNO_ENV`                      | `local`        | `local` / `development` / `staging` / `production`   |
| `OPEN_RUNO_BIND_ADDR`                | `0.0.0.0:8080` | Listener address for the gateway                     |
| `OPEN_RUNO_LOG_LEVEL`                | `info`         | `tracing` filter directive (JSON output via `open-runo-observability`) |
| `OPEN_RUNO_RATE_LIMIT_MAX_REQUESTS`  | `120`          | Requests allowed per client key per window            |
| `OPEN_RUNO_RATE_LIMIT_WINDOW_SECS`   | `60`           | Rolling window (seconds) for the rate limit above     |
| `OPEN_RUNO_OTLP_ENDPOINT`           | unset          | OTLP HTTP endpoint (e.g. `http://localhost:4318`) to export traces to; unset keeps tracing console-only |
| `DATABASE_URL`                      | unset          | Postgres connection string, only read by `open-runo-db`'s `postgres` feature |

See `.env.example` for a copy-pasteable starting point. `open-runo-router`'s
binary wires `OPEN_RUNO_LOG_LEVEL` (and, when set, `OPEN_RUNO_OTLP_ENDPOINT`)
into `open_runo_observability::init_tracing_with_otlp`, and the two
rate-limit variables into `open_runo_router::middleware_hyper::with_rate_limit`
backed by `open_runo_security::RateLimiter`, keyed by the `X-Forwarded-For` /
`X-Real-IP` header (falling back to a single shared bucket when neither is
present).

### Command-line client (`open-runo-cli`)

A `wgc`-equivalent CLI (see `docs/cosmo-parity.md` 4a) talks to a running
`open-runo-router`/`open-runo-gateway` over its REST API -- no manual API
key setup required, it self-issues one on first use the same way the WASM
frontend does:

```bash
cargo run -p open-runo-cli -- schema register --service users --sdl-file schema.graphql
cargo run -p open-runo-cli -- schema history --service users
cargo run -p open-runo-cli -- federation status
cargo run -p open-runo-cli -- openapi --json
```

`--base-url` (default `http://localhost:8080`) and `--api-key` can also be
set via `OPEN_RUNO_CLI_BASE_URL` / `OPEN_RUNO_CLI_API_KEY`.

Prefer Docker Compose for a one-command local stack (gateway + Postgres):

```bash
docker compose up --build
```

## 3. Repository layout

```text
RCosmo/
├── Cargo.toml              # workspace definition, shared deps, lint policy
├── Makefile                 # build/test/fmt/clippy/audit/deny entrypoints
├── rustfmt.toml              # formatting rules
├── clippy.toml               # clippy threshold tuning
├── deny.toml                 # cargo-deny: license/advisory/ban policy
├── rust-toolchain.toml        # pins the stable toolchain + rustfmt/clippy components
├── .env.example               # documents every OPEN_RUNO_* / DATABASE_URL variable
├── Dockerfile / .dockerignore # multi-stage build for open-runo-router
├── docker-compose.yml         # local gateway + Postgres stack
├── LICENSE-APACHE / LICENSE-MIT  # dual-licensed, Apache-2.0 OR MIT
├── CHANGELOG.md               # Keep a Changelog, [Unreleased] tracks Phase 1 progress
├── SECURITY.md                 # vulnerability reporting policy
├── .github/workflows/        # CI (ci.yml) and release (release.yml)
├── crates/
│   ├── open-runo-core/            # shared error type, Config, Environment — no internal deps
│   ├── open-runo-router/          # Poem Gateway Router + binary entrypoint (tracing + rate-limit middleware)
│   ├── open-runo-federation/      # schema composition / breaking-change detection (see examples/)
│   ├── open-runo-schema-registry/ # schema versioning, diff, stage promotion (see examples/)
│   ├── open-runo-ai-routing/      # AI provider routing policies (see examples/)
│   ├── open-runo-versionless-api/ # field-level backward-compatibility rules
│   ├── open-runo-db/              # DbBackend trait + in-memory/Postgres impls
│   ├── open-runo-backup/          # backup job planning/lifecycle
│   ├── open-runo-history/         # git-like change history/rollback
│   ├── open-runo-observability/   # tracing init + in-process counters
│   └── open-runo-security/        # API key validation + rate limiting
└── docs/                      # design docs referenced from README
```

Dependency direction: every crate may depend on `open-runo-core`; nothing
depends back into a "leaf" crate from `open-runo-core`. Higher-level crates
(`open-runo-router`, future orchestration layers) are expected to compose the
others, not the other way around. When adding a new crate, keep it
single-responsibility — that's what lets the Quality Gate Pipeline test each
piece in isolation.

## 4. Day-to-day commands

```bash
make build          # cargo build --workspace --all-features
make test           # cargo test --workspace --all-features
make fmt             # cargo fmt --all (writes changes)
make fmt-check       # cargo fmt --all --check (CI mode)
make clippy          # cargo clippy --all-targets --all-features -- -D warnings
make doc             # cargo doc --workspace --all-features --no-deps
make pre-commit      # fmt + clippy + test — run this before every commit
make quality-gate    # pre-commit + audit + deny — what CI enforces on every PR
```

Run a single crate's tests while iterating:

```bash
cargo test -p open-runo-schema-registry
cargo watch -x 'test -p open-runo-schema-registry'   # if cargo-watch is installed
```

## 5. Working with `open-runo-db`

`open-runo-db` compiles without any database by default — `InMemoryBackend`
is enough for unit tests and for developing every other crate. The
`postgres` feature adds a `sqlx`-backed implementation:

```bash
cargo build -p open-runo-db --features postgres
```

This requires a reachable `DATABASE_URL` only at *runtime* (`PgPool::connect`),
not at compile time — no `DATABASE_URL` is needed to `cargo check`/`cargo
build`, since the crate does not use `sqlx::query!` compile-time-checked
macros. For local development, the simplest option is:

```bash
docker run -d --name rcosmo-pg -e POSTGRES_PASSWORD=rcosmo -p 5432:5432 postgres:16
export DATABASE_URL=postgres://postgres:RCosmo@localhost:5432/postgres
```

## 6. Runnable examples

`open-runo-federation`, `open-runo-schema-registry`, and `open-runo-ai-routing`
each ship a `cargo run --example` you can use to see the crate's public API
in action without writing a test:

```bash
cargo run -p open-runo-federation --example compose_two_services
cargo run -p open-runo-schema-registry --example register_and_promote
cargo run -p open-runo-ai-routing --example pick_provider
```

These are meant as living documentation — when a crate's public API
changes, update its example alongside its doc comments.

## 7. Adding a new crate

1. `cargo new --lib crates/RCosmo-<name>`
2. Add it to the `members` list and to `[workspace.dependencies]` in the
   root `Cargo.toml` (so other crates can depend on it via
   `RCosmo-<name> = { workspace = true }`).
3. Copy the `[lints] workspace = true` line and the `version.workspace = true`
   / `edition.workspace = true` / etc. package fields from an existing crate's
   `Cargo.toml` so it inherits the shared lint policy and metadata.
4. Add `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]`
   near the top of `src/lib.rs` if the crate has unit tests that use
   `.unwrap()`/`.expect()` (production code should not — see §8).
5. Consider adding a `cargo run --example` (see §6) once the crate has a
   non-trivial public API — it doubles as living documentation.
6. Run `make pre-commit` before opening a PR.

## 8. Quality gate expectations

- No `unsafe` (denied workspace-wide).
- No `.unwrap()`/`.expect()` in non-test code (`clippy::unwrap_used` /
  `clippy::expect_used` are warnings promoted to errors by `-D warnings` in
  CI). Tests are exempt via the per-crate `cfg_attr` from §7.
- Every public type should derive or implement `Debug`
  (`missing_debug_implementations` is warn-level workspace-wide).
- `clippy::pedantic` is intentionally **not** enabled workspace-wide (see
  the comment in `Cargo.toml`) — it's too noisy to gate CI on for a young
  codebase. Feel free to enable it locally per-crate as the crate matures.
- `cargo deny check` enforces the Apache-2.0/MIT-compatible license
  allowlist in `deny.toml` and denies known security advisories.

## 9. Troubleshooting

- **`cargo clippy` fails on a lint you don't understand**: run
  `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | less`
  and check `clippy.toml` / the `[workspace.lints.clippy]` table in
  `Cargo.toml` for the active configuration.
- **`sqlx` fails to compile with a TLS-related error**: confirm you're
  using the `runtime-tokio-rustls` feature (not `native-tls`) — see the
  note in `deny.toml` banning `openssl-sys`.
- **`cargo deny check` flags a new dependency's license**: either replace
  the dependency, or open a PR discussion before adding its license to the
  `allow` list in `deny.toml` — don't silence it via `ignore` without
  review.
