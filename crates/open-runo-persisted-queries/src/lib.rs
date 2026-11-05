//! `open-runo-persisted-queries`: Persisted Queries / Trusted Documents.
//!
//! Cosmo restricts persisted queries ("trusted documents") to paid plans;
//! open-runo ships them as plain OSS (see `docs/cosmo-parity.md`).
//!
//! A GraphQL document is registered once and addressed afterwards by its
//! SHA-256 hex hash. In [`EnforcementMode::Enforce`] the gateway only
//! executes registered documents, which eliminates arbitrary-query abuse.
//!
//! Storage goes through [`open_runo_db::DbBackend`], so the same code path
//! works on InMemory (tests), a single database, or the DUAL DATABASE
//! (`persisted_queries` routes to *Both* for durability).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use chrono::{DateTime, Utc};
use open_runo_core::{AppError, Result};
use open_runo_db::DbBackend;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Logical table name; routed to `Both` targets by
/// `open_runo_db::dual::default_routing`.
pub const TABLE: &str = "persisted_queries";

/// How strictly the gateway treats incoming operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementMode {
    /// Persisted-query lookups are disabled; only raw documents execute.
    Disabled,
    /// Raw documents and registered hashes are both accepted. When a client
    /// sends `hash` + `document` together, the document is auto-registered
    /// (Apollo APQ-compatible behaviour).
    #[default]
    Allow,
    /// Trusted Documents: only pre-registered hashes execute. Raw documents
    /// are rejected.
    Enforce,
}

/// A registered document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedQuery {
    pub hash: String,
    pub document: String,
    pub registered_at: DateTime<Utc>,
}

/// SHA-256 hex digest of a GraphQL document (the persisted-query key).
pub fn hash_document(document: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(document.as_bytes());
    hex::encode(hasher.finalize())
}

/// Registry of persisted queries on top of any [`DbBackend`].
#[derive(Debug, Clone)]
pub struct PersistedQueryStore {
    db: Arc<dyn DbBackend>,
    mode: EnforcementMode,
}

impl PersistedQueryStore {
    pub fn new(db: Arc<dyn DbBackend>, mode: EnforcementMode) -> Self {
        Self { db, mode }
    }

    pub fn mode(&self) -> EnforcementMode {
        self.mode
    }

    /// Register `document`, returning its record. Idempotent: registering
    /// the same document twice yields the same hash.
    pub async fn register(&self, document: &str) -> Result<PersistedQuery> {
        let document = document.trim();
        if document.is_empty() {
            return Err(AppError::Validation("document must not be empty".into()));
        }
        let record = PersistedQuery {
            hash: hash_document(document),
            document: document.to_string(),
            registered_at: Utc::now(),
        };
        let json = serde_json::to_string(&record)
            .map_err(|e| AppError::Internal(format!("serialize persisted query: {e}")))?;
        self.db.put(TABLE, &record.hash, &json).await?;
        Ok(record)
    }

    /// Look up a registered document by hash.
    pub async fn get(&self, hash: &str) -> Result<Option<PersistedQuery>> {
        match self.db.get(TABLE, hash).await? {
            None => Ok(None),
            Some(raw) => sonic_rs::from_str(&raw)
                .map(Some)
                .map_err(|e| AppError::Internal(format!("corrupt persisted query record: {e}"))),
        }
    }

    /// Resolve an incoming operation to the executable document according
    /// to the configured [`EnforcementMode`].
    ///
    /// `hash` / `document` mirror the fields a GraphQL client sends
    /// (`extensions.persistedQuery.sha256Hash` / `query`).
    pub async fn resolve(&self, hash: Option<&str>, document: Option<&str>) -> Result<String> {
        match self.mode {
            EnforcementMode::Disabled => document
                .map(str::to_string)
                .ok_or_else(|| AppError::Validation("query document required".into())),

            EnforcementMode::Allow => {
                match (hash, document) {
                    (Some(h), Some(d)) => {
                        // APQ: verify the pair, register on first sight.
                        if hash_document(d.trim()) != h {
                            return Err(AppError::Validation(
                                "provided sha256 does not match query".into(),
                            ));
                        }
                        Ok(self.register(d).await?.document)
                    },
                    (Some(h), None) => self.get(h).await?.map(|p| p.document).ok_or_else(|| {
                        AppError::NotFound(format!("persisted query not found: {h}"))
                    }),
                    (None, Some(d)) => Ok(d.to_string()),
                    (None, None) => Err(AppError::Validation(
                        "query or persisted-query hash required".into(),
                    )),
                }
            },

            EnforcementMode::Enforce => {
                let h = hash.ok_or_else(|| {
                    AppError::Validation(
                        "trusted-documents mode: only registered persisted queries may execute"
                            .into(),
                    )
                })?;
                self.get(h)
                    .await?
                    .map(|p| p.document)
                    .ok_or_else(|| AppError::NotFound(format!("persisted query not found: {h}")))
            },
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_db::InMemoryBackend;

    const DOC: &str = "query Hello { hello }";

    fn store(mode: EnforcementMode) -> PersistedQueryStore {
        PersistedQueryStore::new(Arc::new(InMemoryBackend::new()), mode)
    }

    #[test]
    fn hash_is_stable_sha256_hex() {
        let h = hash_document(DOC);
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_document(DOC));
        assert_ne!(h, hash_document("query Other { other }"));
    }

    #[tokio::test]
    async fn register_and_get_roundtrip() {
        let s = store(EnforcementMode::Allow);
        let rec = s.register(DOC).await.unwrap();
        let found = s.get(&rec.hash).await.unwrap().unwrap();
        assert_eq!(found.document, DOC);
        assert_eq!(found.hash, rec.hash);
    }

    #[tokio::test]
    async fn register_rejects_empty_document() {
        let s = store(EnforcementMode::Allow);
        assert!(s.register("   ").await.is_err());
    }

    #[tokio::test]
    async fn enforce_mode_rejects_raw_documents() {
        let s = store(EnforcementMode::Enforce);
        assert!(s.resolve(None, Some(DOC)).await.is_err());
    }

    #[tokio::test]
    async fn enforce_mode_executes_registered_hash_only() {
        let s = store(EnforcementMode::Enforce);
        let rec = s.register(DOC).await.unwrap();
        assert_eq!(s.resolve(Some(&rec.hash), None).await.unwrap(), DOC);
        assert!(s.resolve(Some("deadbeef"), None).await.is_err());
    }

    #[tokio::test]
    async fn allow_mode_auto_registers_apq_pair() {
        let s = store(EnforcementMode::Allow);
        let h = hash_document(DOC);
        // First request: hash + document → registers.
        assert_eq!(s.resolve(Some(&h), Some(DOC)).await.unwrap(), DOC);
        // Second request: hash only → served from the store.
        assert_eq!(s.resolve(Some(&h), None).await.unwrap(), DOC);
    }

    #[tokio::test]
    async fn allow_mode_rejects_mismatched_hash() {
        let s = store(EnforcementMode::Allow);
        assert!(s.resolve(Some("0000"), Some(DOC)).await.is_err());
    }

    #[tokio::test]
    async fn disabled_mode_requires_raw_document() {
        let s = store(EnforcementMode::Disabled);
        assert_eq!(s.resolve(None, Some(DOC)).await.unwrap(), DOC);
        assert!(s.resolve(Some("abc"), None).await.is_err());
    }
}
