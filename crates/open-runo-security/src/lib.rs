//! `open-runo-security`: authentication, authorization, API key handling,
//! and rate limiting for the open-runo gateway (see README section 10).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod oidc;
pub mod rbac;

use chrono::{DateTime, Utc};
use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub key: String,
    pub owner: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

impl ApiKey {
    /// Validates the key against the current time, returning a descriptive
    /// error for expired/revoked keys instead of a bare boolean so callers
    /// can log/return a helpful message.
    pub fn validate(&self, now: DateTime<Utc>) -> Result<()> {
        if self.revoked {
            return Err(AppError::Validation(format!("API key for '{}' has been revoked", self.owner)));
        }
        if let Some(expires_at) = self.expires_at {
            if now >= expires_at {
                return Err(AppError::Validation(format!("API key for '{}' expired at {expires_at}", self.owner)));
            }
        }
        Ok(())
    }
}

/// Backend-agnostic rate limiting: [`RateLimiter`] (in-process, per-instance
/// memory) and [`redis_backend::RedisRateLimiter`] (shared across instances
/// behind a load balancer, `redis-backend` Cargo feature) both implement
/// this so callers (`open-runo-router::middleware_hyper::
/// with_shared_rate_limit`) can be written against either without knowing
/// which. Async because the Redis-backed implementation is a network call;
/// the in-memory implementation just doesn't `.await` anything internally.
///
/// Closes the "known gap" documented in `docs/deployment-scaling.md`: a
/// client's rate-limit budget used to be per-instance (each process's own
/// `HashMap`), not global, when running N instances behind a load balancer
/// — a client could trivially get N× its intended budget just by landing on
/// different backends. The Redis-backed implementation makes the budget
/// actually shared.
#[async_trait::async_trait]
pub trait RateLimit: Send + Sync + std::fmt::Debug {
    async fn check(&self, key: &str, now: DateTime<Utc>) -> Result<()>;
    async fn seconds_until_reset(&self, key: &str, now: DateTime<Utc>) -> i64;
}

/// A simple fixed-window rate limiter: `max_requests` per `window` per key.
/// Sufficient for Phase 1; a token-bucket/leaky-bucket limiter can replace
/// this without changing the [`RateLimiter::check`] call site. In-process
/// only — see [`RateLimit`]'s doc comment for the multi-instance caveat and
/// [`redis_backend::RedisRateLimiter`] for the shared alternative.
#[derive(Debug)]
pub struct RateLimiter {
    max_requests: u32,
    window: chrono::Duration,
    windows: Mutex<HashMap<String, (DateTime<Utc>, u32)>>,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window: chrono::Duration) -> Self {
        Self {
            max_requests,
            window,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `Ok(())` if `key` is still within its rate limit at `now`,
    /// otherwise `Err(AppError::Validation(..))`. Also records the request.
    pub fn check(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let entry = windows.entry(key.to_string()).or_insert((now, 0));
        if now.signed_duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }

        if entry.1 >= self.max_requests {
            return Err(AppError::Validation(format!(
                "rate limit exceeded for '{key}': {} requests per {:?}",
                self.max_requests, self.window
            )));
        }

        entry.1 += 1;
        Ok(())
    }

    /// Whole seconds until `key`'s current window resets, for a
    /// `Retry-After` response header when [`Self::check`] rejects a
    /// request. `0` if `key` has no active window (the next request would
    /// start a fresh one and succeed immediately).
    pub fn seconds_until_reset(&self, key: &str, now: DateTime<Utc>) -> i64 {
        let windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match windows.get(key) {
            Some((window_start, _)) => (self.window - now.signed_duration_since(*window_start))
                .num_seconds()
                .max(0),
            None => 0,
        }
    }
}

#[async_trait::async_trait]
impl RateLimit for RateLimiter {
    async fn check(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        RateLimiter::check(self, key, now)
    }
    async fn seconds_until_reset(&self, key: &str, now: DateTime<Utc>) -> i64 {
        RateLimiter::seconds_until_reset(self, key, now)
    }
}

#[cfg(feature = "redis-backend")]
pub mod redis_backend {
    //! Redis-backed [`RateLimit`] implementation: the same fixed-window
    //! semantics as [`RateLimiter`], but the window counters live in Redis
    //! instead of process memory, so every instance behind a load balancer
    //! shares one budget per key instead of getting its own.
    use super::{RateLimit, Result};
    use chrono::{DateTime, Utc};
    use open_runo_core::AppError;

    /// Atomically increment `key`'s counter and, only on the very first
    /// increment of a fresh window, set its expiry to the window length —
    /// this is the same "fixed window" semantics as [`super::RateLimiter`]
    /// (a burst right at a window boundary can allow up to `2×max_requests`
    /// in a short span, a known fixed-window tradeoff, not a bug introduced
    /// here). Done as a single Lua script (`EVAL`) so the increment and the
    /// conditional `EXPIRE` are atomic against concurrent requests from
    /// other instances — two plain `INCR` + `EXPIRE` commands would have a
    /// race where a slow request's `EXPIRE` overwrites a fast request's
    /// already-ticking TTL, silently extending the window forever.
    const INCR_WITH_EXPIRY_SCRIPT: &str = r#"
        local count = redis.call('INCR', KEYS[1])
        if count == 1 then
            redis.call('EXPIRE', KEYS[1], ARGV[1])
        end
        return count
    "#;

    pub struct RedisRateLimiter {
        manager: redis::aio::ConnectionManager,
        max_requests: u32,
        window_secs: i64,
        /// Namespaces this limiter's keys from anything else sharing the
        /// same Redis instance (e.g. `open-runo-cache`'s TTL cache,
        /// `edfs`'s Pub/Sub channel).
        key_prefix: &'static str,
    }

    impl std::fmt::Debug for RedisRateLimiter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RedisRateLimiter")
                .field("max_requests", &self.max_requests)
                .field("window_secs", &self.window_secs)
                .finish_non_exhaustive()
        }
    }

    impl RedisRateLimiter {
        pub async fn connect(redis_url: &str, max_requests: u32, window_secs: i64) -> Result<Self> {
            let client = redis::Client::open(redis_url)
                .map_err(|e| AppError::Internal(format!("RedisRateLimiter: invalid URL: {e}")))?;
            let manager = redis::aio::ConnectionManager::new(client)
                .await
                .map_err(|e| AppError::Internal(format!("RedisRateLimiter: connect failed: {e}")))?;
            Ok(Self { manager, max_requests, window_secs, key_prefix: "open-runo:ratelimit:" })
        }

        fn redis_key(&self, key: &str) -> String {
            format!("{}{key}", self.key_prefix)
        }
    }

    #[async_trait::async_trait]
    impl RateLimit for RedisRateLimiter {
        async fn check(&self, key: &str, _now: DateTime<Utc>) -> Result<()> {
            let mut conn = self.manager.clone();
            let count: u32 = redis::Script::new(INCR_WITH_EXPIRY_SCRIPT)
                .key(self.redis_key(key))
                .arg(self.window_secs)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| AppError::Internal(format!("RedisRateLimiter: script failed: {e}")))?;

            if count > self.max_requests {
                return Err(AppError::Validation(format!(
                    "rate limit exceeded for '{key}': {} requests per {}s",
                    self.max_requests, self.window_secs
                )));
            }
            Ok(())
        }

        async fn seconds_until_reset(&self, key: &str, _now: DateTime<Utc>) -> i64 {
            let mut conn = self.manager.clone();
            redis::cmd("TTL")
                .arg(self.redis_key(key))
                .query_async::<i64>(&mut conn)
                .await
                .unwrap_or(-1)
                .max(0)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Requires a real, reachable Redis instance
        /// (`OPEN_RUNO_TEST_REDIS_URL`, e.g. `redis://127.0.0.1:6379/`) --
        /// `#[ignore]`d by default like this workspace's other
        /// live-external-service tests (ClickHouse, PostgreSQL). Run
        /// explicitly: `cargo test -p open-runo-security --features
        /// redis-backend -- --ignored --nocapture`.
        #[tokio::test]
        #[ignore = "requires a live Redis instance reachable via OPEN_RUNO_TEST_REDIS_URL"]
        async fn shared_budget_is_enforced_across_two_independent_limiter_instances() {
            let url = std::env::var("OPEN_RUNO_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".to_string());
            let key = format!("test-key-{}", uuid_like_suffix());

            // Two separate `RedisRateLimiter`s, simulating two different
            // process instances behind a load balancer -- if the budget
            // were per-instance (the bug this fixes), each would allow
            // `max_requests` independently, for 2x the intended total.
            let limiter_a = RedisRateLimiter::connect(&url, 3, 60).await.expect("connect A");
            let limiter_b = RedisRateLimiter::connect(&url, 3, 60).await.expect("connect B");

            let now = Utc::now();
            assert!(limiter_a.check(&key, now).await.is_ok(), "request 1 (via A)");
            assert!(limiter_b.check(&key, now).await.is_ok(), "request 2 (via B)");
            assert!(limiter_a.check(&key, now).await.is_ok(), "request 3 (via A)");
            // Budget of 3 is now exhausted -- request 4, even via the
            // *other* instance, must be rejected. This is the actual
            // cross-instance sharing behavior under test.
            assert!(
                limiter_b.check(&key, now).await.is_err(),
                "request 4 (via B) should be rejected: the budget is shared, not per-instance"
            );

            let retry_after = limiter_a.seconds_until_reset(&key, now).await;
            assert!(retry_after > 0 && retry_after <= 60, "retry_after should be within the 60s window, got {retry_after}");
        }

        fn uuid_like_suffix() -> String {
            format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
        }
    }
}


/// Fine-grained token-bucket rate limiter (Cosmo paid-tier parity).
///
/// Each key gets a bucket of `capacity` tokens refilled continuously at
/// `refill_per_sec`. One token is consumed per request, so short bursts up
/// to `capacity` are allowed while the sustained rate converges to
/// `refill_per_sec`. Per-key overrides support different budgets per
/// API key / operation (`with_override`).
#[derive(Debug)]
pub struct TokenBucketLimiter {
    capacity: f64,
    refill_per_sec: f64,
    overrides: HashMap<String, (f64, f64)>,
    buckets: Mutex<HashMap<String, (f64, DateTime<Utc>)>>,
}

impl TokenBucketLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            capacity: f64::from(capacity),
            refill_per_sec,
            overrides: HashMap::new(),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Give `key` its own budget, independent of the default.
    #[must_use]
    pub fn with_override(mut self, key: impl Into<String>, capacity: u32, refill_per_sec: f64) -> Self {
        self.overrides.insert(key.into(), (f64::from(capacity), refill_per_sec));
        self
    }

    fn budget(&self, key: &str) -> (f64, f64) {
        self.overrides
            .get(key)
            .copied()
            .unwrap_or((self.capacity, self.refill_per_sec))
    }

    /// Consume one token for `key` at `now`. Returns
    /// `Err(AppError::Validation(..))` when the bucket is empty.
    pub fn try_acquire(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        let (capacity, refill) = self.budget(key);
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let (tokens, last) = buckets
            .get(key)
            .copied()
            .unwrap_or((capacity, now));

        let elapsed_secs = now.signed_duration_since(last).num_milliseconds() as f64 / 1000.0;
        let tokens = (tokens + elapsed_secs.max(0.0) * refill).min(capacity);

        if tokens < 1.0 {
            return Err(AppError::Validation(format!(
                "rate limit exceeded for '{key}': bucket empty (capacity {capacity}, {refill}/s)"
            )));
        }

        buckets.insert(key.to_string(), (tokens - 1.0, now));
        Ok(())
    }
}

// ── JWT ────────────────────────────────────────────────────────────────────

/// Claims embedded in an open-runo-issued JWT.
///
/// `sub` identifies the caller (service name or user id); `exp` is a Unix
/// timestamp (seconds) enforced by `jsonwebtoken`'s default validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Encodes and verifies HS256 JWTs for the `Authorization: Bearer <token>`
/// auth path (an alternative to `X-Api-Key`; see `open-runo-router::auth`).
#[derive(Debug, Clone)]
pub struct JwtCodec {
    secret: Vec<u8>,
}

impl JwtCodec {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self { secret: secret.into() }
    }

    /// Load the signing secret from `OPEN_RUNO_JWT_SECRET`. Returns `None`
    /// (JWT auth disabled) if the variable is unset — deployments that only
    /// want `X-Api-Key` auth do not need to set it.
    pub fn from_env() -> Option<Self> {
        std::env::var("OPEN_RUNO_JWT_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .map(Self::new)
    }

    /// Issue a signed token for `subject`, valid for `ttl` from now.
    pub fn encode(&self, subject: &str, roles: Vec<String>, ttl: chrono::Duration) -> Result<String> {
        let claims = Claims {
            sub: subject.to_string(),
            exp: (Utc::now() + ttl).timestamp(),
            roles,
        };
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(&self.secret),
        )
        .map_err(|e| AppError::Validation(format!("jwt encode failed: {e}")))
    }

    /// Verify a bearer token's signature and expiry, returning its claims.
    ///
    /// `leeway` is set to 0 so an expired token is rejected immediately
    /// (jsonwebtoken defaults to 60 s of clock-skew tolerance, which made
    /// short-lived tokens outlive their `exp`).
    pub fn decode(&self, token: &str) -> Result<Claims> {
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.leeway = 0;
        jsonwebtoken::decode::<Claims>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(&self.secret),
            &validation,
        )
        .map(|data| data.claims)
        .map_err(|e| AppError::Validation(format!("invalid or expired token: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn rejects_revoked_key() {
        let key = ApiKey { key: "k".into(), owner: "alice".into(), expires_at: None, revoked: true };
        assert!(key.validate(Utc::now()).is_err());
    }

    #[test]
    fn rejects_expired_key() {
        let key = ApiKey {
            key: "k".into(),
            owner: "alice".into(),
            expires_at: Some(Utc::now() - Duration::seconds(1)),
            revoked: false,
        };
        assert!(key.validate(Utc::now()).is_err());
    }

    #[test]
    fn accepts_valid_key() {
        let key = ApiKey {
            key: "k".into(),
            owner: "alice".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            revoked: false,
        };
        assert!(key.validate(Utc::now()).is_ok());
    }

    #[test]
    fn rate_limiter_blocks_after_max_requests() {
        let limiter = RateLimiter::new(2, Duration::minutes(1));
        let now = Utc::now();
        assert!(limiter.check("client-a", now).is_ok());
        assert!(limiter.check("client-a", now).is_ok());
        assert!(limiter.check("client-a", now).is_err());
    }

    #[test]
    fn rate_limiter_resets_after_window() {
        let limiter = RateLimiter::new(1, Duration::seconds(1));
        let t0 = Utc::now();
        assert!(limiter.check("client-b", t0).is_ok());
        assert!(limiter.check("client-b", t0).is_err());
        let t1 = t0 + Duration::seconds(2);
        assert!(limiter.check("client-b", t1).is_ok());
    }

    #[test]
    fn seconds_until_reset_counts_down_within_the_window() {
        let limiter = RateLimiter::new(1, Duration::seconds(10));
        let t0 = Utc::now();
        assert!(limiter.check("client-c", t0).is_ok());
        assert_eq!(limiter.seconds_until_reset("client-c", t0), 10);
        assert_eq!(limiter.seconds_until_reset("client-c", t0 + Duration::seconds(4)), 6);
    }

    #[test]
    fn seconds_until_reset_is_zero_for_an_unseen_key() {
        let limiter = RateLimiter::new(1, Duration::seconds(10));
        assert_eq!(limiter.seconds_until_reset("never-seen", Utc::now()), 0);
    }

    #[test]
    fn jwt_roundtrip() {
        let codec = JwtCodec::new("test-secret");
        let token = codec
            .encode("alice", vec!["admin".into()], Duration::hours(1))
            .unwrap();
        let claims = codec.decode(&token).unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.roles, vec!["admin".to_string()]);
    }

    #[test]
    fn jwt_rejects_expired_token() {
        let codec = JwtCodec::new("test-secret");
        let token = codec.encode("alice", vec![], Duration::seconds(-1)).unwrap();
        assert!(codec.decode(&token).is_err());
    }

    #[test]
    fn jwt_rejects_wrong_secret() {
        let codec_a = JwtCodec::new("secret-a");
        let codec_b = JwtCodec::new("secret-b");
        let token = codec_a.encode("alice", vec![], Duration::hours(1)).unwrap();
        assert!(codec_b.decode(&token).is_err());
    }

    // ── TokenBucketLimiter ─────────────────────────────────────────────

    #[test]
    fn token_bucket_allows_burst_up_to_capacity() {
        let limiter = TokenBucketLimiter::new(3, 1.0);
        let now = Utc::now();
        assert!(limiter.try_acquire("k", now).is_ok());
        assert!(limiter.try_acquire("k", now).is_ok());
        assert!(limiter.try_acquire("k", now).is_ok());
        assert!(limiter.try_acquire("k", now).is_err());
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let limiter = TokenBucketLimiter::new(1, 1.0); // 1 token/s
        let t0 = Utc::now();
        assert!(limiter.try_acquire("k", t0).is_ok());
        assert!(limiter.try_acquire("k", t0).is_err());
        let t1 = t0 + Duration::milliseconds(1500);
        assert!(limiter.try_acquire("k", t1).is_ok());
    }

    #[test]
    fn token_bucket_keys_are_independent() {
        let limiter = TokenBucketLimiter::new(1, 0.001);
        let now = Utc::now();
        assert!(limiter.try_acquire("a", now).is_ok());
        assert!(limiter.try_acquire("b", now).is_ok());
        assert!(limiter.try_acquire("a", now).is_err());
    }

    #[test]
    fn token_bucket_override_gives_key_its_own_budget() {
        let limiter = TokenBucketLimiter::new(1, 0.001).with_override("vip", 3, 0.001);
        let now = Utc::now();
        assert!(limiter.try_acquire("vip", now).is_ok());
        assert!(limiter.try_acquire("vip", now).is_ok());
        assert!(limiter.try_acquire("vip", now).is_ok());
        assert!(limiter.try_acquire("vip", now).is_err());
        assert!(limiter.try_acquire("normal", now).is_ok());
        assert!(limiter.try_acquire("normal", now).is_err());
    }
}
