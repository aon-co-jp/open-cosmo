//! Poem-free auth check, extracted from `auth.rs`'s `ApiKeyAuth` (a
//! `poem::Middleware` implementation) into a plain function so
//! `hyper_compat::Handler`s can call it directly without depending on
//! poem's `Endpoint`/`Middleware` traits.
//!
//! Scope for this first pass: **`X-Api-Key` only**, mirroring the
//! `ApiKeyAuth` code path used by nearly all existing tests (KeyGuardian
//! verify → RegistryEmpty/Ok pass, everything else 401). JWT bearer, OIDC,
//! SCIM static token, and RBAC are intentionally deferred — see CLAUDE.md
//! HANDOFF for the follow-up plan; routes that need them stay on the poem
//! stack until those are ported too.

use crate::keyring::{KeyDecision, KeyGuardian};
use crate::session::{self, SessionBackend, CSRF_HEADER_NAME};
use hyper::{HeaderMap, Method, StatusCode};
use std::sync::Arc;

/// Checks the `X-Api-Key` header against `guardian`. Returns `Ok(())` when
/// the request may proceed, `Err(status)` otherwise (401 for missing/bad
/// key, matching `ApiKeyAuthEndpoint::call`'s behavior for the API-key path).
pub async fn check_api_key(
    headers: &HeaderMap,
    guardian: &Arc<KeyGuardian>,
) -> Result<(), StatusCode> {
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();

    if api_key.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    match guardian.verify(api_key, chrono::Utc::now()).await {
        KeyDecision::RegistryEmpty | KeyDecision::Ok { .. } => Ok(()),
        KeyDecision::Rejected | KeyDecision::Suspended => Err(StatusCode::UNAUTHORIZED),
    }
}

/// The identity established for a request, regardless of which mechanism
/// produced it.
#[derive(Debug, Clone)]
pub struct Actor {
    pub owner: String,
    pub roles: Vec<String>,
    /// `true` if this identity came from a session cookie rather than
    /// `X-Api-Key` — callers that care about CSRF exposure (e.g. deciding
    /// whether to log a warning) can branch on this.
    pub via_session: bool,
}

/// Poem-parity: Cookie/session management, additive to `X-Api-Key`
/// (`check_api_key` above is untouched and remains the primary path for
/// the WASM frontend, `open-runo-cli`, and every existing test). Accepts
/// **either**:
/// - `X-Api-Key`, verified via `guardian` exactly like `check_api_key`, or
/// - a valid `orn_session` cookie (see `session.rs`), in which case a
///   state-changing request (`POST`/`PUT`/`PATCH`/`DELETE`) must also carry
///   a matching `X-CSRF-Token` header (double-submit CSRF defense — a
///   cross-site request can make the browser attach the cookie
///   automatically, but cannot read the token to put in the header) or
///   the request is rejected with `403` even though the session itself
///   is valid.
///
/// `X-Api-Key` is checked first and, if present, is authoritative — a
/// request that sends both a (possibly stale) session cookie and a fresh
/// API key is not penalized for CSRF just because a cookie happens to be
/// present.
pub async fn authenticate_with_session(
    headers: &HeaderMap,
    method: &Method,
    guardian: &Arc<KeyGuardian>,
    sessions: &Arc<dyn SessionBackend>,
) -> Result<Actor, StatusCode> {
    let api_key = headers.get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("").trim();
    if !api_key.is_empty() {
        return match guardian.verify(api_key, chrono::Utc::now()).await {
            KeyDecision::RegistryEmpty => {
                Ok(Actor { owner: "dev".to_string(), roles: vec![], via_session: false })
            }
            KeyDecision::Ok { owner, roles } => Ok(Actor { owner, roles, via_session: false }),
            KeyDecision::Rejected | KeyDecision::Suspended => Err(StatusCode::UNAUTHORIZED),
        };
    }

    let session_id = session::session_id_from_cookie_header(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let data = sessions.get(&session_id).await.ok_or(StatusCode::UNAUTHORIZED)?;

    let is_state_changing =
        matches!(*method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE);
    if is_state_changing {
        let provided = headers.get(CSRF_HEADER_NAME).and_then(|v| v.to_str().ok()).unwrap_or("");
        if provided.is_empty() || provided != data.csrf_token {
            return Err(StatusCode::FORBIDDEN);
        }
    }

    Ok(Actor { owner: data.owner, roles: data.roles, via_session: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::GuardianConfig;
    use crate::state::AppState;
    use hyper::header::HeaderValue;

    fn guardian() -> Arc<KeyGuardian> {
        let state = AppState::new();
        Arc::new(KeyGuardian::new(Arc::clone(&state.db), GuardianConfig::from_env()))
    }

    #[tokio::test]
    async fn missing_header_is_rejected() {
        let headers = HeaderMap::new();
        let result = check_api_key(&headers, &guardian()).await;
        assert_eq!(result, Err(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn empty_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static(""));
        let result = check_api_key(&headers, &guardian()).await;
        assert_eq!(result, Err(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn any_nonempty_key_passes_when_registry_is_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("anything-goes"));
        let result = check_api_key(&headers, &guardian()).await;
        assert_eq!(result, Ok(()));
    }

    fn sessions() -> Arc<dyn SessionBackend> {
        Arc::new(crate::session::SessionStore::new())
    }

    #[tokio::test]
    async fn authenticate_with_session_prefers_api_key_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("anything-goes"));
        let actor = authenticate_with_session(&headers, &Method::GET, &guardian(), &sessions())
            .await
            .expect("api key should authenticate");
        assert!(!actor.via_session);
    }

    #[tokio::test]
    async fn authenticate_with_session_rejects_when_nothing_provided() {
        let headers = HeaderMap::new();
        let result = authenticate_with_session(&headers, &Method::GET, &guardian(), &sessions()).await;
        assert_eq!(result.unwrap_err(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn authenticate_with_session_rejects_unknown_session_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(hyper::header::COOKIE, "orn_session=no-such-session".parse().unwrap());
        let result = authenticate_with_session(&headers, &Method::GET, &guardian(), &sessions()).await;
        assert_eq!(result.unwrap_err(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn authenticate_with_session_get_needs_no_csrf_token() {
        let store = sessions();
        let (session_id, _csrf) = store.create("alice".to_string(), vec!["developer".to_string()]).await;
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::COOKIE,
            format!("orn_session={session_id}").parse().unwrap(),
        );
        let actor = authenticate_with_session(&headers, &Method::GET, &guardian(), &store)
            .await
            .expect("valid session should authenticate a GET without a CSRF token");
        assert_eq!(actor.owner, "alice");
        assert!(actor.via_session);
    }

    #[tokio::test]
    async fn authenticate_with_session_post_without_csrf_token_is_forbidden() {
        let store = sessions();
        let (session_id, _csrf) = store.create("alice".to_string(), vec![]).await;
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::COOKIE,
            format!("orn_session={session_id}").parse().unwrap(),
        );
        let result = authenticate_with_session(&headers, &Method::POST, &guardian(), &store).await;
        assert_eq!(result.unwrap_err(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn authenticate_with_session_post_with_wrong_csrf_token_is_forbidden() {
        let store = sessions();
        let (session_id, _csrf) = store.create("alice".to_string(), vec![]).await;
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::COOKIE,
            format!("orn_session={session_id}").parse().unwrap(),
        );
        headers.insert(CSRF_HEADER_NAME, "wrong-token".parse().unwrap());
        let result = authenticate_with_session(&headers, &Method::POST, &guardian(), &store).await;
        assert_eq!(result.unwrap_err(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn authenticate_with_session_post_with_correct_csrf_token_succeeds() {
        let store = sessions();
        let (session_id, csrf) = store.create("alice".to_string(), vec!["developer".to_string()]).await;
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::COOKIE,
            format!("orn_session={session_id}").parse().unwrap(),
        );
        headers.insert(CSRF_HEADER_NAME, csrf.parse().unwrap());
        let actor = authenticate_with_session(&headers, &Method::POST, &guardian(), &store)
            .await
            .expect("matching CSRF token should authenticate the POST");
        assert_eq!(actor.owner, "alice");
        assert!(actor.via_session);
    }
}
