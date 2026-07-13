//! Shared REST API request/response types for `open-runo-router` (server),
//! `open-runo-cli`, and the WASM frontend (`apps/desktop-wasm`).
//!
//! Before this crate existed, the "schema version" shape was independently
//! re-declared three times -- once as a private struct in
//! `handlers_hyper.rs`, once in `apps/desktop-wasm/src/api.rs`, and (worse)
//! not at all in `open-runo-cli`, which used untyped `serde_json::Value`.
//! The three definitions had drifted: the register-response copy omitted
//! `sdl`, and the frontend's history copy omitted both `namespace` and
//! `sdl`. `open-runo-cli`'s untyped handling of the history endpoint's
//! `{"versions": [...]}` wrapper shape was mistaken for a bare array and
//! shipped with a real bug, caught only by manual end-to-end testing (see
//! CLAUDE.md HANDOFF, 2026-07-11). Centralizing the types here means a
//! server-side shape change is a compile error in every client instead of
//! a silent runtime mismatch.
//!
//! Pure data types only: no I/O, no async runtime. This crate must compile
//! for `wasm32-unknown-unknown` as well as native targets, since
//! `apps/desktop-wasm` (a separate Cargo workspace) depends on it too.
//!
//! Every type here also derives [`schemars::JsonSchema`], which
//! `open-runo-router::openapi` uses to generate `components.schemas` in
//! the served OpenAPI document directly from these structs -- so the
//! published API spec (and any TypeScript/JS/other-language types a
//! caller generates from it, e.g. via `openapi-typescript`) can't drift
//! from what the server actually sends, the same problem this crate was
//! created to solve for the Rust clients.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A single registered schema version, as returned by `POST /api/schemas`,
/// `GET /api/schemas/:service`, and (as `SchemaHistoryResponse::versions`)
/// `GET /api/schemas/:service/history`. All three endpoints return this
/// exact shape so a client only has to know it once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SchemaVersion {
    pub id: String,
    pub namespace: String,
    pub service_name: String,
    pub sdl: String,
    pub stage: String,
    pub created_at: String,
}

fn default_stage() -> String {
    "local".to_string()
}

/// Request body for `POST /api/schemas`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RegisterSchemaRequest {
    pub service_name: String,
    pub sdl: String,
    #[serde(default = "default_stage")]
    pub stage: String,
    #[serde(default)]
    pub namespace: Option<String>,
}

/// Response body for `GET /api/schemas/:service/history`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SchemaHistoryResponse {
    pub versions: Vec<SchemaVersion>,
}

/// Response body for `GET /api/federation/status`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FederationStatusResponse {
    pub contributing_services: Vec<String>,
    pub type_count: usize,
    pub field_count: usize,
}

/// Response body for any request rejected with `429 Too Many Requests` by
/// `open-runo-router`'s rate-limiting middleware. `retry_after_secs` also
/// appears as the standard `Retry-After` response header -- it's repeated
/// in the body so clients that only look at JSON (rather than headers)
/// still get it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RateLimitedResponse {
    pub error: String,
    pub retry_after_secs: i64,
}

/// A single key/value record, as returned within
/// `DbRecordListResponse::records` by `GET /api/db/:table`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbRecordItem {
    pub key: String,
    pub value: serde_json::Value,
}

/// Response body for `GET /api/db/:table`. Found drifted the same way
/// `SchemaVersion` was (see this crate's top-level doc): the WASM
/// frontend's independent copy of this shape omitted `table` entirely.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbRecordListResponse {
    pub table: String,
    pub count: usize,
    pub records: Vec<DbRecordItem>,
}

/// Response body for `GET /api/db/:table/:key`. Same `table`-omission
/// drift as `DbRecordListResponse` existed in the frontend's copy before
/// this crate.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbRecordResponse {
    pub table: String,
    pub key: String,
    pub value: serde_json::Value,
}

/// Response body for `GET /api/db/:table/:key/at/:commit_id` — the
/// commit-ID read-side query of the VersionLessAPI + Git-on-SQL hybrid
/// (only backed for real by `AruaruDbBackend`; see
/// `open-runo-db::DbBackend::get_at_commit`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbRecordAtCommitResponse {
    pub table: String,
    pub key: String,
    pub commit_id: String,
    pub value: serde_json::Value,
}

/// Request body for `PUT /api/db/:table/:key`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbUpsertRequest {
    pub value: serde_json::Value,
}

/// Response body for `DELETE /api/db/:table/:key`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbDeleteResponse {
    pub table: String,
    pub key: String,
    pub deleted: bool,
}

/// Response body for `GET /api/db/status`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbStatusResponse {
    pub backend: String,
    pub status: String,
}

/// One entry in `DbRoutingInfo::entries`, as returned by `GET /api/db/routing`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbRoutingEntry {
    pub table: String,
    pub target: String,
}

/// Response body for `GET /api/db/routing`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbRoutingInfo {
    pub default_target: String,
    pub entries: Vec<DbRoutingEntry>,
}

/// Request body for `POST /api/feature-flags` (create-or-update, upsert
/// semantics keyed by `name`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeatureFlagRequest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_full_rollout")]
    pub rollout_percent: u8,
    #[serde(default)]
    pub description: String,
}

fn default_true() -> bool {
    true
}

fn default_full_rollout() -> u8 {
    100
}

/// A feature flag as returned by `POST /api/feature-flags`,
/// `GET /api/feature-flags/:name`, and (as `FeatureFlagListResponse::flags`)
/// `GET /api/feature-flags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FeatureFlagResponse {
    pub name: String,
    pub enabled: bool,
    pub rollout_percent: u8,
    pub description: String,
}

/// Response body for `GET /api/feature-flags`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeatureFlagListResponse {
    pub flags: Vec<FeatureFlagResponse>,
}

/// Response body for `GET /api/feature-flags/:name/evaluate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeatureFlagEvaluationResponse {
    pub name: String,
    pub bucket_key: String,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_roundtrips_through_json() {
        let v = SchemaVersion {
            id: "abc".to_string(),
            namespace: "default".to_string(),
            service_name: "users".to_string(),
            sdl: "type User { id: ID! }".to_string(),
            stage: "local".to_string(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: SchemaVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn register_schema_request_defaults_stage_and_namespace() {
        let req: RegisterSchemaRequest =
            serde_json::from_str(r#"{"service_name": "users", "sdl": "type User { id: ID! }"}"#).unwrap();
        assert_eq!(req.stage, "local");
        assert_eq!(req.namespace, None);
    }

    #[test]
    fn schema_history_response_wraps_versions() {
        let json = r#"{"versions": []}"#;
        let resp: SchemaHistoryResponse = serde_json::from_str(json).unwrap();
        assert!(resp.versions.is_empty());
    }

    #[test]
    fn rate_limited_response_roundtrips_through_json() {
        let r = RateLimitedResponse { error: "rate limit exceeded".to_string(), retry_after_secs: 42 };
        let json = serde_json::to_string(&r).unwrap();
        let back: RateLimitedResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.retry_after_secs, 42);
    }

    #[test]
    fn db_record_list_response_includes_table() {
        // This is exactly the field the WASM frontend's independent copy
        // of this shape used to silently drop.
        let json = r#"{"table": "users", "count": 1, "records": [{"key": "1", "value": {"name": "a"}}]}"#;
        let resp: DbRecordListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.table, "users");
        assert_eq!(resp.records[0].key, "1");
    }

    #[test]
    fn db_record_response_includes_table() {
        let json = r#"{"table": "users", "key": "1", "value": "hello"}"#;
        let resp: DbRecordResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.table, "users");
        assert_eq!(resp.value, serde_json::json!("hello"));
    }

    #[test]
    fn db_routing_info_roundtrips_through_json() {
        let info = DbRoutingInfo {
            default_target: "postgresql".to_string(),
            entries: vec![DbRoutingEntry { table: "sessions".to_string(), target: "postgresql".to_string() }],
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: DbRoutingInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entries[0].table, "sessions");
    }

    #[test]
    fn feature_flag_request_defaults_enabled_and_rollout_and_description() {
        let req: FeatureFlagRequest = serde_json::from_str(r#"{"name": "new-checkout"}"#).unwrap();
        assert!(req.enabled);
        assert_eq!(req.rollout_percent, 100);
        assert_eq!(req.description, "");
    }

    #[test]
    fn feature_flag_response_roundtrips_through_json() {
        let f = FeatureFlagResponse {
            name: "new-checkout".to_string(),
            enabled: true,
            rollout_percent: 25,
            description: "canary".to_string(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: FeatureFlagResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn feature_flag_list_response_wraps_flags() {
        let json = r#"{"flags": []}"#;
        let resp: FeatureFlagListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.flags.is_empty());
    }

    #[test]
    fn feature_flag_evaluation_response_roundtrips_through_json() {
        let r = FeatureFlagEvaluationResponse {
            name: "new-checkout".to_string(),
            bucket_key: "user-42".to_string(),
            enabled: true,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(back["name"], "new-checkout");
        assert_eq!(back["bucket_key"], "user-42");
        assert_eq!(back["enabled"], true);
    }
}
