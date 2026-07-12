# Hybrid Network Architecture — Technical Rules / 技術ルールファイル

**Status:** Draft v0.3 (2026-07-12) — merged: zero-data-loss mission, open-web-server audit findings, aruaru-db UPSERT fix, and a standing research-discipline rule
**Scope:** `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db`, `open-raid-z`
**Mission:** Guaranteed delivery + guaranteed read/write for data that must never be lost — 3D online game paid items, online finance, online securities/brokerage. See §0.
**Portability:** This file is written to be dependency-free of any single repo. Copy it as-is into any project in the `aon-co-jp` family; only the "Per-Project Status" table needs updating.

> ⚠️ **Research disclosure**: This document was authored without live web access (no general web-search tool was available in this session — only `github.com`, `crates.io`, `npmjs.com` and similar dev-infra domains were reachable). Claims about "2026-07 state of the art" reflect the author's trained knowledge (cutoff ~2026-01) plus inspection of this codebase, not a fresh literature/web survey. Treat performance claims and library version numbers as **to be verified** before being treated as fact — flag them in review rather than citing them externally.

---

## 0. Mission (使命) — Zero Data Loss for High-Stakes Transactions

This stack's reason for existing, stated plainly so it isn't diluted by later feature work:

> **Deliver and persist data that must never be lost, over a network that must never silently fail, for use cases where "eventually consistent" or "probably arrived" is not an acceptable answer.**

Concrete target domains:
- **3D online game paid items** (課金アイテム) — a purchase or item-transfer that silently fails or duplicates is a real financial loss for a real user, not a cosmetic bug.
- **Online finance / online brokerage (オンライン証券)** — order execution, balance updates, and trade confirmations must be durable and exactly-once, even across network layer switches (L1↔L2↔L3) or mid-session failover.

This mission subsumes and reorganizes the goals in §1 — treat §1–§4 as *how* we achieve zero-loss delivery, not as separate, competing goals. Speed and the 4-layer transport story matter **only insofar as they never compromise** this guarantee. Any optimization that trades durability for latency must be opt-in, explicit, and off by default for these domains (see §4 rules, unchanged and still binding).

**Non-negotiable properties for money/asset-bearing data:**
1. **At-least-once delivery + exactly-once application** — the network layer may retry; the DB layer (`aruaru-db`, ACID) must de-duplicate via idempotency keys/transaction IDs so retries never double-charge or double-grant an item.
2. **Durable before acknowledged** — a client is only told "success" after the write is committed and (where `open-raid-z` is in the path) checksummed/persisted — not merely buffered in a fast transport layer.
3. **Layer-switch transparency** — if a session migrates from QUIC (L2) to TCP (L3) mid-transaction (see §1 table), the in-flight transaction must survive the switch or be safely retried, never silently dropped.
4. **Auditability** — every asset/financial write must be traceable end-to-end across the transport and storage layers for reconciliation and dispute resolution.

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
- **`aruaru-db`**: Hybrid distributed DB with Git-on-SQL. Poem web framework integration confirmed well-optimized, minimal middleware overhead — this is a real, verified data point, not aspirational. **UPSERT gap closed (2026-07-12)**: parser now handles `INSERT ... ON CONFLICT (col) DO UPDATE SET ... / DO NOTHING`, including `EXCLUDED.col` references, wired into a new `upsert()` executor. Verified via an isolated 16-test suite (parser layer, since this sandbox's toolchain cannot compile the full crate — see rule on toolchain limitations below); 4 additional engine-level tests were written but not yet run in a real toolchain/CI — confirm those before treating this as fully closed.
- **`open-raid-z`**: Experimental Windows filesystem with ZFS-style features. `Pool::read_unaligned`/`write_unaligned` implemented. `orzctl migrate` subcommand exists with integration tests. FAT32 test coverage exists. Known constraint: `windows` crate v0.58 is `#![cfg(windows)]`-gated, so Windows-native types are unavailable when developing/testing on Linux — any hybrid-network code touching `open-raid-z` internals must account for this at CI time (feature-gate or mock).
- **`poem-cosmo-tauri`**: Referenced as a Tauri-based desktop counterpart; parity doc exists in `open-runo` (`tauri-parity.md`) but this repo's own transport work is new as of this session per the user.
- **`open-web-server`**: **Audited 2026-07-12.** Far more mature than the earlier "unaudited" placeholder suggested. Already implements:
  - A 4-layer defense-in-depth wire protocol (`open-web-server-wire`): L1 TLS 1.3 (rustls) → L2 mutual auth → L3 ChaCha20-Poly1305 AEAD application-layer payload encryption → L4 replay-guard (monotonic seq + timestamp bound into AEAD associated data, rejecting replayed ciphertext that AEAD alone wouldn't catch).
  - A parallel, orthogonal `udp_channel` module (added 2026-07-11) for **transport-path redundancy** — the same `MutationRequest` is fire-and-forth sent over UDP alongside the authoritative TCP path; UDP failure/timeout never affects the TCP-path commit. This is exactly the §1 L1/L2 relationship, already implemented, not just designed.
  - `open-web-server-core`: `IdempotencyKey`, `MutationRequest`/`MutationReceipt` domain types — every write requires an idempotency key by construction (a `CoreError::DuplicateKey` variant exists specifically to reject re-application).
  - `open-web-server-ledger`: the documented 3-hop commit pipeline — open-web-server (local WAL, fsync) → open-runo (Federation routing + audit log) → aruaru-db (Raft consensus, Git-on-SQL commit id). Client only gets "confirmed" after step 3's commit id returns.
  - `open-web-server-gateway`: hyper/tokio-based HTTP entrypoint (explicitly *not* Poem-dependent as of a 2026-07-10 stack pivot, while keeping API-shape compatibility with the earlier Poem implementation), with `/api/v1/items/grant` and `/api/v1/transactions/charge` handlers and an idempotency-key-required middleware gate.
  - **Conclusion: §0's mission is already substantially implemented here**, ahead of the other four repos. The remaining gap is wiring `aruaru-db`'s new UPSERT support (this session) into the ledger's aruaru-db-facing write path, and connecting `open-raid-z` checksums into the WAL step.

## 3. Where To Start (どこから手を付けるべきか)

Priority order, reasoning included so it can be re-argued later:

1. ~~Fix the `aruaru-db` UPSERT/SQL parser gap.~~ **Done (2026-07-12)** — see §2.
2. ~~Audit `open-web-server`.~~ **Done (2026-07-12)** — turned out to be the most mission-complete of the five repos; see §2.
3. **Confirm the two items above actually compile/pass in a real toolchain.** This session's sandbox could not build the full `aruaru-query` crate (edition2024 transitive-dependency issue, same class as `open-raid-z`'s documented constraint) — the parser fix was verified in isolation, and the engine-level tests are unverified. Do this before building anything on top.
4. **Wire `aruaru-db`'s new UPSERT support into `open-web-server-ledger`'s aruaru-db write path**, replacing whatever placeholder/plain-INSERT logic it currently uses for the final Raft-consensus commit step.
5. **Define the L1–L4 negotiation contract as a shared crate/interface**, consumed by `open-runo`'s gateway and `open-web-server` — `open-web-server-wire`'s 4-layer defense-in-depth stack and `udp_channel` are a strong starting reference implementation; don't reinvent it, extract/generalize it.
6. **Wire `open-raid-z` scrub/checksum hooks into `open-web-server-ledger`'s WAL step and `aruaru-db`'s write path** as an opt-in durability layer.
7. **Only then** build further QUIC/UDP fast-path work — `open-web-server` already has a working UDP redundant path; evaluate whether it needs QUIC specifically or whether the existing design already satisfies §0.

## 4. Technical Rules (ルール)

- **No transport-only "fast path" may bypass ACID guarantees.** If a low-latency path can't preserve durability semantics, it must be explicitly labeled (e.g. `--allow-eventual`) and off by default.
- **Money/asset-bearing writes (game item purchases, brokerage orders, balance changes) always use the zero-loss path from §0** — idempotency key required, ack only after durable commit, no exceptions for "just this once, for latency."
- **Research as part of normal maintenance, not a one-off.** When implementing or reviewing changes in this stack, actively look things up — search the web (e.g. Google) for current protocol specs, CVEs, and library changelogs; check GitHub (issues, releases, source) for the actual current behavior of dependencies (`rustls`, `chacha20poly1305`, `datafusion`, `fjall`, etc.) — rather than relying purely on trained/remembered knowledge, which can be stale or wrong. **Run web searches in both Japanese and English** — search results, advisories, and community discussion differ by language (e.g. Japanese-language blog posts/Qiita/Zenn articles on a crate's quirks, or CVE writeups that only appear in English-language security trackers), so a single-language search risks missing relevant findings. This is especially important for the crypto/replay-guard/consensus code in `open-web-server-wire` and `aruaru-db`, where a wrong assumption about a library's behavior is a security or durability bug, not a cosmetic one. Cite what was actually checked (a URL, an issue number, a changelog entry, and which language it was found in) rather than asserting "current best practice" from memory alone.
- **Toolchain limitations must be recorded, not silently worked around forever.** If a sandbox/CI environment can't build a crate (e.g. the recurring edition2024-transitive-dependency issue affecting both `open-raid-z` and `aruaru-db`'s `datafusion`/`arrow`/`fjall` chains), say so explicitly in the commit and in this doc, and flag any tests as "written but unverified" until a real toolchain confirms them — don't claim something is fixed until it has actually compiled and passed.
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
- Which project owns the idempotency-key / transaction-ID scheme for §0 zero-loss guarantees — `aruaru-db`, or a new shared crate? **Partially answered**: `open-web-server-core::IdempotencyKey` already exists and propagates through the 3-hop pipeline; `aruaru-db::execute_idempotent` also already exists independently. These two schemes need to be reconciled/unified rather than left as two parallel idempotency systems.
- Has the `aruaru-db` UPSERT fix and its engine-level tests actually been compiled and run in a real toolchain (not this session's sandbox)?
- Should `open-runo`'s federation layer generate the `EXCLUDED.col`-style UPSERT SQL that `aruaru-db` now understands, or does it already, making this fix immediately usable end-to-end?
