# Hybrid Network Architecture — Technical Rules / 技術ルールファイル

**Status:** Draft v0.1 (2026-07)
**Scope:** `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db`, `open-raid-z`
**Portability:** This file is written to be dependency-free of any single repo. Copy it as-is into any project in the `aon-co-jp` family; only the "Per-Project Status" table needs updating.

> ⚠️ **Research disclosure**: This document was authored without live web access (no general web-search tool was available in this session — only `github.com`, `crates.io`, `npmjs.com` and similar dev-infra domains were reachable). Claims about "2026-07 state of the art" reflect the author's trained knowledge (cutoff ~2026-01) plus inspection of this codebase, not a fresh literature/web survey. Treat performance claims and library version numbers as **to be verified** before being treated as fact — flag them in review rather than citing them externally.

---

## 1. Goal (目指すもの)

Build a **hybrid transport + storage stack** across the five projects so that a single logical request (e.g. a GraphQL federation query landing on `open-runo`) can, end-to-end:

1. Arrive over the fastest available transport for the client/network conditions (multi-layer transport negotiation).
2. Be served from `aruaru-db` with full ACID guarantees even when the underlying storage is distributed.
3. Persist to `open-raid-z` with ZFS-style integrity (checksums, copy-on-write, scrub) without giving up POSIX/Windows filesystem semantics.
4. Do all of this with security (mTLS/QUIC-native encryption) and speed (zero-copy where possible) as co-equal goals, not one traded for the other.

The "4層4重" framing means: don't pick one transport. Run a layered fallback/negotiation stack:

| Layer | Transport | Primary use case |
|---|---|---|
| L1 | UDP/IP raw + custom framing | LAN-local, lowest latency, trusted network segments |
| L2 | QUIC (HTTP/3) over UDP | WAN, NAT traversal, connection migration, mobile clients |
| L3 | TCP/IP (HTTP/2 or HTTP/1.1 fallback) | Legacy clients, environments where UDP is firewalled |
| L4 | Application-level multiplexing (GraphQL federation over any of the above) | Uniform API regardless of which lower layer was negotiated |

"4重" (fourfold redundancy) = each layer should degrade gracefully to the one below it, and the system should be able to detect and switch mid-session (e.g. QUIC connection migration, or TCP fallback if UDP is blocked mid-handshake).

## 2. Current State (今出来ているもの)

This section must be kept honest and specific — do not let it become aspirational. Update per project:

- **`open-runo`**: GraphQL federation gateway. Has `poem-parity.md`, `cosmo-parity.md`, `tauri-parity.md` tracking feature parity with Poem/Cosmo/Tauri references — meaning transport-layer work is already being scoped, but (as of last working session) UPSERT/SQL compatibility with `aruaru-db` is an open gap, and some crates were not yet pushed (non-compilable state flagged in earlier sessions).
- **`aruaru-db`**: Hybrid distributed DB with Git-on-SQL. Poem web framework integration confirmed well-optimized, minimal middleware overhead — this is a real, verified data point, not aspirational.
- **`open-raid-z`**: Experimental Windows filesystem with ZFS-style features. `Pool::read_unaligned`/`write_unaligned` implemented. `orzctl migrate` subcommand exists with integration tests. FAT32 test coverage exists. Known constraint: `windows` crate v0.58 is `#![cfg(windows)]`-gated, so Windows-native types are unavailable when developing/testing on Linux — any hybrid-network code touching `open-raid-z` internals must account for this at CI time (feature-gate or mock).
- **`poem-cosmo-tauri`**: Referenced as a Tauri-based desktop counterpart; parity doc exists in `open-runo` (`tauri-parity.md`) but this repo's own transport work is new as of this session per the user.
- **`open-web-server`**: Named as an integration target; no verified status yet in this session — needs its own audit before claiming any parity here.

## 3. Where To Start (どこから手を付けるべきか)

Priority order, reasoning included so it can be re-argued later:

1. **Fix the `aruaru-db` UPSERT/SQL parser gap first.** Nothing above the storage layer is trustworthy if writes silently misbehave. This was already flagged as a known issue — close it before building transport features on top.
2. **Audit `open-web-server`.** You cannot design a 4-layer transport story without knowing what this component already does. Don't assume; read its code.
3. **Define the L1–L4 negotiation contract as a shared crate/interface** (e.g. `open-transport-negotiation` or similar), consumed by `open-runo`'s gateway and `open-web-server`, so the fallback logic isn't duplicated per project.
4. **Wire `open-raid-z` scrub/checksum hooks into `aruaru-db`'s write path** as an opt-in durability layer — this is the "ZFS-good + ACID-good" hybrid the user wants, and it's the newest, least-explored integration point.
5. **Only then** build the QUIC/UDP fast paths in `open-runo`/`poem-cosmo-tauri` — transport speed work is the most "exciting" but least useful if the layers below it are inconsistent.

## 4. Technical Rules (ルール)

- **No transport-only "fast path" may bypass ACID guarantees.** If a low-latency path can't preserve durability semantics, it must be explicitly labeled (e.g. `--allow-eventual`) and off by default.
- **Every cross-project integration must have a feature flag.** Follow the `open-raid-z` precedent (`gpu`, `winfsp_backend`, `foreign_fs_fat`/`foreign_fs_exfat`) — don't force one project's platform constraints onto another's default build.
- **Bilingual docs (JP/EN minimum) for anything user-facing**, consistent with existing project convention.
- **Large, meaningful commits** over many small ones, consistent with established workflow preference.
- **Performance claims must cite a benchmark run in this repo**, not general industry claims about QUIC/io_uring/etc. "Poem is well-optimized" was earned by actually profiling it — hold new claims to the same bar.
- **Security and speed are both required, not traded off.** Any PR that improves latency by weakening the encryption/auth story should be rejected or gated.

## 5. Open Questions For Next Session

- Has `open-web-server` been audited yet?
- Is there a shared crate for transport negotiation, or is each project doing its own?
- What's the actual current UPSERT-parser fix status in `aruaru-db`?
- Do we have real benchmark numbers for QUIC vs TCP in this stack, or only from general literature (which should not be cited as fact per §1 disclosure)?
