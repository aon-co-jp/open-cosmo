# Hybrid Network Architecture (Summary)


**Mission (merged in v0.2):** Guaranteed delivery and guaranteed read/write for data that must never be lost — 3D online game paid items, online finance, and online securities/brokerage. Speed and the 4-layer transport story exist to serve this, not compete with it.

**Goal:** Combine a 4-layer transport stack (raw UDP → QUIC/HTTP3 → TCP fallback → GraphQL federation multiplexing) with `aruaru-db`'s ACID guarantees and `open-raid-z`'s ZFS-style integrity, across `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db`, and `open-raid-z`.

**Current state:** `aruaru-db`'s Poem integration is verified fast; SQL UPSERT parity with `open-runo` is still an open gap. `open-raid-z` has working unaligned I/O and migration tooling, but Windows-native types are unavailable on Linux CI. `open-web-server` status is unaudited.

**Next steps:** (1) fix the UPSERT parser gap, (2) audit `open-web-server`, (3) define a shared transport-negotiation contract, (4) wire ZFS-style checksums into the DB write path, (5) build the QUIC/UDP fast path last.

See `docs/HYBRID_NETWORK_ARCHITECTURE.md` for the full technical rules. Note: authored without live web search; treat "state of the art" claims as unverified until benchmarked.

**Research rule:** Development and maintenance should actively search the web (e.g. Google) and GitHub as needed — and searches should be run in **both Japanese and English**, since relevant findings (blog posts, advisories, issues) often only surface in one language.

**Update (v0.6):** poem-cosmo-tauri closed several previously-postponed gaps this session (gRPC streaming/reflection, non-multipart upload, EDFS via Redis, a scoped Cosmo Connect field) and fixed two stale-documentation bugs. See §0.6 in the full doc for the closure log and what's still genuinely environment-blocked.

**Update (v0.7):** aruaru-db now has a concrete ZFS-compatible checksum layer (byte-identical SHA-256 algorithm to open-raid-z's), hybridized with its existing ACID transactions -- every write is checksummed, every read is verified, and a ZFS-scrub-equivalent method finds all corrupted rows. See §0.7 for detail and remaining rollout steps to other repos.
