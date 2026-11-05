//! OIDC SSO token validation (Cosmo Enterprise parity, shipped as OSS).
//!
//! Validates `Authorization: Bearer` tokens issued by an external identity
//! provider (Keycloak, Auth0, Entra ID, …): RS256 signature against the
//! provider's JWKS, plus `iss` / `aud` / `exp` checks.
//!
//! This module is deliberately offline: it consumes a JWKS **document**
//! (JSON string). Fetching `/.well-known/openid-configuration` and the
//! `jwks_uri` over HTTP is the deployment binary's job (startup or a
//! refresh task), keeping this crate free of an HTTP-client dependency.

use crate::Claims;
use open_runo_core::{AppError, Result};
use serde::Deserialize;
use std::collections::HashMap;

/// One JSON Web Key (only RSA signing keys are consumed).
#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Validates RS256 bearer tokens against a provider's JWKS.
pub struct OidcValidator {
    issuer: String,
    audience: Option<String>,
    /// kid → decoding key. Keys without a kid are stored under `""`.
    keys: HashMap<String, open_runo_jwt::RsaPublicKey>,
}

impl std::fmt::Debug for OidcValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcValidator")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl OidcValidator {
    /// Build from a JWKS JSON document (the body served at `jwks_uri`).
    pub fn from_jwks_json(
        issuer: impl Into<String>,
        audience: Option<String>,
        jwks_json: &str,
    ) -> Result<Self> {
        let set: JwkSet = sonic_rs::from_str(jwks_json)
            .map_err(|e| AppError::Validation(format!("invalid JWKS document: {e}")))?;

        let mut keys = HashMap::new();
        for jwk in set.keys {
            if jwk.kty != "RSA" {
                continue;
            }
            let (Some(n), Some(e)) = (&jwk.n, &jwk.e) else {
                continue;
            };
            let key = open_runo_jwt::RsaPublicKey::from_components(n, e)
                .map_err(|err| AppError::Validation(format!("invalid RSA JWK: {err}")))?;
            keys.insert(jwk.kid.unwrap_or_default(), key);
        }

        if keys.is_empty() {
            return Err(AppError::Validation(
                "JWKS contains no usable RSA signing keys".into(),
            ));
        }

        Ok(Self {
            issuer: issuer.into(),
            audience,
            keys,
        })
    }

    /// Load from env: `OPEN_RUNO_OIDC_ISSUER` + `OPEN_RUNO_OIDC_JWKS_FILE`
    /// (path to a JWKS JSON file) and optional `OPEN_RUNO_OIDC_AUDIENCE`.
    /// Returns `None` (OIDC disabled) when the vars are unset.
    pub fn from_env() -> Option<Result<Self>> {
        let issuer = std::env::var("OPEN_RUNO_OIDC_ISSUER")
            .ok()
            .filter(|s| !s.is_empty())?;
        let jwks_path = std::env::var("OPEN_RUNO_OIDC_JWKS_FILE")
            .ok()
            .filter(|s| !s.is_empty())?;
        let audience = std::env::var("OPEN_RUNO_OIDC_AUDIENCE")
            .ok()
            .filter(|s| !s.is_empty());

        Some(match std::fs::read_to_string(&jwks_path) {
            Ok(json) => Self::from_jwks_json(issuer, audience, &json),
            Err(e) => Err(AppError::Validation(format!(
                "cannot read JWKS file {jwks_path}: {e}"
            ))),
        })
    }

    /// Verify an RS256 bearer token, returning its claims.
    pub fn decode(&self, token: &str) -> Result<Claims> {
        let header = open_runo_jwt::decode_header(token)
            .map_err(|e| AppError::Validation(format!("invalid token header: {e}")))?;

        let key = match header.kid.as_deref() {
            Some(kid) => self.keys.get(kid),
            // No kid: unambiguous only when the provider has a single key.
            None if self.keys.len() == 1 => self.keys.values().next(),
            None => None,
        }
        .ok_or_else(|| {
            AppError::Validation(format!("no JWKS key matches token kid {:?}", header.kid))
        })?;

        let validation = open_runo_jwt::Validation {
            leeway: 0,
            issuer: Some(self.issuer.clone()),
            audience: self.audience.clone(),
        };

        open_runo_jwt::decode_rs256::<Claims>(token, key, &validation)
            .map_err(|e| AppError::Validation(format!("OIDC token rejected: {e}")))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PRIVATE_PEM: &str = include_str!("../testdata/test_rsa.pem");
    const JWKS: &str = include_str!("../testdata/test_jwks.json");
    const ISSUER: &str = "https://idp.example.com/realms/open-runo";

    fn sign(claims: &serde_json::Value, kid: Option<&str>) -> String {
        let der = open_runo_jwt::pem_to_der(PRIVATE_PEM).unwrap();
        open_runo_jwt::encode_rs256(claims, kid, &der).unwrap()
    }

    fn validator() -> OidcValidator {
        OidcValidator::from_jwks_json(ISSUER, None, JWKS).unwrap()
    }

    fn exp_in(secs: i64) -> i64 {
        (chrono::Utc::now() + chrono::Duration::seconds(secs)).timestamp()
    }

    #[test]
    fn accepts_valid_rs256_token() {
        let token = sign(
            &json!({ "sub": "alice", "iss": ISSUER, "exp": exp_in(3600), "roles": ["developer"] }),
            Some("test-key-1"),
        );
        let claims = validator().decode(&token).unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.roles, vec!["developer".to_string()]);
    }

    #[test]
    fn rejects_wrong_issuer() {
        let token = sign(
            &json!({ "sub": "alice", "iss": "https://evil.example.com", "exp": exp_in(3600) }),
            Some("test-key-1"),
        );
        assert!(validator().decode(&token).is_err());
    }

    #[test]
    fn rejects_expired_token() {
        let token = sign(
            &json!({ "sub": "alice", "iss": ISSUER, "exp": exp_in(-10) }),
            Some("test-key-1"),
        );
        assert!(validator().decode(&token).is_err());
    }

    #[test]
    fn rejects_unknown_kid() {
        let token = sign(
            &json!({ "sub": "alice", "iss": ISSUER, "exp": exp_in(3600) }),
            Some("rotated-away"),
        );
        assert!(validator().decode(&token).is_err());
    }

    #[test]
    fn missing_kid_falls_back_when_single_key() {
        let token = sign(
            &json!({ "sub": "bob", "iss": ISSUER, "exp": exp_in(3600) }),
            None,
        );
        assert_eq!(validator().decode(&token).unwrap().sub, "bob");
    }

    #[test]
    fn audience_is_enforced_when_configured() {
        let v = OidcValidator::from_jwks_json(ISSUER, Some("open-runo-api".into()), JWKS).unwrap();
        let good = sign(
            &json!({ "sub": "a", "iss": ISSUER, "aud": "open-runo-api", "exp": exp_in(3600) }),
            Some("test-key-1"),
        );
        let bad = sign(
            &json!({ "sub": "a", "iss": ISSUER, "aud": "other-api", "exp": exp_in(3600) }),
            Some("test-key-1"),
        );
        assert!(v.decode(&good).is_ok());
        assert!(v.decode(&bad).is_err());
    }

    #[test]
    fn rejects_hs256_token_signed_with_public_material() {
        // Algorithm-confusion guard: an HS256 token must never validate.
        let token = crate::JwtCodec::new("test-secret")
            .encode("mallory", vec!["admin".into()], chrono::Duration::hours(1))
            .unwrap();
        assert!(validator().decode(&token).is_err());
    }

    #[test]
    fn rejects_garbage_jwks() {
        assert!(OidcValidator::from_jwks_json(ISSUER, None, "not json").is_err());
        assert!(OidcValidator::from_jwks_json(ISSUER, None, r#"{"keys":[]}"#).is_err());
    }
}
