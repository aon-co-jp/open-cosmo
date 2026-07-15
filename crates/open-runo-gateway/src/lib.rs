//! `open-runo-gateway`: exposes open-runo's Schema Registry and Federation
//! Engine through a single `POST /graphql` endpoint (plus a `GET /graphql`
//! GraphiQL playground in non-production environments), so downstream
//! clients get *one* versionless interface instead of hand-written REST
//! routes per resource — the "REST API を不要にする" goal from the
//! project's mission statement.
//!
//! Phase 1 scope: read-only queries over [`open_runo_schema_registry`] and
//! [`open_runo_federation`] state already exposed via
//! [`open_runo_router::state::AppState`]. Mutations (schema registration,
//! federation composition) remain on the REST surface for now; promoting
//! them to GraphQL mutations is tracked as follow-up work once query
//! planning/execution (README Phase 3) lands.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod graphql_hyper;
pub mod ssr;

use async_graphql::{Context, Object, Schema, SimpleObject, Subscription};
use async_graphql_poem::{GraphQLRequest, GraphQLResponse};
use open_runo_cache::{Cache, InMemoryTtlCache};
use open_runo_persisted_queries::{EnforcementMode, PersistedQueryStore};
use open_runo_router::state::AppState;
pub use open_runo_router::state::SchemaEvent as AppStateSchemaEvent;
use open_runo_schema_registry::Stage;
use poem::{get, handler, web::Data, Endpoint, EndpointExt, IntoResponse, Route};
use std::sync::Arc;

pub type OpenRunoSchema = Schema<QueryRoot, async_graphql::EmptyMutation, SubscriptionRoot>;

/// GraphQL projection of [`open_runo_schema_registry::SchemaVersion`].
#[derive(SimpleObject)]
struct SchemaVersionGql {
    id: String,
    service_name: String,
    sdl: String,
    stage: String,
    created_at: String,
}

/// GraphQL projection of [`open_runo_federation::ComposedSchema`].
#[derive(SimpleObject)]
struct FederationStatusGql {
    contributing_services: Vec<String>,
    type_names: Vec<String>,
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Local => "local",
        Stage::Development => "development",
        Stage::Staging => "staging",
        Stage::Production => "production",
    }
}

fn parse_stage(s: &str) -> Stage {
    match s.to_lowercase().as_str() {
        "development" | "dev" => Stage::Development,
        "staging" | "stg" => Stage::Staging,
        "production" | "prod" => Stage::Production,
        _ => Stage::Local,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Simple liveness field so clients can smoke-test the `/graphql`
    /// endpoint without touching domain data.
    async fn health(&self) -> &'static str {
        "ok"
    }

    /// Latest schema for `service_name` at `stage` (defaults to `local`).
    async fn schema(
        &self,
        ctx: &Context<'_>,
        service_name: String,
        stage: Option<String>,
    ) -> Option<SchemaVersionGql> {
        let state = ctx.data_unchecked::<Arc<AppState>>();
        let stage = parse_stage(stage.as_deref().unwrap_or("local"));
        let registry = state
            .schema_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        registry.latest(&service_name, stage).map(|v| SchemaVersionGql {
            id: v.id.to_string(),
            service_name: v.service_name.clone(),
            sdl: v.sdl.clone(),
            stage: stage_name(v.stage).to_string(),
            created_at: v.created_at.to_rfc3339(),
        })
    }

    /// Full version history for `service_name`, oldest first.
    async fn schema_history(&self, ctx: &Context<'_>, service_name: String) -> Vec<SchemaVersionGql> {
        let state = ctx.data_unchecked::<Arc<AppState>>();
        let registry = state
            .schema_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        registry
            .history(&service_name)
            .iter()
            .map(|v| SchemaVersionGql {
                id: v.id.to_string(),
                service_name: v.service_name.clone(),
                sdl: v.sdl.clone(),
                stage: stage_name(v.stage).to_string(),
                created_at: v.created_at.to_rfc3339(),
            })
            .collect()
    }

    /// The current composed (federated) schema summary.
    async fn federation_status(&self, ctx: &Context<'_>) -> FederationStatusGql {
        let state = ctx.data_unchecked::<Arc<AppState>>();
        let composed = state
            .federation_schema
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        FederationStatusGql {
            contributing_services: composed.contributing_services.clone(),
            type_names: composed.types.keys().cloned().collect(),
        }
    }
}

/// GraphQL projection of a realtime [`open_runo_router::state::SchemaEvent`].
#[derive(SimpleObject)]
struct SchemaEventGql {
    service_name: String,
    stage: String,
    at: String,
}

/// Subscription root: realtime change feed over the router's event broker.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubscriptionRoot;

#[Subscription]
impl SubscriptionRoot {
    /// Emits an event every time a schema version is registered.
    /// Wire format matches Cosmo's schema-change notifications conceptually.
    async fn schema_events(
        &self,
        ctx: &Context<'_>,
    ) -> impl futures::Stream<Item = SchemaEventGql> {
        let state = ctx.data_unchecked::<Arc<AppState>>();
        let rx = state.events.subscribe();

        futures::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        return Some((
                            SchemaEventGql {
                                service_name: ev.service_name,
                                stage: ev.stage,
                                at: ev.at,
                            },
                            rx,
                        ));
                    }
                    // Slow consumer: skip the gap, keep streaming.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
    }
}

/// Build the [`OpenRunoSchema`], injecting `state` as query context data.
pub fn build_schema(state: Arc<AppState>) -> OpenRunoSchema {
    Schema::build(QueryRoot, async_graphql::EmptyMutation, SubscriptionRoot)
        .data(state)
        .finish()
}

/// Response-cache configuration for the `/graphql` endpoint
/// (Cosmo paid-tier cache-control parity, operation-level).
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl: chrono::Duration,
}

impl CacheConfig {
    /// `OPEN_RUNO_CACHE=on` enables the cache;
    /// `OPEN_RUNO_CACHE_TTL_SECS` sets the TTL (default 30 s).
    pub fn from_env() -> Self {
        let enabled = matches!(
            std::env::var("OPEN_RUNO_CACHE").as_deref(),
            Ok("on") | Ok("true") | Ok("1")
        );
        let ttl_secs = std::env::var("OPEN_RUNO_CACHE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(30);
        Self { enabled, ttl: chrono::Duration::seconds(ttl_secs.max(1)) }
    }

    pub fn disabled() -> Self {
        Self { enabled: false, ttl: chrono::Duration::seconds(30) }
    }

    pub fn enabled_with_ttl_secs(secs: i64) -> Self {
        Self { enabled: true, ttl: chrono::Duration::seconds(secs.max(1)) }
    }
}

/// Cache key: SHA-256 over document + variables + operation name.
/// Only plain queries are cached (mutations/subscriptions never are).
fn cache_key(request: &async_graphql::Request) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(request.query.as_bytes());
    hasher.update(b"\x00");
    hasher.update(
        serde_json::to_string(&request.variables)
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.update(b"\x00");
    hasher.update(request.operation_name.as_deref().unwrap_or("").as_bytes());
    hex::encode(hasher.finalize())
}

/// Heuristic used before parsing: mutations and subscriptions must never be
/// served from (or written to) the response cache.
fn is_cacheable_query(document: &str) -> bool {
    let trimmed = document.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with("query")
}

/// `OPEN_RUNO_PQ_MODE` → [`EnforcementMode`]:
/// `disabled` / `allow` (default) / `enforce` (Trusted Documents).
pub fn pq_mode_from_env() -> EnforcementMode {
    match std::env::var("OPEN_RUNO_PQ_MODE")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "disabled" => EnforcementMode::Disabled,
        "enforce" => EnforcementMode::Enforce,
        _ => EnforcementMode::Allow,
    }
}

/// Pull `extensions.persistedQuery.sha256Hash` out of a GraphQL request
/// (the wire format Apollo/Cosmo clients use for persisted queries).
fn persisted_query_hash(request: &async_graphql::Request) -> Option<String> {
    match request.extensions.get("persistedQuery")? {
        async_graphql::Value::Object(map) => match map.get("sha256Hash")? {
            async_graphql::Value::String(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

#[handler]
async fn graphql_handler(
    schema: Data<&OpenRunoSchema>,
    store: Data<&Arc<PersistedQueryStore>>,
    cache: Data<&Arc<GatewayCache>>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let mut request = req.0;

    let hash = persisted_query_hash(&request);
    let raw = if request.query.trim().is_empty() {
        None
    } else {
        Some(request.query.clone())
    };

    // Resolve through the persisted-query store whenever a hash is present,
    // and always under Enforce (Trusted Documents: raw queries are refused).
    if hash.is_some() || store.mode() == EnforcementMode::Enforce {
        match store.resolve(hash.as_deref(), raw.as_deref()).await {
            Ok(document) => request.query = document,
            Err(e) => {
                return async_graphql::Response::from_errors(vec![
                    async_graphql::ServerError::new(e.to_string(), None),
                ])
                .into();
            }
        }
    }

    // Response cache: queries only, keyed by document+variables+operation.
    let use_cache = cache.config.enabled && is_cacheable_query(&request.query);
    let key = if use_cache { Some(cache_key(&request)) } else { None };

    if let Some(key) = &key {
        if let Ok(Some(cached)) = cache.store.get(key).await {
            if let Ok(response) = serde_json::from_str::<async_graphql::Response>(&cached) {
                return response.into();
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

    response.into()
}

/// The gateway's response cache: backend + config, injected via poem `Data`.
#[derive(Debug)]
pub struct GatewayCache {
    pub store: Arc<dyn Cache>,
    pub config: CacheConfig,
}

#[handler]
fn graphiql() -> impl IntoResponse {
    poem::web::Html(
        async_graphql::http::GraphiQLSource::build()
            .endpoint("/graphql")
            .finish(),
    )
}

/// Build the GraphQL endpoint (POST for queries, GET for the GraphiQL
/// playground), pre-wired with `state`. Mount this at `/graphql` alongside
/// [`open_runo_router::build_app`]'s routes in the binary entrypoint, e.g.
/// `Route::new().nest("/", rest_app).nest("/graphql", graphql_route(state))`.
pub fn graphql_route(state: Arc<AppState>) -> impl Endpoint {
    graphql_route_with(state, pq_mode_from_env(), CacheConfig::from_env())
}

/// Like [`graphql_route`] but with an explicit [`EnforcementMode`]
/// (used by tests and embedders that don't want env-based config).
pub fn graphql_route_with_mode(state: Arc<AppState>, mode: EnforcementMode) -> impl Endpoint {
    graphql_route_with(state, mode, CacheConfig::disabled())
}

/// Fully explicit variant: persisted-query mode + response-cache config.
pub fn graphql_route_with(
    state: Arc<AppState>,
    mode: EnforcementMode,
    cache_config: CacheConfig,
) -> impl Endpoint {
    let schema = build_schema(Arc::clone(&state));
    let store = Arc::new(PersistedQueryStore::new(Arc::clone(&state.db), mode));
    let cache = Arc::new(GatewayCache {
        store: Arc::new(InMemoryTtlCache::new()),
        config: cache_config,
    });
    Route::new()
        .at("/", get(graphiql).post(graphql_handler))
        // GraphQL Subscriptions over WebSocket (graphql-ws protocol).
        .at("/ws", get(async_graphql_poem::GraphQLSubscription::new(schema.clone())))
        .data(schema)
        .data(store)
        .data(cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_schema_registry::Stage;
    use poem::test::TestClient;
    use serde_json::json;

    #[tokio::test]
    async fn health_field_resolves() {
        let state = Arc::new(AppState::new());
        let client = TestClient::new(graphql_route(state));

        let resp = client
            .post("/")
            .body_json(&json!({ "query": "{ health }" }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["health"], "ok");
    }

    #[tokio::test]
    async fn schema_query_returns_registered_version() {
        let state = Arc::new(AppState::new());
        state
            .schema_registry
            .lock()
            .unwrap()
            .register("users", "type User { id: ID! }", Stage::Local);

        let client = TestClient::new(graphql_route(state));
        let resp = client
            .post("/")
            .body_json(&json!({
                "query": "{ schema(serviceName: \"users\") { serviceName sdl stage } }"
            }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["schema"]["serviceName"], "users");
        assert_eq!(body["data"]["schema"]["stage"], "local");
    }

    #[tokio::test]
    async fn enforce_mode_rejects_raw_query() {
        let state = Arc::new(AppState::new());
        let client = TestClient::new(graphql_route_with_mode(
            state,
            open_runo_persisted_queries::EnforcementMode::Enforce,
        ));

        let resp = client
            .post("/")
            .body_json(&json!({ "query": "{ health }" }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert!(body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("trusted-documents"));
    }

    #[tokio::test]
    async fn enforce_mode_executes_registered_hash() {
        let state = Arc::new(AppState::new());
        // Register through the same underlying DB the route will use.
        let store = open_runo_persisted_queries::PersistedQueryStore::new(
            Arc::clone(&state.db),
            open_runo_persisted_queries::EnforcementMode::Enforce,
        );
        let rec = store.register("{ health }").await.unwrap();

        let client = TestClient::new(graphql_route_with_mode(
            state,
            open_runo_persisted_queries::EnforcementMode::Enforce,
        ));
        let resp = client
            .post("/")
            .body_json(&json!({
                "extensions": { "persistedQuery": { "version": 1, "sha256Hash": rec.hash } }
            }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["health"], "ok");
    }

    #[tokio::test]
    async fn allow_mode_apq_roundtrip() {
        let state = Arc::new(AppState::new());
        let client = TestClient::new(graphql_route_with_mode(
            state,
            open_runo_persisted_queries::EnforcementMode::Allow,
        ));
        let hash = open_runo_persisted_queries::hash_document("{ health }");

        // First request: query + hash → executes and auto-registers.
        let resp = client
            .post("/")
            .body_json(&json!({
                "query": "{ health }",
                "extensions": { "persistedQuery": { "version": 1, "sha256Hash": hash } }
            }))
            .send()
            .await;
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["health"], "ok");

        // Second request: hash only → served from the registry.
        let resp = client
            .post("/")
            .body_json(&json!({
                "extensions": { "persistedQuery": { "version": 1, "sha256Hash": hash } }
            }))
            .send()
            .await;
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["health"], "ok");
    }

    #[tokio::test]
    async fn response_cache_serves_stale_until_ttl() {
        let state = Arc::new(AppState::new());
        let client = TestClient::new(graphql_route_with(
            Arc::clone(&state),
            pq_mode_for_tests(),
            CacheConfig::enabled_with_ttl_secs(60),
        ));

        let q = json!({ "query": "{ schemaHistory(serviceName: \"users\") { serviceName } }" });

        // 1st call: empty history, cached.
        let resp = client.post("/").body_json(&q).send().await;
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["schemaHistory"].as_array().unwrap().len(), 0);

        // Register a schema AFTER the first call.
        state
            .schema_registry
            .lock()
            .unwrap()
            .register("users", "type User { id: ID! }", Stage::Local);

        // 2nd call within TTL: served from cache → still empty (stale by design).
        let resp = client.post("/").body_json(&q).send().await;
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["schemaHistory"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn disabled_cache_always_serves_fresh_data() {
        let state = Arc::new(AppState::new());
        let client = TestClient::new(graphql_route_with(
            Arc::clone(&state),
            pq_mode_for_tests(),
            CacheConfig::disabled(),
        ));

        let q = json!({ "query": "{ schemaHistory(serviceName: \"users\") { serviceName } }" });
        client.post("/").body_json(&q).send().await.assert_status_is_ok();

        state
            .schema_registry
            .lock()
            .unwrap()
            .register("users", "type User { id: ID! }", Stage::Local);

        let resp = client.post("/").body_json(&q).send().await;
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["data"]["schemaHistory"].as_array().unwrap().len(), 1);
    }

    fn pq_mode_for_tests() -> open_runo_persisted_queries::EnforcementMode {
        open_runo_persisted_queries::EnforcementMode::Allow
    }

    #[test]
    fn cacheable_query_heuristic() {
        assert!(is_cacheable_query("{ health }"));
        assert!(is_cacheable_query("query Q { health }"));
        assert!(!is_cacheable_query("mutation M { register }"));
        assert!(!is_cacheable_query("subscription S { events }"));
    }

    #[tokio::test]
    async fn subscription_streams_schema_events() {
        use futures::StreamExt;

        let state = Arc::new(AppState::new());
        let schema = build_schema(Arc::clone(&state));

        let mut stream = schema.execute_stream(async_graphql::Request::new(
            "subscription { schemaEvents { serviceName stage } }",
        ));

        // Publish an event once the subscriber is polling.
        let publisher = Arc::clone(&state);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = publisher.events.send(crate::AppStateSchemaEvent {
                service_name: "users".into(),
                stage: "local".into(),
                at: "2026-07-03T00:00:00Z".into(),
            });
        });

        let response = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("subscription timed out")
            .expect("stream ended unexpectedly");

        let data = response.data.into_json().unwrap();
        assert_eq!(data["schemaEvents"]["serviceName"], "users");
        assert_eq!(data["schemaEvents"]["stage"], "local");
    }
}

