# RCosmo Architecture

This document maps the conceptual architecture described in `README-Japan.md` /
`README-English.md` onto the concrete `crates/` and `apps/` layout.

## Full Stack Overview

```
┌─────────────────────────────────────────────────────────────┐
│  apps/desktop-wasm/  — Rust → WebAssembly Desktop App        │
│  ┌───────────────────────────┐  ┌────────────────────────┐  │
│  │ src/pages.rs (Rust, DOM    │  │  src/api.rs (Rust)     │  │
│  │ via web_sys, no framework) │  │  fetch() → router,     │  │
│  │ wasm32-unknown-unknown     │  │  no Tauri IPC bridge   │  │
│  └───────────────────────────┘  └────────────────────────┘  │
│  Served directly by open-runo-router at GET / and /pkg/*     │
└─────────────────────────────────────────────────────────────┘
                          │ HTTP (same-origin)
┌─────────────────────────────────────────────────────────────┐
│  crates/open-runo-router/  — tokio/hyper Gateway (Rust)       │
│  REST API · X-Api-Key auth · rate limit · tracing, all       │
│  hand-implemented on hyper_compat (no Poem dependency)        │
└─────────────────────────────────────────────────────────────┘
          │               │               │
 open-runo-federation  open-runo-schema  open-runo-ai-routing
 open-runo-db (PG+aruaru-db)  open-runo-history  open-runo-backup
```

**No JavaScript framework** — the Tauri webview uses plain TypeScript with
Bootstrap 5 for styling. No React/Vue/Angular. Vite handles bundling.

## Ecosystem position — open-aruaru central middleware

RCosmo is the **backbone of the entire open-aruaru project family**.
Every product-level subproject talks to its subgraph services *through*
RCosmo — never directly.

```text
   open-e-gov        OpenRedmine       OpenWordPress       aruaru-llm
  (電子政府)       (Rust+Poem 版)     (Rust+Poem 版)      (独自 LLM)
       │                 │                  │                  │
       └────────────┬────┴─────────┬────────┴──────────────────┘
                    │              │
        GraphQL (POST /graphql)    REST (VersionlessAPI, /api/*)
                    │              │
                    ▼              ▼
┌──────────────────────────────────────────────────────────────┐
│                    RCosmo （本リポジトリ）                  │
│                                                              │
│  open-runo-gateway     — 単一 GraphQL エンドポイント           │
│  open-runo-router      — REST ゲートウェイ · SSE (/api/events) │
│  open-runo-federation  — サブグラフ合成 · 破壊的変更検出        │
│  open-runo-ai-routing  — AI プロバイダ選択 (aruaru-ai 連携)    │
└──────────────────────────────────────────────────────────────┘
         │ DUAL DATABASE (open-runo-db)         │ 分析・キャッシュ
         ▼                                     ▼
  PostgreSQL :5432 ── OLTP             Redis/KeyDB :6379
  aruaru-db  :5433 ── Git-on-SQL       ClickHouse  :8123
             (pgwire)                       (analytics)
```

Design consequences:

- Subprojects (open-e-gov, OpenRedmine, OpenWordPress) publish their schemas
  to the Schema Registry; RCosmo composes them into one federated graph.
- A single `X-Api-Key` / JWT auth layer, one rate limiter, and one
  observability pipeline cover every subproject — none of them re-implement
  cross-cutting concerns.
- DUAL DATABASE routing is invisible to subprojects: they address logical
  tables, and `open-runo-db` decides PostgreSQL vs aruaru-db vs both.
  Single-DB deployments use `DualBackend::single` /
  `AppState::with_single_db` and keep the identical code path.

## Component → crate mapping

| README component             | Crate                         | Phase | Status                                                  |
|------------------------------|-------------------------------|-------|---------------------------------------------------------|
| Gateway Router               | `open-runo-router`             | 1     | ✅ HTTP server, health checks, rate limiting, API key auth, real REST endpoints |
| Federation Engine            | `open-runo-federation`         | 2     | ✅ Schema composition, breaking-change detection        |
| Schema Registry              | `open-runo-schema-registry`    | 2     | ✅ Versioning, staging, diff, history                   |
| VersionlessAPI Engine        | `open-runo-versionless-api`    | 3     | ✅ Compatibility rule engine (rename/default/deprecate) |
| AI Routing Engine            | `open-runo-ai-routing`         | 4     | ✅ Cost / latency / local-first / privacy-first routing |
| Database Coordination Layer  | `open-runo-db`                 | 3     | ✅ `DbBackend` trait, in-memory & PostgreSQL backends   |
| Distributed Backup Engine    | `open-runo-backup`             | 5     | ✅ Job lifecycle (Scheduled → Running → Succeeded/Failed) |
| Git-like History Engine      | `open-runo-history`            | 3     | ✅ Commit, approve, rollback                            |
| Observability System         | `open-runo-observability`      | 5     | ✅ Structured tracing init, in-process counters         |
| Security Layer               | `open-runo-security`           | 1     | ✅ API key validation, fixed-window rate limiter        |

`open-runo-core` is the shared foundation (`AppError`, `Result`, `Config`,
`Environment`) that every other crate depends on.

## Crate dependency graph

```text
open-runo-core
├── open-runo-security          (ApiKey, RateLimiter)
├── open-runo-observability     (tracing init, counters)
├── open-runo-federation        (compose, detect_breaking_changes)
├── open-runo-schema-registry   (SchemaRegistry, Stage)
├── open-runo-ai-routing        (route, RoutingPolicy, Provider)
├── open-runo-versionless-api   (apply_compatibility, CompatibilityRule)
├── open-runo-db                (DbBackend trait, InMemoryBackend, PostgresBackend)
├── open-runo-history           (History, ChangeRecord)
├── open-runo-backup            (BackupJob, BackupTarget, BackupKind)
└── open-runo-router            (Gateway binary — depends on all of the above)
```

`open-runo-router` is the only crate that depends on multiple other
`RCosmo-*` crates. All other crates are independently testable with no
cross-crate `RCosmo-*` dependencies.

## Router internal structure

```text
crates/open-runo-router/src/
├── main.rs            ← binary entry point
├── lib.rs             ← Route table, build_app(), integration tests
├── state.rs           ← AppState (Arc<Mutex<SchemaRegistry>>, db: Arc<dyn DbBackend>)
├── auth.rs            ← ApiKeyAuth middleware (X-Api-Key + JWT Bearer)
├── rate_limit.rs      ← RateLimit middleware (wraps open-runo-security)
├── validation.rs      ← JSON schema validation for request bodies
├── middleware/
│   ├── mod.rs
│   └── cors.rs        ← CORS configuration
└── handlers/
    ├── mod.rs
    ├── schemas.rs         ← /api/schemas/*
    ├── federation.rs      ← /api/federation/*
    ├── ai_routing.rs      ← /api/ai/route
    ├── db.rs              ← /api/db/* (DUAL DATABASE key-value REST)
    └── events.rs          ← /api/events (SSE)
```

`crates/open-runo-gateway` builds on the composed federation schema and
exposes it as a single `POST /graphql` endpoint (GraphiQL on `GET /graphql`).

## Request lifecycle

```text
Client Request
  │
  ▼
RateLimit middleware      ← open-runo-security::RateLimiter (per-IP bucket)
  │
  ▼
Tracing middleware        ← open-runo-observability (structured JSON log)
  │
  ▼
ApiKeyAuth middleware     ← checks X-Api-Key header (health routes exempt)
  │
  ▼
Route dispatch
  ├── /health, /healthz               → inline handler (no state needed)
  ├── /api/schemas                    → POST: register_schema
  ├── /api/schemas/:service           → GET:  get_schema
  ├── /api/schemas/:service/history   → GET:  get_schema_history
  ├── /api/federation/compose         → POST: compose_schemas
  ├── /api/federation/status          → GET:  federation_status
  └── /api/ai/route                   → POST: route_request
```

## Why per-responsibility crates

Splitting each concern into its own crate gives:

- Independent `cargo test -p <crate>` cycles during development.
- Independent compile units — a change to `open-runo-backup` does not
  force a rebuild of `open-runo-federation`.
- Clear ownership boundaries that the Quality Gate Pipeline can enforce
  (each crate's tests are its own contract).

See `docs/quality-gates.md` for how this is enforced in CI.
