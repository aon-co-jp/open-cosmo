//! OpenAPI 3.0 spec for the REST admin surface.
//!
//! Poem itself ships a `poem-openapi` crate that auto-generates specs from
//! typed handlers via macros; this crate doesn't depend on Poem, so this
//! is a hand-written equivalent — a static `serde_json::Value` describing
//! every REST path, matching the endpoint table in `lib.rs`'s doc comment.
//! It gives the same practical benefit (import into Postman/Insomnia,
//! generate client SDKs, browse in any Swagger UI) without macro machinery.
//! GraphQL already has interactive docs via GraphiQL (`GET /graphql`);
//! this closes the equivalent gap for the REST side.
//!
//! `components.schemas` is *generated*, not hand-written, from
//! `open-runo-api-types`'s `schemars::JsonSchema` derives (see
//! [`schema_component`]) for the endpoints that use those shared types
//! (schema registry + federation status). That closes the same drift gap
//! `open-runo-api-types` closed for the Rust clients (CLAUDE.md HANDOFF,
//! 2026-07-11) on the JS/TS side too: a caller running this spec through
//! `openapi-typescript` or similar gets real field-level types straight
//! from the server's actual Rust structs, not hand-maintained (and
//! driftable) copies. The other ~25 endpoints still have hand-written
//! `description`-only responses; see `docs/api-examples.md` for the plan
//! to extend this to the rest of the surface.

use crate::hyper_compat::{json_response, Handler};
use hyper::StatusCode;
use serde_json::{json, Map, Value};
use std::sync::Arc;

fn api_key_security() -> Value {
    json!([{ "ApiKeyAuth": [] }])
}

/// Standard error responses every `api_key_security()`-protected route can
/// return, referencing the named `components.responses` entries so the
/// 401/429 shapes are described once instead of duplicated per path.
fn standard_error_responses() -> Value {
    json!({
        "401": { "$ref": "#/components/responses/Unauthorized" },
        "429": { "$ref": "#/components/responses/RateLimited" }
    })
}

/// Rewrite `schemars`' `#/$defs/Name` refs (JSON Schema 2020-12 style,
/// used for nested types like `SchemaHistoryResponse`'s `Vec<SchemaVersion>`)
/// to OpenAPI's `#/components/schemas/Name` convention.
fn rewrite_refs_to_components(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("$ref") {
                if let Some(rest) = s.strip_prefix("#/$defs/") {
                    let rewritten = format!("#/components/schemas/{rest}");
                    map.insert("$ref".to_string(), Value::String(rewritten));
                }
            }
            for v in map.values_mut() {
                rewrite_refs_to_components(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs_to_components),
        _ => {}
    }
}

/// Generate an OpenAPI `components.schemas` entry (name + schema) from a
/// Rust type's `schemars::JsonSchema` impl, plus any nested type
/// definitions it pulled in (schemars' `$defs`) as additional entries to
/// merge in alongside it. `$schema` (a JSON-Schema-document-level field,
/// meaningless on an individual OpenAPI Schema Object) is stripped.
fn schema_component<T: schemars::JsonSchema>() -> Vec<(String, Value)> {
    let schema = schemars::SchemaGenerator::default().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(&schema).expect("schemars output serializes to JSON");
    rewrite_refs_to_components(&mut value);

    let obj = value.as_object_mut().expect("root schema is a JSON object");
    obj.remove("$schema");
    let name = obj
        .remove("title")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| std::any::type_name::<T>().to_string());
    let defs = match obj.remove("$defs") {
        Some(Value::Object(defs)) => defs,
        _ => Map::new(),
    };

    let mut entries = vec![(name, value)];
    entries.extend(defs);
    entries
}

fn components_schemas() -> Value {
    let mut schemas = Map::new();
    for entries in [
        schema_component::<open_runo_api_types::SchemaVersion>(),
        schema_component::<open_runo_api_types::RegisterSchemaRequest>(),
        schema_component::<open_runo_api_types::SchemaHistoryResponse>(),
        schema_component::<open_runo_api_types::FederationStatusResponse>(),
        schema_component::<open_runo_api_types::RateLimitedResponse>(),
        schema_component::<open_runo_api_types::DbRecordItem>(),
        schema_component::<open_runo_api_types::DbRecordListResponse>(),
        schema_component::<open_runo_api_types::DbRecordResponse>(),
        schema_component::<open_runo_api_types::DbUpsertRequest>(),
        schema_component::<open_runo_api_types::DbDeleteResponse>(),
        schema_component::<open_runo_api_types::DbStatusResponse>(),
        schema_component::<open_runo_api_types::DbRoutingEntry>(),
        schema_component::<open_runo_api_types::DbRoutingInfo>(),
        schema_component::<open_runo_api_types::FeatureFlagRequest>(),
        schema_component::<open_runo_api_types::FeatureFlagResponse>(),
        schema_component::<open_runo_api_types::FeatureFlagListResponse>(),
        schema_component::<open_runo_api_types::FeatureFlagEvaluationResponse>(),
    ] {
        for (name, schema) in entries {
            // `SchemaVersion` is pulled in as a nested `$defs` entry by
            // `SchemaHistoryResponse` too; first writer wins, they're
            // identical either way.
            schemas.entry(name).or_insert(schema);
        }
    }
    Value::Object(schemas)
}

/// A `200`-style response body typed via `$ref` to a `components.schemas`
/// entry generated by [`schema_component`].
fn typed_response(description: &str, schema_ref: &str) -> Value {
    json!({
        "description": description,
        "content": { "application/json": { "schema": { "$ref": format!("#/components/schemas/{schema_ref}") } } }
    })
}

/// A `requestBody` typed via `$ref` to a `components.schemas` entry.
fn typed_request_body(schema_ref: &str) -> Value {
    json!({
        "required": true,
        "content": { "application/json": { "schema": { "$ref": format!("#/components/schemas/{schema_ref}") } } }
    })
}

/// Merge `extra` responses into `standard_error_responses()`, `extra`
/// taking priority on key collisions (there shouldn't be any in practice
/// -- callers use this for endpoint-specific 2xx/4xx codes alongside the
/// shared 401/429).
fn with_standard_errors(extra: Value) -> Value {
    let mut responses = match standard_error_responses() {
        Value::Object(map) => map,
        _ => unreachable!("standard_error_responses always returns an object"),
    };
    if let Value::Object(extra_map) = extra {
        for (k, v) in extra_map {
            responses.insert(k, v);
        }
    }
    Value::Object(responses)
}

/// `pub(crate)` (not just `fn`) so `mcp.rs` can expose this same spec as an
/// MCP resource without going through the `GET /api/openapi.json` HTTP
/// handler -- one source of truth for the spec body, two ways to reach it.
pub(crate) fn spec() -> Value {
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "open-runo REST API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST admin surface for open-runo-router. GraphQL Federation is served separately at POST /graphql (GraphiQL at GET /graphql)."
        },
        "components": {
            "securitySchemes": {
                "ApiKeyAuth": {
                    "type": "apiKey",
                    "in": "header",
                    "name": "X-Api-Key"
                }
            },
            "responses": {
                "Unauthorized": {
                    "description": "Missing or invalid X-Api-Key"
                },
                "RateLimited": {
                    "description": "Rate limit exceeded; see Retry-After header and retry_after_secs in the body",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/RateLimitedResponse" }
                        }
                    }
                }
            },
            "schemas": components_schemas()
        },
        "paths": {
            "/health": {
                "get": { "summary": "Service health check", "security": [], "responses": { "200": { "description": "OK" } } }
            },
            "/healthz": {
                "get": { "summary": "Kubernetes-style health alias", "security": [], "responses": { "200": { "description": "OK" } } }
            },
            "/api/schemas": {
                "post": {
                    "summary": "Register a schema version",
                    "security": api_key_security(),
                    "requestBody": typed_request_body("RegisterSchemaRequest"),
                    "responses": with_standard_errors(json!({
                        "200": typed_response("Registered", "SchemaVersion"),
                        "422": { "description": "Validation failed" }
                    }))
                }
            },
            "/api/schemas/{service}": {
                "get": {
                    "summary": "Latest schema for a service",
                    "security": api_key_security(),
                    "parameters": [{ "name": "service", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": with_standard_errors(json!({
                        "200": typed_response("OK", "SchemaVersion"),
                        "404": { "description": "Not found" }
                    }))
                }
            },
            "/api/schemas/{service}/history": {
                "get": {
                    "summary": "Full schema version history",
                    "security": api_key_security(),
                    "parameters": [{ "name": "service", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": with_standard_errors(json!({ "200": typed_response("OK", "SchemaHistoryResponse") }))
                }
            },
            "/api/federation/compose": {
                "post": { "summary": "Compose service schemas into a federated graph", "security": api_key_security(), "responses": { "200": { "description": "Composed" } } }
            },
            "/api/federation/status": {
                "get": {
                    "summary": "Current composed schema summary",
                    "security": api_key_security(),
                    "responses": with_standard_errors(json!({ "200": typed_response("OK", "FederationStatusResponse") }))
                }
            },
            "/api/ai/route": {
                "post": { "summary": "Select the best AI provider for a request", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/db/status": {
                "get": { "summary": "DUAL DATABASE backend name & health", "security": api_key_security(), "responses": with_standard_errors(json!({ "200": typed_response("OK", "DbStatusResponse") })) }
            },
            "/api/db/routing": {
                "get": { "summary": "Per-table routing decisions", "security": api_key_security(), "responses": with_standard_errors(json!({ "200": typed_response("OK", "DbRoutingInfo") })) }
            },
            "/api/db/{table}": {
                "get": { "summary": "List all records in a table", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": with_standard_errors(json!({ "200": typed_response("OK", "DbRecordListResponse") })) }
            },
            "/api/db/{table}/{key}": {
                "get": { "summary": "Get one record", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "key", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": with_standard_errors(json!({ "200": typed_response("OK", "DbRecordResponse"), "404": { "description": "Not found" } })) },
                "put": { "summary": "Upsert a record", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "key", "in": "path", "required": true, "schema": { "type": "string" } }], "requestBody": typed_request_body("DbUpsertRequest"), "responses": with_standard_errors(json!({ "200": typed_response("Saved", "DbRecordResponse") })) },
                "delete": { "summary": "Delete a record", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "key", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": with_standard_errors(json!({ "200": typed_response("Deleted", "DbDeleteResponse") })) }
            },
            "/api/cache/purge": {
                "post": { "summary": "Purge one HTML page from the cache", "security": api_key_security(), "responses": { "200": { "description": "Purged" } } }
            },
            "/api/cache/purge-all": {
                "post": { "summary": "Purge the entire HTML page cache", "security": api_key_security(), "responses": { "200": { "description": "Purged" } } }
            },
            "/api/cache/ai-stats": {
                "get": { "summary": "Self-learning cache predictor stats", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/backup/export": {
                "post": { "summary": "Export a portable backup", "security": api_key_security(), "responses": { "200": { "description": "Exported" } } }
            },
            "/api/backup/import": {
                "post": { "summary": "Import a portable backup", "security": api_key_security(), "responses": { "200": { "description": "Imported" } } }
            },
            "/api/backup/restore-latest": {
                "post": { "summary": "Restore from the newest available backup", "security": api_key_security(), "responses": { "200": { "description": "Restored" }, "404": { "description": "No backup found" } } }
            },
            "/api/migrate/export-sql": {
                "post": { "summary": "SQL dump for engine migration (postgres/mysql/generic)", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/migrate/export-csv": {
                "post": { "summary": "CSV export for BI/Snowflake ingestion", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/integrity/check": {
                "post": { "summary": "Two-database reconciliation, self-healing", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/persisted-queries": {
                "post": { "summary": "Register a GraphQL document (Trusted Documents)", "security": api_key_security(), "responses": { "200": { "description": "Registered" } } }
            },
            "/api/persisted-queries/{hash}": {
                "get": { "summary": "Fetch a registered document by hash", "security": api_key_security(), "parameters": [{ "name": "hash", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" }, "404": { "description": "Not found" } } }
            },
            "/api/feature-flags": {
                "post": {
                    "summary": "Create or update a feature flag (upsert, keyed by name)",
                    "security": api_key_security(),
                    "requestBody": typed_request_body("FeatureFlagRequest"),
                    "responses": with_standard_errors(json!({ "200": typed_response("Upserted", "FeatureFlagResponse") }))
                },
                "get": {
                    "summary": "List all feature flags",
                    "security": api_key_security(),
                    "responses": with_standard_errors(json!({ "200": typed_response("OK", "FeatureFlagListResponse") }))
                }
            },
            "/api/feature-flags/{name}": {
                "get": {
                    "summary": "Fetch a feature flag",
                    "security": api_key_security(),
                    "parameters": [{ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": with_standard_errors(json!({ "200": typed_response("OK", "FeatureFlagResponse"), "404": { "description": "Not found" } }))
                },
                "delete": {
                    "summary": "Delete a feature flag",
                    "security": api_key_security(),
                    "parameters": [{ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": with_standard_errors(json!({ "200": { "description": "Deleted" }, "404": { "description": "Not found" } }))
                }
            },
            "/api/feature-flags/{name}/evaluate": {
                "get": {
                    "summary": "Evaluate a feature flag for a given bucket key (deterministic 0-100 rollout bucketing)",
                    "security": api_key_security(),
                    "parameters": [
                        { "name": "name", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "bucket_key", "in": "query", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": with_standard_errors(json!({ "200": typed_response("OK", "FeatureFlagEvaluationResponse"), "404": { "description": "Not found" } }))
                }
            },
            "/scim/v2/Users": {
                "get": { "summary": "List SCIM users (RFC 7644)", "security": api_key_security(), "responses": { "200": { "description": "OK" } } },
                "post": { "summary": "Create a SCIM user (auto-issues an API key)", "security": api_key_security(), "responses": { "201": { "description": "Created" }, "409": { "description": "userName already exists" } } }
            },
            "/scim/v2/Users/{id}": {
                "get": { "summary": "Fetch a SCIM user", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "put": { "summary": "Replace a SCIM user (deactivation auto-revokes keys)", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "delete": { "summary": "Delete a SCIM user (auto-revokes keys)", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "204": { "description": "Deleted" } } }
            },
            "/scim/v2/Groups": {
                "get": { "summary": "List SCIM groups", "security": api_key_security(), "responses": { "200": { "description": "OK" } } },
                "post": { "summary": "Create a SCIM group", "security": api_key_security(), "responses": { "201": { "description": "Created" } } }
            },
            "/scim/v2/Groups/{id}": {
                "get": { "summary": "Fetch a SCIM group", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "put": { "summary": "Replace a SCIM group's membership", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "delete": { "summary": "Delete a SCIM group", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "204": { "description": "Deleted" } } }
            },
            "/api/events": {
                "get": { "summary": "Server-Sent Events stream of schema/federation changes", "security": api_key_security(), "responses": { "200": { "description": "text/event-stream" } } }
            }
        }
    })
}

/// `GET /api/openapi.json` — serves the static OpenAPI 3.0 document above.
/// No auth required (matches `/health`'s exemption — the spec itself
/// contains no data, just endpoint shapes).
pub fn openapi_handler() -> Handler {
    Arc::new(move |_req, _params| Box::pin(async move { json_response(StatusCode::OK, &spec()) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{serve, Router};
    use hyper::Method;

    #[tokio::test]
    async fn openapi_spec_is_valid_json_and_lists_known_paths() {
        let router = Router::new().route(Method::GET, "/api/openapi.json", openapi_handler());
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/openapi.json"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.expect("valid json body");
        assert_eq!(body["openapi"], "3.0.3");
        assert!(body["paths"]["/health"].is_object());
        assert!(body["paths"]["/api/schemas"]["post"].is_object());
        assert!(body["paths"]["/scim/v2/Users/{id}"]["delete"].is_object());
        assert_eq!(
            body["components"]["securitySchemes"]["ApiKeyAuth"]["name"],
            "X-Api-Key"
        );

        // components.schemas is generated from open-runo-api-types, not
        // hand-written -- assert the shapes came through correctly,
        // including the nested-type $ref rewrite ($defs -> components/schemas).
        let schemas = &body["components"]["schemas"];
        assert_eq!(
            schemas["SchemaVersion"]["properties"]["sdl"]["type"], "string",
            "SchemaVersion should have every field of the real Rust struct, sdl included \
             (this is exactly the field that was missing from the register response before \
             open-runo-api-types existed)"
        );
        assert_eq!(
            schemas["SchemaHistoryResponse"]["properties"]["versions"]["items"]["$ref"],
            "#/components/schemas/SchemaVersion",
            "nested-type refs must point at components/schemas, not schemars' own $defs"
        );
        assert!(schemas["RegisterSchemaRequest"]["properties"]["service_name"].is_object());
        assert!(schemas["FederationStatusResponse"]["properties"]["type_count"].is_object());
        assert!(schemas["RateLimitedResponse"]["properties"]["retry_after_secs"].is_object());
        assert!(
            schemas["SchemaVersion"].get("$schema").is_none(),
            "the JSON-Schema-document-level $schema field shouldn't leak into an OpenAPI Schema Object"
        );

        // The 4 endpoints wired to typed schemas reference them by $ref,
        // and all four (plus every other protected endpoint) share the
        // same named 401/429 response components.
        assert_eq!(
            body["paths"]["/api/schemas"]["post"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/RegisterSchemaRequest"
        );
        assert_eq!(
            body["paths"]["/api/schemas"]["post"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SchemaVersion"
        );
        assert_eq!(
            body["paths"]["/api/schemas"]["post"]["responses"]["429"]["$ref"],
            "#/components/responses/RateLimited"
        );
        assert_eq!(
            body["components"]["responses"]["RateLimited"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/RateLimitedResponse"
        );
    }

    /// `docs/api-examples.md`'s "Coverage note" flagged DB CRUD and
    /// feature flags as still `description`-only (2026-07-11); this test
    /// pins the fix so it can't silently regress back to untyped.
    #[tokio::test]
    async fn db_and_feature_flag_endpoints_are_typed_and_feature_flags_are_documented() {
        let router = Router::new().route(Method::GET, "/api/openapi.json", openapi_handler());
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let body: Value = reqwest::Client::new()
            .get(format!("http://{addr}/api/openapi.json"))
            .send()
            .await
            .expect("request should succeed")
            .json()
            .await
            .expect("valid json body");

        let schemas = &body["components"]["schemas"];
        for name in [
            "DbRecordItem",
            "DbRecordListResponse",
            "DbRecordResponse",
            "DbUpsertRequest",
            "DbDeleteResponse",
            "DbStatusResponse",
            "DbRoutingEntry",
            "DbRoutingInfo",
            "FeatureFlagRequest",
            "FeatureFlagResponse",
            "FeatureFlagListResponse",
            "FeatureFlagEvaluationResponse",
        ] {
            assert!(schemas[name].is_object(), "components.schemas.{name} should exist");
        }

        assert_eq!(
            body["paths"]["/api/db/{table}"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/DbRecordListResponse"
        );
        assert_eq!(
            body["paths"]["/api/db/{table}/{key}"]["put"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/DbUpsertRequest"
        );
        assert_eq!(
            body["paths"]["/api/db/{table}/{key}"]["delete"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/DbDeleteResponse"
        );

        // Feature flags were entirely absent from the OpenAPI paths before
        // this fix, despite being a real, wired REST feature
        // (`crates/open-runo-router/src/lib.rs` routes) with types already
        // sitting unused in `open-runo-api-types`.
        assert!(body["paths"]["/api/feature-flags"]["post"].is_object());
        assert!(body["paths"]["/api/feature-flags"]["get"].is_object());
        assert!(body["paths"]["/api/feature-flags/{name}"]["get"].is_object());
        assert!(body["paths"]["/api/feature-flags/{name}"]["delete"].is_object());
        assert!(body["paths"]["/api/feature-flags/{name}/evaluate"]["get"].is_object());
        assert_eq!(
            body["paths"]["/api/feature-flags"]["post"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/FeatureFlagRequest"
        );
        assert_eq!(
            body["paths"]["/api/feature-flags/{name}/evaluate"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/FeatureFlagEvaluationResponse"
        );
    }
}
