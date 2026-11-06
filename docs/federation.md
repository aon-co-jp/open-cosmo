> **📌 2026-11-05: REST は全廃した。** 本書の `/api/...` の記述は旧 REST のもの(歴史的記録)。現在の管理 API は `POST /open_runo.admin.v1.Admin/<Method>`(gRPC / gRPC-web / Connect、定義は `crates/open-runo-proto/proto/open_runo/admin/v1/admin.proto`)。対応: `/api/schemas`→`RegisterSchema`、`/api/db/...`→`DbGet`/`DbPut` など、`/api/feature-flags`→`*FeatureFlag*`。詳細は `CLAUDE.md` 冒頭の「REST 全廃」節。

# Federation Engine

Implemented in [`crates/open-runo-federation`](../crates/open-runo-federation).

## Scope (Phase 2)

- `ServiceSchema`: a backend service's exposed types/fields.
- `compose(&[ServiceSchema]) -> Result<ComposedSchema>`: merges N service
  schemas into one federated schema, rejecting duplicate service
  registration.
- `detect_breaking_changes(previous, next) -> Vec<String>`: flags removed
  types/fields between two composed schemas.

## SDL parsing and Federation v1/v2 compatibility (2026-07-11)

`crates/open-runo-federation/src/sdl.rs` adds `parse_service_sdl(service_name,
sdl) -> Result<ServiceSchema>`, a minimal hand-written GraphQL SDL parser
that extracts `type`/`interface`/`extend type` field names — this is what
lets `POST /api/federation/compose` accept a subgraph's *real* SDL text
(via an optional `sdl` field on each service entry) instead of requiring
callers to pre-extract a `{type: [fields]}` JSON map by hand.

`detect_federation_version(sdl) -> FederationVersion` classifies a
subgraph's dialect:

- **V2**: an `@link(url: "https://specs.apollo.dev/federation/v2...")`
  import is present.
- **V1**: no such import, but classic directives (`@key`, `@requires`,
  `@provides`, `@external`) appear directly — the pre-Federation-2
  convention where these directives were implicitly available.
- **None**: no federation directives at all.

Both dialects place their directives/arguments in the same syntactic
positions, so the parser is directive-content-agnostic (it skips
`@directive(...)` tokens wherever they occur) and handles v1 and v2
subgraphs identically. See `sdl::tests::v1_and_v2_subgraphs_compose_together_into_one_supergraph`
for a test that parses one authentic v1-style and one authentic v2-style
subgraph and composes them into a single correct supergraph.

## Not yet implemented

Query planning, distributed execution, and full GraphQL/gRPC/OpenAPI
adapter support (see README §2) are out of scope for the current
composition-only implementation. These build on top of `ComposedSchema`
once `open-runo-router` starts dispatching requests through the Federation
Engine (Phase 2 continuation).

## Relationship to Schema Registry

`open-runo-federation` computes *what* a composed schema looks like;
`open-runo-schema-registry` (see `docs/database.md`... actually see its own
crate docs) is responsible for *persisting* schema versions and their
promotion across environments. Wiring composition output into registry
storage is planned but not yet implemented — today they're used
independently.
