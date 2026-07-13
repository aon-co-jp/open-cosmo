//! open-runo Gateway Router — binary entrypoint.
//!
//! Runs on the poem-free `hyper_compat` stack (see CLAUDE.md HANDOFF for
//! the migration history). REST-only binary. For REST + GraphQL in one
//! process, run the `open-runo-gateway` binary instead.

use open_runo_core::Config;
use open_runo_router::{build_hyper_app, grpc, hyper_compat, state::AppState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env().map_err(|e| format!("config error: {e}"))?;

    open_runo_observability::init_tracing_with_otlp(
        &config.log_level,
        config.otlp_endpoint.as_deref(),
        "open-runo-router",
    );

    tracing::info!(
        bind_addr = %config.bind_addr,
        env = %config.environment,
        "starting open-runo-router"
    );

    let state = Arc::new(AppState::new());
    let app = build_hyper_app(
        state,
        config.rate_limit_max_requests,
        config.rate_limit_window_secs as i64,
    );

    let addr = config
        .bind_addr
        .parse()
        .map_err(|e| format!("invalid bind_addr {:?}: {e}", config.bind_addr))?;
    let (bound, handle, _drained) =
        hyper_compat::serve_with_shutdown(app, addr, hyper_compat::shutdown_signal()).await?;
    tracing::info!(%bound, "open-runo-router listening");

    // gRPC (grpc.health.v1.Health/Check, docs/poem-parity.md) is opt-in via
    // a dedicated port -- unset by default so deployments that don't use
    // gRPC don't get a second listener they never asked for. h2c (no TLS)
    // matches how most internal/dev gRPC traffic runs; see grpc.rs's
    // module doc for why this can't share the REST listener's port.
    if let Ok(grpc_bind_addr) = std::env::var("OPEN_RUNO_GRPC_BIND_ADDR") {
        let grpc_addr = grpc_bind_addr
            .parse()
            .map_err(|e| format!("invalid OPEN_RUNO_GRPC_BIND_ADDR {grpc_bind_addr:?}: {e}"))?;
        let (grpc_bound, _grpc_handle) = grpc::serve_grpc(grpc_addr).await?;
        tracing::info!(bound = %grpc_bound, "open-runo-router gRPC (grpc.health.v1.Health) listening");
    }

    handle.await?;

    Ok(())
}
