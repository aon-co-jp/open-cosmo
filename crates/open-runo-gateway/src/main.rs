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

use open_runo_appserver::server::{ServerConfig, ThreadedProxyServer};
use open_runo_appserver::tenant_bridge::SupervisedTenantRegistry;
use open_runo_core::Config;
use open_runo_router::keyring::{GuardianConfig, KeyGuardian};
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
        "starting open-runo-gateway (GraphQL + SCIM/MCP + admin UI)"
    );

    let state = Arc::new(AppState::new());
    let request_metrics = Arc::clone(&state.request_metrics);
    // 管理用 gRPC(`open_runo.admin.v1.Admin`)。通信路は2つ: 素のgRPC(HTTP/2、
    // `OPEN_RUNO_GRPC_BIND_ADDR`で別ポート)と、ブラウザ(管理画面WASM)向けのgRPC-web
    // (本体のHTTPルーター上の`POST /open_runo.admin.v1.Admin/:method`)。
    let grpc_guardian = Arc::new(KeyGuardian::new(
        Arc::clone(&state.db),
        GuardianConfig::from_env(),
    ));
    let admin_grpc = open_runo_router::grpc_admin::AdminGrpc::new(
        Arc::clone(&state),
        Arc::clone(&grpc_guardian),
    );
    // GraphQL(Federationの問い合わせ)の本番向け防御(登録済みクエリのみ・深さ/複雑さ上限・
    // イントロスペクション停止・エイリアス/バッチ対策)。`OPEN_RUNO_ENV=production`で既定オン。
    let graphql_sec =
        open_runo_gateway::security::GraphqlSecurity::for_environment(config.environment);
    tracing::info!(
        pq_mode = ?graphql_sec.pq_mode,
        max_depth = graphql_sec.max_depth,
        max_complexity = graphql_sec.max_complexity,
        max_root_fields = graphql_sec.max_root_fields,
        introspection = graphql_sec.introspection,
        expose_schema = graphql_sec.expose_schema,
        "GraphQL security"
    );
    let graphql_limiter = open_runo_router::middleware_hyper::build_shared_rate_limiter(
        config.rate_limit_max_requests,
        config.rate_limit_window_secs as i64,
    )
    .await;
    let (graphiql, graphql_post) = open_runo_gateway::graphql_hyper::graphql_handlers_with(
        Arc::clone(&state),
        graphql_sec.clone(),
        Some(Arc::clone(&graphql_limiter)),
    );
    let (graphql_sdl, graphql_ws) = open_runo_gateway::graphql_hyper::graphql_extra_handlers_with(
        Arc::clone(&state),
        &graphql_sec,
    );
    let (demo_playground, demo_graphql) = open_runo_gateway::demo::demo_handlers();

    // アプリケーションサーバー層(「第二のTomcat」)の多重テナント管理。
    // `SupervisedTenantRegistry`(2026-08-06、`process_lifecycle`と統合)は
    // 「テナント登録(host→backend_addr)」と「そのバックエンドプロセス自体
    // の起動・監視」を1つのレジストリでオプトインに束ねる——`appserver_
    // tenants`(プロキシ専用登録、後方互換)と`appserver_processes`
    // (プロセス管理、新規)の両管理APIが**同じレジストリを共有**するため、
    // どちらのAPI経由で登録したホストも同じ`Dispatcher`/プロキシリスナー
    // から一貫して到達可能になる(スレッド間共有・実行時追加/削除が可能、
    // ドメインを追加するたびに新しいプロセスを個別インストールする必要が
    // 無くなる「分身の術」構想、2026-07-16)。管理API自体は常時有効、
    // 実際のプロキシリスナー(`OPEN_RUNO_APPSERVER_PROXY_BIND`)は
    // 明示設定時のみ。
    let appserver_guardian = Arc::new(KeyGuardian::new(
        Arc::clone(&state.db),
        GuardianConfig::from_env(),
    ));
    let appserver_registry = Arc::new(SupervisedTenantRegistry::new());

    // ブラウザ向けの入口(gRPC-web / gRPC-web-text / Connect を同じパスで受ける)。
    // 別オリジンのブラウザ(connect-rustのWASMクライアント等)から呼べるようCORSも付ける。
    let grpc_web = open_runo_router::middleware_hyper::with_cors(
        open_runo_router::middleware_hyper::with_tracing(
            open_runo_router::middleware_hyper::with_metrics(
                open_runo_router::middleware_hyper::with_shared_rate_limit(
                    open_runo_router::grpc_admin::browser_handler(admin_grpc.clone()),
                    Arc::clone(&graphql_limiter),
                ),
                Arc::clone(&request_metrics),
            ),
        ),
    );
    let mut app = build_hyper_app(
        state,
        config.rate_limit_max_requests,
        config.rate_limit_window_secs as i64,
    )
    .await
    .route(
        hyper::Method::POST,
        "/open_runo.admin.v1.Admin/:method",
        grpc_web,
    )
    .route(hyper::Method::GET, "/graphql/ws", graphql_ws)
    .route(
        hyper::Method::POST,
        "/graphql",
        // REST全廃後は管理操作もここを通るため、旧`/api/*`と同じく
        // 流量制限→メトリクス→トレース→CORS→圧縮の順で包む(流量制限の枠は
        // `build_hyper_app`側とは別に、同じ設定値で持つ)。
        open_runo_router::middleware_hyper::with_compression(
            open_runo_router::middleware_hyper::with_cors(
                open_runo_router::middleware_hyper::with_tracing(
                    open_runo_router::middleware_hyper::with_metrics(
                        open_runo_router::middleware_hyper::with_shared_rate_limit(
                            graphql_post,
                            graphql_limiter,
                        ),
                        request_metrics,
                    ),
                ),
            ),
        ),
    )
    // Fixed, self-contained GraphQL Federation demo (§ see
    // `open_runo_gateway::demo` module doc): composes two sample
    // subgraphs with the real `open-runo-federation` core and serves
    // real, queryable GraphQL over fixed sample data — no separate
    // upstream services or seeded database required.
    .route(hyper::Method::GET, "/demo", demo_playground)
    .route(hyper::Method::POST, "/demo/graphql", demo_graphql);
    // GraphiQLとSDLは、設定が許すとき(開発)だけ。本番ではスキーマを自分から公開しない。
    app = open_runo_gateway::graphql_hyper::mount_schema_endpoints(
        app,
        &graphql_sec,
        graphiql,
        graphql_sdl,
    );
    for (method, pattern, handler) in open_runo_gateway::appserver_tenants::routes(
        appserver_registry.dispatcher(),
        Arc::clone(&appserver_guardian),
    ) {
        app = app.route(method, pattern, handler);
    }
    for (method, pattern, handler) in open_runo_gateway::appserver_processes::routes(
        Arc::clone(&appserver_registry),
        appserver_guardian,
    ) {
        app = app.route(method, pattern, handler);
    }

    // `ThreadedProxyServer`はDropで自らのワーカースレッドを停止するため、
    // `main`が`handle.await`でブロックしている間ずっと生存させる必要が
    // ある(先に束縛を外に置き、有効時のみ`Some`にする)。
    let _appserver_proxy = match std::env::var("OPEN_RUNO_APPSERVER_PROXY_BIND").ok() {
        Some(bind) => {
            let server = ThreadedProxyServer::start(
                &bind,
                appserver_registry.dispatcher(),
                ServerConfig::default(),
            )?;
            tracing::info!(bind_addr = %bind, "appserver tenant proxy listening (shared, multi-domain, no per-domain install)");
            Some(server)
        },
        None => None,
    };

    // 素のgRPC(HTTP/2)。本番ではTLS 1.3必須(平文は`OPEN_RUNO_GRPC_ALLOW_PLAINTEXT=1`で明示したときだけ)。
    //  - `OPEN_RUNO_GRPC_TLS_CERT` / `OPEN_RUNO_GRPC_TLS_KEY`: サーバー証明書チェーンと秘密鍵(PEMファイル)
    //  - `OPEN_RUNO_GRPC_CLIENT_CA`: 指定するとmTLS(このCAで署名されたクライアント証明書が無い接続は拒否)
    //  - `OPEN_RUNO_GRPC_REFLECTION`: 1/0。未指定なら本番はオフ(APIの構造を外へ見せない)
    let grpc_is_prod = config.environment == open_runo_core::Environment::Production;
    let grpc_opts = open_runo_router::grpc::GrpcOptions {
        reflection: match std::env::var("OPEN_RUNO_GRPC_REFLECTION").ok().as_deref() {
            Some("1" | "true") => true,
            Some("0" | "false") => false,
            _ => !grpc_is_prod,
        },
    };
    let _grpc_server = match std::env::var("OPEN_RUNO_GRPC_BIND_ADDR").ok() {
        Some(bind) => {
            let addr: std::net::SocketAddr = bind
                .parse()
                .map_err(|e| format!("invalid OPEN_RUNO_GRPC_BIND_ADDR {bind:?}: {e}"))?;
            let cert = std::env::var("OPEN_RUNO_GRPC_TLS_CERT").ok();
            let key = std::env::var("OPEN_RUNO_GRPC_TLS_KEY").ok();
            let client_ca = std::env::var("OPEN_RUNO_GRPC_CLIENT_CA").ok();
            match (cert, key) {
                (Some(cert), Some(key)) => {
                    let cfg = open_runo_router::grpc::grpc_tls_config(
                        &std::fs::read(&cert).map_err(|e| format!("reading {cert}: {e}"))?,
                        &std::fs::read(&key).map_err(|e| format!("reading {key}: {e}"))?,
                        client_ca
                            .as_ref()
                            .map(|p| std::fs::read(p).map_err(|e| format!("reading {p}: {e}")))
                            .transpose()?
                            .as_deref(),
                    )?;
                    let (bound, handle) = open_runo_router::grpc::serve_grpc_tls(
                        addr,
                        Some(admin_grpc.clone()),
                        cfg,
                        grpc_opts,
                    )
                    .await?;
                    tracing::info!(
                        %bound,
                        mtls = client_ca.is_some(),
                        reflection = grpc_opts.reflection,
                        "admin gRPC listening (TLS 1.3)"
                    );
                    Some(handle)
                },
                (None, None) => {
                    if grpc_is_prod
                        && std::env::var("OPEN_RUNO_GRPC_ALLOW_PLAINTEXT")
                            .ok()
                            .as_deref()
                            != Some("1")
                    {
                        return Err("production requires TLS 1.3 for gRPC: set \
                            OPEN_RUNO_GRPC_TLS_CERT and OPEN_RUNO_GRPC_TLS_KEY (and optionally \
                            OPEN_RUNO_GRPC_CLIENT_CA for mTLS), or OPEN_RUNO_GRPC_ALLOW_PLAINTEXT=1 \
                            behind a trusted network"
                            .into());
                    }
                    let (bound, handle) = open_runo_router::grpc::serve_grpc_opts(
                        addr,
                        Some(admin_grpc.clone()),
                        grpc_opts,
                    )
                    .await?;
                    tracing::warn!(%bound, "admin gRPC listening in PLAINTEXT (h2c): dev/closed network only");
                    Some(handle)
                },
                _ => {
                    return Err(
                        "OPEN_RUNO_GRPC_TLS_CERT and OPEN_RUNO_GRPC_TLS_KEY must be set together"
                            .into(),
                    )
                },
            }
        },
        None => None,
    };

    // `/graphql` is registered after `build_hyper_app`, so browsers' CORS preflight needs an OPTIONS route here too.
    let app = app.with_cors_preflight();

    let addr = config
        .bind_addr
        .parse()
        .map_err(|e| format!("invalid bind_addr {:?}: {e}", config.bind_addr))?;
    let (bound, handle) = hyper_compat::serve(app, addr).await?;
    tracing::info!(%bound, "open-runo-gateway listening");
    handle.await?;

    Ok(())
}
