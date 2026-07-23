# Quality Gates

RCosmo's stated goal is "BUGを少なく、品質ゲートを強く" (fewer bugs, strong
quality gates). This document is the single source of truth for what that
means mechanically — see `Makefile`, `.github/workflows/ci.yml`,
`Cargo.toml`'s `[workspace.lints]`, `rustfmt.toml`, `clippy.toml`, and
`deny.toml` for the actual configuration this describes.

## The gates

| Gate | Command | Enforced in CI | Blocking? |
|------|---------|-----------------|-----------|
| Formatting | `cargo fmt --all --check` | `fmt` job | Yes |
| Lint | `cargo clippy --all-targets --all-features -- -D warnings` | `clippy` job | Yes |
| Unit + integration tests | `cargo test --workspace --all-features` | `test` job | Yes |
| Doc tests | `cargo test --workspace --all-features --doc` | `test` job | Yes |
| Build (locked deps) | `cargo build --workspace --all-features --locked` | `build` job | Yes |
| Known vulnerabilities | `cargo audit` | `audit` job | Yes |
| License / advisory / ban policy | `cargo deny check` | `deny` job | Yes |

`make quality-gate` runs the local-machine equivalent of all of the above
except the two jobs that need external services (`audit`'s advisory-db
fetch and `deny`'s dependency graph fetch still work locally, they just need
network access).

## Lint policy specifics

Configured once in the workspace root (`Cargo.toml`'s `[workspace.lints]`)
and inherited via `[lints] workspace = true` in every crate's `Cargo.toml`:

- `unsafe_code = "deny"` — RCosmo should never need `unsafe`; if a future
  crate genuinely does, that crate should locally override this with a
  documented justification rather than removing the workspace default.
- `missing_debug_implementations = "warn"` — every public type should be
  inspectable in logs/panics.
- `clippy::unwrap_used` / `clippy::expect_used = "warn"` — production code
  paths must surface errors through `open_runo_core::Result`, not panic.
  Test code is exempted per-crate via
  `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]`.
- `clippy::todo` / `clippy::dbg_macro = "warn"` — catches debug leftovers
  before merge.
- `clippy::pedantic` is **deliberately not** enabled workspace-wide, since
  CI runs with `-D warnings` (every warning becomes a build failure) and
  pedantic's stylistic lints would fail the build for non-functional
  reasons on a fast-moving young codebase. Crates may opt in locally with
  `#![warn(clippy::pedantic)]` once they've stabilized.

## Adding a new quality gate

1. Add the check to `Makefile` (both as its own target and inside
   `quality-gate`).
2. Add a corresponding job to `.github/workflows/ci.yml` and add it to the
   `quality-gate` job's `needs:` list so it becomes part of the required
   status check.
3. Document the new gate in the table above.
