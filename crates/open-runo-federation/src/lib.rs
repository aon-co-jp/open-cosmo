//! `open-runo-federation`: composes multiple backend services (GraphQL,
//! gRPC, OpenAPI, internal Rust services, AI services, ...) into a single
//! federated API surface.
//!
//! This crate currently implements the core data model and a naive
//! composition algorithm sufficient for Phase 2 ("Federation Core"):
//! schema registration, conflict detection on type/field collisions, and a
//! composed-schema output. Query planning/execution land in a later phase.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod sdl;
pub use sdl::{detect_federation_version, parse_service_sdl};

/// Which Apollo Federation dialect a subgraph's raw SDL follows. See
/// [`sdl::detect_federation_version`] for detection rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FederationVersion {
    V1,
    V2,
    None,
}

/// A single backend service participating in federation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSchema {
    pub service_name: String,
    /// Type name -> set of field names exposed by this service for that type.
    pub types: BTreeMap<String, BTreeSet<String>>,
}

/// The result of composing N service schemas into one federated schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComposedSchema {
    pub types: BTreeMap<String, BTreeSet<String>>,
    pub contributing_services: Vec<String>,
}

/// Composes service schemas, merging fields for shared types.
///
/// A conflict is raised when two services declare the *same* field on the
/// *same* type with no owning-service annotation — for this minimal engine
/// we treat same-type/same-field across services as a compatible merge
/// (idempotent union) and only reject when a service redeclares a type it
/// already declared, which signals a caller bug rather than a real
/// federation conflict.
pub fn compose(services: &[ServiceSchema]) -> Result<ComposedSchema> {
    let mut composed = ComposedSchema::default();

    for service in services {
        if composed
            .contributing_services
            .contains(&service.service_name)
        {
            return Err(AppError::Conflict(format!(
                "service '{}' registered more than once",
                service.service_name
            )));
        }
        composed
            .contributing_services
            .push(service.service_name.clone());

        for (type_name, fields) in &service.types {
            composed
                .types
                .entry(type_name.clone())
                .or_default()
                .extend(fields.iter().cloned());
        }
    }

    Ok(composed)
}

/// Detects fields present in `previous` but missing in `next` for the same
/// type — a breaking change from the Federation Engine's point of view.
pub fn detect_breaking_changes(previous: &ComposedSchema, next: &ComposedSchema) -> Vec<String> {
    let mut breaking = Vec::new();
    for (type_name, fields) in &previous.types {
        let next_fields = next.types.get(type_name);
        for field in fields {
            let still_present = next_fields.is_some_and(|f| f.contains(field));
            if !still_present {
                breaking.push(format!("{type_name}.{field} removed"));
            }
        }
        if next_fields.is_none() {
            breaking.push(format!("type {type_name} removed"));
        }
    }
    breaking
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, ty: &str, fields: &[&str]) -> ServiceSchema {
        let mut types = BTreeMap::new();
        types.insert(
            ty.to_string(),
            fields.iter().map(|s| s.to_string()).collect(),
        );
        ServiceSchema {
            service_name: name.to_string(),
            types,
        }
    }

    #[test]
    fn composes_and_merges_shared_type() {
        let a = svc("users-service", "User", &["id", "name"]);
        let b = svc("billing-service", "User", &["id", "plan"]);
        let composed = compose(&[a, b]).unwrap();
        let user_fields = &composed.types["User"];
        assert!(user_fields.contains("name"));
        assert!(user_fields.contains("plan"));
        assert_eq!(composed.contributing_services.len(), 2);
    }

    #[test]
    fn rejects_duplicate_service_registration() {
        let a = svc("users-service", "User", &["id"]);
        let a2 = svc("users-service", "User", &["id"]);
        assert!(compose(&[a, a2]).is_err());
    }

    #[test]
    fn detects_removed_field_as_breaking() {
        let before = compose(&[svc("users-service", "User", &["id", "name"])]).unwrap();
        let after = compose(&[svc("users-service", "User", &["id"])]).unwrap();
        let breaking = detect_breaking_changes(&before, &after);
        assert_eq!(breaking, vec!["User.name removed".to_string()]);
    }
}
