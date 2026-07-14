//! Cookie-based session management — Poem-parity gap ("Cookie/セッション
//! 管理", `docs/poem-parity.md`), hand-rolled without an external session
//! crate to match this module's existing pattern (see `keyring.rs`,
//! `hyper_compat.rs`'s WebSocket/multipart sections).
//!
//! This is strictly **additive** to `X-Api-Key` auth (`auth_hyper.rs`):
//! the existing header-based flow (KeyGuardian, self-issue, the WASM
//! frontend, `open-runo-cli`) is untouched. Sessions exist for the case
//! where a browser-originated client wants the server to remember "who is
//! this" across requests via an `HttpOnly` cookie instead of attaching a
//! header by hand — e.g. a traditional multi-page admin panel, or a tool
//! that already holds an API key and wants a lighter-weight follow-up
//! credential for a burst of requests.
//!
//! Because cookies are sent automatically by the browser (unlike
//! `X-Api-Key`, which a script must set explicitly), session-authenticated
//! state-changing requests are vulnerable to CSRF unless guarded — see
//! [`SessionData::csrf_token`] and `middleware_hyper::with_session_or_api_key`.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;

/// Backend-agnostic session storage: [`SessionStore`] (in-process, the
/// default) and [`redis_backend::RedisSessionStore`] (shared across
/// instances behind a load balancer, `redis-session` Cargo feature) both
/// implement this. Async so a Redis-backed implementation's network calls
/// fit the same interface as the in-memory one.
///
/// **Why this matters for multiple instances**: without a shared backend,
/// a session cookie is only valid against the specific instance that
/// issued it — a load balancer without session affinity (sticky sessions)
/// will intermittently 401 a logged-in client as requests get routed to
/// different instances. `docs/deployment-scaling.md` documents this
/// tradeoff and the alternative (LB session affinity) for deployments
/// that don't opt into `RedisSessionStore`.
#[async_trait::async_trait]
pub trait SessionBackend: Send + Sync + std::fmt::Debug {
    /// Create a new session for `owner`/`roles`. Returns `(session_id,
    /// csrf_token)`, same contract as [`SessionStore::create`].
    async fn create(&self, owner: String, roles: Vec<String>) -> (String, String);
    async fn get(&self, session_id: &str) -> Option<SessionData>;
    async fn destroy(&self, session_id: &str);
}

/// Name of the cookie holding the opaque session id.
pub const SESSION_COOKIE_NAME: &str = "orn_session";

/// Header a session-authenticated client must echo back on state-changing
/// requests, carrying the CSRF token issued at login (double-submit
/// pattern — the token lives in the (non-`HttpOnly`-readable-by-JS-only)
/// response body at login time, not in the cookie itself, so a
/// cross-site request can send the cookie automatically but cannot know
/// the token to put in this header).
pub const CSRF_HEADER_NAME: &str = "x-csrf-token";

/// Session lifetime. `pub` so `handlers_hyper::session_login_handler` can
/// echo the same value back to the client without duplicating the number.
pub const SESSION_TTL_HOURS: i64 = 12;

/// A live session: which identity it belongs to and the CSRF token that
/// must accompany state-changing requests using it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionData {
    pub owner: String,
    pub roles: Vec<String>,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}

/// In-memory session store (mirrors `KeyGuardian`'s in-memory-registry
/// shape — sessions are ephemeral operational state, not durable data, so
/// they don't need to survive a restart any more than a poem/Redis session
/// backend's entries would need to for this deployment's scale).
#[derive(Debug, Default)]
pub struct SessionStore {
    sessions: Mutex<HashMap<String, SessionData>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new session for `owner`/`roles`. Returns the opaque session
    /// id (goes in the `Set-Cookie` header, never in a JSON body) and the
    /// CSRF token (goes in the JSON body, never in a cookie).
    pub fn create(&self, owner: String, roles: Vec<String>) -> (String, String) {
        let session_id = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let csrf_token = uuid::Uuid::new_v4().to_string();
        let expires_at = Utc::now() + chrono::Duration::hours(SESSION_TTL_HOURS);
        let data = SessionData { owner, roles, csrf_token: csrf_token.clone(), expires_at };
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.clone(), data);
        (session_id, csrf_token)
    }

    /// Look up a live session, lazily evicting it if it has expired
    /// (matches `KeyGuardian::verify`'s auto-clean-on-sight behavior for
    /// expired API keys).
    pub fn get(&self, session_id: &str) -> Option<SessionData> {
        let mut sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match sessions.get(session_id) {
            Some(data) if data.expires_at > Utc::now() => Some(data.clone()),
            Some(_) => {
                sessions.remove(session_id);
                None
            }
            None => None,
        }
    }

    /// Destroy a session (logout).
    pub fn destroy(&self, session_id: &str) {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }
}

#[async_trait::async_trait]
impl SessionBackend for SessionStore {
    async fn create(&self, owner: String, roles: Vec<String>) -> (String, String) {
        SessionStore::create(self, owner, roles)
    }
    async fn get(&self, session_id: &str) -> Option<SessionData> {
        SessionStore::get(self, session_id)
    }
    async fn destroy(&self, session_id: &str) {
        SessionStore::destroy(self, session_id)
    }
}

#[cfg(feature = "redis-session")]
pub mod redis_backend {
    //! Redis-backed [`SessionBackend`]: session data lives in Redis
    //! (JSON-serialized `SessionData`, with Redis's own `EX` TTL doing the
    //! expiry work `SessionStore::get`'s lazy-eviction check does
    //! in-process) instead of a single process's memory, so every
    //! instance behind a load balancer can validate a session cookie
    //! regardless of which instance originally issued it.
    use super::{SessionBackend, SessionData, SESSION_TTL_HOURS};
    use open_runo_core::{AppError, Result};

    pub struct RedisSessionStore {
        manager: redis::aio::ConnectionManager,
        key_prefix: &'static str,
    }

    impl std::fmt::Debug for RedisSessionStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RedisSessionStore").finish_non_exhaustive()
        }
    }

    impl RedisSessionStore {
        pub async fn connect(redis_url: &str) -> Result<Self> {
            let client = redis::Client::open(redis_url)
                .map_err(|e| AppError::Internal(format!("RedisSessionStore: invalid URL: {e}")))?;
            let manager = redis::aio::ConnectionManager::new(client)
                .await
                .map_err(|e| AppError::Internal(format!("RedisSessionStore: connect failed: {e}")))?;
            Ok(Self { manager, key_prefix: "open-runo:session:" })
        }

        fn redis_key(&self, session_id: &str) -> String {
            format!("{}{session_id}", self.key_prefix)
        }
    }

    #[async_trait::async_trait]
    impl SessionBackend for RedisSessionStore {
        async fn create(&self, owner: String, roles: Vec<String>) -> (String, String) {
            let session_id = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
            let csrf_token = uuid::Uuid::new_v4().to_string();
            let expires_at = chrono::Utc::now() + chrono::Duration::hours(SESSION_TTL_HOURS);
            let data = SessionData { owner, roles, csrf_token: csrf_token.clone(), expires_at };

            // Best-effort: if Redis is briefly unreachable, the caller
            // still gets a session_id/csrf_token back (matching the
            // in-memory store's infallible `create` signature), but the
            // session won't actually validate on the next request --
            // logged in this codebase's usual tracing::warn! style
            // elsewhere rather than surfaced as an error here, since
            // `create`'s signature (mirroring `SessionStore::create`)
            // has no `Result` to propagate one through.
            if let Ok(json) = serde_json::to_string(&data) {
                let mut conn = self.manager.clone();
                let ttl_secs = (SESSION_TTL_HOURS * 3600) as u64;
                if let Err(e) = redis::cmd("SET")
                    .arg(self.redis_key(&session_id))
                    .arg(json)
                    .arg("EX")
                    .arg(ttl_secs)
                    .query_async::<()>(&mut conn)
                    .await
                {
                    tracing::warn!(error = %e, "RedisSessionStore: failed to persist new session");
                }
            }

            (session_id, csrf_token)
        }

        async fn get(&self, session_id: &str) -> Option<SessionData> {
            let mut conn = self.manager.clone();
            let json: Option<String> = redis::cmd("GET")
                .arg(self.redis_key(session_id))
                .query_async(&mut conn)
                .await
                .ok()?;
            let data: SessionData = serde_json::from_str(&json?).ok()?;
            // Redis's own EX-based TTL is authoritative for expiry (the
            // key simply won't exist anymore past expiry), but this
            // extra check guards against clock skew between instances
            // that wrote/read the session.
            (data.expires_at > chrono::Utc::now()).then_some(data)
        }

        async fn destroy(&self, session_id: &str) {
            let mut conn = self.manager.clone();
            let result: std::result::Result<(), redis::RedisError> =
                redis::cmd("DEL").arg(self.redis_key(session_id)).query_async(&mut conn).await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "RedisSessionStore: failed to delete session");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Requires a real, reachable Redis instance
        /// (`OPEN_RUNO_TEST_REDIS_URL`) -- `#[ignore]`d like this
        /// workspace's other live-external-service tests. Run explicitly:
        /// `cargo test -p open-runo-router --features redis-session --
        /// --ignored --nocapture`.
        #[tokio::test]
        #[ignore = "requires a live Redis instance reachable via OPEN_RUNO_TEST_REDIS_URL"]
        async fn a_session_created_via_one_store_is_readable_via_a_second_independent_store() {
            let url = std::env::var("OPEN_RUNO_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".to_string());

            // Two separate `RedisSessionStore`s, simulating two different
            // process instances behind a load balancer -- the actual
            // cross-instance behavior under test.
            let store_a = RedisSessionStore::connect(&url).await.expect("connect A");
            let store_b = RedisSessionStore::connect(&url).await.expect("connect B");

            let (session_id, csrf_token) = store_a.create("dave".to_string(), vec!["developer".to_string()]).await;

            let data = store_b
                .get(&session_id)
                .await
                .expect("session created via store A should be visible via store B");
            assert_eq!(data.owner, "dave");
            assert_eq!(data.csrf_token, csrf_token);

            store_b.destroy(&session_id).await;
            assert!(
                store_a.get(&session_id).await.is_none(),
                "destroying via store B should be visible via store A"
            );
        }
    }
}

/// Parse the `Cookie:` request header for [`SESSION_COOKIE_NAME`]'s value.
pub fn session_id_from_cookie_header(headers: &hyper::HeaderMap) -> Option<String> {
    let raw = headers.get(hyper::header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let pair = pair.trim();
        let (name, value) = pair.split_once('=')?;
        (name == SESSION_COOKIE_NAME).then(|| value.to_string())
    })
}

/// Build the `Set-Cookie` header value for a freshly created session.
/// `HttpOnly` (never readable from JS, defeating XSS-driven cookie theft)
/// + `SameSite=Strict` (the cookie itself is never sent cross-site at all,
/// which is a stronger primitive than CSRF tokens alone — the token is a
/// defense-in-depth layer for browsers/proxies that don't honor
/// `SameSite`, and for `SameSite=Lax`-only environments).
pub fn set_cookie_header(session_id: &str) -> String {
    format!(
        "{SESSION_COOKIE_NAME}={session_id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_TTL_HOURS * 3600
    )
}

/// Build the `Set-Cookie` header value that clears the session cookie
/// (logout).
pub fn clear_cookie_header() -> String {
    format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::HeaderMap;

    #[test]
    fn create_then_get_returns_matching_session() {
        let store = SessionStore::new();
        let (session_id, csrf_token) = store.create("alice".to_string(), vec!["developer".to_string()]);
        let data = store.get(&session_id).expect("session should exist");
        assert_eq!(data.owner, "alice");
        assert_eq!(data.roles, vec!["developer".to_string()]);
        assert_eq!(data.csrf_token, csrf_token);
    }

    #[test]
    fn get_returns_none_for_unknown_session() {
        let store = SessionStore::new();
        assert!(store.get("no-such-session").is_none());
    }

    #[test]
    fn destroy_removes_the_session() {
        let store = SessionStore::new();
        let (session_id, _) = store.create("bob".to_string(), vec![]);
        assert!(store.get(&session_id).is_some());
        store.destroy(&session_id);
        assert!(store.get(&session_id).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn expired_session_is_lazily_evicted() {
        let store = SessionStore::new();
        let (session_id, _) = store.create("carol".to_string(), vec![]);
        // Force expiry without waiting SESSION_TTL_HOURS in a test.
        store
            .sessions
            .lock()
            .unwrap()
            .get_mut(&session_id)
            .unwrap()
            .expires_at = Utc::now() - chrono::Duration::seconds(1);
        assert!(store.get(&session_id).is_none());
        assert_eq!(store.len(), 0, "expired session should have been evicted on lookup");
    }

    #[test]
    fn session_id_from_cookie_header_finds_the_right_cookie_among_several() {
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::COOKIE,
            "other=1; orn_session=abc123; third=xyz".parse().unwrap(),
        );
        assert_eq!(session_id_from_cookie_header(&headers), Some("abc123".to_string()));
    }

    #[test]
    fn session_id_from_cookie_header_none_when_missing() {
        let headers = HeaderMap::new();
        assert_eq!(session_id_from_cookie_header(&headers), None);

        let mut headers = HeaderMap::new();
        headers.insert(hyper::header::COOKIE, "other=1".parse().unwrap());
        assert_eq!(session_id_from_cookie_header(&headers), None);
    }

    #[test]
    fn set_cookie_header_is_http_only_and_same_site_strict() {
        let header = set_cookie_header("abc123");
        assert!(header.contains("orn_session=abc123"));
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("SameSite=Strict"));
    }

    #[test]
    fn clear_cookie_header_expires_immediately() {
        let header = clear_cookie_header();
        assert!(header.contains("Max-Age=0"));
    }
}
