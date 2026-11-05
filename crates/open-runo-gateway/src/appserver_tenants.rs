//! アプリケーションサーバー層(「第二のTomcat」)のテナント管理API。
//!
//! `open-runo-appserver::SharedDispatcher`(2026-07-16新設、`Arc`越しに
//! 複数スレッドから動的に追加/削除できるテナント辞書)を、この既存の
//! REST gatewayバイナリからHTTP経由で操作できるようにする薄い層。
//!
//! これが無い間は、`poem-cosmo-tauri`をドメインごとのバックエンドとして
//! 使う場合、ドメインごとに新しい`poem-cosmo-tauri`プロセスを個別に
//! インストール・起動する必要があった——本モジュールにより、**1つの
//! 稼働中プロセスが複数ドメインを動的に振り分けられる**ようになる
//! (open-web-server-gatewayの`tenant_router`・Apacheの`a2ensite`相当の
//! 運用を、Tomcat役のこちら側にも実現する、「分身の術」構想の一部)。
//!
//! 認証は既存の`auth_hyper::check_api_key`(`X-Api-Key`)を再利用する
//! ——この gateway の他の管理系エンドポイントと同じ認証方式。

use std::sync::Arc;

use hyper::{Method, StatusCode};
use open_runo_appserver::SharedDispatcher;
use open_runo_router::auth_hyper::check_api_key;
use open_runo_router::hyper_compat::{json_response, read_json_body, Handler, Params};
use open_runo_router::keyring::KeyGuardian;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct UpsertRequest {
    host: String,
    /// 転送先(例: "127.0.0.1:9001")。`open-runo-appserver::tenant_bridge::
    /// parse_backend_addr`と同じ許容形式("http://"/"https://"接頭辞・
    /// 末尾スラッシュも許容)。
    backend_addr: String,
}

#[derive(Serialize)]
struct TenantEntry {
    host: String,
    backend_addr: String,
}

/// `POST /admin/appserver-tenants` — ドメインを1件、無停止で追加/更新する
/// (既存登録があれば上書き、Apacheの`a2ensite`相当)。
pub fn upsert_handler(dispatcher: Arc<SharedDispatcher>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let dispatcher = Arc::clone(&dispatcher);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            let body: UpsertRequest = match read_json_body(req).await {
                Ok(b) => b,
                Err(resp) => return resp,
            };

            let Some(addr) =
                open_runo_appserver::tenant_bridge::parse_backend_addr(&body.backend_addr)
            else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &serde_json::json!({ "error": format!("invalid backend_addr: {}", body.backend_addr) }),
                );
            };

            dispatcher.upsert(&body.host, addr);
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "status": "registered", "host": body.host }),
            )
        })
    })
}

/// `GET /admin/appserver-tenants` — 登録済みドメイン一覧。
pub fn list_handler(dispatcher: Arc<SharedDispatcher>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let dispatcher = Arc::clone(&dispatcher);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            let entries: Vec<TenantEntry> = dispatcher
                .list()
                .into_iter()
                .map(|(host, addr)| TenantEntry {
                    host,
                    backend_addr: format!("{}:{}", addr.host, addr.port),
                })
                .collect();
            json_response(StatusCode::OK, &entries)
        })
    })
}

/// `DELETE /admin/appserver-tenants/:host` — 登録を削除する(未登録でも
/// 冪等に成功、`tenant_router::TenantRegistry::remove`と同じ意味論)。
pub fn remove_handler(dispatcher: Arc<SharedDispatcher>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params: Params| {
        let dispatcher = Arc::clone(&dispatcher);
        let guardian = Arc::clone(&guardian);
        let host = params.get("host").unwrap_or_default().to_string();
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return json_response(status, &serde_json::json!({ "error": "unauthorized" }));
            }

            dispatcher.remove(&host);
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "status": "removed", "host": host }),
            )
        })
    })
}

/// テスト・呼び出し側の利便性のため、`(method, pattern, handler)`の3件を
/// まとめて返す(`main.rs`側での`.route(...)`呼び出し3行をこの1呼び出しに
/// 集約する)。
pub fn routes(
    dispatcher: Arc<SharedDispatcher>,
    guardian: Arc<KeyGuardian>,
) -> Vec<(Method, &'static str, Handler)> {
    vec![
        (
            Method::POST,
            "/admin/appserver-tenants",
            upsert_handler(Arc::clone(&dispatcher), Arc::clone(&guardian)),
        ),
        (
            Method::GET,
            "/admin/appserver-tenants",
            list_handler(Arc::clone(&dispatcher), Arc::clone(&guardian)),
        ),
        (
            Method::DELETE,
            "/admin/appserver-tenants/:host",
            remove_handler(dispatcher, guardian),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_router::hyper_compat::{serve, Router};
    use open_runo_router::keyring::GuardianConfig;
    use open_runo_router::state::AppState;

    /// 実TCPループバック上に本物のサーバーを起動し、`reqwest`で叩く
    /// (この gateway クレートの既存テスト群
    /// [`handlers_hyper.rs`の`federation_status_reflects_composed_schema`
    /// 等]と同じ確立されたパターン——`hyper_compat::Request`は
    /// `hyper::body::Incoming`固定のため、実TCP接続を経由せずに
    /// リクエストを手で組み立てることはできない)。
    async fn test_server() -> (std::net::SocketAddr, Arc<SharedDispatcher>) {
        let state = Arc::new(AppState::new());
        let guardian = Arc::new(KeyGuardian::new(
            Arc::clone(&state.db),
            GuardianConfig::from_env(),
        ));
        let dispatcher = Arc::new(SharedDispatcher::new());
        let mut router = Router::new();
        for (method, pattern, handler) in routes(Arc::clone(&dispatcher), guardian) {
            router = router.route(method, pattern, handler);
        }
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        (addr, dispatcher)
    }

    /// `KeyGuardian`は登録済みキーが0件の間は`RegistryEmpty`として全キーを
    /// 通す(既存のテスト群で確立済みの挙動)ため、ここでは実際の鍵発行
    /// フローを組み立てず、この既定動作に乗って動作確認する。
    #[tokio::test]
    async fn upsert_list_and_remove_round_trip() {
        let (addr, dispatcher) = test_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/admin/appserver-tenants"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"host": "shop.example.jp", "backend_addr": "127.0.0.1:9001"}))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(dispatcher.contains("shop.example.jp"));

        let resp = client
            .get(format!("http://{addr}/admin/appserver-tenants"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body[0]["host"], "shop.example.jp");
        assert_eq!(body[0]["backend_addr"], "127.0.0.1:9001");

        let resp = client
            .delete(format!(
                "http://{addr}/admin/appserver-tenants/shop.example.jp"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(!dispatcher.contains("shop.example.jp"));
    }

    #[tokio::test]
    async fn upsert_rejects_invalid_backend_addr() {
        let (addr, _dispatcher) = test_server().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/admin/appserver-tenants"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"host": "shop.example.jp", "backend_addr": "no-port"}))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_requires_api_key() {
        let (addr, _dispatcher) = test_server().await;
        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/admin/appserver-tenants"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}
