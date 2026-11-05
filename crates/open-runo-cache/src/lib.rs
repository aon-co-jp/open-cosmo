//! `open-runo-cache`: TTL cache layer (Cosmo paid-tier cache-control parity).
//!
//! The gateway uses this to cache GraphQL **query** responses (operation-level
//! response cache); per-field `@cacheControl` directives layer on top later.
//!
//! Backends:
//! - [`InMemoryTtlCache`] — always available; per-process.
//! - `RedisCache` — behind the `redis-backend` feature; shared across
//!   replicas (Redis / KeyDB / DragonflyDB).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod predictor;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use open_runo_core::Result;
use std::collections::HashMap;
use std::sync::Mutex;

/// A string-keyed TTL cache.
#[async_trait]
pub trait Cache: Send + Sync + std::fmt::Debug {
    fn backend_name(&self) -> &'static str;

    /// Fetch `key` if present and not expired.
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Store `value` under `key` for `ttl`.
    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()>;

    /// Drop a single key (entity invalidation).
    async fn invalidate(&self, key: &str) -> Result<()>;

    /// Drop everything (schema-change invalidation).
    async fn clear(&self) -> Result<()>;
}

/// In-process TTL cache. Expired entries are dropped lazily on access and
/// swept opportunistically on writes. An optional entry cap guards against
/// memory exhaustion: when full, the entry closest to expiry is evicted.
#[derive(Debug, Default)]
pub struct InMemoryTtlCache {
    entries: Mutex<HashMap<String, (String, DateTime<Utc>)>>,
    max_entries: Option<usize>,
}

impl InMemoryTtlCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bound the cache to at most `max_entries` live entries (OOM guard).
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_entries: Some(max_entries.max(1)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (String, DateTime<Utc>)>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl Cache for InMemoryTtlCache {
    fn backend_name(&self) -> &'static str {
        "in-memory-ttl"
    }

    async fn get(&self, key: &str) -> Result<Option<String>> {
        let mut entries = self.lock();
        match entries.get(key) {
            Some((_, expires)) if *expires <= Utc::now() => {
                entries.remove(key);
                Ok(None)
            },
            Some((value, _)) => Ok(Some(value.clone())),
            None => Ok(None),
        }
    }

    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()> {
        let now = Utc::now();
        let mut entries = self.lock();
        // Opportunistic sweep keeps the map from growing unboundedly.
        entries.retain(|_, (_, expires)| *expires > now);
        // Capacity guard: evict the entry closest to expiry.
        if let Some(cap) = self.max_entries {
            while entries.len() >= cap && !entries.contains_key(key) {
                let victim = entries
                    .iter()
                    .min_by_key(|(_, (_, expires))| *expires)
                    .map(|(k, _)| k.clone());
                match victim {
                    Some(k) => {
                        entries.remove(&k);
                    },
                    None => break,
                }
            }
        }
        entries.insert(key.to_string(), (value.to_string(), now + ttl));
        Ok(())
    }

    async fn invalidate(&self, key: &str) -> Result<()> {
        self.lock().remove(key);
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        self.lock().clear();
        Ok(())
    }
}

// ── Redis backend (feature = "redis-backend") ──────────────────────────────

#[cfg(feature = "redis-backend")]
pub mod redis_backend {
    use super::*;
    use open_runo_core::AppError;

    /// Redis-backed TTL cache (Redis / KeyDB / DragonflyDB :6379).
    /// Uses `SET key value EX ttl` so expiry is server-side.
    #[derive(Clone)]
    pub struct RedisCache {
        manager: redis::aio::ConnectionManager,
        /// Key prefix so multiple open-runo deployments can share one Redis.
        prefix: String,
    }

    impl std::fmt::Debug for RedisCache {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RedisCache")
                .field("prefix", &self.prefix)
                .finish()
        }
    }

    impl RedisCache {
        pub async fn connect(url: &str, prefix: impl Into<String>) -> Result<Self> {
            let client = redis::Client::open(url)
                .map_err(|e| AppError::Internal(format!("redis client: {e}")))?;
            let manager = redis::aio::ConnectionManager::new(client)
                .await
                .map_err(|e| AppError::Internal(format!("redis connect: {e}")))?;
            Ok(Self {
                manager,
                prefix: prefix.into(),
            })
        }

        fn key(&self, key: &str) -> String {
            format!("{}:{key}", self.prefix)
        }
    }

    #[async_trait]
    impl Cache for RedisCache {
        fn backend_name(&self) -> &'static str {
            "redis"
        }

        async fn get(&self, key: &str) -> Result<Option<String>> {
            let mut conn = self.manager.clone();
            redis::cmd("GET")
                .arg(self.key(key))
                .query_async(&mut conn)
                .await
                .map_err(|e| AppError::Internal(format!("redis GET: {e}")))
        }

        async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()> {
            let mut conn = self.manager.clone();
            redis::cmd("SET")
                .arg(self.key(key))
                .arg(value)
                .arg("EX")
                .arg(ttl.num_seconds().max(1))
                .query_async::<()>(&mut conn)
                .await
                .map_err(|e| AppError::Internal(format!("redis SET: {e}")))
        }

        async fn invalidate(&self, key: &str) -> Result<()> {
            let mut conn = self.manager.clone();
            redis::cmd("DEL")
                .arg(self.key(key))
                .query_async::<()>(&mut conn)
                .await
                .map_err(|e| AppError::Internal(format!("redis DEL: {e}")))
        }

        async fn clear(&self) -> Result<()> {
            // Deliberately scoped: only this deployment's prefix is flushed.
            let mut conn = self.manager.clone();
            let keys: Vec<String> = redis::cmd("KEYS")
                .arg(format!("{}:*", self.prefix))
                .query_async(&mut conn)
                .await
                .map_err(|e| AppError::Internal(format!("redis KEYS: {e}")))?;
            if !keys.is_empty() {
                redis::cmd("DEL")
                    .arg(keys)
                    .query_async::<()>(&mut conn)
                    .await
                    .map_err(|e| AppError::Internal(format!("redis DEL: {e}")))?;
            }
            Ok(())
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get_roundtrip() {
        let cache = InMemoryTtlCache::new();
        cache.set("k", "v", Duration::seconds(60)).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap(), Some("v".to_string()));
        assert_eq!(cache.get("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn expired_entries_are_gone() {
        let cache = InMemoryTtlCache::new();
        cache
            .set("k", "v", Duration::milliseconds(-1))
            .await
            .unwrap();
        assert_eq!(cache.get("k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn overwrite_refreshes_value_and_ttl() {
        let cache = InMemoryTtlCache::new();
        cache.set("k", "v1", Duration::seconds(60)).await.unwrap();
        cache.set("k", "v2", Duration::seconds(60)).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap(), Some("v2".to_string()));
    }

    #[tokio::test]
    async fn invalidate_and_clear() {
        let cache = InMemoryTtlCache::new();
        cache.set("a", "1", Duration::seconds(60)).await.unwrap();
        cache.set("b", "2", Duration::seconds(60)).await.unwrap();

        cache.invalidate("a").await.unwrap();
        assert_eq!(cache.get("a").await.unwrap(), None);
        assert_eq!(cache.get("b").await.unwrap(), Some("2".to_string()));

        cache.clear().await.unwrap();
        assert_eq!(cache.get("b").await.unwrap(), None);
    }

    #[tokio::test]
    async fn write_sweeps_expired_entries() {
        let cache = InMemoryTtlCache::new();
        cache
            .set("old", "x", Duration::milliseconds(-1))
            .await
            .unwrap();
        cache.set("new", "y", Duration::seconds(60)).await.unwrap();
        assert_eq!(cache.lock().len(), 1); // "old" swept on write
    }

    #[tokio::test]
    async fn capacity_bound_evicts_soonest_expiring() {
        let cache = InMemoryTtlCache::with_capacity(2);
        cache
            .set("short", "1", Duration::seconds(10))
            .await
            .unwrap();
        cache
            .set("long", "2", Duration::seconds(1000))
            .await
            .unwrap();
        // Third insert: "short" (closest to expiry) is evicted.
        cache.set("new", "3", Duration::seconds(500)).await.unwrap();

        assert_eq!(cache.get("short").await.unwrap(), None);
        assert_eq!(cache.get("long").await.unwrap(), Some("2".to_string()));
        assert_eq!(cache.get("new").await.unwrap(), Some("3".to_string()));
    }
}
