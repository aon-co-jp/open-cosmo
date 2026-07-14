//! open-runo full-stack binary: REST gateway + GraphQL endpoint.
//!
//! Mounts the complete `open-runo-router` REST surface (including the WASM
//! frontend bundle) at `/` and the versionless GraphQL endpoint at
//! `/graphql`, sharing one `AppState`. Runs on the poem-free
//! `hyper_compat` stack (see CLAUDE.md HANDOFF).
//!
//! **Scope note**: GraphQL Subscriptions over WebSocket are not available
//! on this binary yet (see `open_runo_gateway::graphql_hyper`'s doc
//! comment) — only `GET /graphql` (GraphiQL) and `POST /graphql` (query
//! execution).

use open_runo_core::Config;
use open_runo_router::{build_hyper_app, hyper_compat, state::AppState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env().map_err(|e| format!("config error: {e}"))?;

    open_runo_observability::init_tracing_with_otlp(
        &config.log_level,
        config.otlp_endpoint.as_deref(),
        "open-runo-gateway",
    );

    tracing::info!(
        bind_addr = %config.bind_addr,
        env = %config.environment,
        "starting open-runo-gateway (REST + GraphQL)"
    );

    let state = Arc::new(AppState::new());
    let (graphiql, graphql_post) = open_runo_gateway::graphql_hyper::graphql_handlers(Arc::clone(&state));

    let app = build_hyper_app(
        state,
        config.rate_limit_max_requests,
        config.rate_limit_window_secs as i64,
    )
    .await
    .route(hyper::Method::GET, "/graphql", graphiql)
    .route(hyper::Method::POST, "/graphql", graphql_post);

    let addr = config
        .bind_addr
        .parse()
        .map_err(|e| format!("invalid bind_addr {:?}: {e}", config.bind_addr))?;
    let (bound, handle, _drained) =
        hyper_compat::serve_with_shutdown(app, addr, hyper_compat::shutdown_signal()).await?;
    tracing::info!(%bound, "open-runo-gateway listening");
    handle.await?;

    Ok(())
}
