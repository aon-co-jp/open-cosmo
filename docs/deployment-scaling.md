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

## Known gap (left for a future pass)

Rate limiting (`open-runo-security::RateLimiter`) and session state
currently live in each process's own memory, so with N instances behind a
load balancer, a client's rate-limit budget and session are actually
per-instance, not global. This is a real limitation of running multiple
instances today; moving that state to a shared store (e.g. Redis) is
future work, not implemented in this pass.
