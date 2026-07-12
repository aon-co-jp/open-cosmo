//! `open-runo-router`: the Gateway Router, open-runo's fast entrypoint.
//!
//! ## Endpoints
//!
//! | Method | Path                              | Description                        |
//! |--------|-----------------------------------|------------------------------------|
//! | GET    | `/health`                         | Service health check               |
//! | GET    | `/healthz`                        | Kubernetes-style health alias      |
//! | POST   | `/api/schemas`                    | Register a schema version          |
//! | GET    | `/api/schemas/:service`           | Latest schema for a service        |
//! | GET    | `/api/schemas/:service/history`   | Full schema history                |
//! | POST   | `/api/federation/compose`         | Compose service schemas            |
//! | GET    | `/api/federation/status`          | Current composed schema summary    |
//! | POST   | `/api/ai/route`                   | Select best AI provider            |
//! | GET    | `/api/db/status`                  | DB backend name & health           |
//! | GET    | `/api/db/routing`                 | Per-table routing decisions        |
//! | GET    | `/api/db/:table`                  | List all records in a table        |
//! | GET    | `/api/db/:table/:key`             | Get one record                     |
//! | PUT    | `/api/db/:table/:key`             | Upsert a record                    |
//! | DELETE | `/api/db/:table/:key`             | Delete a record                    |
//!
//! All `/api/*` routes require an `X-Api-Key` header (enforced by
//! [`auth_hyper::check_api_key`]). Health routes are exempt.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![recursion_limit = "256"]

pub mod acme;
pub mod audit;
pub mod auth_hyper;
pub mod edfs;
pub mod grpc;
pub mod handlers_hyper;
pub mod hyper_compat;
pub mod keyring;
pub mod maintenance;
pub mod mcp;
pub mod middleware;
pub mod middleware_hyper;
pub mod openapi;
pub mod session;
pub mod state;
pub mod validation;

use keyring::{GuardianConfig, KeyGuardian};
use open_runo_core::Config;
use state::AppState;
use std::sync::Arc;

/// Resolve the bind address for the gateway from [`Config`].
pub fn bind_addr(config: &Config) -> &str {
    &config.bind_addr
}

// ── App builder (hyper_compat) ───────────────────────────────────────────

/// Build the poem-free [`hyper_compat::Router`] for the gateway. Registers
/// every REST/SCIM/SSE handler, wrapped in CORS + rate-limit + tracing (in
/// that order, outermost first). GraphQL Subscriptions over WebSocket are
/// not available here (see `open-runo-gateway`'s `graphql_hyper` module
/// doc comment); everything else runs poem-free.
pub fn build_hyper_app(state: Arc<AppState>, rate_limit_max: u32, rate_limit_window_secs: i64) -> hyper_compat::Router {
    use hyper::Method;
    use hyper_compat::{Handler, Router};

    let guardian = Arc::new(KeyGuardian::new(Arc::clone(&state.db), GuardianConfig::from_env()));
    let page_cache = Arc::new(middleware::html_cache::HtmlPageCache::new(
        middleware::html_cache::HtmlCacheConfig::from_env(),
    ));
    let limiter = middleware_hyper::build_rate_limiter(rate_limit_max, rate_limit_window_secs);

    maintenance::spawn(Arc::clone(&state), Arc::clone(&page_cache));

    // Wrap every non-health route: rate-limit → tracing → CORS → compression
    // (CORS around the limiter/handler so preflight requests never reach
    // them; compression outermost since it operates on whatever response
    // body the inner stack ultimately produced, including CORS preflight
    // and rate-limit-exceeded responses).
    // Periodically flush buffered request metrics to their configured sink
    // (ClickHouse if `OPEN_RUNO_CLICKHOUSE_URL` + the `clickhouse` feature
    // are set, otherwise an in-memory sink nothing reads back from).
    // Deliberately not awaited/blocking here -- same fire-and-forget
    // background-task shape as `maintenance::spawn` just above.
    open_runo_observability::spawn_periodic_flush(
        Arc::clone(&state.request_metrics),
        std::time::Duration::from_secs(30),
    );

    let wrap = |h: Handler| -> Handler {
        middleware_hyper::with_compression(middleware_hyper::with_cors(middleware_hyper::with_tracing(
            middleware_hyper::with_metrics(
                middleware_hyper::with_shared_rate_limit(h, Arc::clone(&limiter)),
                Arc::clone(&state.request_metrics),
            ),
        )))
    };

    Router::new()
        .route(Method::GET, "/health", wrap(hyper_compat::health_handler()))
        .route(Method::GET, "/healthz", wrap(hyper_compat::health_handler()))
        .route(Method::GET, "/api/openapi.json", wrap(openapi::openapi_handler()))
        .route(
            Method::GET,
            "/.well-known/acme-challenge/:token",
            wrap(acme::challenge_response_handler(Arc::clone(&state.acme_challenges))),
        )
        .route(
            Method::POST,
            "/api/keys/self-issue",
            wrap(handlers_hyper::self_issue_key_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/mcp",
            wrap(mcp::mcp_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/session/login",
            wrap(handlers_hyper::session_login_handler(Arc::clone(&guardian), Arc::clone(&state.sessions))),
        )
        .route(
            Method::POST,
            "/api/session/logout",
            wrap(handlers_hyper::session_logout_handler(Arc::clone(&state.sessions))),
        )
        .route(
            Method::POST,
            "/api/schemas",
            wrap(handlers_hyper::register_schema_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/schemas/upload",
            wrap(handlers_hyper::register_schema_upload_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/schemas/upload-raw",
            wrap(handlers_hyper::register_schema_upload_raw_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/schemas/:service",
            wrap(handlers_hyper::get_schema_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/schemas/:service/history",
            wrap(handlers_hyper::get_schema_history_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/federation/compose",
            wrap(handlers_hyper::compose_schemas_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/federation/status",
            wrap(handlers_hyper::federation_status_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/ai/route",
            wrap(handlers_hyper::route_request_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/db/status",
            wrap(handlers_hyper::db_status_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/db/routing",
            wrap(handlers_hyper::db_routing_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/db/:table",
            wrap(handlers_hyper::db_list_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/db/:table/:key",
            wrap(handlers_hyper::db_get_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::PUT,
            "/api/db/:table/:key",
            wrap(handlers_hyper::db_put_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::DELETE,
            "/api/db/:table/:key",
            wrap(handlers_hyper::db_delete_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/cache/purge",
            wrap(handlers_hyper::purge_page_handler(Arc::clone(&state), Arc::clone(&page_cache), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/cache/purge-all",
            wrap(handlers_hyper::purge_all_pages_handler(Arc::clone(&state), Arc::clone(&page_cache), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/cache/ai-stats",
            wrap(handlers_hyper::ai_stats_handler(Arc::clone(&state), Arc::clone(&page_cache), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/backup/export",
            wrap(handlers_hyper::backup_export_handler(Arc::clone(&state), Arc::clone(&page_cache), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/backup/import",
            wrap(handlers_hyper::backup_import_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/backup/restore-latest",
            wrap(handlers_hyper::backup_restore_latest_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/migrate/export-sql",
            wrap(handlers_hyper::migrate_export_sql_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/migrate/export-csv",
            wrap(handlers_hyper::migrate_export_csv_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/integrity/check",
            wrap(handlers_hyper::integrity_check_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/api/persisted-queries",
            wrap(handlers_hyper::register_persisted_query_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/persisted-queries/:hash",
            wrap(handlers_hyper::get_persisted_query_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/scim/v2/Users",
            wrap(handlers_hyper::scim_list_users_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/scim/v2/Users",
            wrap(handlers_hyper::scim_create_user_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/scim/v2/Users/:id",
            wrap(handlers_hyper::scim_get_user_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::PUT,
            "/scim/v2/Users/:id",
            wrap(handlers_hyper::scim_replace_user_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::DELETE,
            "/scim/v2/Users/:id",
            wrap(handlers_hyper::scim_delete_user_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/scim/v2/Groups",
            wrap(handlers_hyper::scim_list_groups_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::POST,
            "/scim/v2/Groups",
            wrap(handlers_hyper::scim_create_group_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/scim/v2/Groups/:id",
            wrap(handlers_hyper::scim_get_group_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::PUT,
            "/scim/v2/Groups/:id",
            wrap(handlers_hyper::scim_replace_group_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::DELETE,
            "/scim/v2/Groups/:id",
            wrap(handlers_hyper::scim_delete_group_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/events",
            wrap(handlers_hyper::stream_events_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        // Generic WebSocket routes (docs/poem-parity.md 3, "generic
        // WebSocket support"): NOT wrapped in `wrap(...)` -- `with_compression`
        // buffers/rewrites the response body, which is harmless for an empty
        // `101 Switching Protocols` body but pointless overhead for a route
        // that's never going to have a compressible body; CORS/tracing add
        // nothing an upgrade response needs either. Auth for `/api/ws-events`
        // is enforced inside `ws_events_handler` itself instead.
        .route(Method::GET, "/api/ws-echo", handlers_hyper::ws_echo_handler())
        .route(
            Method::GET,
            "/api/ws-events",
            handlers_hyper::ws_events_handler(Arc::clone(&state), Arc::clone(&guardian)),
        )
        .route(
            Method::POST,
            "/api/feature-flags",
            wrap(handlers_hyper::feature_flag_upsert_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/feature-flags",
            wrap(handlers_hyper::feature_flag_list_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/feature-flags/:name",
            wrap(handlers_hyper::feature_flag_get_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::DELETE,
            "/api/feature-flags/:name",
            wrap(handlers_hyper::feature_flag_delete_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/feature-flags/:name/evaluate",
            wrap(handlers_hyper::feature_flag_evaluate_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        // ── Analytics (docs/cosmo-parity.md 4a: monthly request-count
        // metering + Cosmo Studio-style operation latency/error-rate) ────
        .route(
            Method::GET,
            "/api/analytics/requests-per-month",
            wrap(handlers_hyper::requests_per_month_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        .route(
            Method::GET,
            "/api/analytics/operations",
            wrap(handlers_hyper::operations_summary_handler(Arc::clone(&state), Arc::clone(&guardian))),
        )
        // ── WASM frontend bundle (apps/desktop-wasm/www) ─────────────────
        // Directory is configurable via OPEN_RUNO_STATIC_DIR so the
        // binary can be run from any working directory; defaults to the
        // conventional path relative to the repo root (dev convenience).
        .route(
            Method::GET,
            "/",
            wrap(hyper_compat::static_file_handler(
                static_dir().join("index.html"),
                "text/html; charset=utf-8",
            )),
        )
        .route(
            Method::GET,
            "/pkg/open_runo_desktop_wasm.js",
            wrap(hyper_compat::static_file_handler(
                static_dir().join("pkg/open_runo_desktop_wasm.js"),
                "text/javascript",
            )),
        )
        .route(
            Method::GET,
            "/pkg/open_runo_desktop_wasm_bg.wasm",
            wrap(hyper_compat::static_file_handler(
                static_dir().join("pkg/open_runo_desktop_wasm_bg.wasm"),
                "application/wasm",
            )),
        )
        // PWA manifest + icon, so the app can be "installed" from the
        // browser (standalone window, home-screen icon) — the closest
        // no-Tauri equivalent to Tauri's native app-bundle experience.
        .route(
            Method::GET,
            "/manifest.json",
            wrap(hyper_compat::static_file_handler(
                static_dir().join("manifest.json"),
                "application/manifest+json",
            )),
        )
        .route(
            Method::GET,
            "/icon.svg",
            wrap(hyper_compat::static_file_handler(
                static_dir().join("icon.svg"),
                "image/svg+xml",
            )),
        )
        // Every route above registers only its actual method; without this,
        // a cross-origin browser's CORS preflight (OPTIONS) has no handler
        // to reach and the router's own dispatch fallback answers with a
        // bare 405 before with_cors (or any middleware) ever runs. See
        // Router::with_cors_preflight's doc comment.
        .with_cors_preflight()
}

/// Resolve the WASM frontend's static asset directory. Defaults to
/// `apps/desktop-wasm/www` relative to the current working directory
/// (the convention for `cargo run` from the repo root); override with
/// `OPEN_RUNO_STATIC_DIR` for other layouts (e.g. a packaged deploy).
/// Falls back to a `poem-cosmo-tauri/`-prefixed variant for launchers
/// that run `cargo run --manifest-path poem-cosmo-tauri/Cargo.toml`
/// from a parent directory (e.g. this repo's sibling-checkout layout).
fn static_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("OPEN_RUNO_STATIC_DIR") {
        return std::path::PathBuf::from(dir);
    }
    let direct = std::path::PathBuf::from("apps/desktop-wasm/www");
    if direct.join("index.html").exists() {
        return direct;
    }
    std::path::PathBuf::from("poem-cosmo-tauri/apps/desktop-wasm/www")
}

#[cfg(test)]
mod hyper_app_tests {
    use super::*;

    #[tokio::test]
    async fn hyper_app_serves_health_and_protected_routes() {
        let state = Arc::new(AppState::new());
        let app = build_hyper_app(state, 1_000, 60);
        let (addr, _handle) = hyper_compat::serve(app, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/federation/status"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        let resp = client
            .get(format!("http://{addr}/api/federation/status"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // CORS header present on a normal (non-preflight) response too.
        assert!(resp.headers().contains_key("access-control-allow-origin"));
    }

    /// A cross-origin browser sending `X-Api-Key` (a non-simple header)
    /// must first send an `OPTIONS` preflight and get a `2xx` with CORS
    /// headers back before it will send the real request at all. Every
    /// route in `build_hyper_app` registers only its own method (GET/
    /// POST/etc.), never OPTIONS -- without `Router::with_cors_preflight`
    /// wired in, this preflight hit the router's own bare-405 fallback
    /// and no cross-origin browser client could ever call any
    /// `api_key_security()`-protected route. Found via an actual
    /// cross-origin browser test (see CLAUDE.md HANDOFF, 2026-07-11).
    #[tokio::test]
    async fn hyper_app_answers_cors_preflight_on_a_real_protected_route() {
        let state = Arc::new(AppState::new());
        let app = build_hyper_app(state, 1_000, 60);
        let (addr, _handle) = hyper_compat::serve(app, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .request(reqwest::Method::OPTIONS, format!("http://{addr}/api/federation/status"))
            .header("origin", "https://example.com")
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "x-api-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(resp.headers().contains_key("access-control-allow-origin"));
        assert!(resp.headers().contains_key("access-control-allow-headers"));
    }

    #[tokio::test]
    async fn hyper_app_enforces_shared_rate_limit_across_routes() {
        let state = Arc::new(AppState::new());
        let app = build_hyper_app(state, 2, 60);
        let (addr, _handle) = hyper_compat::serve(app, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        client.get(format!("http://{addr}/health")).send().await.unwrap();
        client.get(format!("http://{addr}/healthz")).send().await.unwrap();
        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    }
}
