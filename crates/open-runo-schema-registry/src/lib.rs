//! `open-runo-schema-registry`: registers, versions, and diffs API schemas,
//! with Git-like history and environment promotion (local -> development ->
//! staging -> production), per the README's Schema Registry section.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use chrono::{DateTime, Utc};
use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Stage {
    Local,
    Development,
    Staging,
    Production,
}

/// The namespace used when callers don't specify one (single-graph setups).
pub const DEFAULT_NAMESPACE: &str = "default";

fn default_namespace() -> String {
    DEFAULT_NAMESPACE.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaVersion {
    pub id: Uuid,
    /// Multi-graph namespace (Cosmo parity). Existing single-graph callers
    /// implicitly use [`DEFAULT_NAMESPACE`].
    #[serde(default = "default_namespace")]
    pub namespace: String,
    pub service_name: String,
    pub sdl: String,
    pub stage: Stage,
    pub created_at: DateTime<Utc>,
}

/// In-memory schema registry. A production implementation would persist
/// through `open-runo-db`; this crate defines the domain logic independent
/// of storage so it can be unit tested in isolation.
#[derive(Debug, Default)]
pub struct SchemaRegistry {
    /// Keyed by `namespace \u{0} service_name`.
    versions: HashMap<String, Vec<SchemaVersion>>,
}

fn key(namespace: &str, service_name: &str) -> String {
    format!("{namespace}\u{0}{service_name}")
}

impl SchemaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register in the [`DEFAULT_NAMESPACE`] (single-graph shortcut).
    pub fn register(&mut self, service_name: &str, sdl: &str, stage: Stage) -> SchemaVersion {
        self.register_in(DEFAULT_NAMESPACE, service_name, sdl, stage)
    }

    /// Register a new schema version for a service in a namespace
    /// (multi-graph: one namespace per federated graph, Cosmo parity).
    pub fn register_in(
        &mut self,
        namespace: &str,
        service_name: &str,
        sdl: &str,
        stage: Stage,
    ) -> SchemaVersion {
        let version = SchemaVersion {
            id: Uuid::new_v4(),
            namespace: namespace.to_string(),
            service_name: service_name.to_string(),
            sdl: sdl.to_string(),
            stage,
            created_at: Utc::now(),
        };
        self.versions
            .entry(key(namespace, service_name))
            .or_default()
            .push(version.clone());
        version
    }

    /// Latest version in the [`DEFAULT_NAMESPACE`].
    pub fn latest(&self, service_name: &str, stage: Stage) -> Option<&SchemaVersion> {
        self.latest_in(DEFAULT_NAMESPACE, service_name, stage)
    }

    /// Latest registered version for a service in a namespace at a stage.
    pub fn latest_in(
        &self,
        namespace: &str,
        service_name: &str,
        stage: Stage,
    ) -> Option<&SchemaVersion> {
        self.versions
            .get(&key(namespace, service_name))
            .into_iter()
            .flatten()
            .filter(|v| v.stage == stage)
            .max_by_key(|v| v.created_at)
    }

    /// Full version history in the [`DEFAULT_NAMESPACE`], oldest first.
    pub fn history(&self, service_name: &str) -> &[SchemaVersion] {
        self.history_in(DEFAULT_NAMESPACE, service_name)
    }

    /// Full version history for a service in a namespace, oldest first.
    pub fn history_in(&self, namespace: &str, service_name: &str) -> &[SchemaVersion] {
        self.versions
            .get(&key(namespace, service_name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// All namespaces that have at least one registered schema, sorted.
    pub fn namespaces(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .versions
            .keys()
            .filter_map(|k| k.split('\u{0}').next())
            .map(str::to_string)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Service names registered in a namespace, sorted.
    pub fn services_in(&self, namespace: &str) -> Vec<String> {
        let prefix = format!("{namespace}\u{0}");
        let mut out: Vec<String> = self
            .versions
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(str::to_string)
            .collect();
        out.sort();
        out
    }

    /// Naive line-level diff between two SDL strings, used for
    /// human-readable schema review before promotion.
    pub fn diff(&self, before: &str, after: &str) -> Result<Vec<String>> {
        if before == after {
            return Ok(Vec::new());
        }
        let before_lines: Vec<&str> = before.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();

        let removed = before_lines
            .iter()
            .filter(|l| !after_lines.contains(l))
            .map(|l| format!("- {l}"));
        let added = after_lines
            .iter()
            .filter(|l| !before_lines.contains(l))
            .map(|l| format!("+ {l}"));

        let diff: Vec<String> = removed.chain(added).collect();
        if diff.is_empty() {
            return Err(AppError::Internal(
                "content differs but no line-level diff was produced".into(),
            ));
        }
        Ok(diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_fetch_latest() {
        let mut registry = SchemaRegistry::new();
        registry.register("users-service", "type User { id: ID! }", Stage::Local);
        let latest = registry.register(
            "users-service",
            "type User { id: ID! name: String }",
            Stage::Local,
        );
        assert_eq!(
            registry.latest("users-service", Stage::Local).unwrap().id,
            latest.id
        );
        assert_eq!(registry.history("users-service").len(), 2);
    }

    #[test]
    fn diff_reports_added_line() {
        let registry = SchemaRegistry::new();
        let diff = registry
            .diff(
                "type User { id: ID! }",
                "type User { id: ID!\nname: String }",
            )
            .unwrap();
        assert!(diff.iter().any(|l| l.starts_with('+')));
    }

    #[test]
    fn stage_ordering_supports_promotion_checks() {
        assert!(Stage::Local < Stage::Development);
        assert!(Stage::Staging < Stage::Production);
    }

    #[test]
    fn namespaces_isolate_graphs() {
        let mut registry = SchemaRegistry::new();
        registry.register_in("e-gov", "users", "type User { id: ID! }", Stage::Local);
        registry.register_in("redmine", "users", "type Account { id: ID! }", Stage::Local);

        let egov = registry.latest_in("e-gov", "users", Stage::Local).unwrap();
        let redmine = registry
            .latest_in("redmine", "users", Stage::Local)
            .unwrap();
        assert_ne!(egov.sdl, redmine.sdl);
        assert_eq!(egov.namespace, "e-gov");

        // Default namespace is untouched.
        assert!(registry.latest("users", Stage::Local).is_none());
    }

    #[test]
    fn default_namespace_shortcuts_are_equivalent() {
        let mut registry = SchemaRegistry::new();
        registry.register("users", "type User { id: ID! }", Stage::Local);
        assert!(registry
            .latest_in(DEFAULT_NAMESPACE, "users", Stage::Local)
            .is_some());
        assert_eq!(registry.history_in(DEFAULT_NAMESPACE, "users").len(), 1);
    }

    #[test]
    fn namespace_and_service_listing() {
        let mut registry = SchemaRegistry::new();
        registry.register_in("e-gov", "users", "type U { a: ID }", Stage::Local);
        registry.register_in("e-gov", "billing", "type B { a: ID }", Stage::Local);
        registry.register_in("redmine", "issues", "type I { a: ID }", Stage::Local);

        assert_eq!(registry.namespaces(), vec!["e-gov", "redmine"]);
        assert_eq!(registry.services_in("e-gov"), vec!["billing", "users"]);
    }
}
