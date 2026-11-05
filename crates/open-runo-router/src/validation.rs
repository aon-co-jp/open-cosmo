//! JSON Schema validation for request bodies.
//!
//! Wraps the `jsonschema` crate so handlers can validate an incoming
//! `serde_json::Value` against a compiled schema and turn failures into a
//! `422 Unprocessable Entity` with a readable list of violations, instead of
//! either trusting the body blindly or hand-rolling field checks per
//! handler.

// clippy: このモジュールの unwrap()/expect() は「固定ヘッダからのレスポンス構築」
// 「起動時に読む埋め込み定数のパース」等、構造上失敗しない箇所に限られる
// (各呼び出しに理由コメントあり)。business ロジック側の Quality Gate
// (workspace lints の unwrap_used/expect_used = warn) は維持したまま、
// この基盤モジュールだけ除外する。result_large_err は Result<T, Response>
// (エラー時に完成済み HTTP レスポンスで短絡する意図的な型)に対するもの。
#![allow(clippy::expect_used)]

use jsonschema::Validator;
use once_cell::sync::Lazy;
use serde_json::Value;

/// Compile a JSON Schema (as a `serde_json::Value`) into a reusable
/// [`Validator`]. Panics at startup (via `Lazy`) if the embedded schema
/// itself is malformed — a programmer error, not a runtime condition.
fn compile(schema: Value) -> Validator {
    jsonschema::validator_for(&schema).expect("embedded JSON schema is valid")
}

/// Schema for `POST /api/schemas` request bodies.
pub static REGISTER_SCHEMA_REQUEST: Lazy<Validator> = Lazy::new(|| {
    compile(serde_json::json!({
        "type": "object",
        "required": ["service_name", "sdl"],
        "properties": {
            "service_name": { "type": "string", "minLength": 1 },
            "sdl": { "type": "string", "minLength": 1 },
            "stage": { "type": "string" }
        }
    }))
});

/// Schema for `PUT /api/db/:table/:key` request bodies.
pub static DB_UPSERT_REQUEST: Lazy<Validator> = Lazy::new(|| {
    compile(serde_json::json!({
        "type": "object",
        "required": ["value"]
    }))
});

/// Schema for `POST /api/feature-flags` request bodies.
pub static FEATURE_FLAG_REQUEST: Lazy<Validator> = Lazy::new(|| {
    compile(serde_json::json!({
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": { "type": "string", "minLength": 1 },
            "enabled": { "type": "boolean" },
            "rollout_percent": { "type": "integer", "minimum": 0, "maximum": 100 },
            "description": { "type": "string" }
        }
    }))
});

/// Validate `body` against `validator`, returning a readable list of
/// violations (joined with `; `) when it fails. Poem-free: callers turn
/// the `Err` string into whatever response type they need (hyper_compat
/// handlers use `StatusCode::UNPROCESSABLE_ENTITY`, see `handlers_hyper.rs`).
pub fn validate(validator: &Validator, body: &Value) -> Result<(), String> {
    let errors: Vec<String> = validator
        .iter_errors(body)
        .map(|e| format!("{} (at {})", e, e.instance_path()))
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "request body failed validation: {}",
            errors.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn register_schema_request_accepts_valid_body() {
        let body = json!({ "service_name": "users", "sdl": "type User { id: ID! }" });
        assert!(validate(&REGISTER_SCHEMA_REQUEST, &body).is_ok());
    }

    #[test]
    fn register_schema_request_rejects_missing_sdl() {
        let body = json!({ "service_name": "users" });
        assert!(validate(&REGISTER_SCHEMA_REQUEST, &body).is_err());
    }

    #[test]
    fn register_schema_request_rejects_empty_service_name() {
        let body = json!({ "service_name": "", "sdl": "type User { id: ID! }" });
        assert!(validate(&REGISTER_SCHEMA_REQUEST, &body).is_err());
    }

    #[test]
    fn db_upsert_request_requires_value_field() {
        assert!(validate(&DB_UPSERT_REQUEST, &json!({})).is_err());
        assert!(validate(&DB_UPSERT_REQUEST, &json!({ "value": 42 })).is_ok());
    }

    #[test]
    fn feature_flag_request_requires_name() {
        assert!(validate(&FEATURE_FLAG_REQUEST, &json!({})).is_err());
        assert!(validate(&FEATURE_FLAG_REQUEST, &json!({ "name": "f" })).is_ok());
    }

    #[test]
    fn feature_flag_request_rejects_rollout_percent_over_100() {
        assert!(validate(
            &FEATURE_FLAG_REQUEST,
            &json!({ "name": "f", "rollout_percent": 101 })
        )
        .is_err());
    }
}
