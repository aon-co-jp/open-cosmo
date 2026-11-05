//! `DualBackend`: routes writes/reads across PostgreSQL and aruaru-db.
//!
//! ## Routing strategy
//!
//! | Table category | Primary target | Secondary (shadow-write) |
//! |----------------|----------------|--------------------------|
//! | OLTP (config, users, sessions) | PostgreSQL | — |
//! | Versioned (schemas, history, audit) | aruaru-db | PostgreSQL (replica) |
//! | Analytics | aruaru-db | — |
//!
//! [`DatabaseTarget`] controls where each `put`/`get` is directed.
//! [`DualBackend`] wraps two `Arc<dyn DbBackend>` instances and a
//! [`RoutingTable`] that maps table names to targets.

use crate::{DbBackend, Record};
use open_runo_core::Result;
use serde::Serialize;

/// One divergence found (and healed) between the two databases.
#[derive(Debug, Clone, Serialize)]
pub struct Discrepancy {
    pub table: String,
    pub key: String,
    /// `missing_in_postgres` / `missing_in_aruaru` / `mismatch` /
    /// `corrupt_in_postgres`.
    pub kind: String,
    /// Which side was judged correct and used to overwrite the other.
    pub healed_from: String,
}
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Which database should handle a given logical table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DatabaseTarget {
    /// Route to PostgreSQL only (OLTP hot-path).
    #[default]
    Postgres,
    /// Route to aruaru-db only (versioned / analytics).
    Aruaru,
    /// Write to both; read from PostgreSQL (consistency guarantee).
    Both,
}

/// Per-table routing configuration.
#[derive(Debug, Default, Clone)]
pub struct RoutingTable {
    entries: HashMap<String, DatabaseTarget>,
    default: DatabaseTarget,
}

impl RoutingTable {
    pub fn new(default: DatabaseTarget) -> Self {
        Self {
            entries: HashMap::new(),
            default,
        }
    }

    /// Override the target for a specific table name.
    pub fn route(mut self, table: impl Into<String>, target: DatabaseTarget) -> Self {
        self.entries.insert(table.into(), target);
        self
    }

    pub fn resolve(&self, table: &str) -> DatabaseTarget {
        self.entries.get(table).copied().unwrap_or(self.default)
    }
}

/// Default open-runo routing: OLTP tables → Postgres, versioned tables → aruaru-db.
pub fn default_routing() -> RoutingTable {
    RoutingTable::new(DatabaseTarget::Postgres)
        // Schema registry and change history go to aruaru-db for Git-on-SQL
        .route("schemas", DatabaseTarget::Both)
        .route("schema_history", DatabaseTarget::Aruaru)
        .route("change_records", DatabaseTarget::Aruaru)
        .route("audit_log", DatabaseTarget::Aruaru)
        // Backup metadata in both for durability
        .route("backup_jobs", DatabaseTarget::Both)
        // Persisted queries (trusted documents) in both for durability
        .route("persisted_queries", DatabaseTarget::Both)
        // AI learned model in both: the framework's intelligence survives
        // the loss of either database
        .route("ai_model", DatabaseTarget::Both)
        // OLTP tables stay in PostgreSQL
        .route("sessions", DatabaseTarget::Postgres)
        .route("api_keys", DatabaseTarget::Postgres)
        .route("rate_limits", DatabaseTarget::Postgres)
}

/// Routes database operations to PostgreSQL, aruaru-db, or both.
#[derive(Debug, Clone)]
pub struct DualBackend {
    postgres: Arc<dyn DbBackend>,
    aruaru: Arc<dyn DbBackend>,
    routing: RoutingTable,
}

impl DualBackend {
    pub fn new(
        postgres: Arc<dyn DbBackend>,
        aruaru: Arc<dyn DbBackend>,
        routing: RoutingTable,
    ) -> Self {
        Self {
            postgres,
            aruaru,
            routing,
        }
    }

    /// Build with the default open-runo routing table.
    pub fn with_default_routing(postgres: Arc<dyn DbBackend>, aruaru: Arc<dyn DbBackend>) -> Self {
        Self::new(postgres, aruaru, default_routing())
    }

    /// Wrap a single backend so single-DB deployments share the exact same
    /// code path as DUAL DATABASE deployments.
    ///
    /// Both routing targets point to the same backend, so every
    /// [`DatabaseTarget`] resolves to it. `Both` tables are written twice to
    /// the same store, which is idempotent for a key-value upsert.
    ///
    /// ```rust,ignore
    /// let db = DualBackend::single(Arc::new(SqliteBackend::open(path)?));
    /// let state = AppState::with_db(Arc::new(db));
    /// ```
    pub fn single(backend: Arc<dyn DbBackend>) -> Self {
        Self::new(Arc::clone(&backend), backend, default_routing())
    }

    /// `true` if both routing targets point to the same backend instance
    /// (i.e. built via [`DualBackend::single`]).
    pub fn is_single(&self) -> bool {
        Arc::ptr_eq(&self.postgres, &self.aruaru)
    }

    /// Verify that every `Both`-routed table really holds identical data in
    /// BOTH databases; report and self-heal any divergence.
    ///
    /// Healing policy ("正しい方を正とし、誤りを上書きする"):
    /// 1. present beats absent — a record missing on one side is re-copied
    ///    from the side that has it;
    /// 2. parseable beats corrupt — if only one side is valid JSON, it wins
    ///    and overwrites the corrupt side;
    /// 3. both valid but different — the primary (PostgreSQL) is treated as
    ///    the source of truth and overwrites the replica.
    ///
    /// Every action is returned as a [`Discrepancy`] so the caller can
    /// report it (audit log / operators).
    async fn reconcile(&self) -> Result<Vec<Discrepancy>> {
        let mut found = Vec::new();
        if self.is_single() {
            return Ok(found); // one physical store: nothing to compare
        }

        let both_tables: Vec<String> = self
            .routing
            .entries
            .iter()
            .filter(|(_, target)| **target == DatabaseTarget::Both)
            .map(|(table, _)| table.clone())
            .collect();

        for table in both_tables {
            let a: HashMap<String, String> = self
                .postgres
                .list(&table)
                .await?
                .into_iter()
                .map(|r| (r.key, r.value))
                .collect();
            let b: HashMap<String, String> = self
                .aruaru
                .list(&table)
                .await?
                .into_iter()
                .map(|r| (r.key, r.value))
                .collect();

            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();

            for key in keys {
                match (a.get(key), b.get(key)) {
                    (Some(v), None) => {
                        self.aruaru.put(&table, key, v).await?;
                        found.push(Discrepancy {
                            table: table.clone(),
                            key: key.clone(),
                            kind: "missing_in_aruaru".into(),
                            healed_from: "postgres".into(),
                        });
                    },
                    (None, Some(v)) => {
                        self.postgres.put(&table, key, v).await?;
                        found.push(Discrepancy {
                            table: table.clone(),
                            key: key.clone(),
                            kind: "missing_in_postgres".into(),
                            healed_from: "aruaru-db".into(),
                        });
                    },
                    (Some(x), Some(y)) if x != y => {
                        let x_ok = serde_json::from_str::<serde_json::Value>(x).is_ok();
                        let y_ok = serde_json::from_str::<serde_json::Value>(y).is_ok();
                        // parseable beats corrupt; tie → primary wins.
                        let (winner, kind, from) = if x_ok || !y_ok {
                            (x, "mismatch", "postgres")
                        } else {
                            (y, "corrupt_in_postgres", "aruaru-db")
                        };
                        if from == "postgres" {
                            self.aruaru.put(&table, key, winner).await?;
                        } else {
                            self.postgres.put(&table, key, winner).await?;
                        }
                        found.push(Discrepancy {
                            table: table.clone(),
                            key: key.clone(),
                            kind: kind.into(),
                            healed_from: from.into(),
                        });
                    },
                    _ => {},
                }
            }
        }
        Ok(found)
    }

    fn resolve(&self, table: &str) -> (&dyn DbBackend, Option<&dyn DbBackend>) {
        match self.routing.resolve(table) {
            DatabaseTarget::Postgres => (self.postgres.as_ref(), None),
            DatabaseTarget::Aruaru => (self.aruaru.as_ref(), None),
            DatabaseTarget::Both => (self.postgres.as_ref(), Some(self.aruaru.as_ref())),
        }
    }
}

#[async_trait]
impl DbBackend for DualBackend {
    fn backend_name(&self) -> &'static str {
        if self.is_single() {
            "dual(single)"
        } else {
            "dual(postgres+aruaru-db)"
        }
    }

    async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
        match self.routing.resolve(table) {
            DatabaseTarget::Postgres => self.postgres.put(table, key, value).await,
            DatabaseTarget::Aruaru => self.aruaru.put(table, key, value).await,
            DatabaseTarget::Both => {
                // Write to both; surface the first error but still attempt both.
                let r1 = self.postgres.put(table, key, value).await;
                let r2 = self.aruaru.put(table, key, value).await;
                r1.and(r2)
            },
        }
    }

    async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
        // For `Both` targets, read from the primary (PostgreSQL) first;
        // fall back to aruaru-db if PostgreSQL returns None.
        let (primary, secondary) = self.resolve(table);
        match primary.get(table, key).await? {
            Some(v) => Ok(Some(v)),
            None => match secondary {
                Some(sec) => sec.get(table, key).await,
                None => Ok(None),
            },
        }
    }

    async fn delete(&self, table: &str, key: &str) -> Result<()> {
        match self.routing.resolve(table) {
            DatabaseTarget::Postgres => self.postgres.delete(table, key).await,
            DatabaseTarget::Aruaru => self.aruaru.delete(table, key).await,
            DatabaseTarget::Both => {
                let r1 = self.postgres.delete(table, key).await;
                let r2 = self.aruaru.delete(table, key).await;
                r1.and(r2)
            },
        }
    }

    async fn list(&self, table: &str) -> Result<Vec<Record>> {
        let (primary, _) = self.resolve(table);
        primary.list(table).await
    }

    async fn consistency_check_and_heal(&self) -> Result<Vec<Discrepancy>> {
        self.reconcile().await
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryBackend;

    fn dual() -> DualBackend {
        DualBackend::with_default_routing(
            Arc::new(InMemoryBackend::new()),
            Arc::new(InMemoryBackend::new()),
        )
    }

    #[tokio::test]
    async fn oltp_table_routes_to_postgres_only() {
        let db = dual();
        // "sessions" → Postgres only
        db.put("sessions", "s1", r#"{"user":"alice"}"#)
            .await
            .unwrap();
        assert!(db.postgres.get("sessions", "s1").await.unwrap().is_some());
        assert!(db.aruaru.get("sessions", "s1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn schema_table_writes_to_both() {
        let db = dual();
        // "schemas" → Both
        db.put("schemas", "users", r#"{"sdl":"type User{}"}"#)
            .await
            .unwrap();
        assert!(db.postgres.get("schemas", "users").await.unwrap().is_some());
        assert!(db.aruaru.get("schemas", "users").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn versioned_table_routes_to_aruaru_only() {
        let db = dual();
        // "change_records" → aruaru-db only
        db.put("change_records", "c1", r#"{"author":"bob"}"#)
            .await
            .unwrap();
        assert!(db
            .aruaru
            .get("change_records", "c1")
            .await
            .unwrap()
            .is_some());
        assert!(db
            .postgres
            .get("change_records", "c1")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn get_falls_back_to_secondary_when_primary_empty() {
        let db = dual();
        // Manually insert into aruaru-db only for a "Both" table
        db.aruaru
            .put("schemas", "orphan", r#"{"sdl":"type X{}"}"#)
            .await
            .unwrap();
        // get should find it via fallback
        let val = db.get("schemas", "orphan").await.unwrap();
        assert!(val.is_some());
    }

    #[tokio::test]
    async fn single_backend_shares_dual_code_path() {
        let inner: Arc<dyn DbBackend> = Arc::new(InMemoryBackend::new());
        let db = DualBackend::single(Arc::clone(&inner));

        assert!(db.is_single());
        assert_eq!(db.backend_name(), "dual(single)");

        // Every routing target lands in the same backend.
        db.put("sessions", "s1", r#"{"user":"alice"}"#)
            .await
            .unwrap(); // Postgres route
        db.put("change_records", "c1", r#"{"author":"bob"}"#)
            .await
            .unwrap(); // Aruaru route
        db.put("schemas", "users", r#"{"sdl":"type User{}"}"#)
            .await
            .unwrap(); // Both route

        assert!(inner.get("sessions", "s1").await.unwrap().is_some());
        assert!(inner.get("change_records", "c1").await.unwrap().is_some());
        assert!(inner.get("schemas", "users").await.unwrap().is_some());

        // Reads and deletes go through the same path.
        assert!(db.get("schemas", "users").await.unwrap().is_some());
        db.delete("schemas", "users").await.unwrap();
        assert!(db.get("schemas", "users").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dual_backend_is_not_single() {
        let db = dual();
        assert!(!db.is_single());
        assert_eq!(db.backend_name(), "dual(postgres+aruaru-db)");
    }

    #[tokio::test]
    async fn routing_table_default_and_override() {
        let rt =
            RoutingTable::new(DatabaseTarget::Postgres).route("audit_log", DatabaseTarget::Aruaru);
        assert_eq!(rt.resolve("anything"), DatabaseTarget::Postgres);
        assert_eq!(rt.resolve("audit_log"), DatabaseTarget::Aruaru);
    }

    #[tokio::test]
    async fn consistency_check_heals_missing_and_corrupt_records() {
        let db = dual();

        // Normal write lands on both sides.
        db.put("schemas", "ok", r#"{"sdl":"type A{}"}"#)
            .await
            .unwrap();

        // Simulate: replica lost a record…
        db.postgres
            .put("schemas", "lost", r#"{"sdl":"type B{}"}"#)
            .await
            .unwrap();
        // …primary record got corrupted (not JSON)…
        db.postgres
            .put("schemas", "bad", "%%corrupt%%")
            .await
            .unwrap();
        db.aruaru
            .put("schemas", "bad", r#"{"sdl":"type C{}"}"#)
            .await
            .unwrap();
        // …and primary is missing one the replica has.
        db.aruaru
            .put("schemas", "only-b", r#"{"sdl":"type D{}"}"#)
            .await
            .unwrap();

        let report = db.consistency_check_and_heal().await.unwrap();
        assert_eq!(report.len(), 3);

        // Everything converged: both sides now identical and valid.
        assert_eq!(
            db.postgres.get("schemas", "bad").await.unwrap(),
            db.aruaru.get("schemas", "bad").await.unwrap()
        );
        assert_eq!(
            db.postgres.get("schemas", "bad").await.unwrap().unwrap(),
            r#"{"sdl":"type C{}"}"#
        );
        assert!(db.aruaru.get("schemas", "lost").await.unwrap().is_some());
        assert!(db
            .postgres
            .get("schemas", "only-b")
            .await
            .unwrap()
            .is_some());

        // A second pass finds nothing: the system healed itself.
        assert!(db.consistency_check_and_heal().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn consistency_check_is_noop_for_single_backend() {
        let inner: Arc<dyn DbBackend> = Arc::new(InMemoryBackend::new());
        let db = DualBackend::single(inner);
        assert!(db.consistency_check_and_heal().await.unwrap().is_empty());
    }
}
