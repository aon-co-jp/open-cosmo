//! `FederatedBackend` — 社内に散らばる複数データベースを 1 つに統合運用。
//!
//! [`DualBackend`](crate::dual::DualBackend) generalized to N members: each
//! logical table is routed to one member, broadcast to all, or served by
//! the default member. Reads fall back across members, so a record that
//! lives anywhere in the federation is found.
//!
//! ```rust,ignore
//! let fed = FederatedBackend::builder()
//!     .member("tokyo-pg",  tokyo_postgres)
//!     .member("osaka-my",  osaka_mysql)
//!     .member("archive",   clickhouse)
//!     .route("orders",   "osaka-my")     // stays where the team owns it
//!     .route("audit_log", "archive")
//!     .broadcast("schemas")               // critical data → every member
//!     .default_member("tokyo-pg")
//!     .build()?;
//! ```
//!
//! Combined with [`crate::migrate`], this is the consolidation path: mount
//! every legacy store as a member, operate them as one database, then
//! `transfer` tables member-by-member until everything lives where you
//! want it — zero downtime, no big-bang cutover.

use crate::{DbBackend, Record};
use async_trait::async_trait;
use open_runo_core::{AppError, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Where a table lives inside the federation.
#[derive(Debug, Clone)]
enum Placement {
    /// One named member owns the table.
    Member(String),
    /// Every member holds a copy (write-all, read-primary-then-fallback).
    Broadcast,
}

/// Builder for [`FederatedBackend`].
#[derive(Debug, Default)]
pub struct FederatedBuilder {
    members: Vec<(String, Arc<dyn DbBackend>)>,
    placements: HashMap<String, Placement>,
    default_member: Option<String>,
}

impl FederatedBuilder {
    #[must_use]
    pub fn member(mut self, name: impl Into<String>, backend: Arc<dyn DbBackend>) -> Self {
        self.members.push((name.into(), backend));
        self
    }

    /// Route `table` to the member called `name`.
    #[must_use]
    pub fn route(mut self, table: impl Into<String>, name: impl Into<String>) -> Self {
        self.placements
            .insert(table.into(), Placement::Member(name.into()));
        self
    }

    /// Keep `table` on every member (durability-critical data).
    #[must_use]
    pub fn broadcast(mut self, table: impl Into<String>) -> Self {
        self.placements.insert(table.into(), Placement::Broadcast);
        self
    }

    /// Member that receives tables without an explicit route
    /// (defaults to the first member).
    #[must_use]
    pub fn default_member(mut self, name: impl Into<String>) -> Self {
        self.default_member = Some(name.into());
        self
    }

    pub fn build(self) -> Result<FederatedBackend> {
        if self.members.is_empty() {
            return Err(AppError::Validation(
                "federation needs at least one member".into(),
            ));
        }
        let default_member = self
            .default_member
            .unwrap_or_else(|| self.members[0].0.clone());
        for (table, placement) in &self.placements {
            if let Placement::Member(name) = placement {
                if !self.members.iter().any(|(n, _)| n == name) {
                    return Err(AppError::Validation(format!(
                        "table '{table}' routed to unknown member '{name}'"
                    )));
                }
            }
        }
        if !self.members.iter().any(|(n, _)| *n == default_member) {
            return Err(AppError::Validation(format!(
                "unknown default member '{default_member}'"
            )));
        }
        Ok(FederatedBackend {
            members: self.members,
            placements: self.placements,
            default_member,
        })
    }
}

/// N distributed databases operated as one.
#[derive(Debug)]
pub struct FederatedBackend {
    members: Vec<(String, Arc<dyn DbBackend>)>,
    placements: HashMap<String, Placement>,
    default_member: String,
}

impl FederatedBackend {
    pub fn builder() -> FederatedBuilder {
        FederatedBuilder::default()
    }

    pub fn member_names(&self) -> Vec<&str> {
        self.members.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Access one member directly (e.g. as a `migrate::transfer` source).
    pub fn member(&self, name: &str) -> Option<&Arc<dyn DbBackend>> {
        self.members.iter().find(|(n, _)| n == name).map(|(_, b)| b)
    }

    fn owner_of(&self, table: &str) -> &Arc<dyn DbBackend> {
        let name = match self.placements.get(table) {
            Some(Placement::Member(name)) => name,
            _ => &self.default_member,
        };
        self.members
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b)
            .unwrap_or(&self.members[0].1)
    }

    fn is_broadcast(&self, table: &str) -> bool {
        matches!(self.placements.get(table), Some(Placement::Broadcast))
    }
}

#[async_trait]
impl DbBackend for FederatedBackend {
    fn backend_name(&self) -> &'static str {
        "federated"
    }

    async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
        if self.is_broadcast(table) {
            let mut result = Ok(());
            for (_, member) in &self.members {
                let r = member.put(table, key, value).await;
                if result.is_ok() {
                    result = r;
                }
            }
            result
        } else {
            self.owner_of(table).put(table, key, value).await
        }
    }

    async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
        // Owner first, then fall back across the federation: a record that
        // exists anywhere is found (useful mid-consolidation).
        if let Some(v) = self.owner_of(table).get(table, key).await? {
            return Ok(Some(v));
        }
        for (_, member) in &self.members {
            if let Some(v) = member.get(table, key).await? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    async fn delete(&self, table: &str, key: &str) -> Result<()> {
        // Delete everywhere so no stale copy resurfaces via fallback.
        let mut result = Ok(());
        for (_, member) in &self.members {
            let r = member.delete(table, key).await;
            if result.is_ok() {
                result = r;
            }
        }
        result
    }

    async fn list(&self, table: &str) -> Result<Vec<Record>> {
        // Union across members; the owner's copy wins on key collisions.
        let mut merged: HashMap<String, String> = HashMap::new();
        for (_, member) in self.members.iter().rev() {
            for rec in member.list(table).await? {
                merged.insert(rec.key, rec.value);
            }
        }
        for rec in self.owner_of(table).list(table).await? {
            merged.insert(rec.key, rec.value);
        }
        let mut records: Vec<Record> = merged
            .into_iter()
            .map(|(key, value)| Record { key, value })
            .collect();
        records.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(records)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryBackend;

    fn fed() -> FederatedBackend {
        FederatedBackend::builder()
            .member("tokyo", Arc::new(InMemoryBackend::new()))
            .member("osaka", Arc::new(InMemoryBackend::new()))
            .member("archive", Arc::new(InMemoryBackend::new()))
            .route("orders", "osaka")
            .route("audit_log", "archive")
            .broadcast("schemas")
            .default_member("tokyo")
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn tables_route_to_their_owning_member() {
        let f = fed();
        f.put("orders", "o1", "osaka-data").await.unwrap();
        f.put("sessions", "s1", "tokyo-data").await.unwrap(); // default

        assert!(f
            .member("osaka")
            .unwrap()
            .get("orders", "o1")
            .await
            .unwrap()
            .is_some());
        assert!(f
            .member("tokyo")
            .unwrap()
            .get("orders", "o1")
            .await
            .unwrap()
            .is_none());
        assert!(f
            .member("tokyo")
            .unwrap()
            .get("sessions", "s1")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn broadcast_tables_land_on_every_member() {
        let f = fed();
        f.put("schemas", "svc", "sdl").await.unwrap();
        for name in ["tokyo", "osaka", "archive"] {
            assert!(
                f.member(name)
                    .unwrap()
                    .get("schemas", "svc")
                    .await
                    .unwrap()
                    .is_some(),
                "missing on {name}"
            );
        }
    }

    #[tokio::test]
    async fn reads_fall_back_across_the_federation() {
        let f = fed();
        // A legacy record sits on the "wrong" member (pre-consolidation).
        f.member("archive")
            .unwrap()
            .put("orders", "legacy", "old")
            .await
            .unwrap();
        // The federation still finds it.
        assert_eq!(f.get("orders", "legacy").await.unwrap().unwrap(), "old");
        // And list() surfaces it too.
        let all = f.list("orders").await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn delete_removes_every_copy() {
        let f = fed();
        f.put("schemas", "svc", "sdl").await.unwrap();
        f.delete("schemas", "svc").await.unwrap();
        assert!(f.get("schemas", "svc").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn consolidation_via_migrate_transfer() {
        let f = fed();
        // Legacy data on osaka; consolidate "orders" onto tokyo.
        f.member("osaka")
            .unwrap()
            .put("orders", "o1", "d1")
            .await
            .unwrap();
        f.member("osaka")
            .unwrap()
            .put("orders", "o2", "d2")
            .await
            .unwrap();

        let (report, issues) = crate::migrate::transfer_verified(
            f.member("osaka").unwrap().as_ref(),
            f.member("tokyo").unwrap().as_ref(),
            &["orders"],
        )
        .await
        .unwrap();
        assert_eq!(report.total, 2);
        assert!(issues.is_empty());
    }

    #[test]
    fn builder_rejects_bad_config() {
        assert!(FederatedBackend::builder().build().is_err());
        assert!(FederatedBackend::builder()
            .member("a", Arc::new(InMemoryBackend::new()))
            .route("t", "nonexistent")
            .build()
            .is_err());
    }
}
