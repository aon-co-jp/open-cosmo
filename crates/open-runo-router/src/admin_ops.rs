//! 管理操作の共通ロジック(REST廃止計画 第1段階、CLAUDE.md「2026-11-01追記」)。
//!
//! `handlers_hyper`のREST版ハンドラと`open-runo-gateway`のGraphQL
//! Query/Mutationが**同じ関数**を呼ぶことで、二重実装による挙動の乖離を
//! 防ぐ。認証(`auth_hyper::authenticate_with_session`)は呼び出し側の責務で、
//! ここには「認証済みの`actor`文字列」だけが渡る(RESTも現状は認証のみで
//! ロール別の検査は無く、GraphQLも同じ権限で揃える)。

use crate::middleware::html_cache::HtmlPageCache;
use crate::state::AppState;
use hyper::StatusCode;
use open_runo_api_types::{
    FeatureFlagEvaluationResponse, FeatureFlagRequest, FeatureFlagResponse, RegisterSchemaRequest,
    SchemaVersion,
};
use open_runo_feature_flags::FeatureFlag;
use open_runo_schema_registry::{Stage, DEFAULT_NAMESPACE};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// 管理操作の失敗。`status`はREST版のHTTPステータスと同じ値。
#[derive(Debug, Clone)]
pub struct AdminError {
    pub status: StatusCode,
    pub message: String,
}

impl AdminError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type AdminResult<T> = Result<T, AdminError>;

pub fn parse_stage(s: &str) -> Stage {
    match s.to_lowercase().as_str() {
        "development" | "dev" => Stage::Development,
        "staging" | "stg" => Stage::Staging,
        "production" | "prod" => Stage::Production,
        _ => Stage::Local,
    }
}

pub fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Local => "local",
        Stage::Development => "development",
        Stage::Staging => "staging",
        Stage::Production => "production",
    }
}

fn to_schema_version(v: &open_runo_schema_registry::SchemaVersion) -> SchemaVersion {
    SchemaVersion {
        id: v.id.to_string(),
        namespace: v.namespace.clone(),
        service_name: v.service_name.clone(),
        sdl: v.sdl.clone(),
        stage: stage_name(v.stage).to_string(),
        created_at: v.created_at.to_rfc3339(),
    }
}

// ── スキーマ ─────────────────────────────────────────────────────────────

/// スキーマ登録(JSON/multipart/raw/GraphQLの全入口で共通)。
pub async fn register_schema(
    state: &Arc<AppState>,
    actor: &str,
    body: RegisterSchemaRequest,
) -> SchemaVersion {
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

    // EDFS: 他インスタンスへの配信(失敗は非致命、詳細はedfs.rs)。
    #[cfg(feature = "edfs")]
    {
        let target = state
            .edfs_publish
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some((redis_url, channel)) = target {
            if let Err(error) = crate::edfs::publish(&redis_url, &channel, &event).await {
                tracing::warn!(%error, "EDFS: failed to publish schema event to Redis");
            }
        }
    }

    to_schema_version(&version)
}

pub fn get_schema(
    state: &AppState,
    namespace: Option<&str>,
    service: &str,
    stage: Option<&str>,
) -> AdminResult<SchemaVersion> {
    let stage_str = stage.unwrap_or("local");
    let namespace = namespace.unwrap_or(DEFAULT_NAMESPACE);
    let registry = state
        .schema_registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry
        .latest_in(namespace, service, parse_stage(stage_str))
        .map(to_schema_version)
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::NOT_FOUND,
                format!("no schema found for '{service}' at stage '{stage_str}'"),
            )
        })
}

pub fn schema_history(
    state: &AppState,
    namespace: Option<&str>,
    service: &str,
) -> Vec<SchemaVersion> {
    let namespace = namespace.unwrap_or(DEFAULT_NAMESPACE);
    let registry = state
        .schema_registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry
        .history_in(namespace, service)
        .iter()
        .map(to_schema_version)
        .collect()
}

// ── Federation ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ComposeService {
    pub service_name: String,
    pub types: BTreeMap<String, Vec<String>>,
    pub sdl: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ComposeOutput {
    pub contributing_services: Vec<String>,
    pub types: BTreeMap<String, Vec<String>>,
    pub breaking_changes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FederationStatus {
    pub contributing_services: Vec<String>,
    pub type_count: usize,
    pub field_count: usize,
}

pub fn federation_status(state: &AppState) -> FederationStatus {
    let schema = state
        .federation_schema
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let field_count: usize = schema.types.values().map(|f| f.len()).sum();
    FederationStatus {
        contributing_services: schema.contributing_services,
        type_count: schema.types.len(),
        field_count,
    }
}

pub fn compose(state: &AppState, services: Vec<ComposeService>) -> AdminResult<ComposeOutput> {
    let unprocessable = |e: String| AdminError::new(StatusCode::UNPROCESSABLE_ENTITY, e);
    let mut service_schemas = Vec::new();
    for s in services {
        let schema = if let Some(sdl) = s.sdl {
            open_runo_federation::parse_service_sdl(&s.service_name, &sdl)
                .map_err(|e| unprocessable(e.to_string()))?
        } else {
            open_runo_federation::ServiceSchema {
                service_name: s.service_name,
                types: s
                    .types
                    .into_iter()
                    .map(|(k, v)| (k, BTreeSet::from_iter(v)))
                    .collect(),
            }
        };
        service_schemas.push(schema);
    }

    let new_composed = open_runo_federation::compose(&service_schemas)
        .map_err(|e| unprocessable(e.to_string()))?;

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

    let out = ComposeOutput {
        contributing_services: new_composed.contributing_services.clone(),
        types: new_composed
            .types
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
            .collect(),
        breaking_changes: breaking,
    };
    *state
        .federation_schema
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = new_composed;
    Ok(out)
}

// ── フィーチャーフラグ ───────────────────────────────────────────────────

pub fn to_flag_response(flag: &FeatureFlag) -> FeatureFlagResponse {
    FeatureFlagResponse {
        name: flag.name.clone(),
        enabled: flag.enabled,
        rollout_percent: flag.rollout_percent,
        description: flag.description.clone(),
    }
}

fn flag_not_found(name: &str) -> AdminError {
    AdminError::new(
        StatusCode::NOT_FOUND,
        format!("feature flag not found: {name}"),
    )
}

pub async fn flag_upsert(
    state: &AppState,
    actor: &str,
    body: FeatureFlagRequest,
) -> AdminResult<FeatureFlagResponse> {
    let flag = FeatureFlag::new(body.name)
        .enabled(body.enabled)
        .rollout_percent(body.rollout_percent)
        .description(body.description);
    // ガードはawaitの前に落とす(futureをSendに保つ)。
    let result = state
        .feature_flags
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .upsert(flag);
    match result {
        Ok(saved) => {
            crate::audit::record(state, actor, "feature_flag.upsert", saved.name.clone()).await;
            Ok(to_flag_response(&saved))
        },
        Err(e) => Err(AdminError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            e.to_string(),
        )),
    }
}

pub fn flag_list(state: &AppState) -> Vec<FeatureFlagResponse> {
    state
        .feature_flags
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .list()
        .iter()
        .map(|f| to_flag_response(f))
        .collect()
}

pub fn flag_get(state: &AppState, name: &str) -> AdminResult<FeatureFlagResponse> {
    let registry = state
        .feature_flags
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry
        .get(name)
        .map(to_flag_response)
        .ok_or_else(|| flag_not_found(name))
}

pub async fn flag_delete(state: &AppState, actor: &str, name: &str) -> AdminResult<()> {
    let existed = state
        .feature_flags
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .delete(name);
    if !existed {
        return Err(flag_not_found(name));
    }
    crate::audit::record(state, actor, "feature_flag.delete", name.to_string()).await;
    Ok(())
}

pub fn flag_evaluate(
    state: &AppState,
    name: &str,
    bucket_key: &str,
) -> AdminResult<FeatureFlagEvaluationResponse> {
    let registry = state
        .feature_flags
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry
        .evaluate(name, bucket_key)
        .map(|enabled| FeatureFlagEvaluationResponse {
            name: name.to_string(),
            bucket_key: bucket_key.to_string(),
            enabled,
        })
        .ok_or_else(|| flag_not_found(name))
}

// ── 永続クエリ ───────────────────────────────────────────────────────────

fn persisted_query_store(state: &AppState) -> open_runo_persisted_queries::PersistedQueryStore {
    open_runo_persisted_queries::PersistedQueryStore::new(
        Arc::clone(&state.db),
        open_runo_persisted_queries::EnforcementMode::Allow,
    )
}

#[derive(Debug, Clone)]
pub struct PersistedQueryRecord {
    pub hash: String,
    pub query: String,
    pub registered_at: String,
}

pub async fn persisted_query_register(
    state: &AppState,
    actor: &str,
    query: &str,
) -> AdminResult<PersistedQueryRecord> {
    let record = persisted_query_store(state)
        .register(query)
        .await
        .map_err(|e| AdminError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    crate::audit::record(
        state,
        actor,
        "persisted_query.register",
        record.hash.clone(),
    )
    .await;
    Ok(PersistedQueryRecord {
        hash: record.hash,
        query: record.document,
        registered_at: record.registered_at.to_rfc3339(),
    })
}

pub async fn persisted_query_get(
    state: &AppState,
    hash: &str,
) -> AdminResult<PersistedQueryRecord> {
    match persisted_query_store(state).get(hash).await {
        Ok(Some(record)) => Ok(PersistedQueryRecord {
            hash: record.hash,
            query: record.document,
            registered_at: record.registered_at.to_rfc3339(),
        }),
        Ok(None) => Err(AdminError::new(
            StatusCode::NOT_FOUND,
            format!("persisted query not found: {hash}"),
        )),
        Err(e) => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            e.to_string(),
        )),
    }
}

// ── キャッシュ消去 ───────────────────────────────────────────────────────

pub async fn cache_purge(
    state: &AppState,
    cache: &HtmlPageCache,
    actor: &str,
    path: &str,
) -> String {
    cache.purge(path).await;
    crate::audit::record(state, actor, "cache.purge", path.to_string()).await;
    path.to_string()
}

pub async fn cache_purge_all(state: &AppState, cache: &HtmlPageCache, actor: &str) -> String {
    cache.purge_all().await;
    crate::audit::record(state, actor, "cache.purge_all", "*").await;
    "*".to_string()
}
