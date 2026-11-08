//! 残っているRESTハンドラ(REST全廃、CLAUDE.md「2026-11-01追記」の第5段階後の姿)。
//!
//! 管理操作の`/api/*`ハンドラは削除し、同じ処理は`admin_ops`/`admin_sys`の共通関数
//! 経由でGraphQL(`open-runo-gateway`の`admin`)が提供する。ここに残すのは、REST以外の
//! 手段が無い**外部プロトコル**だけ:
//! - SCIM 2.0(`/scim/v2/Users`・`/Groups`): RFC 7644でIdPがRESTで叩く標準プロトコル。
//! (旧`POST /internal/db/mutate`は`Admin/DbMutate`のConnect/gRPCへ移行済み。`db_mutate_core`)

use crate::auth_hyper::authenticate_with_session;
use crate::hyper_compat::{
    empty_status, json_response, query_params, read_json_body, Handler, Response,
};
use crate::keyring::KeyGuardian;
use crate::state::AppState;
use hyper::StatusCode;
use std::sync::Arc;

fn admin_err(e: crate::admin_ops::AdminError) -> Response {
    json_response(e.status, &serde_json::json!({ "error": e.message }))
}

/// リクエスト側 `open-web-server-ledger::MutationRequest` と同じJSON形状
/// (フィールド名・型のみ一致、Rust型としては別リポジトリのため共有しない
/// ——`open-web-server`側と同じ「OPEN_RUNO_ENDPOINT + JSON over HTTPのみ、
/// Rustクレート依存は共有しない」既存方針を踏襲)。
#[derive(serde::Deserialize)]
struct MutationRequestWire {
    idempotency_key: String,
    #[allow(dead_code)]
    account_id: String,
    target: String,
    payload: serde_json::Value,
    #[allow(dead_code)]
    requested_at: chrono::DateTime<chrono::Utc>,
}

/// `open-web-server-ledger::MutationReceipt`と同じJSON形状。
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct MutationReceiptWire {
    idempotency_key: String,
    committed: bool,
    db_commit_id: Option<String>,
    committed_at: Option<chrono::DateTime<chrono::Utc>>,
}

const MUTATION_RECEIPTS_TABLE: &str = "__mutation_receipts";

/// `open_runo.admin.v1.Admin/DbMutate`(旧`POST /internal/db/mutate`、REST全廃で移行)。
/// `open-web-server-ledger::Ledger::commit()`が`forward_once()`で呼ぶ先。
/// 認証は`AdminGrpc`側(`x-api-key`/セッション)で済ませてから呼ばれる。
///
/// 冪等性は`__mutation_receipts`テーブルへ`idempotency_key`をキーとして受領票を保存して実現する
/// (`Ledger`側のWAL冪等性チェックとは独立した、受信側のもう一段の防御)。
/// 受領票の保存はベストエフォート。実データの書き込みと受領票の保存は別々の`put`で、
/// 両者の間でプロセスが落ちると同じキーの再送で重複コミットが1件残り得る
/// (`DbBackend`に複数キーのトランザクションが無いため。実害は同値の上書きのみ)。
/// 戻り値のErrは(gRPC風の理由, メッセージ)。
pub async fn db_mutate_core(
    state: &AppState,
    body: serde_json::Value,
) -> Result<serde_json::Value, (&'static str, String)> {
    let body: MutationRequestWire = serde_json::from_value(body)
        .map_err(|e| ("invalid_argument", format!("bad mutation request: {e}")))?;
    match state
        .db
        .get(MUTATION_RECEIPTS_TABLE, &body.idempotency_key)
        .await
    {
        Ok(Some(existing_raw)) => {
            if let Ok(existing) = sonic_rs::from_str::<MutationReceiptWire>(&existing_raw) {
                return Ok(serde_json::to_value(&existing).unwrap_or_default());
            }
        },
        Ok(None) => {},
        Err(e) => return Err(("internal", e.to_string())),
    }
    let serialized = serde_json::to_string(&body.payload)
        .map_err(|e| ("internal", format!("serialize payload: {e}")))?;
    let commit_message = format!("mutation {} on {}", body.idempotency_key, body.target);
    let db_commit_id = state
        .db
        .put_versioned(
            &body.target,
            &body.idempotency_key,
            &serialized,
            &commit_message,
        )
        .await
        .map_err(|e| ("internal", e.to_string()))?;
    let receipt = MutationReceiptWire {
        idempotency_key: body.idempotency_key.clone(),
        committed: true,
        db_commit_id,
        committed_at: Some(chrono::Utc::now()),
    };
    if let Ok(receipt_json) = serde_json::to_string(&receipt) {
        let _ = state
            .db
            .put(MUTATION_RECEIPTS_TABLE, &body.idempotency_key, &receipt_json)
            .await;
    }
    Ok(serde_json::to_value(&receipt).unwrap_or_default())
}

fn scim_user_store(state: &AppState) -> open_runo_scim::ScimUserStore {
    open_runo_scim::ScimUserStore::new(Arc::clone(&state.db))
}

fn scim_group_store(state: &AppState) -> open_runo_scim::ScimGroupStore {
    open_runo_scim::ScimGroupStore::new(Arc::clone(&state.db))
}

/// GET /scim/v2/Users — poem-free port of `handlers::scim::list_users`.
pub fn scim_list_users_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) =
                authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await
            {
                return empty_status(status);
            }
            let query = query_params(&req);
            match crate::admin_sys::scim_list_users(&state, query.get("filter").map(String::as_str))
                .await
            {
                Ok(r) => json_response(StatusCode::OK, &r),
                Err(e) => admin_err(e),
            }
        })
    })
}

/// POST /scim/v2/Users — poem-free port of `handlers::scim::create_user`,
/// including KeyGuardian's auto-issue on provisioning.
pub fn scim_create_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor =
                match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions)
                    .await
                {
                    Ok(actor) => format!(
                        "{}{}",
                        if actor.via_session { "session:" } else { "" },
                        actor.owner
                    ),
                    Err(status) => return empty_status(status),
                };
            let input: open_runo_scim::UserInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            match crate::admin_sys::scim_create_user(&state, &guardian, &actor, input).await {
                Ok(body) => json_response(StatusCode::CREATED, &body),
                Err(e) => admin_err(e),
            }
        })
    })
}

/// GET /scim/v2/Users/:id — poem-free port of `handlers::scim::get_user`.
pub fn scim_get_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) =
                authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await
            {
                return empty_status(status);
            }
            let id = params.get("id").unwrap_or("").to_string();
            match scim_user_store(&state).get(&id).await {
                Ok(Some(user)) => json_response(StatusCode::OK, &user),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("user not found: {id}") }),
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// PUT /scim/v2/Users/:id — poem-free port of `handlers::scim::replace_user`,
/// including KeyGuardian's auto-revoke on deactivation.
pub fn scim_replace_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor =
                match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions)
                    .await
                {
                    Ok(actor) => format!(
                        "{}{}",
                        if actor.via_session { "session:" } else { "" },
                        actor.owner
                    ),
                    Err(status) => return empty_status(status),
                };
            let id = params.get("id").unwrap_or("").to_string();
            let input: open_runo_scim::UserInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let user = match scim_user_store(&state).replace(&id, input).await {
                Ok(u) => u,
                Err(e) => {
                    let msg = e.to_string();
                    let status = if msg.contains("not found") {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::CONFLICT
                    };
                    return json_response(status, &serde_json::json!({ "error": msg }));
                },
            };

            crate::audit::record(&state, &actor, "scim.user.replace", user.user_name.clone()).await;

            if !user.active {
                if let Ok(n) = guardian.revoke_owner(&user.user_name).await {
                    if n > 0 {
                        crate::audit::record(
                            &state,
                            "key-guardian",
                            "key.auto_revoke",
                            format!("{} ({n} keys, deactivated)", user.user_name),
                        )
                        .await;
                    }
                }
            }

            json_response(StatusCode::OK, &user)
        })
    })
}

/// DELETE /scim/v2/Users/:id — poem-free port of `handlers::scim::delete_user`,
/// including KeyGuardian's auto-revoke on deletion.
pub fn scim_delete_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor =
                match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions)
                    .await
                {
                    Ok(actor) => format!(
                        "{}{}",
                        if actor.via_session { "session:" } else { "" },
                        actor.owner
                    ),
                    Err(status) => return empty_status(status),
                };
            let id = params.get("id").unwrap_or("").to_string();

            match crate::admin_sys::scim_delete_user(&state, &guardian, &actor, &id).await {
                Ok(()) => empty_status(StatusCode::NO_CONTENT),
                Err(e) => admin_err(e),
            }
        })
    })
}

/// GET /scim/v2/Groups — poem-free port of `handlers::scim::list_groups`.
pub fn scim_list_groups_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) =
                authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await
            {
                return empty_status(status);
            }
            let query = query_params(&req);
            let filter = query
                .get("filter")
                .and_then(|f| open_runo_scim::ScimGroupStore::parse_display_name_filter(f));

            match scim_group_store(&state).list(filter.as_deref()).await {
                Ok(groups) => json_response(
                    StatusCode::OK,
                    &open_runo_scim::GroupListResponse {
                        schemas: vec!["urn:ietf:params:scim:api:messages:2.0:ListResponse".into()],
                        total_results: groups.len(),
                        resources: groups,
                    },
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// POST /scim/v2/Groups — poem-free port of `handlers::scim::create_group`.
pub fn scim_create_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor =
                match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions)
                    .await
                {
                    Ok(actor) => format!(
                        "{}{}",
                        if actor.via_session { "session:" } else { "" },
                        actor.owner
                    ),
                    Err(status) => return empty_status(status),
                };
            let input: open_runo_scim::GroupInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            match scim_group_store(&state).create(input).await {
                Ok(group) => {
                    crate::audit::record(
                        &state,
                        &actor,
                        "scim.group.create",
                        group.display_name.clone(),
                    )
                    .await;
                    json_response(StatusCode::CREATED, &group)
                },
                Err(e) => json_response(
                    StatusCode::CONFLICT,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// GET /scim/v2/Groups/:id — poem-free port of `handlers::scim::get_group`.
pub fn scim_get_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) =
                authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await
            {
                return empty_status(status);
            }
            let id = params.get("id").unwrap_or("").to_string();
            match scim_group_store(&state).get(&id).await {
                Ok(Some(group)) => json_response(StatusCode::OK, &group),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("group not found: {id}") }),
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// PUT /scim/v2/Groups/:id — poem-free port of `handlers::scim::replace_group`.
pub fn scim_replace_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor =
                match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions)
                    .await
                {
                    Ok(actor) => format!(
                        "{}{}",
                        if actor.via_session { "session:" } else { "" },
                        actor.owner
                    ),
                    Err(status) => return empty_status(status),
                };
            let id = params.get("id").unwrap_or("").to_string();
            let input: open_runo_scim::GroupInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            match scim_group_store(&state).replace(&id, input).await {
                Ok(group) => {
                    crate::audit::record(
                        &state,
                        &actor,
                        "scim.group.replace",
                        group.display_name.clone(),
                    )
                    .await;
                    json_response(StatusCode::OK, &group)
                },
                Err(e) => {
                    let msg = e.to_string();
                    let status = if msg.contains("not found") {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::CONFLICT
                    };
                    json_response(status, &serde_json::json!({ "error": msg }))
                },
            }
        })
    })
}

/// DELETE /scim/v2/Groups/:id — poem-free port of `handlers::scim::delete_group`.
pub fn scim_delete_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor =
                match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions)
                    .await
                {
                    Ok(actor) => format!(
                        "{}{}",
                        if actor.via_session { "session:" } else { "" },
                        actor.owner
                    ),
                    Err(status) => return empty_status(status),
                };
            let id = params.get("id").unwrap_or("").to_string();

            if let Err(e) = scim_group_store(&state).delete(&id).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "scim.group.delete", id).await;
            empty_status(StatusCode::NO_CONTENT)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{serve, Router};
    use crate::keyring::GuardianConfig;
    use hyper::Method;

    fn guardian(state: &Arc<AppState>) -> Arc<KeyGuardian> {
        Arc::new(KeyGuardian::new(
            Arc::clone(&state.db),
            GuardianConfig::from_env(),
        ))
    }

    fn scim_router(state: &Arc<AppState>, guardian: Arc<KeyGuardian>) -> Router {
        Router::new()
            .route(
                Method::GET,
                "/scim/v2/Users",
                scim_list_users_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::POST,
                "/scim/v2/Users",
                scim_create_user_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/scim/v2/Users/:id",
                scim_get_user_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::PUT,
                "/scim/v2/Users/:id",
                scim_replace_user_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::DELETE,
                "/scim/v2/Users/:id",
                scim_delete_user_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/scim/v2/Groups",
                scim_list_groups_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::POST,
                "/scim/v2/Groups",
                scim_create_group_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/scim/v2/Groups/:id",
                scim_get_group_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::PUT,
                "/scim/v2/Groups/:id",
                scim_replace_group_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::DELETE,
                "/scim/v2/Groups/:id",
                scim_delete_group_handler(Arc::clone(state), guardian),
            )
    }

    #[tokio::test]
    async fn scim_user_lifecycle_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = scim_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();
        let key = "x-api-key";

        let resp = client
            .post(format!("http://{addr}/scim/v2/Users"))
            .header(key, "test-key")
            .json(&serde_json::json!({
                "userName": "alice@example.com",
                "displayName": "Alice",
                "emails": [{ "value": "alice@example.com", "primary": true }],
                "roles": ["developer"]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["userName"], "alice@example.com");
        assert_eq!(body["meta"]["resourceType"], "User");

        let issued = body["urn:open-runo:params:scim:api-key"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = client
            .post(format!("http://{addr}/scim/v2/Users"))
            .header(key, &issued)
            .json(&serde_json::json!({ "userName": "alice@example.com" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users"))
            .query(&[("filter", r#"userName eq "alice@example.com""#)])
            .header(key, &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["totalResults"], 1);

        let resp = client
            .put(format!("http://{addr}/scim/v2/Users/{id}"))
            .header(key, &issued)
            .json(&serde_json::json!({ "userName": "alice@example.com", "active": false }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["active"], false);

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users/{id}"))
            .header(key, &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn scim_group_lifecycle_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = scim_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();
        let key = "x-api-key";

        let resp = client
            .post(format!("http://{addr}/scim/v2/Groups"))
            .header(key, "test-key")
            .json(&serde_json::json!({
                "displayName": "engineering",
                "members": [{ "value": "user-1", "display": "Alice" }]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["meta"]["resourceType"], "Group");

        let resp = client
            .put(format!("http://{addr}/scim/v2/Groups/{id}"))
            .header(key, "test-key")
            .json(&serde_json::json!({
                "displayName": "engineering",
                "members": [{ "value": "user-1" }, { "value": "user-2" }]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["members"].as_array().unwrap().len(), 2);

        let resp = client
            .delete(format!("http://{addr}/scim/v2/Groups/{id}"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    }

    /// SCIM is the third real (non-toy) route group rolled onto
    /// `authenticate_with_session` -- after DB CRUD and Feature Flags --
    /// completing the broader rollout past the original
    /// `register_schema_handler`-only example noted in
    /// docs/poem-parity.md. Create (state-changing, needs CSRF) then list
    /// (read-only, cookie alone suffices), no X-Api-Key anywhere.
    #[tokio::test]
    async fn scim_create_and_list_accept_session_cookie_with_csrf_token() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let real_key = guardian
            .issue("frank", vec!["developer".to_string()], None)
            .await
            .expect("issuing a real key should succeed");

        let router = scim_router(&state, Arc::clone(&guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        // ログインはGraphQLの`sessionLogin`(admin_sys::session_login)に移ったため、
        // ここでは同じ共通関数でセッションを作り、そのCookie+CSRFで使えることを確認する。
        let (login, set_cookie) =
            crate::admin_sys::session_login(&guardian, &state.sessions, &real_key)
                .await
                .expect("login should succeed");
        let cookie_pair = set_cookie.split(';').next().unwrap().to_string();
        let csrf_token = login.csrf_token;

        let resp = client
            .post(format!("http://{addr}/scim/v2/Users"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .header("x-csrf-token", &csrf_token)
            .json(&serde_json::json!({ "userName": "via-session@example.com", "roles": ["developer"] }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users"))
            .query(&[("filter", r#"userName eq "via-session@example.com""#)])
            .header(reqwest::header::COOKIE, &cookie_pair)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["totalResults"], 1);
    }

    #[tokio::test]
    async fn key_guardian_full_auto_lifecycle() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = scim_router(&state, Arc::clone(&guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        client
            .get(format!("http://{addr}/scim/v2/Users"))
            .header("x-api-key", "anything-goes")
            .send()
            .await
            .expect("request should succeed");

        let resp = client
            .post(format!("http://{addr}/scim/v2/Users"))
            .header("x-api-key", "bootstrap")
            .json(&serde_json::json!({ "userName": "eve@example.com", "roles": ["developer"] }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let issued = body["urn:open-runo:params:scim:api-key"]
            .as_str()
            .unwrap()
            .to_string();
        let user_id = body["id"].as_str().unwrap().to_string();
        assert!(issued.starts_with("orn_"));

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users"))
            .header("x-api-key", "anything-goes")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users"))
            .header("x-api-key", &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .delete(format!("http://{addr}/scim/v2/Users/{user_id}"))
            .header("x-api-key", &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users"))
            .header("x-api-key", &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    fn mreq(key: &str, item: &str) -> serde_json::Value {
        serde_json::json!({
            "idempotency_key": key,
            "account_id": "user-1",
            "target": "items",
            "payload": {"item_id": item, "quantity": 1},
            "requested_at": chrono::Utc::now().to_rfc3339(),
        })
    }

    /// `Admin/DbMutate`の本体: 200相当で受領票が返り、実データが書き込まれる。
    #[tokio::test]
    async fn db_mutate_writes_the_payload_and_returns_a_receipt() {
        let state = AppState::new();
        let receipt = db_mutate_core(&state, mreq("mutate-key-1", "sword"))
            .await
            .expect("mutate should succeed");
        assert_eq!(receipt["committed"], serde_json::json!(true));
        assert_eq!(receipt["idempotency_key"], serde_json::json!("mutate-key-1"));
        // InMemoryBackendはコミット概念を持たないため正直にNoneが返る。
        assert_eq!(receipt["db_commit_id"], serde_json::json!(null));
        let stored: serde_json::Value = serde_json::from_str(
            &state
                .db
                .get("items", "mutate-key-1")
                .await
                .expect("db read should succeed")
                .expect("the payload should have been written"),
        )
        .expect("stored value should be JSON");
        assert_eq!(stored["item_id"], serde_json::json!("sword"));
    }

    /// 同じ`idempotency_key`での再送は最初の受領票をそのまま返す。
    #[tokio::test]
    async fn db_mutate_is_idempotent_across_retries() {
        let state = AppState::new();
        let first = db_mutate_core(&state, mreq("mutate-key-retry", "shield"))
            .await
            .unwrap();
        let second = db_mutate_core(&state, mreq("mutate-key-retry", "shield"))
            .await
            .unwrap();
        assert_eq!(
            first["committed_at"], second["committed_at"],
            "retry must return the original receipt, not a fresh one"
        );
    }

    #[tokio::test]
    async fn db_mutate_rejects_a_malformed_request() {
        let state = AppState::new();
        let e = db_mutate_core(&state, serde_json::json!({"target": "x"}))
            .await
            .unwrap_err();
        assert_eq!(e.0, "invalid_argument");
    }
}
