//! Registers successive schema versions for a service, diffs two versions,
//! and walks through the local -> development -> staging -> production
//! promotion stages.
//!
//! Run with:
//!   cargo run -p open-runo-schema-registry --example register_and_promote

use open_runo_schema_registry::{SchemaRegistry, Stage};

fn main() -> open_runo_core::Result<()> {
    let mut registry = SchemaRegistry::new();

    let v1_sdl = "type User {\n  id: ID!\n}";
    let v2_sdl = "type User {\n  id: ID!\n  name: String\n}";

    let v1 = registry.register("users-service", v1_sdl, Stage::Local);
    println!("registered users-service at {:?} (id={})", v1.stage, v1.id);

    let v2 = registry.register("users-service", v2_sdl, Stage::Local);
    println!("registered users-service at {:?} (id={})", v2.stage, v2.id);

    let diff = registry.diff(v1_sdl, v2_sdl)?;
    println!("diff between v1 and v2:");
    for line in &diff {
        println!("  {line}");
    }

    // Promote v2 through the stage pipeline. `register` records a new
    // version per stage; a real deployment would gate each promotion on
    // CI/manual approval rather than calling these back-to-back.
    for stage in [Stage::Development, Stage::Staging, Stage::Production] {
        let promoted = registry.register("users-service", v2_sdl, stage);
        println!("promoted to {stage:?} (id={})", promoted.id);
    }

    println!(
        "full history has {} versions",
        registry.history("users-service").len()
    );
    assert!(
        Stage::Local < Stage::Production,
        "stage ordering enables promotion gating"
    );

    Ok(())
}
