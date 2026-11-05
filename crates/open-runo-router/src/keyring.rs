//! `KeyGuardian` — self-operating API-key registry (no human management).
//!
//! Keys are the "spare keys" of the system: even a REST-free, GraphQL-only
//! deployment needs to reject unknown callers. What operators should NOT
//! need to do is issue, watch, and revoke those keys by hand. KeyGuardian
//! automates the whole lifecycle:
//!
//! - **auto-issue**: SCIM provisioning (`POST /scim/v2/Users`) mints a key
//!   bound to the user's RBAC roles — the IdP drives key creation.
//! - **auto-revoke**: deactivating or deleting the user kills the key.
//! - **auto-clean**: expired keys are dropped lazily on sight.
//! - **auto-defend**: per-key request rates are learned (EWMA, same
//!   self-learning approach as the cache predictor — no external LLM).
//!   A key that suddenly runs far hotter than its own history (stolen key,
//!   runaway script) is quarantined automatically and recovers after a
//!   cooldown; every action lands in the audit log via the caller.
//!
//! Keys are stored **hashed** (SHA-256) in the `api_keys` table, which the
//! DUAL DATABASE routes to PostgreSQL. The plaintext is shown exactly once
//! at issue time.
//!
//! Zero-config default: while the registry is empty, verification returns
//! [`KeyDecision::RegistryEmpty`] and the auth middleware keeps its
//! permissive dev behaviour. The moment the first key is issued, unknown
//! keys are rejected — production hardens itself automatically.

use chrono::{DateTime, Duration, Utc};
use open_runo_core::{AppError, Result};
use open_runo_db::DbBackend;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;

/// Logical table (DUAL routing: PostgreSQL).
pub const TABLE: &str = "api_keys";

/// EWMA smoothing for the learned request rate.
const ALPHA: f64 = 0.3;

/// A registered key (stored under its SHA-256 hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRecord {
    pub owner: String,
    pub roles: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

/// Verification outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDecision {
    /// No keys registered yet → caller decides (dev mode stays permissive).
    RegistryEmpty,
    /// Key verified; RBAC roles attached.
    Ok { owner: String, roles: Vec<String> },
    /// Unknown / revoked / expired key.
    Rejected,
    /// Anomalous usage detected → temporarily quarantined.
    Suspended,
}

#[derive(Debug, Clone)]
pub struct GuardianConfig {
    /// Suspend when the observed rate exceeds the learned rate × this factor.
    pub anomaly_factor: f64,
    /// Requests to observe before anomaly detection arms itself.
    pub warmup_requests: u64,
    /// Quarantine length after an anomaly.
    pub cooldown: Duration,
}

impl Default for GuardianConfig {
    fn default() -> Self {
        Self {
            anomaly_factor: 20.0,
            warmup_requests: 50,
            cooldown: Duration::minutes(5),
        }
    }
}

impl GuardianConfig {
    /// `OPEN_RUNO_KEY_ANOMALY_FACTOR` / `OPEN_RUNO_KEY_WARMUP_REQUESTS` /
    /// `OPEN_RUNO_KEY_COOLDOWN_SECS`.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            anomaly_factor: std::env::var("OPEN_RUNO_KEY_ANOMALY_FACTOR")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.anomaly_factor),
            warmup_requests: std::env::var("OPEN_RUNO_KEY_WARMUP_REQUESTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.warmup_requests),
            cooldown: Duration::seconds(
                std::env::var("OPEN_RUNO_KEY_COOLDOWN_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(d.cooldown.num_seconds()),
            ),
        }
    }
}

/// Per-key learned usage (in-memory; rebuilt after restart).
#[derive(Debug, Default, Clone)]
struct Usage {
    requests: u64,
    /// EWMA of seconds between requests.
    interval_secs: Option<f64>,
    last_seen: Option<DateTime<Utc>>,
    suspended_until: Option<DateTime<Utc>>,
}

/// The self-operating registry.
#[derive(Debug)]
pub struct KeyGuardian {
    db: std::sync::Arc<dyn DbBackend>,
    config: GuardianConfig,
    usage: Mutex<HashMap<String, Usage>>,
    /// Cached "is the registry non-empty?" flag (set on first issue/verify).
    known_nonempty: std::sync::atomic::AtomicBool,
}

/// SHA-256 hex of a plaintext key.
pub fn hash_key(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    hex::encode(h.finalize())
}

impl KeyGuardian {
    pub fn new(db: std::sync::Arc<dyn DbBackend>, config: GuardianConfig) -> Self {
        Self {
            db,
            config,
            usage: Mutex::new(HashMap::new()),
            known_nonempty: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Auto-issue a key for `owner` with `roles`. Returns the plaintext —
    /// the only moment it ever exists outside the caller's hands.
    pub async fn issue(
        &self,
        owner: &str,
        roles: Vec<String>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<String> {
        let plaintext = format!(
            "orn_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let record = KeyRecord {
            owner: owner.to_string(),
            roles,
            created_at: Utc::now(),
            expires_at,
            revoked: false,
        };
        let json = serde_json::to_string(&record)
            .map_err(|e| AppError::Internal(format!("serialize key record: {e}")))?;
        self.db.put(TABLE, &hash_key(&plaintext), &json).await?;
        self.known_nonempty
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(plaintext)
    }

    /// Auto-revoke every key belonging to `owner` (user deactivated/deleted).
    /// Returns how many keys were revoked.
    pub async fn revoke_owner(&self, owner: &str) -> Result<usize> {
        let mut revoked = 0;
        for rec in self.db.list(TABLE).await? {
            if let Ok(mut record) = sonic_rs::from_str::<KeyRecord>(&rec.value) {
                if record.owner == owner && !record.revoked {
                    record.revoked = true;
                    if let Ok(json) = serde_json::to_string(&record) {
                        self.db.put(TABLE, &rec.key, &json).await?;
                        revoked += 1;
                    }
                }
            }
        }
        Ok(revoked)
    }

    /// Verify a plaintext key, learn its usage, and auto-defend.
    pub async fn verify(&self, key: &str, now: DateTime<Utc>) -> KeyDecision {
        // Empty-registry fast path (dev mode).
        if !self
            .known_nonempty
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            match self.db.list(TABLE).await {
                Ok(records) if records.is_empty() => return KeyDecision::RegistryEmpty,
                Ok(_) => self
                    .known_nonempty
                    .store(true, std::sync::atomic::Ordering::Relaxed),
                Err(_) => return KeyDecision::RegistryEmpty,
            }
        }

        let hashed = hash_key(key);
        let raw = match self.db.get(TABLE, &hashed).await {
            Ok(Some(raw)) => raw,
            _ => return KeyDecision::Rejected,
        };
        let record: KeyRecord = match sonic_rs::from_str(&raw) {
            Ok(r) => r,
            Err(_) => return KeyDecision::Rejected,
        };

        if record.revoked {
            return KeyDecision::Rejected;
        }
        if let Some(expiry) = record.expires_at {
            if now >= expiry {
                // Auto-clean: expired keys are removed on sight.
                let _ = self.db.delete(TABLE, &hashed).await;
                return KeyDecision::Rejected;
            }
        }

        // ── Self-learning anomaly defence ────────────────────────────────
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let u = usage.entry(hashed).or_default();

        if let Some(until) = u.suspended_until {
            if now < until {
                return KeyDecision::Suspended;
            }
            u.suspended_until = None; // auto-recover after cooldown
        }

        if let Some(last) = u.last_seen {
            let gap = (now - last).num_milliseconds().max(0) as f64 / 1000.0;
            let learned = u.interval_secs.unwrap_or(gap);
            // Armed only after warm-up, and only when history says this key
            // is normally slow. A gap `anomaly_factor`× faster than learned
            // behaviour looks like a stolen key / runaway loop.
            if u.requests >= self.config.warmup_requests
                && learned > 0.0
                && gap > 0.0
                && learned / gap >= self.config.anomaly_factor
            {
                u.suspended_until = Some(now + self.config.cooldown);
                tracing::warn!(
                    owner = %record.owner,
                    "KeyGuardian: anomalous request rate — key quarantined"
                );
                return KeyDecision::Suspended;
            }
            u.interval_secs = Some(learned * (1.0 - ALPHA) + gap * ALPHA);
        }
        u.requests = u.requests.saturating_add(1);
        u.last_seen = Some(now);

        KeyDecision::Ok {
            owner: record.owner,
            roles: record.roles,
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_db::InMemoryBackend;
    use std::sync::Arc;

    fn guardian() -> KeyGuardian {
        KeyGuardian::new(
            Arc::new(InMemoryBackend::new()),
            GuardianConfig {
                anomaly_factor: 10.0,
                warmup_requests: 3,
                cooldown: Duration::seconds(60),
            },
        )
    }

    #[tokio::test]
    async fn empty_registry_stays_permissive() {
        let g = guardian();
        assert_eq!(
            g.verify("anything", Utc::now()).await,
            KeyDecision::RegistryEmpty
        );
    }

    #[tokio::test]
    async fn issue_verify_roundtrip_with_roles() {
        let g = guardian();
        let key = g
            .issue("alice", vec!["developer".into()], None)
            .await
            .unwrap();
        assert!(key.starts_with("orn_"));

        match g.verify(&key, Utc::now()).await {
            KeyDecision::Ok { owner, roles } => {
                assert_eq!(owner, "alice");
                assert_eq!(roles, vec!["developer".to_string()]);
            },
            other => panic!("expected Ok, got {other:?}"),
        }

        // Once a key exists, unknown keys are rejected (auto-hardening).
        assert_eq!(
            g.verify("wrong-key", Utc::now()).await,
            KeyDecision::Rejected
        );
    }

    #[tokio::test]
    async fn revoke_owner_kills_all_their_keys() {
        let g = guardian();
        let k1 = g.issue("bob", vec![], None).await.unwrap();
        let k2 = g.issue("bob", vec![], None).await.unwrap();
        let alice = g.issue("alice", vec![], None).await.unwrap();

        assert_eq!(g.revoke_owner("bob").await.unwrap(), 2);
        assert_eq!(g.verify(&k1, Utc::now()).await, KeyDecision::Rejected);
        assert_eq!(g.verify(&k2, Utc::now()).await, KeyDecision::Rejected);
        assert!(matches!(
            g.verify(&alice, Utc::now()).await,
            KeyDecision::Ok { .. }
        ));
    }

    #[tokio::test]
    async fn expired_keys_auto_clean() {
        let g = guardian();
        let key = g
            .issue("carol", vec![], Some(Utc::now() - Duration::seconds(1)))
            .await
            .unwrap();
        assert_eq!(g.verify(&key, Utc::now()).await, KeyDecision::Rejected);
        // Record was deleted, not just rejected.
        assert!(g.db.get(TABLE, &hash_key(&key)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn anomaly_suspends_then_auto_recovers() {
        let g = guardian();
        let key = g.issue("dave", vec![], None).await.unwrap();
        let t0 = Utc::now();

        // Warm-up: a steady request every 60 s teaches the normal rate.
        for i in 0..4 {
            let decision = g.verify(&key, t0 + Duration::seconds(60 * i)).await;
            assert!(
                matches!(decision, KeyDecision::Ok { .. }),
                "warmup {i}: {decision:?}"
            );
        }
        let after_warmup = t0 + Duration::seconds(180);

        // Sudden burst: next request 0.1 s later = 600× the learned rate.
        let burst = after_warmup + Duration::milliseconds(100);
        assert_eq!(g.verify(&key, burst).await, KeyDecision::Suspended);

        // Still quarantined during cooldown.
        assert_eq!(
            g.verify(&key, burst + Duration::seconds(10)).await,
            KeyDecision::Suspended
        );

        // Auto-recovery after the cooldown elapses.
        assert!(matches!(
            g.verify(&key, burst + Duration::seconds(61)).await,
            KeyDecision::Ok { .. }
        ));
    }
}
