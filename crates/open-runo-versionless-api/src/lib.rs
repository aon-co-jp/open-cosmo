//! `open-runo-versionless-api`: applies field-level compatibility rules
//! (rename, default-fill, deprecation) so clients on different capability
//! levels can share one evolving schema instead of `/v1`, `/v2`, ... .

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use serde_json::Value;

/// A single backward-compatibility rule applied when transforming a
/// response for an older client.
#[derive(Debug, Clone)]
pub enum CompatibilityRule {
    /// The field was renamed; old clients still expect the old name.
    RenamedField { old_name: String, new_name: String },
    /// The field was removed; old clients get a default value instead.
    RemovedFieldDefault { field: String, default: Value },
    /// The field is deprecated but still present; no transform needed,
    /// this variant only exists so it shows up in compatibility reports.
    Deprecated { field: String, since: String },
}

/// Applies a set of compatibility rules to a JSON response object,
/// producing the shape an older client capability level expects.
pub fn apply_compatibility(mut payload: Value, rules: &[CompatibilityRule]) -> Value {
    if let Value::Object(map) = &mut payload {
        for rule in rules {
            match rule {
                CompatibilityRule::RenamedField { old_name, new_name } => {
                    if let Some(v) = map.remove(new_name.as_str()) {
                        map.insert(old_name.clone(), v);
                    }
                },
                CompatibilityRule::RemovedFieldDefault { field, default } => {
                    map.entry(field.clone()).or_insert_with(|| default.clone());
                },
                CompatibilityRule::Deprecated { .. } => {},
            }
        }
    }
    payload
}

/// Deprecated fields still present in `rules`, for surfacing in
/// documentation / observability without changing the payload.
pub fn deprecated_fields(rules: &[CompatibilityRule]) -> Vec<(&str, &str)> {
    rules
        .iter()
        .filter_map(|r| match r {
            CompatibilityRule::Deprecated { field, since } => {
                Some((field.as_str(), since.as_str()))
            },
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renames_field_back_for_old_clients() {
        let payload = json!({ "userId": "u_123" });
        let rules = vec![CompatibilityRule::RenamedField {
            old_name: "user_id".into(),
            new_name: "userId".into(),
        }];
        let out = apply_compatibility(payload, &rules);
        assert_eq!(out["user_id"], "u_123");
    }

    #[test]
    fn fills_default_for_removed_field() {
        let payload = json!({ "id": "u_123" });
        let rules = vec![CompatibilityRule::RemovedFieldDefault {
            field: "legacy_flag".into(),
            default: json!(false),
        }];
        let out = apply_compatibility(payload, &rules);
        assert_eq!(out["legacy_flag"], false);
    }

    #[test]
    fn lists_deprecated_fields() {
        let rules = vec![CompatibilityRule::Deprecated {
            field: "old_field".into(),
            since: "0.3.0".into(),
        }];
        assert_eq!(deprecated_fields(&rules), vec![("old_field", "0.3.0")]);
    }
}
