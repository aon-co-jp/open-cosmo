//! アプリケーションサーバー層(「第二のTomcat」)の**プロセス管理**API。
//!
//! `appserver_tenants.rs`(2026-07-16新設)がテナント登録
//! (host→backend_addrの動的振り分け)を管理API化したのに対し、本モジュールは
//! 2026-08-06新設の`open_runo_appserver::process_lifecycle::
//! ProcessLifecycleManager`(実プロセスの起動・HTTPヘルスチェック・
//! crash-loop backoff自動再起動・明示的停止/再起動)を、同じHTTP経由の
//! 管理APIとして公開する薄い層——HANDOFF 2026-08-06エントリの
//! 「正直な開示・未着手事項 (2)」で明記されていた、このリポジトリの
//! Rustレベルの管理メソッド(`stop_process`/`restart_process`/
//! `process_status`)止まりだった状態を、HTTP経由でも操作可能にする。
//!
//! `open_runo_appserver::tenant_bridge::SupervisedTenantRegistry`は
//! 「テナント登録(host→backend_addr)」と「そのバックエンドプロセス自体の
//! 起動・監視」をオプトインで束ねる単一のレジストリ——`appserver_tenants`
//! (プロキシ専用登録・後方互換)と本モジュール(プロセス管理・新機能)は
//! **同じ`SupervisedTenantRegistry`インスタンスを共有**することで、
//! どちらのAPI経由で登録したホストも同じ`Dispatcher`/管理対象として
//! 一貫して扱える(`main.rs`側の配線を参照)。
//!
//! 認証は既存の`appserver_tenants`と同じく`auth_hyper::check_api_key`
//! (`X-Api-Key`)を再利用する。

use std::sync::Arc;
use std::time::Duration;

use hyper::{Method, StatusCode};
use open_runo_appserver::process_lifecycle::HealthState;
use open_runo_appserver::tenant_bridge::SupervisedTenantRegistry;
use open_runo_appserver::{RestartPolicy, RuntimeProfile};
use open_runo_router::auth_hyper::check_api_key;
use open_runo_router::hyper_compat::{
    json_response, query_params, read_json_body, Handler, Params,
};
use open_runo_router::keyring::KeyGuardian;
use serde::{Deserialize, Serialize};

/// `?timeout_ms=N`のようなクエリパラメータからグレースフルシャットダウンの
/// タイムアウトを読み取る。未指定・パース不能なら`default_ms`
/// (このモジュールの既定値、呼び出し側で明記)。
fn graceful_timeout_from_query(
    req: &open_runo_router::hyper_compat::Request,
    default_ms: u64,
) -> Duration {
    let ms = query_params(req)
        .get("timeout_ms")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default_ms);
    Duration::from_millis(ms)
}

/// `POST /admin/appserver-processes`のリクエストボディ。
/// `restart_policy`/`poll_interval_ms`は省略可(既定値を使う)。
#[derive(Deserialize)]
struct RegisterManagedRequest {
    host: String,
    profile: RuntimeProfile,
    #[serde(default)]
    restart_policy: Option<RestartPolicy>,
    /// 監視スレッドがヘルスチェック+crash検知を行う間隔(ミリ秒)。
    /// 省略時は1000ms(このモジュールが定める運用既定値、
    /// `ProcessLifecycleManager`自体は特定の既定値を強制しない)。
    #[serde(default)]
    poll_interval_ms: Option<u64>,
}

#[derive(Serialize)]
struct ProcessStatusEntry {
    host: String,
    /// 管理対象プロセスが存在しない(プロキシ専用登録、または未登録)場合は
    /// `null`——「管理していない」ことと「異常」を混同しないための明示。
    status: Option<HealthState>,
}

/// `POST /admin/appserver-processes/:host/stop-graceful`・
/// `stop-all-graceful`共通のレスポンス片。`graceful=true`はSIGTERMのみで
/// 終了(理想的な後始末)、`false`はタイムアウトして強制終了
/// (SIGKILL相当)へフォールバックしたことを示す——エラーではないので
/// ステータスコードは常に200(対象が存在しない場合のみ404)。
#[derive(Serialize)]
struct GracefulStopEntry {
    host: String,
    graceful: bool,
}

/// `POST /admin/appserver-processes` — テナント登録と同時にバックエンド
/// プロセス自体を起動・監視下に置く(`register_with_managed_process`)。
/// 既に同じホストに管理対象プロセスがあれば、まず停止してから新しい
/// プロファイルで再登録する(`SupervisedTenantRegistry`の冪等な再設定)。
pub fn register_handler(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let registry = Arc::clone(&registry);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            let body: RegisterManagedRequest = match read_json_body(req).await {
                Ok(b) => b,
                Err(resp) => return resp,
            };

            let policy = body.restart_policy.unwrap_or_default();
            let poll_interval = Duration::from_millis(body.poll_interval_ms.unwrap_or(1000));

            // `register_with_managed_process`は実際に子プロセスをspawnする
            // ブロッキング呼び出しのため、tokioワーカースレッドを塞がない
            // よう`spawn_blocking`へ退避する(既存の`maintenance.rs`と同じ
            // 「非同期コンテキスト内のブロッキング呼び出しはオフロード」
            // 方針、2026-07-13運用ルール参照)。
            let host = body.host.clone();
            let host_for_task = host.clone();
            let result = tokio::task::spawn_blocking(move || {
                registry.register_with_managed_process(
                    &host_for_task,
                    body.profile,
                    policy,
                    poll_interval,
                );
            })
            .await;

            if result.is_err() {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": "failed to start managed process" }),
                );
            }

            json_response(
                StatusCode::OK,
                &serde_json::json!({ "status": "registered", "host": host }),
            )
        })
    })
}

/// `GET /admin/appserver-processes/:host` — 管理対象プロセスの現在の
/// 健康状態(`ProcessLifecycleManager::status()`相当)。
pub fn status_handler(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, params: Params| {
        let registry = Arc::clone(&registry);
        let guardian = Arc::clone(&guardian);
        let host = params.get("host").unwrap_or_default().to_string();
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            let status = registry.process_status(&host);
            json_response(StatusCode::OK, &ProcessStatusEntry { host, status })
        })
    })
}

/// `POST /admin/appserver-processes/:host/stop` — 明示的停止
/// (`stop_process`、以後この登録は監視スレッドによる自動再起動をしない
/// ——`ProcessLifecycleManager`の既存の「停止は再起動しない」意味論の
/// まま)。管理対象でないホスト(プロキシ専用登録・未登録)を指定した
/// 場合は404。
pub fn stop_handler(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, params: Params| {
        let registry = Arc::clone(&registry);
        let guardian = Arc::clone(&guardian);
        let host = params.get("host").unwrap_or_default().to_string();
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            if registry.stop_process(&host) {
                json_response(
                    StatusCode::OK,
                    &serde_json::json!({ "status": "stopped", "host": host }),
                )
            } else {
                json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("no managed process for host: {host}") }),
                )
            }
        })
    })
}

/// `POST /admin/appserver-processes/:host/restart` — 明示的再起動
/// (`restart_process`、`Stopped`/`GaveUp`いずれからも復帰可能)。
/// 管理対象でないホストを指定した場合は404。
pub fn restart_handler(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, params: Params| {
        let registry = Arc::clone(&registry);
        let guardian = Arc::clone(&guardian);
        let host = params.get("host").unwrap_or_default().to_string();
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            if registry.restart_process(&host) {
                json_response(
                    StatusCode::OK,
                    &serde_json::json!({ "status": "restarted", "host": host }),
                )
            } else {
                json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("no managed process for host: {host}") }),
                )
            }
        })
    })
}

/// `GET /admin/appserver-processes` — **複数プロセスの一括管理(1)**:
/// このレジストリが管理している全プロセスの一覧+健康状態
/// (`SupervisedTenantRegistry::list_managed`)。プロキシ専用登録
/// (`register_proxy_only`)のホストは含まれない(`status_handler`の
/// `null`と同じ線引き)。
pub fn list_handler(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let registry = Arc::clone(&registry);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }
            let entries: Vec<ProcessStatusEntry> = registry
                .list_managed()
                .into_iter()
                .map(|(host, status)| ProcessStatusEntry {
                    host,
                    status: Some(status),
                })
                .collect();
            json_response(StatusCode::OK, &serde_json::json!({ "processes": entries }))
        })
    })
}

/// `POST /admin/appserver-processes/stop-all` — **複数プロセスの一括管理
/// (2)**: 管理対象の全プロセスへ即時停止(`stop()`、SIGKILL相当)を発行
/// する(`SupervisedTenantRegistry::stop_all`)。段階的グレースフル停止が
/// 必要な場合は`?graceful=true&timeout_ms=N`(既定5000ms)を付ける。
pub fn stop_all_handler(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let registry = Arc::clone(&registry);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            let graceful_requested = query_params(&req)
                .get("graceful")
                .map(|v| v == "true")
                .unwrap_or(false);
            let timeout = graceful_timeout_from_query(&req, 5000);

            if graceful_requested {
                let results = {
                    let registry = Arc::clone(&registry);
                    tokio::task::spawn_blocking(move || registry.stop_all_graceful(timeout))
                        .await
                        .unwrap_or_default()
                };
                let entries: Vec<GracefulStopEntry> = results
                    .into_iter()
                    .map(|(host, graceful)| GracefulStopEntry { host, graceful })
                    .collect();
                json_response(StatusCode::OK, &serde_json::json!({ "stopped": entries }))
            } else {
                let stopped = registry.stop_all();
                json_response(StatusCode::OK, &serde_json::json!({ "stopped": stopped }))
            }
        })
    })
}

/// `POST /admin/appserver-processes/:host/stop-graceful?timeout_ms=N`
/// (既定5000ms) — **段階的グレースフルシャットダウン**: SIGTERM相当を
/// 送って`timeout`まで自発的終了を待ち、応答が無ければ強制終了
/// (SIGKILL相当)にフォールバックする(`Supervisor::stop_graceful`)。
/// 管理対象でないホストは404(既存`stop_handler`/`restart_handler`と
/// 同じ意味論)。
pub fn stop_graceful_handler(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, params: Params| {
        let registry = Arc::clone(&registry);
        let guardian = Arc::clone(&guardian);
        let host = params.get("host").unwrap_or_default().to_string();
        let timeout = graceful_timeout_from_query(&req, 5000);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            let host_for_task = host.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                registry.stop_process_graceful(&host_for_task, timeout)
            })
            .await
            .unwrap_or(None);

            match outcome {
                Some(graceful) => {
                    json_response(StatusCode::OK, &GracefulStopEntry { host, graceful })
                },
                None => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("no managed process for host: {host}") }),
                ),
            }
        })
    })
}

/// `(method, pattern, handler)`の7件をまとめて返す(`appserver_tenants::
/// routes`と同じ利便性パターン)。
pub fn routes(
    registry: Arc<SupervisedTenantRegistry>,
    guardian: Arc<KeyGuardian>,
) -> Vec<(Method, &'static str, Handler)> {
    vec![
        (
            Method::POST,
            "/admin/appserver-processes",
            register_handler(Arc::clone(&registry), Arc::clone(&guardian)),
        ),
        (
            Method::GET,
            "/admin/appserver-processes",
            list_handler(Arc::clone(&registry), Arc::clone(&guardian)),
        ),
        (
            Method::POST,
            "/admin/appserver-processes/stop-all",
            stop_all_handler(Arc::clone(&registry), Arc::clone(&guardian)),
        ),
        (
            Method::GET,
            "/admin/appserver-processes/:host",
            status_handler(Arc::clone(&registry), Arc::clone(&guardian)),
        ),
        (
            Method::POST,
            "/admin/appserver-processes/:host/stop",
            stop_handler(Arc::clone(&registry), Arc::clone(&guardian)),
        ),
        (
            Method::POST,
            "/admin/appserver-processes/:host/stop-graceful",
            stop_graceful_handler(Arc::clone(&registry), Arc::clone(&guardian)),
        ),
        (
            Method::POST,
            "/admin/appserver-processes/:host/restart",
            restart_handler(registry, guardian),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_appserver::Stack;
    use open_runo_router::hyper_compat::{serve, Router};
    use open_runo_router::keyring::GuardianConfig;
    use open_runo_router::state::AppState;
    use std::net::TcpListener;

    /// `crates/open-runo-appserver/examples/dummy_http_server.rs`の
    /// ビルド済みexe(`[[bin]] open-runo-dummy-http-server`として
    /// `open-runo-appserver/Cargo.toml`に登録済み)を、ワークスペース共通の
    /// `target/{debug,release}/`から実行時に解決する
    /// (`open_runo_appserver::process_lifecycle`の同名テスト専用ヘルパーが
    /// `pub(crate)`のためこのクレートから再利用できず、同じロジックを
    /// ここでも用意する——2クレートにまたがる重複だが、テスト専用の
    /// 数行のパス解決ロジックであり、公開APIとして格上げするほどの
    /// 価値は無いと判断)。
    fn dummy_app_exe_path() -> std::path::PathBuf {
        let mut dir = std::env::current_exe().expect("current_exe");
        while dir
            .file_name()
            .map(|n| n != "debug" && n != "release")
            .unwrap_or(false)
        {
            if !dir.pop() {
                break;
            }
        }
        let name = if cfg!(windows) {
            "open-runo-dummy-http-server.exe"
        } else {
            "open-runo-dummy-http-server"
        };
        dir.push(name);
        assert!(
            dir.exists(),
            "expected dummy http server binary at {dir:?} (run `cargo build -p open-runo-appserver --bin open-runo-dummy-http-server` first)"
        );
        dir
    }

    fn free_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    fn dummy_profile(port: u16) -> RuntimeProfile {
        let mut p = RuntimeProfile::template(Stack::Custom("dummy".into()), "dummy", ".", port);
        p.command = dummy_app_exe_path().to_string_lossy().to_string();
        p.args.clear();
        p.health_path = Some("/health".into());
        p
    }

    async fn test_server() -> (std::net::SocketAddr, Arc<SupervisedTenantRegistry>) {
        let state = Arc::new(AppState::new());
        let guardian = Arc::new(KeyGuardian::new(
            Arc::clone(&state.db),
            GuardianConfig::from_env(),
        ));
        let registry = Arc::new(SupervisedTenantRegistry::new());
        let mut router = Router::new();
        for (method, pattern, handler) in routes(Arc::clone(&registry), guardian) {
            router = router.route(method, pattern, handler);
        }
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        (addr, registry)
    }

    async fn wait_for_status(
        client: &reqwest::Client,
        addr: std::net::SocketAddr,
        host: &str,
        want: &str,
        tries: u32,
    ) -> bool {
        for _ in 0..tries {
            let resp = client
                .get(format!("http://{addr}/admin/appserver-processes/{host}"))
                .header("x-api-key", "test-key")
                .send()
                .await
                .expect("request should succeed");
            let body: serde_json::Value = resp.json().await.expect("valid json body");
            if body["status"] == want {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// 実プロセス・実HTTPでの一気通貫検証: 登録→起動→ヘルスチェックで
    /// Healthy確認→HTTP経由の明示的停止→HTTP経由の明示的再起動→復帰確認。
    /// モック無し(`dummy_http_server`を本当にspawn・本当にHTTPで疎通確認)。
    #[tokio::test]
    async fn register_stop_restart_round_trip_over_real_http() {
        let (addr, _registry) = test_server().await;
        let client = reqwest::Client::new();
        let port = free_port();

        let resp = client
            .post(format!("http://{addr}/admin/appserver-processes"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
                "host": "app1.example.jp",
                "profile": dummy_profile(port),
                "poll_interval_ms": 50,
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        assert!(
            wait_for_status(&client, addr, "app1.example.jp", "healthy", 100).await,
            "process should become healthy via real HTTP health checks"
        );

        let resp = client
            .post(format!(
                "http://{addr}/admin/appserver-processes/app1.example.jp/stop"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(wait_for_status(&client, addr, "app1.example.jp", "stopped", 40).await);

        let resp = client
            .post(format!(
                "http://{addr}/admin/appserver-processes/app1.example.jp/restart"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(wait_for_status(&client, addr, "app1.example.jp", "healthy", 100).await);
    }

    #[tokio::test]
    async fn stop_and_restart_on_unmanaged_host_returns_404() {
        let (addr, _registry) = test_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!(
                "http://{addr}/admin/appserver-processes/unknown.example.jp/stop"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

        let resp = client
            .post(format!(
                "http://{addr}/admin/appserver-processes/unknown.example.jp/restart"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn status_of_unregistered_host_is_null_not_an_error() {
        let (addr, _registry) = test_server().await;
        let resp = reqwest::Client::new()
            .get(format!(
                "http://{addr}/admin/appserver-processes/never-registered.example.jp"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert!(body["status"].is_null());
    }

    /// **複数プロセスの一括管理**の実HTTP検証: 2プロセスを登録し、
    /// `GET /admin/appserver-processes`で両方が一覧に現れること、
    /// `POST /admin/appserver-processes/stop-all`で両方とも実際に
    /// 停止することを確認する(モック無し、実プロセス2つ)。
    #[tokio::test]
    async fn list_and_stop_all_operate_on_every_registered_process_over_real_http() {
        let (addr, _registry) = test_server().await;
        let client = reqwest::Client::new();
        let ports = [free_port(), free_port()];

        for (i, &port) in ports.iter().enumerate() {
            let resp = client
                .post(format!("http://{addr}/admin/appserver-processes"))
                .header("x-api-key", "test-key")
                .json(&serde_json::json!({
                    "host": format!("bulk{i}.example.jp"),
                    "profile": dummy_profile(port),
                    "poll_interval_ms": 50,
                }))
                .send()
                .await
                .expect("request should succeed");
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
        }

        for i in 0..2 {
            assert!(
                wait_for_status(
                    &client,
                    addr,
                    &format!("bulk{i}.example.jp"),
                    "healthy",
                    100
                )
                .await
            );
        }

        let resp = client
            .get(format!("http://{addr}/admin/appserver-processes"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let hosts: Vec<String> = body["processes"]
            .as_array()
            .expect("processes array")
            .iter()
            .map(|e| e["host"].as_str().unwrap().to_string())
            .collect();
        assert!(hosts.contains(&"bulk0.example.jp".to_string()));
        assert!(hosts.contains(&"bulk1.example.jp".to_string()));

        let resp = client
            .post(format!("http://{addr}/admin/appserver-processes/stop-all"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["stopped"].as_array().expect("stopped array").len(), 2);

        for i in 0..2 {
            assert!(
                wait_for_status(&client, addr, &format!("bulk{i}.example.jp"), "stopped", 40).await
            );
        }
    }

    /// **段階的グレースフルシャットダウン**の実HTTP検証: `dummy_http_server`
    /// はSIGTERMハンドラを持たないため既定の振る舞いで終了し、Unixでは
    /// タイムアウトを待たずに`graceful: true`が返ることを確認する
    /// (Windowsでは常にタイムアウト経過後の強制終了になる、という
    /// プラットフォーム差はSupervisor::stop_gracefulのテストで詳細検証済み
    /// のためここでは「実際にプロセスが停止すること」のみ厳密に確認する)。
    #[tokio::test]
    async fn stop_graceful_over_real_http_actually_terminates_the_process() {
        let (addr, _registry) = test_server().await;
        let client = reqwest::Client::new();
        let port = free_port();

        let resp = client
            .post(format!("http://{addr}/admin/appserver-processes"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
                "host": "graceful1.example.jp",
                "profile": dummy_profile(port),
                "poll_interval_ms": 50,
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(wait_for_status(&client, addr, "graceful1.example.jp", "healthy", 100).await);

        let resp = client
            .post(format!("http://{addr}/admin/appserver-processes/graceful1.example.jp/stop-graceful?timeout_ms=3000"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["host"], "graceful1.example.jp");
        assert!(body["graceful"].is_boolean());

        assert!(wait_for_status(&client, addr, "graceful1.example.jp", "stopped", 100).await);
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
            "process must actually be terminated"
        );
    }

    #[tokio::test]
    async fn stop_graceful_on_unmanaged_host_returns_404() {
        let (addr, _registry) = test_server().await;
        let resp = reqwest::Client::new()
            .post(format!(
                "http://{addr}/admin/appserver-processes/unknown.example.jp/stop-graceful"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn register_requires_api_key() {
        let (addr, _registry) = test_server().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/admin/appserver-processes"))
            .json(&serde_json::json!({"host": "x", "profile": dummy_profile(free_port())}))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}
