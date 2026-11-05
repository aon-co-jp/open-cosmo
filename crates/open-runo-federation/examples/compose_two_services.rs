//! Composes two backend service schemas into one federated schema, then
//! shows how `detect_breaking_changes` flags a field removal across a
//! schema evolution.
//!
//! Run with:
//!   cargo run -p open-runo-federation --example compose_two_services

use open_runo_federation::{compose, detect_breaking_changes, ServiceSchema};
use std::collections::{BTreeMap, BTreeSet};

fn service(name: &str, type_name: &str, fields: &[&str]) -> ServiceSchema {
    let mut types = BTreeMap::new();
    types.insert(
        type_name.to_string(),
        fields
            .iter()
            .map(|f| f.to_string())
            .collect::<BTreeSet<_>>(),
    );
    ServiceSchema {
        service_name: name.to_string(),
        types,
    }
}

fn main() -> anyhow::Result<()> {
    // Two services both contribute fields to the shared `User` type.
    let users_service = service("users-service", "User", &["id", "name", "email"]);
    let billing_service = service("billing-service", "User", &["id", "plan", "trial_ends_at"]);

    let before = compose(&[users_service.clone(), billing_service.clone()])?;
    println!("composed types: {:#?}", before.types);
    println!("contributing services: {:?}", before.contributing_services);

    // Simulate billing-service dropping `trial_ends_at` in a later release.
    let billing_service_v2 = service("billing-service", "User", &["id", "plan"]);
    let after = compose(&[users_service, billing_service_v2])?;

    let breaking = detect_breaking_changes(&before, &after);
    if breaking.is_empty() {
        println!("no breaking changes detected");
    } else {
        println!("breaking changes detected:");
        for change in breaking {
            println!("  - {change}");
        }
    }

    Ok(())
}
