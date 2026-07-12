//! Poem-free handler implementations, migrated one at a time from
//! `handlers/*.rs` (which stay on `poem` until every handler here has an
//! equivalent and `lib.rs::build_app` switches over). Each function here
//! returns a `hyper_compat::Handler` closing over whatever state it needs,
//! matching the JSON shape/status codes of its poem counterpart exactly.

use crate::auth_hyper::authenticate_with_session;
use crate::hyper_compat::{
    empty_status, json_response, query_params, read_json_body, sse_response, Handler, Response, SseEvent,
};
use crate::keyring::{KeyDecision, KeyGuardian};
use crate::session::SessionStore;
use crate::state::AppState;
use crate::validation::{DB_UPSERT_REQUEST, FEATURE_FLAG_REQUEST, REGISTER_SCHEMA_REQUEST};
use http_body_util::BodyExt;
use hyper::StatusCode;
use open_runo_api_types::{
    DbDeleteResponse, DbRecordItem, DbRecordListResponse, DbRecordResponse, DbRoutingEntry, DbRoutingInfo,
    DbStatusResponse, DbUpsertRequest, FederationStatusResponse, FeatureFlagEvaluationResponse,
    FeatureFlagListResponse, FeatureFlagRequest, FeatureFlagResponse, RegisterSchemaRequest, SchemaHistoryResponse,
    SchemaVersion,
};
use open_runo_feature_flags::FeatureFlag;
use open_runo_schema_registry::{Stage, DEFAULT_NAMESPACE};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn parse_stage(s: &str) -> Stage {
    match s.to_lowercase().as_str() {
        "development" | "dev" => Stage::Development,
        "staging" | "stg" => Stage::Staging,
        "production" | "prod" => Stage::Production,
        _ => Stage::Local,
    }
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Local => "local",
        Stage::Development => "development",
        Stage::Staging => "staging",
        Stage::Production => "production",
    }
}

/// GET /api/federation/status — poem-free port of
/// `handlers::federation::federation_status`, now with the same
/// `X-Api-Key` gate as the poem route (see `auth_hyper::check_api_key`).
pub fn federation_status_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let schema = state
                .federation_schema
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let field_count: usize = schema.types.values().map(|f| f.len()).sum();
            json_response(
                StatusCode::OK,
                &FederationStatusResponse {
                    contributing_services: schema.contributing_services,
                    type_count: schema.types.len(),
                    field_count,
                },
            )
        })
    })
}

#[derive(Debug, Deserialize)]
struct ServiceInput {
    service_name: String,
    /// Pre-structured type -> field-name map. Optional when `sdl` is
    /// given; otherwise required.
    #[serde(default)]
    types: std::collections::BTreeMap<String, Vec<String>>,
    /// Real GraphQL subgraph SDL text (Federation v1 *or* v2 style — both
    /// are accepted transparently, see `open_runo_federation::sdl`). When
    /// present, this is parsed to derive the type/field map instead of
    /// requiring the caller to pre-extract it into `types`.
    #[serde(default)]
    sdl: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComposeRequest {
    services: Vec<ServiceInput>,
}

#[derive(Serialize)]
struct ComposeResponse {
    contributing_services: Vec<String>,
    types: std::collections::BTreeMap<String, Vec<String>>,
    breaking_changes: Vec<String>,
}

/// POST /api/federation/compose — poem-free port of
/// `handlers::federation::compose_schemas`.
pub fn compose_schemas_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let body: ComposeRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let mut service_schemas: Vec<open_runo_federation::ServiceSchema> = Vec::new();
            for s in body.services {
                let schema = if let Some(sdl) = s.sdl {
                    match open_runo_federation::parse_service_sdl(&s.service_name, &sdl) {
                        Ok(schema) => schema,
                        Err(e) => {
                            return json_response(
                                StatusCode::UNPROCESSABLE_ENTITY,
                                &serde_json::json!({ "error": e.to_string() }),
                            )
                        }
                    }
                } else {
                    open_runo_federation::ServiceSchema {
                        service_name: s.service_name,
                        types: s
                            .types
                            .into_iter()
                            .map(|(k, v)| (k, std::collections::BTreeSet::from_iter(v)))
                            .collect(),
                    }
                };
                service_schemas.push(schema);
            }

            let new_composed = match open_runo_federation::compose(&service_schemas) {
                Ok(c) => c,
                Err(e) => {
                    return json_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            let breaking = {
                let previous = state
                    .federation_schema
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if previous.contributing_services.is_empty() {
                    vec![]
                } else {
                    open_runo_federation::detect_breaking_changes(&previous, &new_composed)
                }
            };

            let contributing_services = new_composed.contributing_services.clone();
            let types_out: std::collections::BTreeMap<String, Vec<String>> = new_composed
                .types
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect();

            *state
                .federation_schema
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = new_composed;

            json_response(
                StatusCode::OK,
                &ComposeResponse {
                    contributing_services,
                    types: types_out,
                    breaking_changes: breaking,
                },
            )
        })
    })
}

/// GET /api/db/status — poem-free port of `handlers::db::db_status`, gated
/// by the same `X-Api-Key` check as the poem route.
pub fn db_status_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            json_response(
                StatusCode::OK,
                &DbStatusResponse {
                    backend: state.db.backend_name().to_string(),
                    status: "ok".to_string(),
                },
            )
        })
    })
}

/// GET /api/db/routing — poem-free port of `handlers::db::db_routing`.
/// The routing table is a static description (see the poem handler's
/// doc comment), so `state` is only needed for session-cookie auth
/// (`state.sessions`), not for the response body itself.
pub fn db_routing_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let entries = vec![
                DbRoutingEntry { table: "sessions".into(), target: "postgresql".into() },
                DbRoutingEntry { table: "api_keys".into(), target: "postgresql".into() },
                DbRoutingEntry { table: "rate_limits".into(), target: "postgresql".into() },
                DbRoutingEntry { table: "schemas".into(), target: "both".into() },
                DbRoutingEntry { table: "backup_jobs".into(), target: "both".into() },
                DbRoutingEntry { table: "persisted_queries".into(), target: "both".into() },
                DbRoutingEntry { table: "schema_history".into(), target: "aruaru-db".into() },
                DbRoutingEntry { table: "change_records".into(), target: "aruaru-db".into() },
                DbRoutingEntry { table: "audit_log".into(), target: "aruaru-db".into() },
            ];
            json_response(
                StatusCode::OK,
                &DbRoutingInfo {
                    default_target: "postgresql".into(),
                    entries,
                },
            )
        })
    })
}

fn parse_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or(serde_json::Value::String(raw.to_string()))
}

/// GET /api/db/:table — poem-free port of `handlers::db::db_list`.
pub fn db_list_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let table = params.get("table").unwrap_or("").to_string();
            let records = match state.db.list(&table).await {
                Ok(r) => r,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };
            let items: Vec<DbRecordItem> = records
                .into_iter()
                .map(|r| DbRecordItem { key: r.key, value: parse_value(&r.value) })
                .collect();
            json_response(
                StatusCode::OK,
                &DbRecordListResponse { count: items.len(), table, records: items },
            )
        })
    })
}

/// GET /api/db/:table/:key — poem-free port of `handlers::db::db_get`.
pub fn db_get_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let table = params.get("table").unwrap_or("").to_string();
            let key = params.get("key").unwrap_or("").to_string();
            match state.db.get(&table, &key).await {
                Ok(Some(raw)) => json_response(
                    StatusCode::OK,
                    &DbRecordResponse { table, key, value: parse_value(&raw) },
                ),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("record not found: {table}/{key}") }),
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// PUT /api/db/:table/:key — poem-free port of `handlers::db::db_put`.
pub fn db_put_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let table = params.get("table").unwrap_or("").to_string();
            let key = params.get("key").unwrap_or("").to_string();

            let raw: serde_json::Value = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let errors: Vec<String> = DB_UPSERT_REQUEST
                .iter_errors(&raw)
                .map(|e| format!("{} (at {})", e, e.instance_path))
                .collect();
            if !errors.is_empty() {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({
                        "error": format!("request body failed validation: {}", errors.join("; "))
                    }),
                );
            }

            let body: DbUpsertRequest = match serde_json::from_value(raw) {
                Ok(b) => b,
                Err(e) => {
                    return json_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &serde_json::json!({ "error": format!("deserialize body: {e}") }),
                    )
                }
            };

            let serialized = match serde_json::to_string(&body.value) {
                Ok(s) => s,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": format!("serialize value: {e}") }),
                    )
                }
            };

            if let Err(e) = state.db.put(&table, &key, &serialized).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "db.put", format!("{table}/{key}")).await;

            json_response(
                StatusCode::OK,
                &DbRecordResponse { table, key, value: body.value },
            )
        })
    })
}

/// DELETE /api/db/:table/:key — poem-free port of `handlers::db::db_delete`.
pub fn db_delete_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let table = params.get("table").unwrap_or("").to_string();
            let key = params.get("key").unwrap_or("").to_string();

            if let Err(e) = state.db.delete(&table, &key).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "db.delete", format!("{table}/{key}")).await;

            json_response(
                StatusCode::OK,
                &DbDeleteResponse { table, key, deleted: true },
            )
        })
    })
}

/// GET /api/schemas/:service — poem-free port of `handlers::schemas::get_schema`.
pub fn get_schema_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let query = query_params(&req);
            let service = params.get("service").unwrap_or("").to_string();
            let stage_str = query.get("stage").map(String::as_str).unwrap_or("local");
            let stage = parse_stage(stage_str);
            let namespace = query
                .get("namespace")
                .map(String::as_str)
                .unwrap_or(DEFAULT_NAMESPACE);

            let registry = state
                .schema_registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            match registry.latest_in(namespace, &service, stage) {
                Some(v) => json_response(
                    StatusCode::OK,
                    &SchemaVersion {
                        id: v.id.to_string(),
                        namespace: v.namespace.clone(),
                        service_name: v.service_name.clone(),
                        sdl: v.sdl.clone(),
                        stage: stage_name(v.stage).to_string(),
                        created_at: v.created_at.to_rfc3339(),
                    },
                ),
                None => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({
                        "error": format!("no schema found for '{service}' at stage '{stage_str}'")
                    }),
                ),
            }
        })
    })
}

/// GET /api/schemas/:service/history — poem-free port of
/// `handlers::schemas::get_schema_history`.
pub fn get_schema_history_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let query = query_params(&req);
            let service = params.get("service").unwrap_or("").to_string();
            let namespace = query
                .get("namespace")
                .map(String::as_str)
                .unwrap_or(DEFAULT_NAMESPACE);

            let registry = state
                .schema_registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            let versions = registry
                .history_in(namespace, &service)
                .iter()
                .map(|v| SchemaVersion {
                    id: v.id.to_string(),
                    namespace: v.namespace.clone(),
                    service_name: v.service_name.clone(),
                    sdl: v.sdl.clone(),
                    stage: stage_name(v.stage).to_string(),
                    created_at: v.created_at.to_rfc3339(),
                })
                .collect();

            json_response(StatusCode::OK, &SchemaHistoryResponse { versions })
        })
    })
}

/// Shared registration logic for both the JSON (`register_schema_handler`)
/// and multipart-file-upload (`register_schema_upload_handler`) entry
/// points, so the two transports can never drift on what "register a
/// schema" actually does.
async fn register_schema_and_respond(
    state: &Arc<AppState>,
    actor: &str,
    body: RegisterSchemaRequest,
) -> Response {
    let stage = parse_stage(&body.stage);
    let namespace = body
        .namespace
        .clone()
        .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
    let version = state
        .schema_registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .register_in(&namespace, &body.service_name, &body.sdl, stage);

    crate::audit::record(
        state,
        actor,
        "schema.register",
        format!("{}@{}", body.service_name, body.stage),
    )
    .await;

    let event = crate::state::SchemaEvent {
        service_name: body.service_name.clone(),
        stage: body.stage.clone(),
        at: chrono::Utc::now().to_rfc3339(),
    };
    let _ = state.events.send(event.clone());

    // EDFS (docs/cosmo-parity.md §4a): if configured, also publish this
    // event to Redis so instances *other* than this one (in a
    // load-balanced deployment) pick it up via their own edfs::spawn_bridge
    // task and deliver it to their locally-connected subscribers. A
    // publish failure is deliberately non-fatal here -- see edfs.rs's
    // `publish` doc comment for why -- and is a no-op entirely when the
    // `edfs` Cargo feature isn't compiled in.
    #[cfg(feature = "edfs")]
    {
        let target = state.edfs_publish.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some((redis_url, channel)) = target {
            if let Err(error) = crate::edfs::publish(&redis_url, &channel, &event).await {
                tracing::warn!(%error, "EDFS: failed to publish schema event to Redis");
            }
        }
    }

    json_response(
        StatusCode::OK,
        &SchemaVersion {
            id: version.id.to_string(),
            namespace: version.namespace.clone(),
            service_name: version.service_name.clone(),
            sdl: version.sdl.clone(),
            stage: stage_name(version.stage).to_string(),
            created_at: version.created_at.to_rfc3339(),
        },
    )
}

/// POST /api/schemas — poem-free port of `handlers::schemas::register_schema`.
pub fn register_schema_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };

            let raw: serde_json::Value = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let errors: Vec<String> = REGISTER_SCHEMA_REQUEST
                .iter_errors(&raw)
                .map(|e| format!("{} (at {})", e, e.instance_path))
                .collect();
            if !errors.is_empty() {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({
                        "error": format!("request body failed validation: {}", errors.join("; "))
                    }),
                );
            }

            let body: RegisterSchemaRequest = match serde_json::from_value(raw) {
                Ok(b) => b,
                Err(e) => {
                    return json_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            register_schema_and_respond(&state, &actor, body).await
        })
    })
}

/// POST /api/schemas/upload — `multipart/form-data` equivalent of
/// `register_schema_handler`, for uploading an SDL file directly (e.g. from
/// an `<input type="file">` in the WASM admin UI) instead of inlining the
/// SDL text into a JSON body. Poem-free hand-rolled multipart parsing, see
/// `hyper_compat::read_multipart_body` (the Poem-parity gap this closes).
///
/// Expected fields: `service_name` (text), `stage` (text, optional,
/// defaults like the JSON path), `namespace` (text, optional), `sdl_file`
/// (file — its raw bytes become the SDL text).
pub fn register_schema_upload_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };

            let fields = match crate::hyper_compat::read_multipart_body(req).await {
                Ok(f) => f,
                Err(resp) => return resp,
            };

            let mut service_name: Option<String> = None;
            let mut stage: Option<String> = None;
            let mut namespace: Option<String> = None;
            let mut sdl: Option<String> = None;

            for field in fields {
                match field.name.as_str() {
                    "service_name" => service_name = Some(String::from_utf8_lossy(&field.data).into_owned()),
                    "stage" => stage = Some(String::from_utf8_lossy(&field.data).into_owned()),
                    "namespace" => namespace = Some(String::from_utf8_lossy(&field.data).into_owned()),
                    "sdl_file" => sdl = Some(String::from_utf8_lossy(&field.data).into_owned()),
                    _ => {}
                }
            }

            let Some(service_name) = service_name else {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({ "error": "missing required field: service_name" }),
                );
            };
            let Some(sdl) = sdl else {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({ "error": "missing required file field: sdl_file" }),
                );
            };

            let body = RegisterSchemaRequest {
                service_name,
                sdl,
                stage: stage.unwrap_or_else(|| "local".to_string()),
                namespace,
            };

            register_schema_and_respond(&state, &actor, body).await
        })
    })
}

/// POST /api/schemas/upload-raw?service_name=...&stage=...&namespace=... —
/// a **non-multipart** equivalent of `register_schema_upload_handler`, for
/// clients that just want to `PUT`/`POST` the raw SDL bytes as the entire
/// request body (`Content-Type: application/octet-stream` or
/// `text/plain`) rather than building a `multipart/form-data` envelope.
/// Closes the "file attachment besides Multipart" Poem-parity gap: some
/// HTTP clients (simple `curl --data-binary @file`, some language SDKs'
/// default upload helpers) don't have convenient multipart support, but
/// can always POST a raw byte stream.
///
/// `service_name` (required) and `stage`/`namespace` (optional, same
/// defaults as the other two registration entry points) are read from the
/// query string, since the request body here is 100% the SDL bytes with
/// no room for other fields. Delegates to the same
/// `register_schema_and_respond` used by the JSON and multipart paths, so
/// this is a transport-only difference, never a behavioral one.
pub fn register_schema_upload_raw_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };

            let query = query_params(&req);
            let Some(service_name) = query.get("service_name").cloned() else {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({ "error": "missing required query parameter: service_name" }),
                );
            };
            let stage = query.get("stage").cloned().unwrap_or_else(|| "local".to_string());
            let namespace = query.get("namespace").cloned();

            let sdl_bytes = match req.into_body().collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({ "error": "failed to read request body" }),
                    )
                }
            };
            if sdl_bytes.is_empty() {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({ "error": "request body (SDL bytes) must not be empty" }),
                );
            }
            let sdl = String::from_utf8_lossy(&sdl_bytes).into_owned();

            let body = RegisterSchemaRequest {
                service_name,
                sdl,
                stage,
                namespace,
            };

            register_schema_and_respond(&state, &actor, body).await
        })
    })
}

fn parse_provider(s: &str) -> open_runo_ai_routing::Provider {
    use open_runo_ai_routing::Provider;
    match s.to_lowercase().replace('-', "_").as_str() {
        "openai" => Provider::OpenAi,
        "anthropic" | "anthropic_claude" => Provider::AnthropicClaude,
        "google" | "google_gemini" | "gemini" => Provider::GoogleGemini,
        "deepseek" => Provider::DeepSeek,
        "local" | "local_llm" => Provider::LocalLlm,
        _ => Provider::CustomOpenAiCompatible,
    }
}

fn parse_policy(s: &str) -> open_runo_ai_routing::RoutingPolicy {
    use open_runo_ai_routing::RoutingPolicy;
    match s.to_lowercase().as_str() {
        "latency" | "latency_optimized" => RoutingPolicy::LatencyOptimized,
        "local" | "local_first" => RoutingPolicy::LocalFirst,
        "privacy" | "privacy_first" => RoutingPolicy::PrivacyFirst,
        _ => RoutingPolicy::CostOptimized,
    }
}

fn provider_name(p: &open_runo_ai_routing::Provider) -> &'static str {
    use open_runo_ai_routing::Provider;
    match p {
        Provider::OpenAi => "openai",
        Provider::AnthropicClaude => "anthropic_claude",
        Provider::GoogleGemini => "google_gemini",
        Provider::DeepSeek => "deepseek",
        Provider::LocalLlm => "local_llm",
        Provider::CustomOpenAiCompatible => "custom_openai_compatible",
    }
}

#[derive(Debug, Deserialize)]
struct CandidateInput {
    provider: String,
    estimated_cost_usd_per_1k_tokens: f64,
    estimated_latency_ms: u32,
    is_local: bool,
    context_length: u32,
}

#[derive(Debug, Deserialize)]
struct RouteRequest {
    policy: String,
    min_context_length: Option<u32>,
    candidates: Vec<CandidateInput>,
}

#[derive(Serialize)]
struct RouteResponse {
    selected_provider: String,
    is_local: bool,
    estimated_cost_usd_per_1k_tokens: f64,
    estimated_latency_ms: u32,
}

/// POST /api/ai/route — poem-free port of `handlers::ai_routing::route_request`.
pub fn route_request_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let body: RouteRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let candidates: Vec<open_runo_ai_routing::Candidate> = body
                .candidates
                .iter()
                .map(|c| open_runo_ai_routing::Candidate {
                    provider: parse_provider(&c.provider),
                    estimated_cost_usd_per_1k_tokens: c.estimated_cost_usd_per_1k_tokens,
                    estimated_latency_ms: c.estimated_latency_ms,
                    is_local: c.is_local,
                    context_length: c.context_length,
                })
                .collect();

            let policy = parse_policy(&body.policy);
            let min_ctx = body.min_context_length.unwrap_or(0);

            match open_runo_ai_routing::route(&candidates, policy, min_ctx) {
                Ok(chosen) => json_response(
                    StatusCode::OK,
                    &RouteResponse {
                        selected_provider: provider_name(&chosen.provider).to_string(),
                        is_local: chosen.is_local,
                        estimated_cost_usd_per_1k_tokens: chosen.estimated_cost_usd_per_1k_tokens,
                        estimated_latency_ms: chosen.estimated_latency_ms,
                    },
                ),
                Err(e) => json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

fn persisted_query_store(state: &AppState) -> open_runo_persisted_queries::PersistedQueryStore {
    open_runo_persisted_queries::PersistedQueryStore::new(
        Arc::clone(&state.db),
        open_runo_persisted_queries::EnforcementMode::Allow,
    )
}

#[derive(Deserialize)]
struct PqRegisterRequest {
    query: String,
}

#[derive(Serialize)]
struct PqRegisterResponse {
    hash: String,
    registered_at: String,
}

/// POST /api/persisted-queries — poem-free port of
/// `handlers::persisted_queries::register_persisted_query`.
pub fn register_persisted_query_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let body: PqRegisterRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let record = match persisted_query_store(&state).register(&body.query).await {
                Ok(r) => r,
                Err(e) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            crate::audit::record(&state, &actor, "persisted_query.register", record.hash.clone()).await;

            json_response(
                StatusCode::OK,
                &PqRegisterResponse {
                    hash: record.hash,
                    registered_at: record.registered_at.to_rfc3339(),
                },
            )
        })
    })
}

#[derive(Serialize)]
struct PqQueryResponse {
    hash: String,
    query: String,
    registered_at: String,
}

/// GET /api/persisted-queries/:hash — poem-free port of
/// `handlers::persisted_queries::get_persisted_query`.
pub fn get_persisted_query_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let hash = params.get("hash").unwrap_or("").to_string();
            match persisted_query_store(&state).get(&hash).await {
                Ok(Some(record)) => json_response(
                    StatusCode::OK,
                    &PqQueryResponse {
                        hash: record.hash,
                        query: record.document,
                        registered_at: record.registered_at.to_rfc3339(),
                    },
                ),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("persisted query not found: {hash}") }),
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

#[derive(Deserialize)]
struct PurgeRequest {
    path: String,
}

#[derive(Serialize)]
struct PurgeResponse {
    purged: String,
}

/// POST /api/cache/purge — poem-free port of `handlers::cache::purge_page`.
pub fn purge_page_handler(
    state: Arc<AppState>,
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let body: PurgeRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            cache.purge(&body.path).await;
            crate::audit::record(&state, &actor, "cache.purge", body.path.clone()).await;
            json_response(StatusCode::OK, &PurgeResponse { purged: body.path })
        })
    })
}

/// POST /api/cache/purge-all — poem-free port of
/// `handlers::cache::purge_all_pages`.
pub fn purge_all_pages_handler(
    state: Arc<AppState>,
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            cache.purge_all().await;
            crate::audit::record(&state, &actor, "cache.purge_all", "*").await;
            json_response(StatusCode::OK, &PurgeResponse { purged: "*".to_string() })
        })
    })
}

#[derive(Serialize)]
struct PatternStat {
    pattern: String,
    requests: u64,
    arrival_interval_secs: Option<f64>,
    update_interval_secs: Option<f64>,
    render_cost_secs: Option<f64>,
}

#[derive(Serialize)]
struct AiStatsResponse {
    ai_enabled: bool,
    cache_hits: u64,
    cache_misses: u64,
    admitted: u64,
    rejected: u64,
    hit_ratio: f64,
    tracked_keys: usize,
    tracked_patterns: usize,
    top_patterns: Vec<PatternStat>,
}

/// GET /api/cache/ai-stats — poem-free port of `handlers::cache::ai_stats`.
pub fn ai_stats_handler(
    state: Arc<AppState>,
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let resp = match cache.predictor() {
                None => AiStatsResponse {
                    ai_enabled: false,
                    cache_hits: 0,
                    cache_misses: 0,
                    admitted: 0,
                    rejected: 0,
                    hit_ratio: 0.0,
                    tracked_keys: 0,
                    tracked_patterns: 0,
                    top_patterns: Vec::new(),
                },
                Some(p) => {
                    let snap = p.snapshot();
                    let total = snap.outcomes.cache_hits + snap.outcomes.cache_misses;
                    let mut patterns: Vec<PatternStat> = snap
                        .patterns
                        .iter()
                        .map(|(k, s)| PatternStat {
                            pattern: k.clone(),
                            requests: s.requests,
                            arrival_interval_secs: s.arrival_interval_secs,
                            update_interval_secs: s.update_interval_secs,
                            render_cost_secs: s.render_cost_secs,
                        })
                        .collect();
                    patterns.sort_by(|a, b| b.requests.cmp(&a.requests));
                    patterns.truncate(20);

                    AiStatsResponse {
                        ai_enabled: true,
                        cache_hits: snap.outcomes.cache_hits,
                        cache_misses: snap.outcomes.cache_misses,
                        admitted: snap.outcomes.admitted,
                        rejected: snap.outcomes.rejected,
                        hit_ratio: if total == 0 {
                            0.0
                        } else {
                            snap.outcomes.cache_hits as f64 / total as f64
                        },
                        tracked_keys: snap.keys.len(),
                        tracked_patterns: snap.patterns.len(),
                        top_patterns: patterns,
                    }
                }
            };
            json_response(StatusCode::OK, &resp)
        })
    })
}

#[derive(Serialize)]
struct ExportResponse {
    written: Vec<String>,
    records: usize,
}

/// POST /api/backup/export — poem-free port of `handlers::maintenance::backup_export`.
pub fn backup_export_handler(
    state: Arc<AppState>,
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let config = crate::maintenance::BackupConfig::from_env();
            let (written, records) = match crate::maintenance::export_backup(&state, &cache, &config).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(
                &state,
                &actor,
                "backup.export",
                format!("{records} records → {}", written.join(", ")),
            )
            .await;
            json_response(StatusCode::OK, &ExportResponse { written, records })
        })
    })
}

#[derive(Deserialize)]
struct ImportRequest {
    path: String,
}

#[derive(Serialize)]
struct ImportResponse {
    restored: usize,
}

/// POST /api/backup/import — poem-free port of `handlers::maintenance::backup_import`.
pub fn backup_import_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let body: ImportRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let restored = match crate::maintenance::import_backup(&state, &body.path).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(
                &state,
                &actor,
                "backup.import",
                format!("{restored} records ← {}", body.path),
            )
            .await;
            json_response(StatusCode::OK, &ImportResponse { restored })
        })
    })
}

#[derive(Serialize)]
struct IntegrityResponse {
    backend: &'static str,
    healed: usize,
    discrepancies: Vec<open_runo_db::dual::Discrepancy>,
}

/// POST /api/integrity/check — poem-free port of `handlers::maintenance::integrity_check`.
pub fn integrity_check_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let discrepancies = match state.db.consistency_check_and_heal().await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };
            for d in &discrepancies {
                crate::audit::record(
                    &state,
                    &actor,
                    "integrity.heal",
                    format!("{}/{} {} (from {})", d.table, d.key, d.kind, d.healed_from),
                )
                .await;
            }
            json_response(
                StatusCode::OK,
                &IntegrityResponse {
                    backend: state.db.backend_name(),
                    healed: discrepancies.len(),
                    discrepancies,
                },
            )
        })
    })
}

#[derive(Serialize)]
struct RestoreLatestResponse {
    restored_from: String,
    restored: usize,
}

/// POST /api/backup/restore-latest — poem-free port of
/// `handlers::maintenance::backup_restore_latest`.
pub fn backup_restore_latest_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let config = crate::maintenance::BackupConfig::from_env();
            let path = match crate::maintenance::find_latest_backup(&config) {
                Some(p) => p,
                None => {
                    return json_response(
                        StatusCode::NOT_FOUND,
                        &serde_json::json!({ "error": "no backup file found" }),
                    )
                }
            };
            let path_str = path.display().to_string();
            let restored = match crate::maintenance::import_backup(&state, &path_str).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(
                &state,
                &actor,
                "backup.restore_latest",
                format!("{restored} records ← {path_str}"),
            )
            .await;
            json_response(
                StatusCode::OK,
                &RestoreLatestResponse { restored_from: path_str, restored },
            )
        })
    })
}

#[derive(Deserialize)]
struct ExportSqlRequest {
    dialect: crate::maintenance::SqlDialect,
}

#[derive(Serialize)]
struct ConversionResponse {
    written: Vec<String>,
}

/// POST /api/migrate/export-sql — poem-free port of
/// `handlers::maintenance::migrate_export_sql`.
pub fn migrate_export_sql_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let body: ExportSqlRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let sql = match crate::maintenance::export_sql(&state, body.dialect).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            let name = format!(
                "open-runo-dump-{:?}-{}.sql",
                body.dialect,
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            )
            .to_lowercase();
            let written = match crate::maintenance::write_to_backup_dirs(
                &crate::maintenance::BackupConfig::from_env(),
                &name,
                &sql,
            ) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(&state, &actor, "migrate.export_sql", written.join(", ")).await;
            json_response(StatusCode::OK, &ConversionResponse { written })
        })
    })
}

/// POST /api/migrate/export-csv — poem-free port of
/// `handlers::maintenance::migrate_export_csv`.
pub fn migrate_export_csv_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let csv = match crate::maintenance::export_csv(&state).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            let name = format!(
                "open-runo-dump-{}.csv",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            );
            let written = match crate::maintenance::write_to_backup_dirs(
                &crate::maintenance::BackupConfig::from_env(),
                &name,
                &csv,
            ) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(&state, &actor, "migrate.export_csv", written.join(", ")).await;
            json_response(StatusCode::OK, &ConversionResponse { written })
        })
    })
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
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let query = query_params(&req);
            let filter = query
                .get("filter")
                .and_then(|f| open_runo_scim::ScimUserStore::parse_user_name_filter(f));

            match scim_user_store(&state).list(filter.as_deref()).await {
                Ok(users) => json_response(
                    StatusCode::OK,
                    &open_runo_scim::ListResponse {
                        schemas: vec!["urn:ietf:params:scim:api:messages:2.0:ListResponse".into()],
                        total_results: users.len(),
                        resources: users,
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

/// POST /scim/v2/Users — poem-free port of `handlers::scim::create_user`,
/// including KeyGuardian's auto-issue on provisioning.
pub fn scim_create_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let input: open_runo_scim::UserInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let user = match scim_user_store(&state).create(input).await {
                Ok(u) => u,
                Err(e) => {
                    return json_response(
                        StatusCode::CONFLICT,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            crate::audit::record(&state, &actor, "scim.user.create", user.user_name.clone()).await;

            let mut body = match serde_json::to_value(&user) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };
            match guardian.issue(&user.user_name, user.roles.clone(), None).await {
                Ok(plaintext) => {
                    body["urn:open-runo:params:scim:api-key"] = serde_json::Value::String(plaintext);
                    crate::audit::record(&state, "key-guardian", "key.auto_issue", user.user_name.clone()).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, owner = %user.user_name, "auto key issue failed");
                }
            }

            json_response(StatusCode::CREATED, &body)
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
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
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
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
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
                }
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
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let id = params.get("id").unwrap_or("").to_string();

            let owner = match scim_user_store(&state).get(&id).await {
                Ok(u) => u.map(|u| u.user_name),
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            if let Err(e) = scim_user_store(&state).delete(&id).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "scim.user.delete", id).await;

            if let Some(owner) = owner {
                if let Ok(n) = guardian.revoke_owner(&owner).await {
                    if n > 0 {
                        crate::audit::record(
                            &state,
                            "key-guardian",
                            "key.auto_revoke",
                            format!("{owner} ({n} keys, deleted)"),
                        )
                        .await;
                    }
                }
            }

            empty_status(StatusCode::NO_CONTENT)
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
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
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
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let input: open_runo_scim::GroupInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            match scim_group_store(&state).create(input).await {
                Ok(group) => {
                    crate::audit::record(&state, &actor, "scim.group.create", group.display_name.clone()).await;
                    json_response(StatusCode::CREATED, &group)
                }
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
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
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
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let id = params.get("id").unwrap_or("").to_string();
            let input: open_runo_scim::GroupInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            match scim_group_store(&state).replace(&id, input).await {
                Ok(group) => {
                    crate::audit::record(&state, &actor, "scim.group.replace", group.display_name.clone()).await;
                    json_response(StatusCode::OK, &group)
                }
                Err(e) => {
                    let msg = e.to_string();
                    let status = if msg.contains("not found") {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::CONFLICT
                    };
                    json_response(status, &serde_json::json!({ "error": msg }))
                }
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
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
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

const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// GET /api/events — poem-free port of `handlers::events::stream_events`.
/// Same heartbeat/history-change SSE semantics as the poem version.
pub fn stream_events_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }

            let initial_len = state.history.lock().map(|h| h.log().len()).unwrap_or(0);
            let stream = futures::stream::unfold((state, initial_len), |(state, mut last_len)| async move {
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
                let current_len = state.history.lock().map(|h| h.log().len()).unwrap_or(last_len);
                let event = if current_len > last_len {
                    last_len = current_len;
                    SseEvent {
                        event_type: Some("history"),
                        data: format!("{{\"history_len\":{current_len}}}"),
                    }
                } else {
                    SseEvent { event_type: Some("heartbeat"), data: "ping".to_string() }
                };
                Some((event, (state, last_len)))
            });

            sse_response(stream)
        })
    })
}

/// `GET /api/ws-echo` — the minimum-viable proof of the generic
/// `hyper_compat::websocket_handler` primitive: echoes back every text or
/// binary frame it receives, unchanged, until the client closes.
pub fn ws_echo_handler() -> Handler {
    crate::hyper_compat::websocket_handler(|mut conn| {
        Box::pin(async move {
            while let Some(msg) = conn.recv().await {
                let result = match &msg {
                    crate::hyper_compat::WsMessage::Text(text) => conn.send_text(text).await,
                    crate::hyper_compat::WsMessage::Binary(data) => conn.send_binary(data).await,
                };
                if result.is_err() {
                    break;
                }
            }
        })
    })
}

/// `GET /api/ws-events` — a second, more substantive WebSocket route:
/// the same `state.events` broadcast broker already consumed by
/// `stream_events_handler` (SSE) and the poem-based GraphQL Subscriptions
/// path, now also exposed as a plain WebSocket alternative. Purely
/// additive: neither of those other two consumers is touched.
pub fn ws_events_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    let auth_state = Arc::clone(&state);
    let inner = crate::hyper_compat::websocket_handler(move |mut conn| {
        let mut rx = state.events.subscribe();
        Box::pin(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
                        if conn.send_text(&payload).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    });
    Arc::new(move |req, params| {
        let guardian = Arc::clone(&guardian);
        let auth_state = Arc::clone(&auth_state);
        let inner = Arc::clone(&inner);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &auth_state.sessions).await {
                return empty_status(status);
            }
            inner(req, params).await
        })
    })
}

#[derive(Serialize)]
struct SelfIssueResponse {
    api_key: String,
    expires_at: String,
}

/// POST /api/keys/self-issue — lets a caller (typically the WASM frontend
/// on first load) obtain a working API key **without a human ever typing
/// or configuring one**. No auth required to reach this endpoint (like
/// `/health`) — the key it hands back is itself the credential, scoped to
/// the `developer` role and expiring after
/// [`SELF_ISSUE_KEY_TTL_HOURS`], so an unattended caller can't accumulate
/// standing access. Every issuance is audited. This completes
/// KeyGuardian's "no human key management" promise for browser clients,
/// which previously had to embed a fixed placeholder string that would
/// have been rejected the moment the registry went non-empty in production.
pub fn self_issue_key_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |_req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let owner = format!("wasm-frontend-{}", uuid::Uuid::new_v4());
            let expires_at = chrono::Utc::now() + chrono::Duration::hours(SELF_ISSUE_KEY_TTL_HOURS);
            match guardian
                .issue(&owner, vec!["developer".to_string()], Some(expires_at))
                .await
            {
                Ok(api_key) => {
                    crate::audit::record(&state, "key-guardian", "key.self_issue", owner).await;
                    json_response(
                        StatusCode::OK,
                        &SelfIssueResponse { api_key, expires_at: expires_at.to_rfc3339() },
                    )
                }
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

const SELF_ISSUE_KEY_TTL_HOURS: i64 = 24;

#[derive(Serialize)]
struct SessionLoginResponse {
    owner: String,
    csrf_token: String,
    expires_in_secs: i64,
}

/// POST /api/session/login — Poem-parity: Cookie/session management
/// (`docs/poem-parity.md`), additive to `X-Api-Key`. Exchanges an already-
/// valid `X-Api-Key` (self-issued, SCIM-issued, whatever the caller already
/// holds — this endpoint doesn't replace KeyGuardian, it rides on top of
/// it) for an `HttpOnly` session cookie plus a CSRF token, for callers that
/// would rather have the browser remember "who is this" than attach a
/// header by hand on every request. See `session.rs` module doc for the
/// CSRF double-submit design and `auth_hyper::authenticate_with_session`
/// for how the resulting session is later accepted.
pub fn session_login_handler(guardian: Arc<KeyGuardian>, sessions: Arc<SessionStore>) -> Handler {
    Arc::new(move |req, _params| {
        let guardian = Arc::clone(&guardian);
        let sessions = Arc::clone(&sessions);
        Box::pin(async move {
            let api_key = req
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .trim()
                .to_string();
            if api_key.is_empty() {
                return empty_status(StatusCode::UNAUTHORIZED);
            }

            let (owner, roles) = match guardian.verify(&api_key, chrono::Utc::now()).await {
                KeyDecision::RegistryEmpty => ("dev".to_string(), Vec::new()),
                KeyDecision::Ok { owner, roles } => (owner, roles),
                KeyDecision::Rejected | KeyDecision::Suspended => {
                    return empty_status(StatusCode::UNAUTHORIZED)
                }
            };

            let (session_id, csrf_token) = sessions.create(owner.clone(), roles);
            let mut resp = json_response(
                StatusCode::OK,
                &SessionLoginResponse {
                    owner,
                    csrf_token,
                    expires_in_secs: crate::session::SESSION_TTL_HOURS * 3600,
                },
            );
            if let Ok(value) = crate::session::set_cookie_header(&session_id).parse() {
                resp.headers_mut().insert(hyper::header::SET_COOKIE, value);
            }
            resp
        })
    })
}

/// POST /api/session/logout — destroys the session named by the
/// `orn_session` cookie (if any) and clears the cookie. Idempotent: logging
/// out with no session, or an already-expired one, still returns `200`.
pub fn session_logout_handler(sessions: Arc<SessionStore>) -> Handler {
    Arc::new(move |req, _params| {
        let sessions = Arc::clone(&sessions);
        Box::pin(async move {
            if let Some(session_id) = crate::session::session_id_from_cookie_header(req.headers()) {
                sessions.destroy(&session_id);
            }
            let mut resp = json_response(StatusCode::OK, &serde_json::json!({ "logged_out": true }));
            if let Ok(value) = crate::session::clear_cookie_header().parse() {
                resp.headers_mut().insert(hyper::header::SET_COOKIE, value);
            }
            resp
        })
    })
}

fn to_flag_response(flag: &FeatureFlag) -> FeatureFlagResponse {
    FeatureFlagResponse {
        name: flag.name.clone(),
        enabled: flag.enabled,
        rollout_percent: flag.rollout_percent,
        description: flag.description.clone(),
    }
}

/// POST /api/feature-flags — create-or-update a feature flag (Cosmo
/// Feature Flags parity: canary releases / percentage-based traffic
/// routing, see `docs/cosmo-parity.md` 4a and
/// `open_runo_feature_flags::FeatureFlagRegistry`).
pub fn feature_flag_upsert_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };

            let raw: serde_json::Value = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let errors: Vec<String> = FEATURE_FLAG_REQUEST
                .iter_errors(&raw)
                .map(|e| format!("{} (at {})", e, e.instance_path))
                .collect();
            if !errors.is_empty() {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({
                        "error": format!("request body failed validation: {}", errors.join("; "))
                    }),
                );
            }

            let body: FeatureFlagRequest = match serde_json::from_value(raw) {
                Ok(b) => b,
                Err(e) => {
                    return json_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &serde_json::json!({ "error": format!("deserialize body: {e}") }),
                    )
                }
            };

            let flag = FeatureFlag::new(body.name)
                .enabled(body.enabled)
                .rollout_percent(body.rollout_percent)
                .description(body.description);

            // The mutex guard is a temporary here (not bound to a variable),
            // so it drops at the end of this statement -- before the `.await`
            // below -- keeping the returned future `Send` (see db/schema
            // handlers above for the same pattern).
            let result = state
                .feature_flags
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .upsert(flag);

            match result {
                Ok(saved) => {
                    crate::audit::record(&state, &actor, "feature_flag.upsert", saved.name.clone()).await;
                    json_response(StatusCode::OK, &to_flag_response(&saved))
                }
                Err(e) => json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// GET /api/feature-flags — list every registered flag, sorted by name.
pub fn feature_flag_list_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let registry = state
                .feature_flags
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let flags = registry.list().iter().map(|f| to_flag_response(f)).collect();
            json_response(StatusCode::OK, &FeatureFlagListResponse { flags })
        })
    })
}

/// GET /api/feature-flags/:name — fetch a single flag definition.
pub fn feature_flag_get_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let name = params.get("name").unwrap_or("").to_string();
            let registry = state
                .feature_flags
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match registry.get(&name) {
                Some(flag) => json_response(StatusCode::OK, &to_flag_response(flag)),
                None => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("feature flag not found: {name}") }),
                ),
            }
        })
    })
}

/// DELETE /api/feature-flags/:name — remove a flag definition.
pub fn feature_flag_delete_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            let actor = match authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                Ok(actor) => format!("{}{}", if actor.via_session { "session:" } else { "" }, actor.owner),
                Err(status) => return empty_status(status),
            };
            let name = params.get("name").unwrap_or("").to_string();

            let existed = {
                let mut registry = state
                    .feature_flags
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                registry.delete(&name)
            };

            if !existed {
                return json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("feature flag not found: {name}") }),
                );
            }

            crate::audit::record(&state, &actor, "feature_flag.delete", name.clone()).await;
            json_response(StatusCode::OK, &serde_json::json!({ "name": name, "deleted": true }))
        })
    })
}

/// GET /api/feature-flags/:name/evaluate?bucket_key=... — deterministically
/// evaluate a flag for a caller (typically a user id, session id, or API
/// key). 404 if the flag itself is unknown (distinct from "off").
pub fn feature_flag_evaluate_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let name = params.get("name").unwrap_or("").to_string();
            let query = query_params(&req);
            let bucket_key = query.get("bucket_key").cloned().unwrap_or_default();

            let registry = state
                .feature_flags
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match registry.evaluate(&name, &bucket_key) {
                Some(enabled) => json_response(
                    StatusCode::OK,
                    &FeatureFlagEvaluationResponse { name, bucket_key, enabled },
                ),
                None => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("feature flag not found: {name}") }),
                ),
            }
        })
    })
}

/// GET /api/analytics/requests-per-month — monthly request-count metering
/// (`docs/cosmo-parity.md` 4a). An **operational metric only**: this is
/// never used to throttle or bill a caller (rate limiting is entirely
/// separate, see `open-runo-security::RateLimiter`). Served from the
/// in-process aggregate (`open_runo_observability::RequestMetrics`), not
/// a ClickHouse query, so it works identically whether or not a
/// ClickHouse export sink is configured -- see that crate's doc comment
/// for why the two read paths are kept separate.
pub fn requests_per_month_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "months": state.request_metrics.requests_per_month() }),
            )
        })
    })
}

/// GET /api/analytics/operations — per-operation (method+path) latency and
/// error-rate breakdown (Cosmo Studio parity, `docs/cosmo-parity.md` 4a),
/// sorted by total time contributed (busiest/slowest first). Same
/// in-process aggregate as [`requests_per_month_handler`].
pub fn operations_summary_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let method = req.method().clone();
            if let Err(status) = authenticate_with_session(req.headers(), &method, &guardian, &state.sessions).await {
                return empty_status(status);
            }
            let operations: Vec<serde_json::Value> = state
                .request_metrics
                .operations_summary()
                .into_iter()
                .map(|op| {
                    serde_json::json!({
                        "method": op.method,
                        "path": op.path,
                        "count": op.count,
                        "error_count": op.error_count,
                        "total_duration_ms": op.total_duration_ms,
                        "avg_duration_ms": op.avg_duration_ms(),
                        "error_rate": op.error_rate(),
                    })
                })
                .collect();
            json_response(StatusCode::OK, &serde_json::json!({ "operations": operations }))
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
        Arc::new(KeyGuardian::new(Arc::clone(&state.db), GuardianConfig::from_env()))
    }

    #[tokio::test]
    async fn federation_status_reflects_composed_schema() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/federation/status",
            federation_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/federation/status"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["contributing_services"], serde_json::json!([]));
        assert_eq!(body["type_count"], 0);
        assert_eq!(body["field_count"], 0);
    }

    #[tokio::test]
    async fn federation_status_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/federation/status",
            federation_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/federation/status"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn db_status_reports_in_memory_backend() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/db/status",
            db_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["backend"], "in-memory");
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn db_status_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/db/status",
            db_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/status"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn db_routing_has_expected_tables() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::GET, "/api/db/routing", db_routing_handler(Arc::clone(&state), guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/routing"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert!(body["entries"].as_array().unwrap().len() >= 8);
    }

    #[tokio::test]
    async fn db_routing_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::GET, "/api/db/routing", db_routing_handler(Arc::clone(&state), guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/routing"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_schema_returns_registered_version() {
        use open_runo_schema_registry::Stage;

        let state = Arc::new(AppState::new());
        state
            .schema_registry
            .lock()
            .unwrap()
            .register_in("default", "users", "type User { id: ID! }", Stage::Local);

        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service",
            get_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/users"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["service_name"], "users");
        assert_eq!(body["sdl"], "type User { id: ID! }");
    }

    #[tokio::test]
    async fn get_schema_missing_service_returns_404() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service",
            get_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/nonexistent"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_schema_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service",
            get_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/users"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_schema_history_returns_all_versions() {
        use open_runo_schema_registry::Stage;

        let state = Arc::new(AppState::new());
        {
            let mut registry = state.schema_registry.lock().unwrap();
            registry.register_in("default", "users", "type User { id: ID! }", Stage::Local);
            registry.register_in("default", "users", "type User { id: ID! name: String }", Stage::Local);
        }

        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service/history",
            get_schema_history_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/users/history"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["versions"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn register_and_fetch_schema_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/schemas",
                register_schema_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/schemas/:service",
                get_schema_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/schemas"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
                "service_name": "users",
                "sdl": "type User { id: ID! name: String }",
                "stage": "local"
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["service_name"], "users");

        let resp = client
            .get(format!("http://{addr}/api/schemas/users"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    /// Same registration flow as `register_and_fetch_schema_roundtrip`, but
    /// via the `multipart/form-data` upload endpoint (SDL as a file field)
    /// instead of an inline JSON body -- proves the two transports actually
    /// converge on the same registered schema state.
    #[tokio::test]
    async fn register_schema_upload_and_fetch_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/schemas/upload",
                register_schema_upload_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/schemas/:service",
                get_schema_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let form = reqwest::multipart::Form::new()
            .text("service_name", "orders")
            .text("stage", "local")
            .part(
                "sdl_file",
                reqwest::multipart::Part::bytes(b"type Order { id: ID! total: Float }".to_vec())
                    .file_name("orders.graphql")
                    .mime_str("text/plain")
                    .unwrap(),
            );

        let resp = client
            .post(format!("http://{addr}/api/schemas/upload"))
            .header("x-api-key", "test-key")
            .multipart(form)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["service_name"], "orders");
        assert_eq!(body["sdl"], "type Order { id: ID! total: Float }");

        let resp = client
            .get(format!("http://{addr}/api/schemas/orders"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["sdl"], "type Order { id: ID! total: Float }");
    }

    /// Same registration flow again, but via the raw (non-multipart)
    /// `/api/schemas/upload-raw` endpoint -- SDL bytes as the entire
    /// request body, `service_name`/`stage` in the query string. Proves
    /// all three transports (JSON, multipart, raw) converge on the same
    /// registered schema state via the shared `register_schema_and_respond`.
    #[tokio::test]
    async fn register_schema_upload_raw_and_fetch_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/schemas/upload-raw",
                register_schema_upload_raw_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/schemas/:service",
                get_schema_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!(
                "http://{addr}/api/schemas/upload-raw?service_name=invoices&stage=local"
            ))
            .header("x-api-key", "test-key")
            .header("content-type", "application/octet-stream")
            .body("type Invoice { id: ID! amount: Float }")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["service_name"], "invoices");
        assert_eq!(body["sdl"], "type Invoice { id: ID! amount: Float }");

        let resp = client
            .get(format!("http://{addr}/api/schemas/invoices"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["sdl"], "type Invoice { id: ID! amount: Float }");
    }

    #[tokio::test]
    async fn register_schema_upload_raw_requires_service_name() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/schemas/upload-raw",
            register_schema_upload_raw_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        // No ?service_name=... in the query string.
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/schemas/upload-raw"))
            .header("x-api-key", "test-key")
            .body("type Foo { id: ID! }")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn register_schema_upload_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/schemas/upload",
            register_schema_upload_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let form = reqwest::multipart::Form::new()
            .text("service_name", "orders")
            .part("sdl_file", reqwest::multipart::Part::bytes(b"type Order { id: ID! }".to_vec()));

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/schemas/upload"))
            .multipart(form)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_schema_upload_rejects_missing_sdl_file() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/schemas/upload",
            register_schema_upload_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let form = reqwest::multipart::Form::new().text("service_name", "orders");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/schemas/upload"))
            .header("x-api-key", "test-key")
            .multipart(form)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// Full Poem-parity Cookie/session lifecycle over real HTTP: issue a
    /// real API key (not the RegistryEmpty dev fast-path), exchange it for
    /// a session via `/api/session/login`, register a schema using
    /// *only* the session cookie + CSRF token (no `X-Api-Key` at all),
    /// confirm a state-changing request without the CSRF token is
    /// rejected, then log out and confirm the session no longer works.
    #[tokio::test]
    async fn session_login_then_register_schema_via_cookie_then_logout() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let real_key = guardian
            .issue("carol", vec!["developer".to_string()], None)
            .await
            .expect("issuing a real key should succeed");

        let router = Router::new()
            .route(
                Method::POST,
                "/api/session/login",
                session_login_handler(Arc::clone(&guardian), Arc::clone(&state.sessions)),
            )
            .route(
                Method::POST,
                "/api/session/logout",
                session_logout_handler(Arc::clone(&state.sessions)),
            )
            .route(
                Method::POST,
                "/api/schemas",
                register_schema_handler(Arc::clone(&state), Arc::clone(&guardian)),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        // 1. Exchange the real API key for a session.
        let login_resp = client
            .post(format!("http://{addr}/api/session/login"))
            .header("x-api-key", &real_key)
            .send()
            .await
            .expect("login request should succeed");
        assert_eq!(login_resp.status(), reqwest::StatusCode::OK);
        let set_cookie = login_resp
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .expect("login should set a session cookie")
            .to_str()
            .unwrap()
            .to_string();
        assert!(set_cookie.contains("HttpOnly"));
        let cookie_pair = set_cookie.split(';').next().unwrap().to_string();
        let login_body: serde_json::Value = login_resp.json().await.unwrap();
        assert_eq!(login_body["owner"], "carol");
        let csrf_token = login_body["csrf_token"].as_str().unwrap().to_string();
        assert!(!csrf_token.is_empty());

        // 2. A state-changing request with the cookie but NO CSRF token is
        // forbidden, even though the session itself is valid.
        let resp = client
            .post(format!("http://{addr}/api/schemas"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .json(&serde_json::json!({ "service_name": "orders", "sdl": "type Order { id: ID! }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

        // 3. With the cookie AND the matching CSRF token, it succeeds --
        // and no X-Api-Key header was sent at all for this request.
        let resp = client
            .post(format!("http://{addr}/api/schemas"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .header("x-csrf-token", &csrf_token)
            .json(&serde_json::json!({ "service_name": "orders", "sdl": "type Order { id: ID! }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["service_name"], "orders");

        // 4. Log out, which destroys the session and clears the cookie.
        let logout_resp = client
            .post(format!("http://{addr}/api/session/logout"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .send()
            .await
            .expect("logout request should succeed");
        assert_eq!(logout_resp.status(), reqwest::StatusCode::OK);
        let clear_cookie = logout_resp
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .expect("logout should clear the cookie")
            .to_str()
            .unwrap();
        assert!(clear_cookie.contains("Max-Age=0"));

        // 5. The now-destroyed session no longer authenticates anything,
        // even with the previously-valid CSRF token.
        let resp = client
            .post(format!("http://{addr}/api/schemas"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .header("x-csrf-token", &csrf_token)
            .json(&serde_json::json!({ "service_name": "orders2", "sdl": "type Order2 { id: ID! }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn session_login_rejects_missing_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/session/login",
            session_login_handler(Arc::clone(&guardian), Arc::clone(&state.sessions)),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/session/login"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn session_logout_without_a_cookie_is_still_ok() {
        let state = Arc::new(AppState::new());
        let router = Router::new().route(
            Method::POST,
            "/api/session/logout",
            session_logout_handler(Arc::clone(&state.sessions)),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/session/logout"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn register_schema_rejects_invalid_body() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/schemas",
            register_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/schemas"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "not_a_valid_field": true }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn register_schema_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/schemas",
            register_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/schemas"))
            .json(&serde_json::json!({ "service_name": "users", "sdl": "type User { id: ID! }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ai_route_returns_best_provider() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::POST, "/api/ai/route", route_request_handler(Arc::clone(&state), guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/ai/route"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
                "policy": "cost",
                "min_context_length": 4000,
                "candidates": [
                    {
                        "provider": "local_llm",
                        "estimated_cost_usd_per_1k_tokens": 0.0,
                        "estimated_latency_ms": 900,
                        "is_local": true,
                        "context_length": 8000
                    },
                    {
                        "provider": "anthropic",
                        "estimated_cost_usd_per_1k_tokens": 3.0,
                        "estimated_latency_ms": 400,
                        "is_local": false,
                        "context_length": 200000
                    }
                ]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["selected_provider"], "local_llm");
    }

    #[tokio::test]
    async fn ai_route_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::POST, "/api/ai/route", route_request_handler(Arc::clone(&state), guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/ai/route"))
            .json(&serde_json::json!({ "policy": "cost", "candidates": [] }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn compose_and_status_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/federation/compose",
                compose_schemas_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/federation/status",
                federation_status_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/federation/compose"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
                "services": [
                    { "service_name": "users", "types": { "User": ["id", "name"] } },
                    { "service_name": "billing", "types": { "Invoice": ["id", "amount"] } }
                ]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/federation/status"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["type_count"], 2);
    }

    #[tokio::test]
    async fn compose_schemas_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/federation/compose",
            compose_schemas_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/federation/compose"))
            .json(&serde_json::json!({ "services": [] }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    fn db_router(state: &Arc<AppState>, guardian: Arc<KeyGuardian>) -> Router {
        Router::new()
            .route(Method::GET, "/api/db/:table", db_list_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/api/db/:table/:key", db_get_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::PUT, "/api/db/:table/:key", db_put_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::DELETE, "/api/db/:table/:key", db_delete_handler(Arc::clone(state), guardian))
    }

    #[tokio::test]
    async fn db_crud_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = db_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();
        let key = "x-api-key";

        let resp = client
            .put(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .json(&serde_json::json!({ "value": { "hello": "world" } }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["value"]["hello"], "world");

        let resp = client
            .get(format!("http://{addr}/api/db/test_table"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["count"], 1);

        let resp = client
            .delete(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn db_get_missing_key_returns_404() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = db_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/schemas/nonexistent"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn db_put_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = db_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .put(format!("http://{addr}/api/db/test_table/rec1"))
            .json(&serde_json::json!({ "value": 1 }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    /// Proves the DB CRUD handlers actually accept session-cookie auth now
    /// (not just X-Api-Key) -- the broader rollout of
    /// `authenticate_with_session` past `register_schema_handler`
    /// mentioned as a follow-up in docs/poem-parity.md. A real login (real
    /// issued key, not the RegistryEmpty dev fast-path) followed by a real
    /// PUT/GET/DELETE using only the cookie + CSRF token, no X-Api-Key
    /// header at all.
    #[tokio::test]
    async fn db_crud_accepts_session_cookie_with_csrf_token() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let real_key = guardian
            .issue("dana", vec!["developer".to_string()], None)
            .await
            .expect("issuing a real key should succeed");

        let router = db_router(&state, Arc::clone(&guardian)).route(
            Method::POST,
            "/api/session/login",
            session_login_handler(Arc::clone(&guardian), Arc::clone(&state.sessions)),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let login_resp = client
            .post(format!("http://{addr}/api/session/login"))
            .header("x-api-key", &real_key)
            .send()
            .await
            .expect("login should succeed");
        let cookie_pair = login_resp
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf_token = login_resp.json::<serde_json::Value>().await.unwrap()["csrf_token"]
            .as_str()
            .unwrap()
            .to_string();

        // PUT via cookie + CSRF, no X-Api-Key at all.
        let resp = client
            .put(format!("http://{addr}/api/db/test_table/session_rec"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .header("x-csrf-token", &csrf_token)
            .json(&serde_json::json!({ "value": { "via": "session" } }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // GET is not state-changing -- the cookie alone (no CSRF token) is
        // enough, matching authenticate_with_session's method-based rule.
        let resp = client
            .get(format!("http://{addr}/api/db/test_table/session_rec"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["value"]["via"], "session");

        // DELETE is state-changing again -- needs the CSRF token.
        let resp = client
            .delete(format!("http://{addr}/api/db/test_table/session_rec"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .header("x-csrf-token", &csrf_token)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn persisted_query_register_and_fetch_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/persisted-queries",
                register_persisted_query_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/persisted-queries/:hash",
                get_persisted_query_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/persisted-queries"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "query": "{ health }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let hash = body["hash"].as_str().unwrap().to_string();
        assert_eq!(hash.len(), 64);

        let resp = client
            .get(format!("http://{addr}/api/persisted-queries/{hash}"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["query"], "{ health }");

        let resp = client
            .get(format!(
                "http://{addr}/api/persisted-queries/0000000000000000000000000000000000000000000000000000000000000000"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn register_persisted_query_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/persisted-queries",
            register_persisted_query_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/persisted-queries"))
            .json(&serde_json::json!({ "query": "{ health }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn cache_purge_endpoints_respond() {
        use crate::middleware::html_cache::{HtmlCacheConfig, HtmlPageCache};

        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let cache = Arc::new(HtmlPageCache::new(HtmlCacheConfig::from_env()));
        let router = Router::new()
            .route(
                Method::POST,
                "/api/cache/purge",
                purge_page_handler(Arc::clone(&state), Arc::clone(&cache), Arc::clone(&guardian)),
            )
            .route(
                Method::POST,
                "/api/cache/purge-all",
                purge_all_pages_handler(Arc::clone(&state), Arc::clone(&cache), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/cache/purge"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "path": "/page/123" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["purged"], "/page/123");

        let resp = client
            .post(format!("http://{addr}/api/cache/purge-all"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn ai_stats_reports_disabled_when_no_predictor() {
        use crate::middleware::html_cache::{HtmlCacheConfig, HtmlPageCache};

        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let mut config = HtmlCacheConfig::from_env();
        config.ai = false;
        let cache = Arc::new(HtmlPageCache::new(config));
        let router = Router::new().route(Method::GET, "/api/cache/ai-stats", ai_stats_handler(Arc::clone(&state), cache, guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/cache/ai-stats"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["ai_enabled"], false);
    }

    #[tokio::test]
    async fn integrity_check_endpoint_responds() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/integrity/check",
            integrity_check_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/integrity/check"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["healed"], 0);
    }

    #[tokio::test]
    async fn backup_export_and_import_roundtrip() {
        use crate::middleware::html_cache::{HtmlCacheConfig, HtmlPageCache};

        let state = Arc::new(AppState::new());
        state.schema_registry.lock().unwrap().register_in(
            "default",
            "bk",
            "type B { x: ID }",
            open_runo_schema_registry::Stage::Local,
        );

        let guardian = guardian(&state);
        let cache = Arc::new(HtmlPageCache::new(HtmlCacheConfig::from_env()));
        let router = Router::new()
            .route(
                Method::POST,
                "/api/backup/export",
                backup_export_handler(Arc::clone(&state), cache, Arc::clone(&guardian)),
            )
            .route(
                Method::POST,
                "/api/backup/import",
                backup_import_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let dir = std::env::temp_dir().join(format!("orn-hyper-e2e-{}", uuid::Uuid::new_v4()));
        std::env::set_var("OPEN_RUNO_BACKUP_DIR", &dir);

        let resp = client
            .post(format!("http://{addr}/api/backup/export"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let path = body["written"][0].as_str().unwrap().to_string();
        assert!(body["records"].as_u64().unwrap() >= 1);

        let resp = client
            .post(format!("http://{addr}/api/backup/import"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        std::env::remove_var("OPEN_RUNO_BACKUP_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scim_router(state: &Arc<AppState>, guardian: Arc<KeyGuardian>) -> Router {
        Router::new()
            .route(Method::GET, "/scim/v2/Users", scim_list_users_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::POST, "/scim/v2/Users", scim_create_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/scim/v2/Users/:id", scim_get_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::PUT, "/scim/v2/Users/:id", scim_replace_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::DELETE, "/scim/v2/Users/:id", scim_delete_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/scim/v2/Groups", scim_list_groups_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::POST, "/scim/v2/Groups", scim_create_group_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/scim/v2/Groups/:id", scim_get_group_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::PUT, "/scim/v2/Groups/:id", scim_replace_group_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::DELETE, "/scim/v2/Groups/:id", scim_delete_group_handler(Arc::clone(state), guardian))
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

        let router = scim_router(&state, Arc::clone(&guardian)).route(
            Method::POST,
            "/api/session/login",
            session_login_handler(Arc::clone(&guardian), Arc::clone(&state.sessions)),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let login_resp = client
            .post(format!("http://{addr}/api/session/login"))
            .header("x-api-key", &real_key)
            .send()
            .await
            .expect("login should succeed");
        let cookie_pair = login_resp
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf_token = login_resp.json::<serde_json::Value>().await.unwrap()["csrf_token"]
            .as_str()
            .unwrap()
            .to_string();

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
        let router = scim_router(&state, Arc::clone(&guardian))
            .route(Method::GET, "/api/db/status", db_status_handler(Arc::clone(&state), Arc::clone(&guardian)));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        client
            .get(format!("http://{addr}/api/db/status"))
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
        let issued = body["urn:open-runo:params:scim:api-key"].as_str().unwrap().to_string();
        let user_id = body["id"].as_str().unwrap().to_string();
        assert!(issued.starts_with("orn_"));

        let resp = client
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", "anything-goes")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        let resp = client
            .get(format!("http://{addr}/api/db/status"))
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
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn events_endpoint_returns_event_stream_content_type() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::GET, "/api/events", stream_events_handler(state, guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/events"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
    }

    #[tokio::test]
    async fn events_endpoint_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::GET, "/api/events", stream_events_handler(state, guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/events"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn self_issue_key_requires_no_auth_and_the_key_it_returns_works() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/keys/self-issue",
                self_issue_key_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/db/status",
                db_status_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        // No X-Api-Key header at all -- self-issue must still succeed.
        let resp = client
            .post(format!("http://{addr}/api/keys/self-issue"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let key = body["api_key"].as_str().unwrap().to_string();
        assert!(key.starts_with("orn_"));
        assert!(!body["expires_at"].as_str().unwrap().is_empty());

        // The freshly self-issued key must now authenticate real requests.
        let resp = client
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", &key)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    fn feature_flags_router(state: &Arc<AppState>, guardian: Arc<KeyGuardian>) -> Router {
        Router::new()
            .route(
                Method::POST,
                "/api/feature-flags",
                feature_flag_upsert_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/feature-flags",
                feature_flag_list_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/feature-flags/:name",
                feature_flag_get_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::DELETE,
                "/api/feature-flags/:name",
                feature_flag_delete_handler(Arc::clone(state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/feature-flags/:name/evaluate",
                feature_flag_evaluate_handler(Arc::clone(state), guardian),
            )
    }

    #[tokio::test]
    async fn feature_flag_upsert_and_get_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = feature_flags_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/feature-flags"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "name": "new-checkout", "rollout_percent": 25, "description": "canary" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["name"], "new-checkout");
        assert_eq!(body["rollout_percent"], 25);
        assert_eq!(body["enabled"], true);

        let resp = client
            .get(format!("http://{addr}/api/feature-flags/new-checkout"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["description"], "canary");
    }

    #[tokio::test]
    async fn feature_flag_upsert_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = feature_flags_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/feature-flags"))
            .json(&serde_json::json!({ "name": "f" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn feature_flag_get_unknown_is_404() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = feature_flags_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/feature-flags/ghost"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn feature_flag_list_reflects_upserts() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = feature_flags_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        for name in ["zeta", "alpha"] {
            let resp = client
                .post(format!("http://{addr}/api/feature-flags"))
                .header("x-api-key", "test-key")
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await
                .expect("request should succeed");
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
        }

        let resp = client
            .get(format!("http://{addr}/api/feature-flags"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let names: Vec<&str> = body["flags"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[tokio::test]
    async fn feature_flag_evaluate_unknown_flag_is_404() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = feature_flags_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/feature-flags/ghost/evaluate?bucket_key=user-1"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn feature_flag_evaluate_full_rollout_is_true_for_any_bucket_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = feature_flags_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/feature-flags"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "name": "always-on", "rollout_percent": 100 }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/feature-flags/always-on/evaluate?bucket_key=whoever"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["bucket_key"], "whoever");
    }

    #[tokio::test]
    async fn feature_flag_delete_then_get_is_404() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = feature_flags_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/feature-flags"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "name": "temp" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .delete(format!("http://{addr}/api/feature-flags/temp"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/feature-flags/temp"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    /// Feature flags are the second real (non-toy) route group -- after
    /// the DB CRUD group -- rolled onto `authenticate_with_session`, past
    /// the original `register_schema_handler`-only example. Upsert
    /// (state-changing, needs CSRF) then evaluate (read-only, cookie
    /// alone suffices), no X-Api-Key at all.
    #[tokio::test]
    async fn feature_flag_upsert_and_evaluate_accept_session_cookie_with_csrf_token() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let real_key = guardian
            .issue("erin", vec!["developer".to_string()], None)
            .await
            .expect("issuing a real key should succeed");

        let router = feature_flags_router(&state, Arc::clone(&guardian)).route(
            Method::POST,
            "/api/session/login",
            session_login_handler(Arc::clone(&guardian), Arc::clone(&state.sessions)),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let login_resp = client
            .post(format!("http://{addr}/api/session/login"))
            .header("x-api-key", &real_key)
            .send()
            .await
            .expect("login should succeed");
        let cookie_pair = login_resp
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf_token = login_resp.json::<serde_json::Value>().await.unwrap()["csrf_token"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = client
            .post(format!("http://{addr}/api/feature-flags"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .header("x-csrf-token", &csrf_token)
            .json(&serde_json::json!({ "name": "session-flag", "rollout_percent": 100 }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/feature-flags/session-flag/evaluate?bucket_key=anyone"))
            .header(reqwest::header::COOKIE, &cookie_pair)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["enabled"], true);
    }

    /// Real end-to-end WebSocket round trip: a real TCP listener + real
    /// hyper HTTP/1.1 connection with `.with_upgrades()`, and a real
    /// WebSocket client (`tokio-tungstenite`, test-only -- the server side
    /// is entirely hand-rolled in `hyper_compat`). Connects, does the RFC
    /// 6455 handshake, sends a text frame and a binary frame, asserts the
    /// echo comes back unchanged for each, then closes cleanly.
    #[tokio::test]
    async fn websocket_echo_round_trip_over_real_tcp() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let router = Router::new().route(Method::GET, "/api/ws-echo", ws_echo_handler());
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let (mut ws, response) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws-echo"))
            .await
            .expect("client should complete the WebSocket handshake");
        assert_eq!(response.status(), 101);

        ws.send(Message::Text("hello websocket".into())).await.expect("send text");
        let reply = ws.next().await.expect("a reply frame").expect("valid frame");
        assert_eq!(reply, Message::Text("hello websocket".into()));

        ws.send(Message::Binary(vec![1, 2, 3, 4])).await.expect("send binary");
        let reply = ws.next().await.expect("a reply frame").expect("valid frame");
        assert_eq!(reply, Message::Binary(vec![1, 2, 3, 4]));

        ws.close(None).await.expect("close should send cleanly");
    }

    /// `/api/ws-events` requires the same `X-Api-Key` auth as every other
    /// protected route -- verified by hand-crafting the raw HTTP/1.1
    /// upgrade request (no `X-Api-Key` header) and asserting a plain `401`
    /// rather than a `101 Switching Protocols`.
    #[tokio::test]
    async fn ws_events_rejects_missing_api_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::GET, "/api/ws-events", ws_events_handler(state, guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let request = "GET /api/ws-events HTTP/1.1\r\n\
             Host: localhost\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n";
        stream.write_all(request.as_bytes()).await.expect("write request");

        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 401"), "expected 401, got: {response}");
    }
}
