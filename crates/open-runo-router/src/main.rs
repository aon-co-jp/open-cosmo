//! open-runo Gateway Router — binary entrypoint.
//!
//! Runs on the poem-free `hyper_compat` stack (see CLAUDE.md HANDOFF for
//! the migration history). REST-only binary. For REST + GraphQL in one
//! process, run the `open-runo-gateway` binary instead.

use open_runo_core::Config;
use open_runo_router::{build_hyper_app, grpc, hyper_compat, state::AppState, udp_notice};
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

    // 2026-08-19新設(自動アップデート機能): 既定は無効
    // (`OPEN_RUNO_ROUTER_SELF_UPDATE=true`が必要、詳細は
    // `self_update.rs`モジュールdoc「正直な開示」参照)。
    tokio::spawn(open_runo_router::self_update::check_and_apply_update());

    let state = Arc::new(AppState::new());
    let app = build_hyper_app(
        Arc::clone(&state),
        config.rate_limit_max_requests,
        config.rate_limit_window_secs as i64,
    )
    .await;

    // EDFS (Event-Driven Federated Subscriptions, docs/cosmo-parity.md
    // §4a): opt-in via env vars, unset by default so a single-instance
    // deployment (the existing default assumption) doesn't need Redis at
    // all. When configured, this instance both publishes its own schema
    // events to Redis (so other instances see them) and subscribes to
    // receive events *other* instances published (so this instance's own
    // GraphQL Subscription clients see them too) -- see edfs.rs's module
    // doc for the full bridge design.
    #[cfg(feature = "edfs")]
    if let Ok(redis_url) = std::env::var("OPEN_RUNO_EDFS_REDIS_URL") {
        let channel = std::env::var("OPEN_RUNO_EDFS_CHANNEL")
            .unwrap_or_else(|_| "open-runo:schema-events".to_string());
        *state
            .edfs_publish
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((redis_url.clone(), channel.clone()));
        match open_runo_router::edfs::spawn_bridge(Arc::clone(&state), &redis_url, &channel).await {
            Ok(_handle) => tracing::info!(channel, "EDFS: subscribed to Redis Pub/Sub bridge"),
            Err(error) => {
                tracing::warn!(%error, "EDFS: failed to start Redis Pub/Sub bridge; continuing without it")
            },
        }
    }

    let addr = config
        .bind_addr
        .parse()
        .map_err(|e| format!("invalid bind_addr {:?}: {e}", config.bind_addr))?;
    let (bound, handle) = hyper_compat::serve(app, addr).await?;
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

    // UDP-IP冗長経路(即時通知)の受信側。`open-web-server-ledger`側の
    // 送信(`Ledger::enable_udp_redundant_path`)は2026-07-11から実装
    // されていたが、この受信リスナー自体(`udp_notice::spawn_from_env`)は
    // 2026-07-23に実装されたにもかかわらず、この起動シーケンスから一度も
    // 呼び出されていなかった——「送信側は動くが受信側が実際には起動
    // しない」という実配線漏れが発覚したため追加(ユーザー指摘、
    // 2026-07-23)。`OPEN_RUNO_UDP_NOTICE_BIND`等が未設定なら何もしない
    // (既存のgRPC/EDFSと同じopt-inパターン)。
    let udp_notice_stats = Arc::new(udp_notice::NoticeStats::default());
    if let Some((udp_bound, _udp_handle)) =
        udp_notice::spawn_from_env(Arc::clone(&udp_notice_stats)).await
    {
        tracing::info!(bound = %udp_bound, "open-runo-router UDP advance-notice listener started");
    }

    handle.await?;

    Ok(())
}
