# Changelog

All notable changes to open-cosmo are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/) once a
`1.0.0` is cut. Before `1.0.0`, breaking changes may land in minor versions.

## [Unreleased]

### Added

- Initial Rust workspace with 11 crates: `open-runo-core`, `open-runo-router`,
  `open-runo-federation`, `open-runo-schema-registry`, `open-runo-ai-routing`,
  `open-runo-versionless-api`, `open-runo-db`, `open-runo-backup`,
  `open-runo-history`, `open-runo-observability`, `open-runo-security`.
- `open-runo-router`: Poem-based HTTP gateway with `/health` and `/healthz`,
  structured logging via `open-runo-observability`, and per-client rate
  limiting via `open-runo-security`.
- Quality gate tooling: `rustfmt.toml`, `clippy.toml`, `deny.toml`,
  workspace-wide `[lints]` policy, `Makefile`, and GitHub Actions CI
  (`fmt`, `clippy`, `test`, `build`, `audit`, `deny`).
- `docs/` design documentation mapping each README architecture component
  to its implementing crate.
- `DEVELOPMENT.md`, `CONTRIBUTING.md`, `SECURITY.md`, dual
  Apache-2.0/MIT licensing.
- `docker-compose.yml` for local PostgreSQL + gateway development.
- `cargo run --example` for `open-runo-federation`, `open-runo-schema-registry`,
  and `open-runo-ai-routing`, doubling as living API documentation.

### Status

This is pre-`0.1.0` scaffolding work (Development Roadmap Phase 1, see
README). APIs, crate boundaries, and the compatibility guarantees implied by
`open-runo-versionless-api` do not yet apply to open-cosmo's own crates.

[Unreleased]: https://github.com/aon-co-jp/open-cosmo/compare/main...HEAD
