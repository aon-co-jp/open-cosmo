# Horizontal scaling: N instances behind a reverse-proxy load balancer

## Why this exists (2026-07-13)

This ecosystem's `CLAUDE.md` previously concluded that Nginx-style
FastCGI-buffer tuning and named-upstream keepalive pooling had "no
equivalent need" here, reasoning that `open-runo`/`poem-cosmo-tauri` *is*
the Rust server, not a proxy in front of one. A user pushed back with a
concrete analogy: Apache HTTPD commonly sits in front of Tomcat
(`mod_proxy_http`/`mod_proxy_ajp`), providing TLS termination, static-asset
offload, load balancing across multiple Tomcat instances, and
connection-pooling/keepalive from the proxy to the backend.

After bilingual research (Japanese + English search on "Rust hyper server
behind nginx reverse proxy production best practice" /
"tokio async server 複数プロセス ロードバランス デプロイ") plus reasoning
about the two servers' concurrency models, the conclusion is a **partial
correction**, not a full reversal:

- Tomcat's classic thread-per-connection model is *why* Apache-in-front
  buffering/connection-management mattered so much: a slow client can tie
  up one of Tomcat's limited worker threads for the life of the connection,
  so Apache absorbing/buffering that traffic protects the backend's
  thread pool. tokio's async I/O model does not have this problem — a slow
  client ties up an async task, not an OS thread, and tokio can hold many
  thousands of those cheaply. So FastCGI-buffer-style tuning and
  connection-pooling-for-slow-client-protection genuinely have **no
  equivalent need** here. That part of the original conclusion holds.
- The *narrower* subset of reverse-proxy benefits still applies to a
  tokio/hyper server, for different reasons than Tomcat's: TLS-termination
  convenience (keep certs/ACME renewal at one edge layer instead of N app
  processes), horizontal scaling across **multiple machines** (simpler to
  hand off to an external LB than to build cluster-awareness into the
  app), and zero-downtime rolling restarts (the LB drops an instance from
  rotation, waits for it to drain, restarts it, re-adds it).

## What changed in the codebase because of this

- **Graceful shutdown** (`crates/open-runo-router/src/hyper_compat.rs`:
  `serve_with_shutdown` + `shutdown_signal`): on `SIGINT`/`SIGTERM`, the
  listener stops accepting new connections, existing connections are told
  to finish their current request/response via hyper's
  `graceful_shutdown()`, and the process only exits once every connection
  has drained. This is the piece a rolling restart behind a load balancer
  needs — without it, an instance being taken out of rotation could sever
  in-flight requests instead of letting them finish. Both
  `open-runo-router` and `open-runo-gateway`'s `main.rs` use it. See
  `hyper_compat::tests::graceful_shutdown_lets_an_in_flight_request_finish_before_the_server_stops`
  for a test that proves an in-flight (artificially slow) request still
  completes after the shutdown signal fires, and that the listener stops
  accepting new connections afterward.
- **Real health checks** (`hyper_compat::health_handler`): `GET /health`
  and `GET /healthz` used to unconditionally return `200 {"status":"ok"}`
  regardless of backend state — a load balancer health check that always
  says "healthy" defeats the point of clustering (it can never know to
  pull an instance with a dead DB connection out of rotation). The handler
  now calls the real `DbBackend::list()` against a dedicated probe table
  and returns `503 {"status":"degraded"}` if that fails.

## Recipe: running N instances behind nginx

```nginx
upstream open_runo_backends {
    # keepalive here is proxy->backend (Apache/Tomcat's classic use case);
    # cheap to keep since our backend has no thread-per-connection limit
    # to protect, but it still saves a TCP+TLS handshake per proxied
    # request, so it's worth setting.
    keepalive 32;

    server 127.0.0.1:8081 max_fails=2 fail_timeout=10s;
    server 127.0.0.1:8082 max_fails=2 fail_timeout=10s;
    server 127.0.0.1:8083 max_fails=2 fail_timeout=10s;
}

server {
    listen 443 ssl;
    # ... TLS termination here, so each open-runo-router instance below
    # doesn't need its own cert/ACME renewal (though it can -- see the
    # `tls` feature in hyper_compat.rs if you'd rather terminate TLS in
    # the app itself and skip the proxy for a given deployment).

    location /health {
        proxy_pass http://open_runo_backends;
        # nginx's own passive health check (max_fails/fail_timeout above)
        # already removes an instance whose /health starts 503ing.
    }

    location / {
        proxy_pass http://open_runo_backends;
        proxy_http_version 1.1;
        proxy_set_header Connection "";   # required to use the keepalive pool
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Run each instance with a distinct `OPEN_RUNO_BIND_ADDR` (e.g.
`127.0.0.1:8081`, `:8082`, `:8083`). For a rolling restart: send `SIGTERM`
to one instance, wait for its process to exit (graceful shutdown drains
in-flight requests first), restart it, confirm `/health` returns 200 again,
then move to the next instance.

## What this does *not* need

- FastCGI-buffer-style tuning, named-upstream keepalive pooling as a
  *ported Rust feature* — not applicable, see rationale above.
- Built-in multi-instance clustering/gossip inside the app itself — an
  external LB is simpler and is what this recipe uses.

## Rate limiting: shared across instances (2026-07-14)

**Resolved.** Rate limiting used to live in each process's own memory, so
with N instances behind a load balancer, a client could get roughly `N×`
its intended budget just by landing on a different backend each time.

`open-runo-security::RateLimit` is now a trait implemented by both the
original in-process `RateLimiter` (default, zero setup) and
`redis_backend::RedisRateLimiter` (`redis-backend` Cargo feature): set
`OPEN_RUNO_RATE_LIMIT_REDIS_URL` and every instance pointed at the same
Redis shares one budget per client key, enforced atomically via a Lua
script (`INCR` + conditional `EXPIRE` in one round trip, so two
instances' concurrent requests can't race each other's window timer).
Connecting fails gracefully — an unreachable/misconfigured Redis falls
back to per-instance limiting with a warning log, rather than taking the
app down.

`build_hyper_app` is now `async` to accommodate the (async) Redis
connection setup at startup; existing single-instance deployments that
never set `OPEN_RUNO_RATE_LIMIT_REDIS_URL` see no behavior change.

**Honest verification limit**: this sandbox has neither `redis-server`
nor Docker available, so the `RedisRateLimiter` test
(`shared_budget_is_enforced_across_two_independent_limiter_instances`,
`open-runo-security`) is `#[ignore]`d like this workspace's other
live-external-service tests (ClickHouse, PostgreSQL) — run it explicitly
against a real Redis with `cargo test -p open-runo-security --features
redis-backend -- --ignored --nocapture` (optionally set
`OPEN_RUNO_TEST_REDIS_URL`). The Lua script's atomicity and the
fixed-window algorithm itself are unit-testable without Redis (see the
in-process `RateLimiter` tests, which exercise the same window
semantics) and were verified there; what remains genuinely unverified
here is Redis connectivity/the script executing correctly against a real
server, not the algorithm design.

## Session state: shared across instances (2026-07-14)

**Resolved.** Session cookies used to live in each process's own memory
— a client's session was only valid against the instance that issued it,
so a load balancer without session affinity would intermittently 401 a
logged-in client.

`session::SessionBackend` is now a trait implemented by both the
original in-process `SessionStore` (default, `AppState::new()`) and
`session::redis_backend::RedisSessionStore` (`redis-session` Cargo
feature): connect it yourself (an async operation) and attach it via
`AppState::with_sessions(...)` before wrapping the state in an `Arc` for
`build_hyper_app`. Session data is JSON-encoded and stored with Redis's
own `EX` TTL doing the expiry work the in-process store's lazy-eviction
check otherwise does.

Unlike the rate limiter (which connects and falls back automatically
inside `build_hyper_app`), wiring `RedisSessionStore` in is a manual
step at the call site — `AppState` construction itself stays fully
synchronous so the ~150 existing `#[tokio::test]`s that call
`AppState::new()` don't need to change.

**Honest verification limit**: same as the rate limiter — no
`redis-server`/Docker in this sandbox, so
`a_session_created_via_one_store_is_readable_via_a_second_independent_store`
(`open-runo-router`) is `#[ignore]`d; run it explicitly with
`cargo test -p open-runo-router --features redis-session -- --ignored
--nocapture`. As an alternative to Redis-backed sessions, load-balancer
session affinity (`ip_hash` in nginx, cookie-based affinity in most LBs)
remains a valid option; the `X-Api-Key` auth path is stateless and
unaffected either way.
