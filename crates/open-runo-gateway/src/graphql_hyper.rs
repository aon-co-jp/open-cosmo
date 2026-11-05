//! Poem-free `/graphql` endpoint, built on `open_runo_router::hyper_compat`
//! instead of `async_graphql_poem`. `async-graphql` itself has no Poem
//! dependency — only its poem integration crate did — so this is a
//! straight port of `graphql_handler`/`graphiql` from `lib.rs` onto plain
//! hyper request/response types.
//!
//! **Scope note**: GraphQL Subscriptions over WebSocket (`/graphql/ws`,
//! the `graphql-ws` protocol) are not ported here yet — hyper's raw
//! `Upgrade` handling for WebSocket is a separate, larger piece of work.
//! `graphql_route` (poem-based, still available in `lib.rs`) remains the
//! only way to get subscriptions until that lands.

use crate::remote_subgraphs::{RemoteSubgraphs, Routed};
use crate::security::{count_root_fields, GraphqlSecurity};
use crate::{
    cache_key, is_cacheable_query, persisted_query_hash, CacheConfig, GatewayCache, OpenRunoSchema,
};
use http_body_util::BodyExt;
use open_runo_cache::InMemoryTtlCache;
use open_runo_persisted_queries::{EnforcementMode, PersistedQueryStore};
use open_runo_router::auth_hyper::authenticate_with_session;
use open_runo_router::hyper_compat::{
    html_response, json_response, text_response, websocket_upgrade, Handler, WsMessage,
};
use open_runo_router::keyring::{GuardianConfig, KeyGuardian};
use open_runo_router::state::AppState;
use open_runo_security::RateLimit;
use std::sync::Arc;

/// `GET /graphql` — the GraphiQL playground (static HTML, no auth).
pub fn graphiql_handler() -> Handler {
    Arc::new(move |_req, _params| {
        Box::pin(async move {
            let html = async_graphql::http::GraphiQLSource::build()
                .endpoint("/graphql")
                .finish();
            html_response(hyper::StatusCode::OK, html)
        })
    })
}

/// 要求本文の上限(これを超える問い合わせは読まずに拒否する)。
const MAX_BODY_BYTES: usize = 1024 * 1024;

fn gql_error(
    status: hyper::StatusCode,
    message: impl Into<String>,
) -> hyper::Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>> {
    let resp = async_graphql::Response::from_errors(vec![async_graphql::ServerError::new(
        message.into(),
        None,
    )]);
    json_response(status, &resp)
}

/// `POST /graphql` — poem-free port of `graphql_handler`. Same persisted-
/// query resolution and response-cache behavior as the poem version, plus
/// the production defences in [`GraphqlSecurity`]: JSON-array batches are
/// refused, root fields (aliases included) are capped, and the extra root
/// fields are charged against the rate limiter so aliasing can't be used
/// to pack many operations into one rate-limit unit.
pub fn graphql_post_handler(
    schema: OpenRunoSchema,
    store: Arc<PersistedQueryStore>,
    cache: Arc<GatewayCache>,
    remotes: RemoteSubgraphs,
    sec: Arc<GraphqlSecurity>,
    limiter: Option<Arc<dyn RateLimit>>,
) -> Handler {
    Arc::new(move |req, _params| {
        let remotes = remotes.clone();
        let schema = schema.clone();
        let store = Arc::clone(&store);
        let cache = Arc::clone(&cache);
        let sec = Arc::clone(&sec);
        let limiter = limiter.clone();
        Box::pin(async move {
            // レート制限のキー(`with_shared_rate_limit`と同じ求め方)。
            let client_key = req
                .headers()
                .get("x-forwarded-for")
                .or_else(|| req.headers().get("x-real-ip"))
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .unwrap_or_else(|| "anonymous".to_string());
            let body = match http_body_util::Limited::new(req.into_body(), MAX_BODY_BYTES)
                .collect()
                .await
            {
                Ok(b) => b.to_bytes(),
                Err(_) => {
                    return gql_error(
                        hyper::StatusCode::PAYLOAD_TOO_LARGE,
                        "request body too large",
                    )
                },
            };
            // バッチ(JSON配列)は受け付けない: 1通信に複数操作を詰め込む攻撃を許さない。
            if body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
                return gql_error(
                    hyper::StatusCode::BAD_REQUEST,
                    "batched requests are not supported; send one operation per request",
                );
            }
            let mut request: async_graphql::Request = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    return gql_error(
                        hyper::StatusCode::BAD_REQUEST,
                        format!("invalid request: {e}"),
                    )
                },
            };

            let hash = persisted_query_hash(&request);
            let raw = if request.query.trim().is_empty() {
                None
            } else {
                Some(request.query.clone())
            };

            if hash.is_some() || store.mode() == EnforcementMode::Enforce {
                match store.resolve(hash.as_deref(), raw.as_deref()).await {
                    Ok(document) => request.query = document,
                    Err(e) => {
                        let resp = async_graphql::Response::from_errors(vec![
                            async_graphql::ServerError::new(e.to_string(), None),
                        ]);
                        return json_response(hyper::StatusCode::OK, &resp);
                    },
                }
            }

            // ルートフィールド(エイリアス含む)の上限。永続クエリを解決した後の本文で数える。
            if !request.query.trim().is_empty() {
                match count_root_fields(&request.query) {
                    Ok(n) if n > sec.max_root_fields => {
                        return gql_error(
                            hyper::StatusCode::OK,
                            format!(
                                "too many root fields/aliases: {n} (max {})",
                                sec.max_root_fields
                            ),
                        );
                    },
                    Ok(n) => {
                        // 1通信=1回分では足りない: 超過分(n-1)を流量制限の枠から余計に消費する。
                        if let Some(limiter) = &limiter {
                            let now = chrono::Utc::now();
                            for _ in 1..n {
                                if limiter.check(&client_key, now).await.is_err() {
                                    let retry = limiter.seconds_until_reset(&client_key, now).await;
                                    let mut resp = json_response(
                                        hyper::StatusCode::TOO_MANY_REQUESTS,
                                        &serde_json::json!({
                                            "error": "rate limit exceeded (root fields count against the limit), see retry_after_secs",
                                            "retry_after_secs": retry,
                                        }),
                                    );
                                    if let Ok(v) = retry.to_string().parse() {
                                        resp.headers_mut().insert("retry-after", v);
                                    }
                                    return resp;
                                }
                            }
                        }
                    },
                    // 構文エラーは実行段階で通常のGraphQLエラーとして返す。
                    Err(_) => {},
                }
            }

            // Remote subgraph forwarding (OPEN_RUNO_SUBGRAPHS): see `remote_subgraphs`.
            match remotes
                .route(
                    &request.query,
                    request.operation_name.as_deref(),
                    serde_json::to_value(&request.variables).unwrap_or(serde_json::Value::Null),
                )
                .await
            {
                Routed::Local => {},
                Routed::Forwarded(v) => return json_response(hyper::StatusCode::OK, &v),
                Routed::Rejected(msg) => {
                    let resp = async_graphql::Response::from_errors(vec![
                        async_graphql::ServerError::new(msg, None),
                    ]);
                    return json_response(hyper::StatusCode::OK, &resp);
                },
            }

            let use_cache = cache.config.enabled && is_cacheable_query(&request.query);
            let key = if use_cache {
                Some(cache_key(&request))
            } else {
                None
            };

            if let Some(key) = &key {
                if let Ok(Some(cached)) = cache.store.get(key).await {
                    if let Ok(response) = sonic_rs::from_str::<async_graphql::Response>(&cached) {
                        return json_response(hyper::StatusCode::OK, &response);
                    }
                }
            }

            let response = schema.execute(request).await;

            if let Some(key) = &key {
                if response.is_ok() {
                    if let Ok(serialized) = serde_json::to_string(&response) {
                        let _ = cache.store.set(key, &serialized, cache.config.ttl).await;
                    }
                }
            }

            json_response(hyper::StatusCode::OK, &response)
        })
    })
}

/// `GET /graphql/schema.graphql` — GraphQLスキーマをSDLで配信する(旧`/api/openapi*`の
/// 置き換え。認証不要、内容は型定義だけでデータは含まない)。
pub fn schema_sdl_handler(schema: OpenRunoSchema) -> Handler {
    Arc::new(move |_req, _params| {
        let sdl = schema.sdl();
        Box::pin(
            async move { text_response(hyper::StatusCode::OK, "text/plain; charset=utf-8", sdl) },
        )
    })
}

/// `GET /graphql/ws` — GraphQL Subscriptions(graphql-transport-ws / graphql-ws)。
/// 認証はアップグレード要求のヘッダ(`X-Api-Key`/セッションCookie)か、ブラウザ向けに
/// `connection_init`のペイロード(`{"x-api-key": "..."}`)で行う。
pub fn graphql_ws_handler(
    schema: OpenRunoSchema,
    state: Arc<AppState>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let schema = schema.clone();
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            use futures::StreamExt;

            let protocol = req
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|v| v.to_str().ok())
                .map(|v| {
                    if v.split(',').any(|p| p.trim() == "graphql-ws") {
                        async_graphql::http::WebSocketProtocols::SubscriptionsTransportWS
                    } else {
                        async_graphql::http::WebSocketProtocols::GraphQLWS
                    }
                })
                .unwrap_or(async_graphql::http::WebSocketProtocols::GraphQLWS);
            // アップグレード要求は`GET`なのでCSRFトークンは要らない(Cookie認証でも通る)。
            let header_actor =
                authenticate_with_session(req.headers(), req.method(), &guardian, &state.sessions)
                    .await
                    .ok();

            websocket_upgrade(req, move |mut conn| {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    let input = futures::stream::unfold(rx, |mut rx| async move {
                        rx.recv().await.map(|m| (m, rx))
                    });
                    let init_guardian = Arc::clone(&guardian);
                    let ws = async_graphql::http::WebSocket::new(schema, input, protocol)
                        .on_connection_init(move |payload: serde_json::Value| async move {
                            let mut data = async_graphql::Data::default();
                            let key = payload
                                .get("x-api-key")
                                .or_else(|| payload.get("apiKey"))
                                .and_then(serde_json::Value::as_str)
                                .map(str::trim)
                                .filter(|k| !k.is_empty());
                            let actor = match key {
                                Some(k) => {
                                    match init_guardian.verify(k, chrono::Utc::now()).await {
                                        open_runo_router::keyring::KeyDecision::RegistryEmpty => {
                                            Some(open_runo_router::auth_hyper::Actor {
                                                owner: "dev".to_string(),
                                                roles: vec![],
                                                via_session: false,
                                            })
                                        },
                                        open_runo_router::keyring::KeyDecision::Ok {
                                            owner,
                                            roles,
                                        } => Some(open_runo_router::auth_hyper::Actor {
                                            owner,
                                            roles,
                                            via_session: false,
                                        }),
                                        _ => None,
                                    }
                                },
                                None => header_actor,
                            };
                            match actor {
                                Some(a) => {
                                    data.insert(a);
                                    Ok(data)
                                },
                                None => Err(async_graphql::Error::new(
                                    "authentication required (X-Api-Key or session cookie)",
                                )),
                            }
                        });
                    let mut ws = Box::pin(ws);
                    loop {
                        tokio::select! {
                            incoming = conn.recv() => match incoming {
                                Some(WsMessage::Text(t)) => {
                                    if tx.send(t).is_err() { break; }
                                },
                                Some(WsMessage::Binary(_)) => {},
                                None => break,
                            },
                            outgoing = ws.next() => match outgoing {
                                Some(async_graphql::http::WsMessage::Text(t)) => {
                                    if conn.send_text(t).await.is_err() { break; }
                                },
                                Some(async_graphql::http::WsMessage::Close(..)) | None => {
                                    let _ = conn.close().await;
                                    break;
                                },
                            },
                        }
                    }
                })
            })
        })
    })
}

/// `GET /graphql/schema.graphql`(SDL配信)と`GET /graphql/ws`(Subscriptions)のハンドラ。
/// スキーマ・鍵管理は`graphql_handlers`と同じ`state`を共有する。
pub fn graphql_extra_handlers(state: Arc<AppState>) -> (Handler, Handler) {
    graphql_extra_handlers_with(state, &GraphqlSecurity::from_env())
}

/// [`graphql_extra_handlers`]に防御設定を明示して渡す版(購読にも深さ・複雑さの上限が効く)。
pub fn graphql_extra_handlers_with(
    state: Arc<AppState>,
    sec: &GraphqlSecurity,
) -> (Handler, Handler) {
    let schema = crate::build_schema_with(Arc::clone(&state), sec);
    let guardian = Arc::new(KeyGuardian::new(
        Arc::clone(&state.db),
        GuardianConfig::from_env(),
    ));
    (
        schema_sdl_handler(schema.clone()),
        graphql_ws_handler(schema, state, guardian),
    )
}

/// GraphiQL(`GET /graphql`)とSDL(`GET /graphql/schema.graphql`)を、設定が許すときだけ登録する。
/// 本番(`expose_schema = false`)ではスキーマを自分から公開しない(ルート自体を作らない=404)。
pub fn mount_schema_endpoints(
    router: open_runo_router::hyper_compat::Router,
    sec: &GraphqlSecurity,
    graphiql: Handler,
    sdl: Handler,
) -> open_runo_router::hyper_compat::Router {
    if !sec.expose_schema {
        return router;
    }
    router
        .route(hyper::Method::GET, "/graphql", graphiql)
        .route(hyper::Method::GET, "/graphql/schema.graphql", sdl)
}

/// Build the poem-free `/graphql` GET+POST handlers, pre-wired with
/// `state`. Register both on a `hyper_compat::Router` at the same path
/// (the router dispatches by method, so `GET /graphql` → GraphiQL and
/// `POST /graphql` → query execution can share one path string).
pub fn graphql_handlers(state: Arc<AppState>) -> (Handler, Handler) {
    graphql_handlers_with(state, GraphqlSecurity::from_env(), None)
}

/// [`graphql_handlers`]に防御設定と流量制限(エイリアス超過分の課金用)を明示して渡す版。
pub fn graphql_handlers_with(
    state: Arc<AppState>,
    sec: GraphqlSecurity,
    limiter: Option<Arc<dyn RateLimit>>,
) -> (Handler, Handler) {
    let schema = crate::build_schema_with(Arc::clone(&state), &sec);
    let store = Arc::new(PersistedQueryStore::new(Arc::clone(&state.db), sec.pq_mode));
    let cache = Arc::new(GatewayCache {
        store: Arc::new(InMemoryTtlCache::new()),
        config: CacheConfig::from_env(),
    });
    (
        graphiql_handler(),
        graphql_post_handler(
            schema,
            store,
            cache,
            RemoteSubgraphs::from_env(),
            Arc::new(sec),
            limiter,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_router::hyper_compat::{serve, Router};

    #[tokio::test]
    async fn graphiql_serves_html() {
        let state = Arc::new(AppState::new());
        let (get_h, _post_h) = graphql_handlers(state);
        let router = Router::new().route(hyper::Method::GET, "/graphql", get_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/graphql"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        let body = resp.text().await.unwrap();
        assert!(body.contains("graphiql") || body.contains("GraphiQL"));
    }

    #[tokio::test]
    async fn graphql_post_executes_health_query() {
        let state = Arc::new(AppState::new());
        let (_get_h, post_h) = graphql_handlers(state);
        let router = Router::new().route(hyper::Method::POST, "/graphql", post_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/graphql"))
            .json(&serde_json::json!({ "query": "{ health }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["data"]["health"], "ok");
    }

    #[tokio::test]
    async fn graphql_post_reports_query_errors() {
        let state = Arc::new(AppState::new());
        let (_get_h, post_h) = graphql_handlers(state);
        let router = Router::new().route(hyper::Method::POST, "/graphql", post_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/graphql"))
            .json(&serde_json::json!({ "query": "{ notAField }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert!(body["errors"].as_array().is_some_and(|e| !e.is_empty()));
    }

    // ── 本番向け防御(security.rs)の通し試験 ──

    use crate::security::GraphqlSecurity;
    use open_runo_core::Environment;
    use open_runo_security::RateLimiter;

    async fn serve_with(sec: GraphqlSecurity, limiter: Option<Arc<dyn RateLimit>>) -> String {
        let state = Arc::new(AppState::new());
        let (_g, post) = graphql_handlers_with(state, sec, limiter);
        let router = Router::new().route(hyper::Method::POST, "/graphql", post);
        let (addr, h) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        std::mem::forget(h);
        format!("http://{addr}/graphql")
    }

    async fn post(url: &str, body: &str) -> (reqwest::StatusCode, serde_json::Value) {
        let r = reqwest::Client::new()
            .post(url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(serde_json::Value::Null))
    }

    fn q(query: &str) -> String {
        serde_json::json!({ "query": query }).to_string()
    }

    fn msg(v: &serde_json::Value) -> String {
        v["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn loose() -> GraphqlSecurity {
        GraphqlSecurity::for_environment(Environment::Local)
    }

    #[tokio::test]
    async fn json_array_batches_are_refused() {
        let url = serve_with(loose(), None).await;
        let (st, v) = post(&url, r#"[{"query":"{ health }"},{"query":"{ health }"}]"#).await;
        assert_eq!(st, 400);
        assert!(
            msg(&v).contains("batched requests are not supported"),
            "{v}"
        );
        // 通常の1件は通る。
        let (st, v) = post(&url, &q("{ health }")).await;
        assert_eq!(
            (st.as_u16(), v["data"]["health"].as_str()),
            (200, Some("ok"))
        );
    }

    #[tokio::test]
    async fn root_fields_including_aliases_are_capped() {
        let mut sec = loose();
        sec.max_root_fields = 3;
        let url = serve_with(sec, None).await;
        let (_, ok) = post(&url, &q("{ a: health b: health c: health }")).await;
        assert!(ok["errors"].is_null(), "{ok}");
        let (_, v) = post(&url, &q("{ a: health b: health c: health d: health }")).await;
        assert!(msg(&v).contains("too many root fields"), "{v}");
        // フラグメント経由でも数える。
        let (_, v) = post(
            &url,
            &q("query { ...F } fragment F on QueryRoot { a: health b: health c: health d: health }"),
        )
        .await;
        assert!(msg(&v).contains("too many root fields"), "{v}");
    }

    #[tokio::test]
    async fn alias_amplification_is_charged_against_the_rate_limit() {
        // 流量制限の枠は5。エイリアス4個のクエリは超過分3回ぶんを余計に消費する。
        let limiter: Arc<dyn RateLimit> =
            Arc::new(RateLimiter::new(5, chrono::Duration::seconds(60)));
        let url = serve_with(loose(), Some(limiter)).await;
        let aliased = q("{ a: health b: health c: health d: health }");
        let (st, _) = post(&url, &aliased).await;
        assert_eq!(st, 200);
        // 2回目: 残り2回ぶんしか無いのに3回ぶん要る → 429。
        let (st, v) = post(&url, &aliased).await;
        assert_eq!(st, 429);
        assert!(v["error"].as_str().unwrap().contains("root fields"));
    }

    #[tokio::test]
    async fn depth_and_complexity_limits_reject_before_execution() {
        let mut sec = loose();
        sec.max_depth = 1;
        let url = serve_with(sec, None).await;
        let (_, v) = post(&url, &q("{ federationStatus { typeNames } }")).await;
        assert!(msg(&v).to_lowercase().contains("nested too deep"), "{v}");
        let (_, v) = post(&url, &q("{ health }")).await;
        assert!(v["errors"].is_null(), "{v}");

        let mut sec = loose();
        sec.max_complexity = 2;
        let url = serve_with(sec, None).await;
        let (_, v) = post(&url, &q("{ a: health b: health c: health }")).await;
        assert!(msg(&v).to_lowercase().contains("too complex"), "{v}");
    }

    #[tokio::test]
    async fn introspection_is_off_when_disabled() {
        let mut sec = loose();
        sec.introspection = false;
        let url = serve_with(sec, None).await;
        let (_, v) = post(&url, &q("{ __schema { types { name } } }")).await;
        // 無効化時は`__schema`がnullで返り、型情報は一切出ない(エラー文での名前漏れも無い)。
        assert!(
            v["data"].is_null() || v["data"]["__schema"].is_null(),
            "{v}"
        );
        assert!(!v.to_string().contains("QueryRoot"), "{v}");
        // 開発設定ならイントロスペクションは使える。
        let url = serve_with(loose(), None).await;
        let (_, v) = post(&url, &q("{ __schema { queryType { name } } }")).await;
        assert!(v["errors"].is_null(), "{v}");
    }

    #[tokio::test]
    async fn production_only_accepts_registered_queries() {
        let state = Arc::new(AppState::new());
        let sec = GraphqlSecurity::for_environment(Environment::Production);
        assert_eq!(sec.pq_mode, EnforcementMode::Enforce);
        let (_g, post_h) = graphql_handlers_with(Arc::clone(&state), sec, None);
        let router = Router::new().route(hyper::Method::POST, "/graphql", post_h);
        let (addr, _h) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let url = format!("http://{addr}/graphql");
        // 任意のクエリ文字列は拒否。
        let (_, v) = post(&url, &q("{ health }")).await;
        assert!(msg(&v).contains("trusted-documents"), "{v}");
        // 登録済みのクエリ(ハッシュ指定)は通る。
        let store = PersistedQueryStore::new(Arc::clone(&state.db), EnforcementMode::Allow);
        let rec = store.register("{ health }").await.unwrap();
        let body = serde_json::json!({
            "extensions": { "persistedQuery": { "version": 1, "sha256Hash": rec.hash } }
        })
        .to_string();
        let (_, v) = post(&url, &body).await;
        assert_eq!(v["data"]["health"], "ok", "{v}");
    }

    #[tokio::test]
    async fn schema_endpoints_are_not_mounted_in_production() {
        let state = Arc::new(AppState::new());
        for (env, expect) in [(Environment::Production, 404), (Environment::Local, 200)] {
            let sec = GraphqlSecurity::for_environment(env);
            let (graphiql, _post) = graphql_handlers_with(Arc::clone(&state), sec.clone(), None);
            let (sdl, _ws) = graphql_extra_handlers_with(Arc::clone(&state), &sec);
            let router = mount_schema_endpoints(Router::new(), &sec, graphiql, sdl);
            let (addr, _h) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            for path in ["/graphql", "/graphql/schema.graphql"] {
                let r = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
                assert_eq!(r.status().as_u16(), expect, "{env:?} {path}");
            }
        }
    }
}
