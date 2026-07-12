//! Shared in-memory state passed to every handler via [`poem::web::Data`].
//!
//! Each field wraps a domain struct from the relevant crate behind an
//! `Arc<Mutex<_>>` so handlers can share it safely across async tasks.
//!
//! The `db` field exposes the DUAL DATABASE backend so `/api/db/*`
//! handlers can persist and retrieve records across PostgreSQL and aruaru-db.

use crate::acme::ChallengeStore;
use crate::session::SessionStore;
use open_runo_db::{DbBackend, InMemoryBackend};
use open_runo_federation::ComposedSchema;
use open_runo_feature_flags::FeatureFlagRegistry;
use open_runo_history::History;
use open_runo_observability::{InMemorySink, RequestMetrics};
use open_runo_schema_registry::SchemaRegistry;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// A change event published on the in-process broker
/// (consumed by GraphQL Subscriptions and, later, the SSE stream).
/// `Deserialize` is needed alongside `Serialize` so the `edfs` module can
/// decode this same shape back out of a Redis Pub/Sub message payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchemaEvent {
    pub service_name: String,
    pub stage: String,
    pub at: String,
}

/// Broker capacity: slow subscribers older than this many events lag-skip.
const EVENT_CAPACITY: usize = 256;

/// Global shared state for the open-runo gateway.
#[derive(Debug, Clone)]
pub struct AppState {
    pub schema_registry: Arc<Mutex<SchemaRegistry>>,
    pub federation_schema: Arc<Mutex<ComposedSchema>>,
    pub history: Arc<Mutex<History>>,
    /// DUAL DATABASE backend: PostgreSQL + aruaru-db (or InMemory in tests).
    pub db: Arc<dyn DbBackend>,
    /// In-process event broker for realtime consumers (GraphQL Subscriptions).
    pub events: broadcast::Sender<SchemaEvent>,
    /// Feature flags: canary releases / percentage-based traffic routing
    /// (Cosmo Feature Flags parity, `docs/cosmo-parity.md` 4a). In-memory,
    /// like `schema_registry` -- flag definitions are operational config,
    /// not durable application data.
    pub feature_flags: Arc<Mutex<FeatureFlagRegistry>>,
    /// Cookie-based sessions, additive to `X-Api-Key` auth (Poem-parity
    /// gap: Cookie/session management, see `session.rs`).
    pub sessions: Arc<SessionStore>,
    /// Published HTTP-01 challenge responses, served at
    /// `GET /.well-known/acme-challenge/:token` (Poem-parity gap: ACME,
    /// see `acme.rs`). Always present regardless of the `acme` feature --
    /// serving this path is cheap and useful even when an external ACME
    /// client (not this crate's own) is the one publishing into it.
    pub acme_challenges: Arc<ChallengeStore>,
    /// EDFS (Event-Driven Federated Subscriptions, `edfs.rs`) publish
    /// target: `(redis_url, channel)`, set once at startup if
    /// `OPEN_RUNO_EDFS_REDIS_URL` is configured. Always present (as
    /// `None` by default) regardless of the `edfs` Cargo feature being
    /// enabled, so call sites that check "should I publish?" don't need
    /// their own `#[cfg(feature = "edfs")]` -- only `edfs.rs`'s actual
    /// Redis-touching code is feature-gated.
    pub edfs_publish: Arc<Mutex<Option<(String, String)>>>,
    /// Monthly request-count metering + per-operation latency/error-rate
    /// analytics (Cosmo Launch/Scale parity, `docs/cosmo-parity.md` 4a),
    /// fed from `middleware_hyper::with_metrics` and read by
    /// `handlers_hyper::{requests_per_month_handler, operations_summary_handler}`.
    /// This is an operational metric only -- never used for throttling
    /// or billing (see `open_runo_observability::request_metrics` doc
    /// comment).
    pub request_metrics: Arc<RequestMetrics>,
}

impl AppState {
    /// Build with all in-memory defaults (suitable for tests and local dev).
    pub fn new() -> Self {
        Self {
            schema_registry: Arc::new(Mutex::new(SchemaRegistry::new())),
            federation_schema: Arc::new(Mutex::new(ComposedSchema::default())),
            history: Arc::new(Mutex::new(History::new())),
            db: Arc::new(InMemoryBackend::new()),
            events: broadcast::channel(EVENT_CAPACITY).0,
            feature_flags: Arc::new(Mutex::new(FeatureFlagRegistry::new())),
            sessions: Arc::new(SessionStore::new()),
            acme_challenges: Arc::new(ChallengeStore::new()),
            edfs_publish: Arc::new(Mutex::new(None)),
            request_metrics: default_request_metrics(),
        }
    }

    /// Build with a pre-configured [`DbBackend`] (production use: `DualBackend`).
    pub fn with_db(db: Arc<dyn DbBackend>) -> Self {
        Self {
            schema_registry: Arc::new(Mutex::new(SchemaRegistry::new())),
            federation_schema: Arc::new(Mutex::new(ComposedSchema::default())),
            history: Arc::new(Mutex::new(History::new())),
            db,
            events: broadcast::channel(EVENT_CAPACITY).0,
            feature_flags: Arc::new(Mutex::new(FeatureFlagRegistry::new())),
            sessions: Arc::new(SessionStore::new()),
            acme_challenges: Arc::new(ChallengeStore::new()),
            edfs_publish: Arc::new(Mutex::new(None)),
            request_metrics: default_request_metrics(),
        }
    }

    /// Build for a single-DB deployment: wraps `backend` in
    /// [`open_runo_db::dual::DualBackend::single`] so the routing code path is
    /// identical to DUAL DATABASE deployments.
    pub fn with_single_db(backend: Arc<dyn DbBackend>) -> Self {
        Self::with_db(Arc::new(open_runo_db::dual::DualBackend::single(backend)))
    }
}

/// Build the [`RequestMetrics`] every `AppState` constructor starts with.
/// The in-process aggregates (`requests_per_month`/`operations_summary`)
/// always work regardless of ClickHouse -- only the buffered export sink
/// varies: `OPEN_RUNO_CLICKHOUSE_URL` + the `clickhouse` Cargo feature
/// together select a real `ClickHouseSink`; otherwise rows are still
/// buffered (so `buffered_len()`/`flush()` behave identically in tests)
/// but simply accumulate in an `InMemorySink` that nothing ever reads
/// back out of in production.
fn default_request_metrics() -> Arc<RequestMetrics> {
    #[cfg(feature = "clickhouse")]
    if let Ok(url) = std::env::var("OPEN_RUNO_CLICKHOUSE_URL") {
        let table = std::env::var("OPEN_RUNO_CLICKHOUSE_METRICS_TABLE")
            .unwrap_or_else(|_| "request_metrics".to_string());
        let sink = Arc::new(open_runo_observability::ClickHouseSink::new(&url, table));
        return Arc::new(RequestMetrics::new(sink));
    }
    Arc::new(RequestMetrics::new(Arc::new(InMemorySink::new())))
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
