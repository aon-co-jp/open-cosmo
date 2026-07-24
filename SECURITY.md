# Security Policy

## Supported Versions

open-cosmo is currently in pre-`0.1.0` / Phase 1 development (see the
Development Roadmap in `README-English.md` / `README-Japan.md`). There is no
long-term-supported release line yet; security fixes land on `main` and are
included in the next tagged release.

| Version | Supported |
|---------|-----------|
| `main`  | :white_check_mark: |
| < 0.1.0 tags | :x: (pre-release, best effort only) |

## Reporting a Vulnerability

Please do **not** open a public GitHub issue for security vulnerabilities.

Instead:

1. Open a private [GitHub Security Advisory](https://github.com/aon-co-jp/open-cosmo/security/advisories/new)
   for this repository, or
2. If that is not available, contact a maintainer directly with a
   description of the issue, affected crate(s)/version(s), and, if
   possible, a minimal reproduction.

We aim to acknowledge reports within 5 business days. Once a fix is ready,
we will coordinate a disclosure timeline with the reporter before making
details public.

## Scope

Given open-cosmo's architecture (see `docs/architecture.md`), security-relevant
reports are especially welcome for:

- `open-runo-security` (authentication, API keys, rate limiting)
- `open-runo-router` (the public HTTP entrypoint)
- `open-runo-db` (data access, especially the `postgres` feature)
- Supply-chain issues surfaced by `cargo audit` / `cargo deny` (see
  `docs/quality-gates.md`) that aren't yet caught by CI

## Our Commitments

- We will not take legal action against good-faith security research
  conducted under this policy.
- We will credit reporters in the fix's changelog entry (`CHANGELOG.md`)
  unless anonymity is requested.
