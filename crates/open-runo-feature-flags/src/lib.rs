//! `open-runo-feature-flags`: Cosmo Enterprise "Feature Flags" parity
//! (schema-change canary releases + percentage-based traffic routing),
//! shipped as plain OSS. See `docs/cosmo-parity.md` 4a.
//!
//! A [`FeatureFlag`] has a boolean on/off switch plus a `rollout_percent`
//! (0-100). [`FeatureFlagRegistry::evaluate`] answers, for a given
//! `bucket_key` (typically a user id, session id, or API key), whether
//! that caller falls inside the rollout -- deterministically, so the same
//! caller always gets the same answer for a given flag (no flip-flopping
//! between a canary and stable version mid-session). This is the same
//! sticky-bucketing shape Cosmo's Feature Flags use for canary releases
//! and preview-environment traffic routing.
//!
//! Storage is in-memory (mirrors `open_runo_schema_registry::SchemaRegistry`,
//! which `open-runo-router`'s `AppState` also holds directly rather than
//! through `open_runo_db` -- flag definitions are operational config, not
//! durable application data).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// A named feature flag: an on/off switch plus an optional percentage
/// rollout for gradual (canary) release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub name: String,
    /// Master switch. `false` means the flag evaluates to `false` for
    /// every caller regardless of `rollout_percent`.
    pub enabled: bool,
    /// `0..=100`. The fraction of callers (by deterministic bucketing of
    /// `bucket_key`) who see this flag as "on" when `enabled` is `true`.
    pub rollout_percent: u8,
    #[serde(default)]
    pub description: String,
}

impl FeatureFlag {
    /// A new flag, enabled at 100% rollout by default (the common case:
    /// "on" or "off", with percentage rollout as an opt-in refinement).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            rollout_percent: 100,
            description: String::new(),
        }
    }

    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[must_use]
    pub fn rollout_percent(mut self, percent: u8) -> Self {
        self.rollout_percent = percent;
        self
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(AppError::Validation(
                "feature flag name must not be empty".into(),
            ));
        }
        if self.rollout_percent > 100 {
            return Err(AppError::Validation(format!(
                "rollout_percent must be 0..=100, got {}",
                self.rollout_percent
            )));
        }
        Ok(())
    }
}

/// In-memory registry of feature flags, keyed by name.
#[derive(Debug, Default)]
pub struct FeatureFlagRegistry {
    flags: HashMap<String, FeatureFlag>,
}

impl FeatureFlagRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create or replace a flag definition. Validates `rollout_percent` is
    /// `0..=100` and `name` is non-empty.
    pub fn upsert(&mut self, flag: FeatureFlag) -> Result<FeatureFlag> {
        flag.validate()?;
        self.flags.insert(flag.name.clone(), flag.clone());
        Ok(flag)
    }

    pub fn get(&self, name: &str) -> Option<&FeatureFlag> {
        self.flags.get(name)
    }

    /// All flags, sorted by name for stable listing output.
    pub fn list(&self) -> Vec<&FeatureFlag> {
        let mut flags: Vec<&FeatureFlag> = self.flags.values().collect();
        flags.sort_by(|a, b| a.name.cmp(&b.name));
        flags
    }

    /// Remove a flag. Returns `true` if it existed.
    pub fn delete(&mut self, name: &str) -> bool {
        self.flags.remove(name).is_some()
    }

    /// Deterministically decide whether `bucket_key` is inside `name`'s
    /// rollout. Returns `None` if no such flag is registered (caller
    /// should treat this as "flag unknown", not "flag off" -- the 404
    /// case). The same `(name, bucket_key)` pair always yields the same
    /// result, so a given caller stays on the same side of a canary
    /// rollout for as long as the flag's `rollout_percent` is unchanged.
    pub fn evaluate(&self, name: &str, bucket_key: &str) -> Option<bool> {
        let flag = self.flags.get(name)?;
        if !flag.enabled {
            return Some(false);
        }
        if flag.rollout_percent >= 100 {
            return Some(true);
        }
        if flag.rollout_percent == 0 {
            return Some(false);
        }
        Some(bucket(name, bucket_key) < u32::from(flag.rollout_percent))
    }
}

/// Deterministic `0..100` bucket for `(name, bucket_key)`. Uses
/// `DefaultHasher`, which (unlike `HashMap`'s randomized `RandomState`) is
/// seeded with fixed keys, so the same input always hashes the same way
/// -- across calls, across processes, and across restarts.
fn bucket(name: &str, bucket_key: &str) -> u32 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    0u8.hash(&mut hasher); // separator, avoids "ab"+"c" colliding with "a"+"bc"
    bucket_key.hash(&mut hasher);
    (hasher.finish() % 100) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_rejects_empty_name() {
        let mut reg = FeatureFlagRegistry::new();
        let err = reg.upsert(FeatureFlag::new("  ")).unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn upsert_rejects_percent_over_100() {
        let mut reg = FeatureFlagRegistry::new();
        let err = reg
            .upsert(FeatureFlag::new("new-checkout").rollout_percent(101))
            .unwrap_err();
        assert!(err.to_string().contains("rollout_percent"));
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(
            FeatureFlag::new("new-checkout")
                .rollout_percent(25)
                .description("canary"),
        )
        .unwrap();
        let flag = reg.get("new-checkout").expect("flag should exist");
        assert_eq!(flag.rollout_percent, 25);
        assert_eq!(flag.description, "canary");
    }

    #[test]
    fn upsert_replaces_existing_flag_of_the_same_name() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("f").rollout_percent(10))
            .unwrap();
        reg.upsert(FeatureFlag::new("f").rollout_percent(90))
            .unwrap();
        assert_eq!(reg.get("f").unwrap().rollout_percent, 90);
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn list_is_sorted_by_name() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("zeta")).unwrap();
        reg.upsert(FeatureFlag::new("alpha")).unwrap();
        let names: Vec<&str> = reg.list().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn delete_removes_flag_and_reports_prior_existence() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("f")).unwrap();
        assert!(reg.delete("f"));
        assert!(!reg.delete("f"));
        assert!(reg.get("f").is_none());
    }

    #[test]
    fn evaluate_unknown_flag_is_none() {
        let reg = FeatureFlagRegistry::new();
        assert_eq!(reg.evaluate("ghost", "user-1"), None);
    }

    #[test]
    fn evaluate_disabled_flag_is_always_false() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("f").enabled(false).rollout_percent(100))
            .unwrap();
        assert_eq!(reg.evaluate("f", "any-user"), Some(false));
    }

    #[test]
    fn evaluate_full_rollout_is_always_true() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("f").rollout_percent(100))
            .unwrap();
        for key in ["a", "b", "c", "very-different-key"] {
            assert_eq!(reg.evaluate("f", key), Some(true));
        }
    }

    #[test]
    fn evaluate_zero_rollout_is_always_false() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("f").rollout_percent(0))
            .unwrap();
        for key in ["a", "b", "c", "very-different-key"] {
            assert_eq!(reg.evaluate("f", key), Some(false));
        }
    }

    #[test]
    fn evaluate_is_deterministic_for_the_same_key() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("f").rollout_percent(50))
            .unwrap();
        let first = reg.evaluate("f", "user-42");
        for _ in 0..20 {
            assert_eq!(reg.evaluate("f", "user-42"), first);
        }
    }

    #[test]
    fn evaluate_distribution_roughly_matches_rollout_percentage() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("f").rollout_percent(30))
            .unwrap();
        let total = 5_000;
        let on = (0..total)
            .filter(|i| reg.evaluate("f", &format!("user-{i}")) == Some(true))
            .count();
        let ratio = on as f64 / total as f64;
        // Hash-based bucketing over 5000 samples should land close to 30%;
        // generous tolerance keeps this test non-flaky.
        assert!(
            (0.24..=0.36).contains(&ratio),
            "expected ~30% rollout, got {:.1}%",
            ratio * 100.0
        );
    }

    #[test]
    fn different_flags_bucket_the_same_key_independently() {
        let mut reg = FeatureFlagRegistry::new();
        reg.upsert(FeatureFlag::new("flag-a").rollout_percent(50))
            .unwrap();
        reg.upsert(FeatureFlag::new("flag-b").rollout_percent(50))
            .unwrap();
        // Not asserting a specific relationship (that would be flaky by
        // construction) -- just that the bucketing key includes the flag
        // name, i.e. two flags don't always agree for every user.
        let agreements = (0..200)
            .filter(|i| {
                let key = format!("user-{i}");
                reg.evaluate("flag-a", &key) == reg.evaluate("flag-b", &key)
            })
            .count();
        assert!(
            agreements < 200,
            "flag-a and flag-b should not always agree"
        );
    }
}
