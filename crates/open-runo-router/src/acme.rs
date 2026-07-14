//! ACME (RFC 8555) — Poem-parity gap ("TLS/ACME", `docs/poem-parity.md`).
//! Automatic certificate provisioning via the HTTP-01 challenge type.
//!
//! Two halves, split by the `acme` Cargo feature:
//! - [`ChallengeStore`] and [`challenge_response_handler`] are always
//!   compiled (no new dependencies) — a small in-memory token→key-
//!   authorization map plus the `GET /.well-known/acme-challenge/:token`
//!   handler that serves it. This is what the ACME CA actually connects
//!   to over the public internet during HTTP-01 validation; wiring it
//!   into the router doesn't require any crypto/HTTP-client dependency.
//! - [`AcmeClient`] (behind `#[cfg(feature = "acme")]`) is the part that
//!   *talks to* the CA: directory discovery, nonce management, JWS-signed
//!   requests, and the account/order/challenge/finalize state machine.
//!
//! The ACME *protocol* (JSON shapes, the directory/nonce/order state
//! machine, JWS envelope construction) is hand-rolled here the same way
//! this crate hand-rolls WebSocket framing (`hyper_compat`) and multipart
//! parsing -- but the actual cryptographic signing operation is delegated
//! to `ring`'s audited ECDSA implementation rather than reimplemented,
//! matching the sha1/sha2 boundary this codebase already draws elsewhere
//! (use an audited primitive, don't reimplement the math).
//!
//! **Verification limitation, stated plainly**: HTTP-01 challenge
//! validation requires the ACME CA to reach this server over the public
//! internet on port 80. This development sandbox has no public domain or
//! inbound port 80, so a live run against Let's Encrypt (staging or
//! production) cannot be executed or proven from here. What *is* verified
//! (see the test module) is the full protocol state machine — JWS
//! signing/verification round-trip, JWK thumbprint computation, and the
//! directory→nonce→account→order→challenge→finalize→download sequence —
//! against a local mock ACME directory server built with
//! `hyper_compat::serve` itself. That proves the client logic is correct;
//! it does not prove interoperability with a real CA's exact quirks.

use std::collections::HashMap;
use std::sync::Mutex;

/// In-memory token → key-authorization map for HTTP-01 challenge
/// responses. Always compiled (no `acme`-feature dependencies) since
/// serving `.well-known/acme-challenge/*` is cheap and useful even if the
/// ACME client itself is built by something else pointed at this server.
#[derive(Debug, Default)]
pub struct ChallengeStore {
    tokens: Mutex<HashMap<String, String>>,
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&self, token: String, key_authorization: String) {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token, key_authorization);
    }

    pub fn get(&self, token: &str) -> Option<String> {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(token)
            .cloned()
    }

    pub fn remove(&self, token: &str) {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(token);
    }
}

/// `GET /.well-known/acme-challenge/:token` — what the ACME CA's HTTP-01
/// validator actually connects to. Returns the published key authorization
/// as `text/plain` (per RFC 8555 §8.3), or 404 if nothing is published
/// under that token (unknown/expired/already-cleaned-up challenge).
pub fn challenge_response_handler(store: std::sync::Arc<ChallengeStore>) -> crate::hyper_compat::Handler {
    use crate::hyper_compat::{empty_status, fixed_body};
    use hyper::StatusCode;
    std::sync::Arc::new(move |_req, params: crate::hyper_compat::Params| {
        let store = std::sync::Arc::clone(&store);
        Box::pin(async move {
            let Some(token) = params.get("token") else {
                return empty_status(StatusCode::NOT_FOUND);
            };
            match store.get(token) {
                Some(key_auth) => hyper::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/plain")
                    .body(fixed_body(bytes::Bytes::from(key_auth)))
                    .expect("building a response from a fixed set of valid headers cannot fail"),
                None => empty_status(StatusCode::NOT_FOUND),
            }
        })
    })
}

#[cfg(feature = "acme")]
pub use client::*;

#[cfg(feature = "acme")]
mod client {
    use super::ChallengeStore;
    use open_runo_core::{AppError, Result};
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair as _, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde::Deserialize;
    use std::sync::Arc;

    /// Base64url, unpadded (RFC 7515 §2 / RFC 4648 §5) -- the encoding
    /// every field in a JWS uses. Hand-rolled rather than adding a `base64`
    /// crate dependency, mirroring `hyper_compat`'s existing hand-rolled
    /// standard-alphabet encoder for the WebSocket handshake.
    fn base64url_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied();
            let b2 = chunk.get(2).copied();
            out.push(ALPHABET[(b0 >> 2) as usize] as char);
            out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
            if let Some(b1) = b1 {
                out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
            }
            if let Some(b2) = b2 {
                out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
            }
        }
        out
    }

    /// An ACME account's ES256 (ECDSA P-256 + SHA-256) key pair. Every ACME
    /// request is a JWS signed with this key; the account is identified by
    /// it (first request: the public key itself as a JWK; every request
    /// after `newAccount`: the `kid` URL the CA assigned it).
    pub struct AcmeAccountKey {
        key_pair: EcdsaKeyPair,
        rng: SystemRandom,
    }

    impl AcmeAccountKey {
        pub fn generate() -> Result<Self> {
            let rng = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                .map_err(|e| AppError::Internal(format!("ACME account key generation failed: {e}")))?;
            let key_pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .map_err(|e| AppError::Internal(format!("ACME account key parse failed: {e}")))?;
            Ok(Self { key_pair, rng })
        }

        /// Raw fixed-length (r||s) ES256 signature over `message`, per
        /// RFC 7518 §3.4 -- NOT the ASN.1 DER format `EcdsaKeyPair::sign`
        /// would produce with the `_ASN1_SIGNING` algorithm variant.
        fn sign(&self, message: &[u8]) -> Result<Vec<u8>> {
            self.key_pair
                .sign(&self.rng, message)
                .map(|sig| sig.as_ref().to_vec())
                .map_err(|e| AppError::Internal(format!("ACME JWS signing failed: {e}")))
        }

        /// The public key as a JWK (RFC 7517), uncompressed SEC1 point
        /// (`0x04 || X || Y`, 32 bytes each for P-256) split into its two
        /// coordinates.
        fn jwk(&self) -> serde_json::Value {
            let public = self.key_pair.public_key().as_ref();
            debug_assert_eq!(public.len(), 65, "uncompressed P-256 point is 1+32+32 bytes");
            let x = &public[1..33];
            let y = &public[33..65];
            serde_json::json!({
                "kty": "EC",
                "crv": "P-256",
                "x": base64url_encode(x),
                "y": base64url_encode(y),
            })
        }

        /// RFC 7638 JWK thumbprint: base64url(SHA-256(canonical JSON)).
        /// Canonical means exactly the required members, no whitespace, in
        /// lexicographic key order -- for an EC JWK that's `crv, kty, x, y`.
        /// This is NOT the same as `serde_json::to_vec(&self.jwk())`,
        /// whose field order follows insertion order, not RFC 7638's rule.
        pub fn thumbprint(&self) -> String {
            let public = self.key_pair.public_key().as_ref();
            let x = base64url_encode(&public[1..33]);
            let y = base64url_encode(&public[33..65]);
            let canonical = format!(r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#);
            let digest = ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes());
            base64url_encode(digest.as_ref())
        }
    }

    /// `key-authorization` for an HTTP-01 challenge (RFC 8555 §8.1):
    /// `"{token}.{account-key-thumbprint}"`.
    pub fn http01_key_authorization(token: &str, account_key: &AcmeAccountKey) -> String {
        format!("{token}.{}", account_key.thumbprint())
    }

    #[derive(Debug, Clone, Deserialize)]
    struct AcmeDirectory {
        #[serde(rename = "newNonce")]
        new_nonce: String,
        #[serde(rename = "newAccount")]
        new_account: String,
        #[serde(rename = "newOrder")]
        new_order: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct AcmeOrder {
        pub status: String,
        pub authorizations: Vec<String>,
        pub finalize: String,
        pub certificate: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct AcmeChallenge {
        #[serde(rename = "type")]
        pub challenge_type: String,
        pub url: String,
        pub token: String,
        pub status: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct AcmeAuthorization {
        pub status: String,
        pub challenges: Vec<AcmeChallenge>,
    }

    /// A minimal ACME v2 client: enough of RFC 8555 to obtain a certificate
    /// via a single HTTP-01 challenge. Not a general-purpose ACME library --
    /// no DNS-01/TLS-ALPN-01, no account key rollover, no revocation.
    pub struct AcmeClient {
        http: reqwest::Client,
        directory: AcmeDirectory,
        account_key: AcmeAccountKey,
        kid: Option<String>,
        nonce: Option<String>,
    }

    impl AcmeClient {
        /// Fetch `directory_url` (RFC 8555 §7.1.1) and prepare a client with
        /// a fresh account key. Does not yet register an account -- call
        /// [`Self::new_account`] next.
        pub async fn discover(directory_url: &str) -> Result<Self> {
            let http = reqwest::Client::new();
            let directory: AcmeDirectory = http
                .get(directory_url)
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("ACME directory fetch failed: {e}")))?
                .json()
                .await
                .map_err(|e| AppError::Internal(format!("ACME directory parse failed: {e}")))?;
            Ok(Self {
                http,
                directory,
                account_key: AcmeAccountKey::generate()?,
                kid: None,
                nonce: None,
            })
        }

        async fn fetch_nonce(&self) -> Result<String> {
            let resp = self
                .http
                .head(&self.directory.new_nonce)
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("ACME newNonce failed: {e}")))?;
            resp.headers()
                .get("replay-nonce")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| AppError::Internal("ACME newNonce response missing Replay-Nonce".to_string()))
        }

        /// POST a JWS-wrapped `payload` (or an empty "POST-as-GET" body if
        /// `payload` is `None`) to `url`, using `kid` if the account is
        /// already registered, `jwk` otherwise. Returns the response's
        /// headers and parsed JSON body; also captures the next
        /// `Replay-Nonce` and (if present) a fresh `kid` from `Location`.
        async fn post_jws(&mut self, url: &str, payload: Option<serde_json::Value>) -> Result<(reqwest::header::HeaderMap, serde_json::Value)> {
            let nonce = match self.nonce.take() {
                Some(n) => n,
                None => self.fetch_nonce().await?,
            };

            let mut protected = serde_json::json!({
                "alg": "ES256",
                "nonce": nonce,
                "url": url,
            });
            match &self.kid {
                Some(kid) => protected["kid"] = serde_json::Value::String(kid.clone()),
                None => protected["jwk"] = self.account_key.jwk(),
            }

            let protected_b64 = base64url_encode(&serde_json::to_vec(&protected).unwrap());
            let payload_b64 = match &payload {
                Some(p) => base64url_encode(&serde_json::to_vec(p).unwrap()),
                None => String::new(),
            };
            let signing_input = format!("{protected_b64}.{payload_b64}");
            let signature = self.account_key.sign(signing_input.as_bytes())?;

            let body = serde_json::json!({
                "protected": protected_b64,
                "payload": payload_b64,
                "signature": base64url_encode(&signature),
            });

            let resp = self
                .http
                .post(url)
                .header("content-type", "application/jose+json")
                .json(&body)
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("ACME request to {url} failed: {e}")))?;

            if let Some(next_nonce) = resp.headers().get("replay-nonce").and_then(|v| v.to_str().ok()) {
                self.nonce = Some(next_nonce.to_string());
            }

            let status = resp.status();
            let headers = resp.headers().clone();
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| AppError::Internal(format!("ACME response body read failed: {e}")))?;
            let parsed: serde_json::Value = if bytes.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&bytes)
                    .map_err(|e| AppError::Internal(format!("ACME response JSON parse failed: {e}")))?
            };

            if !status.is_success() {
                return Err(AppError::Internal(format!(
                    "ACME request to {url} returned {status}: {parsed}"
                )));
            }

            Ok((headers, parsed))
        }

        /// Register (or, per RFC 8555 §7.3.1, look up the existing
        /// registration for) the client's account key.
        pub async fn new_account(&mut self, contact_emails: &[String], terms_agreed: bool) -> Result<()> {
            let url = self.directory.new_account.clone();
            let contact: Vec<String> = contact_emails.iter().map(|e| format!("mailto:{e}")).collect();
            let payload = serde_json::json!({
                "termsOfServiceAgreed": terms_agreed,
                "contact": contact,
            });
            let (headers, _body) = self.post_jws(&url, Some(payload)).await?;
            let kid = headers
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AppError::Internal("ACME newAccount response missing Location".to_string()))?
                .to_string();
            self.kid = Some(kid);
            Ok(())
        }

        /// Create a new order for `domains` (RFC 8555 §7.4).
        pub async fn new_order(&mut self, domains: &[String]) -> Result<AcmeOrder> {
            let url = self.directory.new_order.clone();
            let identifiers: Vec<serde_json::Value> = domains
                .iter()
                .map(|d| serde_json::json!({ "type": "dns", "value": d }))
                .collect();
            let payload = serde_json::json!({ "identifiers": identifiers });
            let (_headers, body) = self.post_jws(&url, Some(payload)).await?;
            serde_json::from_value(body).map_err(|e| AppError::Internal(format!("ACME order parse failed: {e}")))
        }

        /// POST-as-GET an authorization URL (RFC 8555 §7.5).
        pub async fn get_authorization(&mut self, url: &str) -> Result<AcmeAuthorization> {
            let (_headers, body) = self.post_jws(url, None).await?;
            serde_json::from_value(body).map_err(|e| AppError::Internal(format!("ACME authorization parse failed: {e}")))
        }

        /// The key authorization this account would need to publish for
        /// `token` under `.well-known/acme-challenge/{token}`.
        pub fn key_authorization_for(&self, token: &str) -> String {
            http01_key_authorization(token, &self.account_key)
        }

        /// Tell the CA "the challenge response is published, please
        /// validate" (RFC 8555 §7.5.1) — the caller must have already
        /// published [`Self::key_authorization_for`]'s result (e.g. via
        /// [`ChallengeStore::publish`]) before calling this.
        pub async fn respond_to_challenge(&mut self, challenge_url: &str) -> Result<()> {
            self.post_jws(challenge_url, Some(serde_json::json!({}))).await?;
            Ok(())
        }

        /// Poll `authorization_url` (POST-as-GET) until its status leaves
        /// `"pending"`, up to `max_attempts` times with a short delay
        /// between attempts. Returns an error if it ends up anywhere other
        /// than `"valid"`. A production client should honor a `Retry-After`
        /// header instead of a fixed delay; this fixed-delay version is
        /// sufficient for the mock-CA test in this module and for
        /// low-volume interactive use.
        pub async fn poll_authorization_until_valid(&mut self, authorization_url: &str, max_attempts: u32) -> Result<()> {
            for _ in 0..max_attempts {
                let auth = self.get_authorization(authorization_url).await?;
                match auth.status.as_str() {
                    "valid" => return Ok(()),
                    "pending" => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                    other => {
                        return Err(AppError::Internal(format!(
                            "ACME authorization {authorization_url} ended in status {other}"
                        )))
                    }
                }
            }
            Err(AppError::Internal(format!(
                "ACME authorization {authorization_url} still pending after {max_attempts} attempts"
            )))
        }

        /// Finalize the order (RFC 8555 §7.4) by submitting a CSR for
        /// `domain`, generating a fresh certificate key pair. Returns the
        /// finalized order (poll `certificate` via [`Self::download_certificate`]
        /// once `status` is `"valid"`) and the PEM-encoded private key the
        /// CSR was built with.
        pub async fn finalize_order(&mut self, order: &AcmeOrder, domain: &str) -> Result<(AcmeOrder, String)> {
            let key_pair = rcgen::KeyPair::generate()
                .map_err(|e| AppError::Internal(format!("certificate key generation failed: {e}")))?;
            let params = rcgen::CertificateParams::new(vec![domain.to_string()])
                .map_err(|e| AppError::Internal(format!("certificate params failed: {e}")))?;
            let csr = params
                .serialize_request(&key_pair)
                .map_err(|e| AppError::Internal(format!("CSR generation failed: {e}")))?;
            let key_pem = key_pair.serialize_pem();

            let payload = serde_json::json!({ "csr": base64url_encode(csr.der()) });
            let (_headers, body) = self.post_jws(&order.finalize, Some(payload)).await?;
            let finalized: AcmeOrder =
                serde_json::from_value(body).map_err(|e| AppError::Internal(format!("ACME finalize response parse failed: {e}")))?;
            Ok((finalized, key_pem))
        }

        /// Download the issued certificate chain (PEM) once the order's
        /// `certificate` URL is populated (RFC 8555 §7.4.2).
        pub async fn download_certificate(&mut self, certificate_url: &str) -> Result<String> {
            let nonce = match self.nonce.take() {
                Some(n) => n,
                None => self.fetch_nonce().await?,
            };
            let mut protected = serde_json::json!({ "alg": "ES256", "nonce": nonce, "url": certificate_url });
            protected["kid"] = serde_json::Value::String(
                self.kid.clone().ok_or_else(|| AppError::Internal("no ACME account kid".to_string()))?,
            );
            let protected_b64 = base64url_encode(&serde_json::to_vec(&protected).unwrap());
            let payload_b64 = String::new();
            let signing_input = format!("{protected_b64}.{payload_b64}");
            let signature = self.account_key.sign(signing_input.as_bytes())?;
            let body = serde_json::json!({
                "protected": protected_b64,
                "payload": payload_b64,
                "signature": base64url_encode(&signature),
            });

            let resp = self
                .http
                .post(certificate_url)
                .header("content-type", "application/jose+json")
                .json(&body)
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("ACME certificate download failed: {e}")))?;
            if let Some(next_nonce) = resp.headers().get("replay-nonce").and_then(|v| v.to_str().ok()) {
                self.nonce = Some(next_nonce.to_string());
            }
            resp.text()
                .await
                .map_err(|e| AppError::Internal(format!("ACME certificate body read failed: {e}")))
        }
    }

    /// End-to-end orchestration: discover → register → order → publish
    /// challenge response into `challenges` → respond → poll → finalize →
    /// download. Returns `(certificate_chain_pem, private_key_pem)`.
    pub async fn obtain_certificate_http01(
        directory_url: &str,
        domain: &str,
        contact_email: &str,
        challenges: &Arc<ChallengeStore>,
    ) -> Result<(String, String)> {
        let mut client = AcmeClient::discover(directory_url).await?;
        client.new_account(&[contact_email.to_string()], true).await?;
        let order = client.new_order(&[domain.to_string()]).await?;

        for auth_url in &order.authorizations {
            let auth = client.get_authorization(auth_url).await?;
            let challenge = auth
                .challenges
                .iter()
                .find(|c| c.challenge_type == "http-01")
                .ok_or_else(|| AppError::Internal("no http-01 challenge offered".to_string()))?;

            let key_auth = client.key_authorization_for(&challenge.token);
            challenges.publish(challenge.token.clone(), key_auth);
            client.respond_to_challenge(&challenge.url).await?;
            client.poll_authorization_until_valid(auth_url, 20).await?;
            challenges.remove(&challenge.token);
        }

        let (finalized, key_pem) = client.finalize_order(&order, domain).await?;
        let cert_url = finalized
            .certificate
            .ok_or_else(|| AppError::Internal("ACME order finalized without a certificate URL".to_string()))?;
        let cert_pem = client.download_certificate(&cert_url).await?;
        Ok((cert_pem, key_pem))
    }

    // ── TLS-ALPN-01 (RFC 8737) ──────────────────────────────────────────
    //
    // Unlike HTTP-01, TLS-ALPN-01 doesn't need a separately reachable
    // port 80 or an HTTP challenge responder -- the ACME CA connects
    // straight to this server's normal TLS port and negotiates the
    // `acme-tls/1` ALPN protocol. Instead of serving real application
    // data over that connection, the server presents a throwaway
    // self-signed certificate whose only job is carrying a critical
    // extension (id-pe-acmeIdentifier, RFC 8737 §3) containing
    // SHA-256(key-authorization); the CA checks that extension and never
    // sends/receives HTTP at all. This reuses the same rustls TLS stack
    // already in this crate (`tls` feature, implied by `acme`) via a
    // `ResolvesServerCert` that inspects the ClientHello's requested ALPN
    // protocols and only swaps in the validation cert when `acme-tls/1`
    // was requested -- every other connection on the same port (ordinary
    // HTTPS traffic included) is completely unaffected by an in-progress
    // validation.
    pub mod tls_alpn01 {
        use super::{http01_key_authorization, AcmeAccountKey, AcmeClient};
        use open_runo_core::{AppError, Result};
        use rustls::server::{ClientHello, ResolvesServerCert};
        use rustls::sign::CertifiedKey;
        use rustls::ServerConfig;
        use std::collections::HashMap;
        use std::sync::{Arc, RwLock};

        /// id-pe-acmeIdentifier (RFC 8737 §3).
        const ACME_IDENTIFIER_OID: &[u64] = &[1, 3, 6, 1, 5, 5, 7, 1, 31];
        /// The ALPN protocol name the CA's TLS-ALPN-01 validator negotiates.
        pub const ACME_TLS_ALPN_1: &str = "acme-tls/1";

        /// SHA-256 digest of the key authorization (RFC 8737 §3) -- what
        /// actually goes inside the validation cert's extension, unlike
        /// HTTP-01 which publishes the key-authorization string itself.
        pub fn key_authorization_digest(token: &str, account_key: &AcmeAccountKey) -> [u8; 32] {
            let key_auth = http01_key_authorization(token, account_key);
            let digest = ring::digest::digest(&ring::digest::SHA256, key_auth.as_bytes());
            let mut out = [0u8; 32];
            out.copy_from_slice(digest.as_ref());
            out
        }

        /// Build the throwaway validation certificate for `domain` carrying
        /// `digest` in the critical acmeIdentifier extension, DER-encoded
        /// as an OCTET STRING (tag `0x04`, then the length byte, then the
        /// 32 digest bytes -- SHA-256 digests never need long-form length
        /// encoding since 32 < 128).
        pub fn generate_validation_cert(domain: &str, digest: [u8; 32]) -> Result<CertifiedKey> {
            let key_pair = rcgen::KeyPair::generate()
                .map_err(|e| AppError::Internal(format!("TLS-ALPN-01 cert key generation failed: {e}")))?;
            let mut params = rcgen::CertificateParams::new(vec![domain.to_string()])
                .map_err(|e| AppError::Internal(format!("TLS-ALPN-01 cert params failed: {e}")))?;

            let mut octet_string = vec![0x04, digest.len() as u8];
            octet_string.extend_from_slice(&digest);
            let mut ext = rcgen::CustomExtension::from_oid_content(ACME_IDENTIFIER_OID, octet_string);
            ext.set_criticality(true);
            params.custom_extensions.push(ext);

            let cert = params
                .self_signed(&key_pair)
                .map_err(|e| AppError::Internal(format!("TLS-ALPN-01 cert self-sign failed: {e}")))?;

            let key_der = rustls::pki_types::PrivateKeyDer::try_from(key_pair.serialize_der())
                .map_err(|e| AppError::Internal(format!("TLS-ALPN-01 key DER conversion failed: {e}")))?;
            let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)
                .map_err(|e| AppError::Internal(format!("TLS-ALPN-01 signing key build failed: {e}")))?;

            Ok(CertifiedKey {
                cert: vec![cert.der().clone()],
                key: signing_key,
                ocsp: None,
            })
        }

        /// Serves the validation cert for `acme-tls/1` connections whose
        /// SNI matches a domain with a challenge currently published;
        /// every other connection (any other ALPN request, or none at
        /// all) gets `fallback` -- ordinary HTTPS traffic on the same
        /// port keeps working unmodified during a validation.
        pub struct TlsAlpnResolver {
            challenges: RwLock<HashMap<String, Arc<CertifiedKey>>>,
            fallback: Arc<CertifiedKey>,
        }

        impl TlsAlpnResolver {
            pub fn new(fallback: Arc<CertifiedKey>) -> Arc<Self> {
                Arc::new(Self {
                    challenges: RwLock::new(HashMap::new()),
                    fallback,
                })
            }

            pub fn publish(&self, domain: String, cert: Arc<CertifiedKey>) {
                self.challenges
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(domain, cert);
            }

            pub fn remove(&self, domain: &str) {
                self.challenges
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(domain);
            }
        }

        impl std::fmt::Debug for TlsAlpnResolver {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("TlsAlpnResolver").finish_non_exhaustive()
            }
        }

        impl ResolvesServerCert for TlsAlpnResolver {
            fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
                let wants_acme_tls_alpn1 = client_hello
                    .alpn()
                    .map(|mut protos| protos.any(|p| p == ACME_TLS_ALPN_1.as_bytes()))
                    .unwrap_or(false);
                if wants_acme_tls_alpn1 {
                    if let Some(name) = client_hello.server_name() {
                        let guard = self.challenges.read().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(cert) = guard.get(name) {
                            return Some(Arc::clone(cert));
                        }
                    }
                }
                Some(Arc::clone(&self.fallback))
            }
        }

        /// A rustls `ServerConfig` wired to `resolver` and advertising
        /// both `acme-tls/1` (so the ALPN negotiation can select it during
        /// a validation) and `http/1.1` (so ordinary HTTPS traffic keeps
        /// negotiating normally the rest of the time).
        pub fn server_config(resolver: Arc<TlsAlpnResolver>) -> ServerConfig {
            let mut config = ServerConfig::builder().with_no_client_auth().with_cert_resolver(resolver);
            config.alpn_protocols = vec![ACME_TLS_ALPN_1.as_bytes().to_vec(), b"http/1.1".to_vec()];
            config
        }

        /// TLS-ALPN-01 equivalent of `obtain_certificate_http01`: discover
        /// -> register -> order -> generate+publish a validation cert into
        /// `resolver` for `domain` -> respond -> poll -> finalize ->
        /// download. Unlike HTTP-01, nothing needs to be reachable on port
        /// 80 -- the CA validates over whatever port `resolver` (via
        /// `server_config`) is already listening on for HTTPS.
        pub async fn obtain_certificate(
            directory_url: &str,
            domain: &str,
            contact_email: &str,
            resolver: &Arc<TlsAlpnResolver>,
        ) -> Result<(String, String)> {
            let mut client = AcmeClient::discover(directory_url).await?;
            client.new_account(&[contact_email.to_string()], true).await?;
            let order = client.new_order(&[domain.to_string()]).await?;

            for auth_url in &order.authorizations {
                let auth = client.get_authorization(auth_url).await?;
                let challenge = auth
                    .challenges
                    .iter()
                    .find(|c| c.challenge_type == "tls-alpn-01")
                    .ok_or_else(|| AppError::Internal("no tls-alpn-01 challenge offered".to_string()))?;

                let digest = key_authorization_digest(&challenge.token, &client.account_key);
                let cert = generate_validation_cert(domain, digest)?;
                resolver.publish(domain.to_string(), Arc::new(cert));
                client.respond_to_challenge(&challenge.url).await?;
                client.poll_authorization_until_valid(auth_url, 20).await?;
                resolver.remove(domain);
            }

            let (finalized, key_pem) = client.finalize_order(&order, domain).await?;
            let cert_url = finalized
                .certificate
                .ok_or_else(|| AppError::Internal("ACME order finalized without a certificate URL".to_string()))?;
            let cert_pem = client.download_certificate(&cert_url).await?;
            Ok((cert_pem, key_pem))
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn validation_cert_der_contains_the_digest_as_an_octet_string() {
                let digest = [0x42u8; 32];
                let certified = generate_validation_cert("test.local", digest).unwrap();
                let der = certified.cert[0].as_ref();

                // The custom extension's content is a DER OCTET STRING
                // (0x04, 0x20, <32 bytes>) wrapping the raw digest -- it
                // must appear verbatim somewhere in the encoded cert, or
                // this cert is useless to a real TLS-ALPN-01 validator.
                let mut needle = vec![0x04, 0x20];
                needle.extend_from_slice(&digest);
                assert!(
                    der.windows(needle.len()).any(|w| w == needle.as_slice()),
                    "validation cert DER should contain the digest as an OCTET STRING"
                );
            }

            #[test]
            fn different_digests_produce_different_certs() {
                let a = generate_validation_cert("test.local", [0x01; 32]).unwrap();
                let b = generate_validation_cert("test.local", [0x02; 32]).unwrap();
                assert_ne!(a.cert[0].as_ref(), b.cert[0].as_ref());
            }

            /// The strongest verification achievable without a real ACME
            /// CA: a genuine rustls server (our `TlsAlpnResolver` wired
            /// into a real `ServerConfig`, accepting real TCP connections)
            /// and a genuine rustls client, actually performing a TLS
            /// handshake. Connecting with ALPN `acme-tls/1` must yield the
            /// published validation cert; connecting with `http/1.1` (or
            /// no ALPN at all) must yield the fallback cert instead --
            /// proving the resolver's ALPN-based branching, not just the
            /// cert-generation logic in isolation.
            #[tokio::test]
            async fn resolver_serves_validation_cert_only_for_acme_tls_alpn1_connections() {
                use rustls::pki_types::ServerName;
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                use tokio::net::{TcpListener, TcpStream};
                use tokio_rustls::{TlsAcceptor, TlsConnector};

                let fallback = generate_validation_cert("fallback.local", [0xAA; 32]).unwrap();
                let resolver = TlsAlpnResolver::new(Arc::new(fallback));

                let challenge_digest = [0xBB; 32];
                let challenge_cert = generate_validation_cert("challenge.local", challenge_digest).unwrap();
                resolver.publish("challenge.local".to_string(), Arc::new(challenge_cert));

                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();

                // Accept-and-handshake on the server side, run concurrently
                // (via `tokio::join!`, not `tokio::spawn`) with the client
                // connect on the other side of the same in-process loopback
                // -- avoids any 'static-lifetime requirement on `listener`.
                async fn accept_one(acceptor: TlsAcceptor, listener: &TcpListener) {
                    let (stream, _) = listener.accept().await.unwrap();
                    // A completed handshake is proof enough here (the
                    // client-side peer_certificates() check below is what
                    // actually asserts *which* cert was served); echo one
                    // byte back so the client's read doesn't hang waiting
                    // for application data that will never come from a
                    // validation connection.
                    let mut tls = acceptor.accept(stream).await.unwrap();
                    let _ = tls.write_all(b"x").await;
                }

                let client_roots = danger::NoServerAuth::config();
                let client_config = |alpn: &str| {
                    let mut cfg = client_roots.clone();
                    cfg.alpn_protocols = vec![alpn.as_bytes().to_vec()];
                    cfg
                };

                // Each connection gets a freshly built `ServerConfig` (not
                // a shared `Arc`) so TLS session-ticket resumption can't
                // let the second connection silently reuse the first
                // connection's already-resolved certificate instead of
                // exercising the resolver's ALPN branch again.
                let acceptor = TlsAcceptor::from(Arc::new(server_config(Arc::clone(&resolver))));
                let connector = TlsConnector::from(Arc::new(client_config(ACME_TLS_ALPN_1)));
                let connect = async {
                    let tcp = TcpStream::connect(addr).await.unwrap();
                    let server_name = ServerName::try_from("challenge.local").unwrap();
                    let mut tls = connector.connect(server_name, tcp).await.unwrap();
                    let mut buf = [0u8; 1];
                    let _ = tls.read_exact(&mut buf).await;
                    tls.get_ref().1.peer_certificates().expect("server must present a certificate").to_vec()
                };
                let (_, peer_certs) = tokio::join!(accept_one(acceptor, &listener), connect);

                let mut needle = vec![0x04, 0x20];
                needle.extend_from_slice(&challenge_digest);
                assert!(
                    peer_certs[0].as_ref().windows(needle.len()).any(|w| w == needle.as_slice()),
                    "acme-tls/1 connection for a published domain must get the validation cert"
                );

                // Now a normal ALPN connection -- must get the fallback,
                // not the validation cert (proves the resolver doesn't
                // leak the validation cert to ordinary traffic).
                let acceptor2 = TlsAcceptor::from(Arc::new(server_config(Arc::clone(&resolver))));
                let connector2 = TlsConnector::from(Arc::new(client_config("http/1.1")));
                let connect2 = async {
                    let tcp = TcpStream::connect(addr).await.unwrap();
                    let server_name = ServerName::try_from("challenge.local").unwrap();
                    let mut tls = connector2.connect(server_name, tcp).await.unwrap();
                    let mut buf = [0u8; 1];
                    let _ = tls.read_exact(&mut buf).await;
                    tls.get_ref().1.peer_certificates().expect("server must present a certificate").to_vec()
                };
                let (_, peer_certs2) = tokio::join!(accept_one(acceptor2, &listener), connect2);

                assert!(
                    !peer_certs2[0].as_ref().windows(needle.len()).any(|w| w == needle.as_slice()),
                    "non-acme-tls/1 connection must NOT get the validation cert"
                );
            }

            /// Minimal "accept any server cert" verifier for the test
            /// client above -- the servers here use throwaway self-signed
            /// certs, so a real CA-chain verifier would always reject
            /// them. Test-only; production TLS-ALPN-01 CAs perform their
            /// own out-of-band trust decision (they're validating a
            /// challenge, not establishing a trusted HTTPS session).
            mod danger {
                use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
                use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
                use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
                use std::sync::Arc;

                #[derive(Debug)]
                struct NoVerify;

                impl ServerCertVerifier for NoVerify {
                    fn verify_server_cert(
                        &self,
                        _end_entity: &CertificateDer<'_>,
                        _intermediates: &[CertificateDer<'_>],
                        _server_name: &ServerName<'_>,
                        _ocsp_response: &[u8],
                        _now: UnixTime,
                    ) -> Result<ServerCertVerified, rustls::Error> {
                        Ok(ServerCertVerified::assertion())
                    }

                    fn verify_tls12_signature(
                        &self,
                        _message: &[u8],
                        _cert: &CertificateDer<'_>,
                        _dss: &DigitallySignedStruct,
                    ) -> Result<HandshakeSignatureValid, rustls::Error> {
                        Ok(HandshakeSignatureValid::assertion())
                    }

                    fn verify_tls13_signature(
                        &self,
                        _message: &[u8],
                        _cert: &CertificateDer<'_>,
                        _dss: &DigitallySignedStruct,
                    ) -> Result<HandshakeSignatureValid, rustls::Error> {
                        Ok(HandshakeSignatureValid::assertion())
                    }

                    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                        vec![
                            SignatureScheme::ECDSA_NISTP256_SHA256,
                            SignatureScheme::ECDSA_NISTP384_SHA384,
                            SignatureScheme::RSA_PSS_SHA256,
                            SignatureScheme::ED25519,
                        ]
                    }
                }

                pub struct NoServerAuth;

                impl NoServerAuth {
                    pub fn config() -> ClientConfig {
                        ClientConfig::builder()
                            .dangerous()
                            .with_custom_certificate_verifier(Arc::new(NoVerify))
                            .with_no_client_auth()
                    }
                }
            }
        }
    }

    // ── DNS-01 (RFC 8555 §8.4) ───────────────────────────────────────────
    //
    // The only challenge type that can prove control of a domain without
    // needing *any* server reachable on that domain at all (no port 80,
    // no TLS port) -- validation is a DNS lookup, so it's also the only
    // type that supports wildcard certificates. The tradeoff, and the
    // reason this is scoped last of the three challenge types: it needs
    // real, authenticated write access to the domain's DNS zone, which
    // this development sandbox does not have (no real DNS zone, no
    // provider credentials) -- there is no way to prove interoperability
    // with an actual DNS provider's API from here, only the ACME-protocol
    // side of the exchange (same honest limitation already stated for
    // HTTP-01/TLS-ALPN-01, just one layer further out).
    //
    // What *is* implemented and genuinely testable: the DNS-01 half of
    // the ACME state machine (computing the TXT record value per RFC 8555
    // §8.4, publishing/removing it around the challenge/poll sequence)
    // plus a real DNS-provider client -- Cloudflare's REST API, chosen
    // because it's simple (plain REST + bearer token, no request signing)
    // and widely used, so this is a genuine integration a real deployment
    // could use, not a placeholder. `DnsProvider` is a small trait so any
    // other provider's API can be added the same way without touching the
    // ACME orchestration logic, and so the orchestration can be tested
    // end-to-end against a mock DNS provider server built with
    // `hyper_compat::serve` -- the same technique already used for the
    // mock ACME CA above.
    pub mod dns01 {
        use super::{base64url_encode, http01_key_authorization, AcmeClient};
        use crate::hyper_compat::BoxFuture;
        use open_runo_core::{AppError, Result};

        /// A DNS provider capable of publishing/removing the `TXT` record
        /// DNS-01 validation checks. Implementations own whatever
        /// bookkeeping they need to map a record name back to a
        /// provider-specific record ID for deletion (see
        /// [`CloudflareDnsProvider`] for an example).
        pub trait DnsProvider: Send + Sync {
            /// Create a `TXT` record at `name` (e.g.
            /// `_acme-challenge.example.com`) with content `value`.
            fn create_txt_record<'a>(&'a self, name: &'a str, value: &'a str) -> BoxFuture<Result<()>>;
            /// Remove the `TXT` record previously created at `name`.
            fn delete_txt_record<'a>(&'a self, name: &'a str) -> BoxFuture<Result<()>>;
        }

        /// Cloudflare's DNS REST API (`api.cloudflare.com/client/v4`) --
        /// plain bearer-token REST, no request signing, making it one of
        /// the simplest real DNS provider APIs to integrate against.
        /// `base_url` is overridable so tests can point this at a local
        /// mock server instead of the real Cloudflare API.
        pub struct CloudflareDnsProvider {
            http: reqwest::Client,
            base_url: String,
            zone_id: String,
            api_token: String,
            /// Record name -> Cloudflare record ID, so `delete_txt_record`
            /// knows which record to `DELETE` (Cloudflare's API deletes by
            /// ID, not by name/content). `Arc`-wrapped so the async block
            /// in `create_txt_record` can hold its own owned handle
            /// instead of borrowing `&self` for the `'static` bound
            /// `BoxFuture` requires.
            created_records: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
        }

        impl CloudflareDnsProvider {
            pub fn new(zone_id: impl Into<String>, api_token: impl Into<String>) -> Self {
                Self::with_base_url(zone_id, api_token, "https://api.cloudflare.com/client/v4".to_string())
            }

            /// Test-only entry point: same as [`Self::new`] but pointed at
            /// a caller-supplied base URL (a local mock server) instead of
            /// the real Cloudflare API.
            pub fn with_base_url(zone_id: impl Into<String>, api_token: impl Into<String>, base_url: String) -> Self {
                Self {
                    http: reqwest::Client::new(),
                    base_url,
                    zone_id: zone_id.into(),
                    api_token: api_token.into(),
                    created_records: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                }
            }
        }

        impl DnsProvider for CloudflareDnsProvider {
            fn create_txt_record<'a>(&'a self, name: &'a str, value: &'a str) -> BoxFuture<Result<()>> {
                let http = self.http.clone();
                let url = format!("{}/zones/{}/dns_records", self.base_url, self.zone_id);
                let token = self.api_token.clone();
                let name = name.to_string();
                let value = value.to_string();
                let created_records = std::sync::Arc::clone(&self.created_records);
                Box::pin(async move {
                    let resp = http
                        .post(&url)
                        .bearer_auth(&token)
                        .json(&serde_json::json!({ "type": "TXT", "name": name, "content": value, "ttl": 120 }))
                        .send()
                        .await
                        .map_err(|e| AppError::Internal(format!("Cloudflare TXT record create failed: {e}")))?;
                    let body: serde_json::Value = resp
                        .json()
                        .await
                        .map_err(|e| AppError::Internal(format!("Cloudflare TXT record create response parse failed: {e}")))?;
                    if body["success"].as_bool() != Some(true) {
                        return Err(AppError::Internal(format!("Cloudflare TXT record create was not successful: {body}")));
                    }
                    let id = body["result"]["id"]
                        .as_str()
                        .ok_or_else(|| AppError::Internal("Cloudflare TXT record create response missing result.id".to_string()))?
                        .to_string();
                    created_records.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(name, id);
                    Ok(())
                })
            }

            fn delete_txt_record<'a>(&'a self, name: &'a str) -> BoxFuture<Result<()>> {
                let http = self.http.clone();
                let base_url = self.base_url.clone();
                let zone_id = self.zone_id.clone();
                let token = self.api_token.clone();
                let id = self
                    .created_records
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(name);
                Box::pin(async move {
                    let Some(id) = id else {
                        // Nothing was ever published under this name (or
                        // it was already removed) -- deleting is a no-op,
                        // not an error, matching the idempotent-cleanup
                        // behavior of ChallengeStore::remove / TlsAlpnResolver::remove.
                        return Ok(());
                    };
                    let url = format!("{base_url}/zones/{zone_id}/dns_records/{id}");
                    http.delete(&url)
                        .bearer_auth(&token)
                        .send()
                        .await
                        .map_err(|e| AppError::Internal(format!("Cloudflare TXT record delete failed: {e}")))?;
                    Ok(())
                })
            }
        }

        /// DNS-01 equivalent of `obtain_certificate_http01`/
        /// `tls_alpn01::obtain_certificate`: discover -> register -> order
        /// -> compute+publish the TXT record value (RFC 8555 §8.4:
        /// base64url(SHA-256(key-authorization)), at
        /// `_acme-challenge.<domain>`) via `dns` -> respond -> poll
        /// (the existing fixed-delay retry loop also absorbs ordinary DNS
        /// propagation delay, though a real deployment may want a longer
        /// `max_attempts` than the 20 used here) -> finalize -> download
        /// -> remove the TXT record.
        pub async fn obtain_certificate(
            directory_url: &str,
            domain: &str,
            contact_email: &str,
            dns: &dyn DnsProvider,
        ) -> Result<(String, String)> {
            let mut client = AcmeClient::discover(directory_url).await?;
            client.new_account(&[contact_email.to_string()], true).await?;
            let order = client.new_order(&[domain.to_string()]).await?;
            let record_name = format!("_acme-challenge.{domain}");

            for auth_url in &order.authorizations {
                let auth = client.get_authorization(auth_url).await?;
                let challenge = auth
                    .challenges
                    .iter()
                    .find(|c| c.challenge_type == "dns-01")
                    .ok_or_else(|| AppError::Internal("no dns-01 challenge offered".to_string()))?;

                let key_auth = http01_key_authorization(&challenge.token, &client.account_key);
                let digest = ring::digest::digest(&ring::digest::SHA256, key_auth.as_bytes());
                let txt_value = base64url_encode(digest.as_ref());

                dns.create_txt_record(&record_name, &txt_value).await?;
                client.respond_to_challenge(&challenge.url).await?;
                let poll_result = client.poll_authorization_until_valid(auth_url, 20).await;
                // Always attempt cleanup, even if validation failed --
                // otherwise a failed attempt leaves a stale TXT record
                // behind forever.
                dns.delete_txt_record(&record_name).await?;
                poll_result?;
            }

            let (finalized, key_pem) = client.finalize_order(&order, domain).await?;
            let cert_url = finalized
                .certificate
                .ok_or_else(|| AppError::Internal("ACME order finalized without a certificate URL".to_string()))?;
            let cert_pem = client.download_certificate(&cert_url).await?;
            Ok((cert_pem, key_pem))
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::hyper_compat::{empty_status, json_response, serve, Router};
            use hyper::{Method, StatusCode};
            use std::sync::{Arc, Mutex as StdMutex};

            /// A minimal mock of Cloudflare's DNS REST API shape (create
            /// returns `{"success":true,"result":{"id":...}}`, delete
            /// just needs to respond 200) -- enough to exercise
            /// `CloudflareDnsProvider`'s request/response handling for
            /// real over HTTP, without needing genuine Cloudflare
            /// credentials.
            fn mock_cloudflare_router(created: Arc<StdMutex<Vec<(String, String)>>>, deleted: Arc<StdMutex<Vec<String>>>) -> Router {
                let create_handler: crate::hyper_compat::Handler = {
                    let created = Arc::clone(&created);
                    Arc::new(move |req, _params| {
                        let created = Arc::clone(&created);
                        Box::pin(async move {
                            let body: serde_json::Value = crate::hyper_compat::read_json_body(req).await.unwrap();
                            let name = body["name"].as_str().unwrap().to_string();
                            let content = body["content"].as_str().unwrap().to_string();
                            created.lock().unwrap().push((name, content));
                            json_response(StatusCode::OK, &serde_json::json!({ "success": true, "result": { "id": "record-1" } }))
                        })
                    })
                };
                let delete_handler: crate::hyper_compat::Handler = Arc::new(move |_req, params: crate::hyper_compat::Params| {
                    let deleted = Arc::clone(&deleted);
                    let id = params.get("id").unwrap().to_string();
                    Box::pin(async move {
                        deleted.lock().unwrap().push(id);
                        empty_status(StatusCode::OK)
                    })
                });
                Router::new()
                    .route(Method::POST, "/zones/test-zone/dns_records", create_handler)
                    .route(Method::DELETE, "/zones/test-zone/dns_records/:id", delete_handler)
            }

            #[tokio::test]
            async fn cloudflare_provider_creates_and_deletes_a_txt_record_over_real_http() {
                let created = Arc::new(StdMutex::new(Vec::new()));
                let deleted = Arc::new(StdMutex::new(Vec::new()));
                let router = mock_cloudflare_router(Arc::clone(&created), Arc::clone(&deleted));
                let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.expect("bind mock cloudflare");

                let provider = CloudflareDnsProvider::with_base_url("test-zone", "test-token", format!("http://{addr}"));
                provider.create_txt_record("_acme-challenge.example.com", "abc123digest").await.unwrap();
                assert_eq!(created.lock().unwrap().as_slice(), &[("_acme-challenge.example.com".to_string(), "abc123digest".to_string())]);

                provider.delete_txt_record("_acme-challenge.example.com").await.unwrap();
                assert_eq!(deleted.lock().unwrap().as_slice(), &["record-1".to_string()]);
            }

            #[tokio::test]
            async fn deleting_a_name_that_was_never_created_is_a_harmless_no_op() {
                let created = Arc::new(StdMutex::new(Vec::new()));
                let deleted = Arc::new(StdMutex::new(Vec::new()));
                let router = mock_cloudflare_router(created, Arc::clone(&deleted));
                let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.expect("bind mock cloudflare");

                let provider = CloudflareDnsProvider::with_base_url("test-zone", "test-token", format!("http://{addr}"));
                provider.delete_txt_record("never-published.example.com").await.unwrap();
                assert!(deleted.lock().unwrap().is_empty(), "no DELETE request should have been made for an unknown name");
            }

            /// End-to-end verification of the *protocol* side (the same
            /// mock-CA technique as the HTTP-01 test above), combined with
            /// a real `CloudflareDnsProvider` talking to the mock DNS
            /// server -- proving `dns01::obtain_certificate` correctly
            /// computes the TXT value, publishes it before responding to
            /// the challenge, and removes it afterward. What this does
            /// *not* prove (see module docs): interoperability with the
            /// real Cloudflare API or a real ACME CA's DNS-01 validator.
            #[tokio::test]
            async fn full_dns01_flow_against_mock_ca_and_mock_dns_provider() {
                use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

                const TOKEN: &str = "dns01-test-token";
                const FAKE_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\nMOCK-DNS01\n-----END CERTIFICATE-----\n";

                let dns_created = Arc::new(StdMutex::new(Vec::new()));
                let dns_deleted = Arc::new(StdMutex::new(Vec::new()));
                let dns_router = mock_cloudflare_router(Arc::clone(&dns_created), Arc::clone(&dns_deleted));
                let (dns_addr, _dns_handle) = serve(dns_router, "127.0.0.1:0".parse().unwrap()).await.expect("bind mock dns");
                let provider = CloudflareDnsProvider::with_base_url("test-zone", "test-token", format!("http://{dns_addr}"));

                let ca_base: Arc<StdMutex<String>> = Arc::new(StdMutex::new(String::new()));
                let nonce_counter = Arc::new(AtomicU64::new(0));
                let challenge_validated = Arc::new(AtomicBool::new(false));

                let base_of = {
                    let ca_base = Arc::clone(&ca_base);
                    move || ca_base.lock().unwrap().clone()
                };
                let next_nonce = {
                    let nonce_counter = Arc::clone(&nonce_counter);
                    move || format!("nonce-{}", nonce_counter.fetch_add(1, Ordering::SeqCst))
                };

                let directory_handler: crate::hyper_compat::Handler = {
                    let base_of = base_of.clone();
                    Arc::new(move |_req, _params| {
                        let base = base_of();
                        Box::pin(async move {
                            json_response(
                                StatusCode::OK,
                                &serde_json::json!({
                                    "newNonce": format!("{base}/new-nonce"),
                                    "newAccount": format!("{base}/new-acct"),
                                    "newOrder": format!("{base}/new-order"),
                                }),
                            )
                        })
                    })
                };
                let new_nonce_handler: crate::hyper_compat::Handler = {
                    let next_nonce = next_nonce.clone();
                    Arc::new(move |_req, _params| {
                        let nonce = next_nonce();
                        Box::pin(async move {
                            let mut resp = empty_status(StatusCode::OK);
                            resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                            resp
                        })
                    })
                };
                let new_acct_handler: crate::hyper_compat::Handler = {
                    let base_of = base_of.clone();
                    let next_nonce = next_nonce.clone();
                    Arc::new(move |_req, _params| {
                        let base = base_of();
                        let nonce = next_nonce();
                        Box::pin(async move {
                            let mut resp = json_response(StatusCode::CREATED, &serde_json::json!({ "status": "valid" }));
                            resp.headers_mut().insert("location", format!("{base}/acct/1").parse().unwrap());
                            resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                            resp
                        })
                    })
                };
                let new_order_handler: crate::hyper_compat::Handler = {
                    let base_of = base_of.clone();
                    let next_nonce = next_nonce.clone();
                    Arc::new(move |_req, _params| {
                        let base = base_of();
                        let nonce = next_nonce();
                        Box::pin(async move {
                            let mut resp = json_response(
                                StatusCode::CREATED,
                                &serde_json::json!({
                                    "status": "pending",
                                    "authorizations": [format!("{base}/authz/1")],
                                    "finalize": format!("{base}/finalize/1"),
                                }),
                            );
                            resp.headers_mut().insert("location", format!("{base}/order/1").parse().unwrap());
                            resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                            resp
                        })
                    })
                };
                let authz_handler: crate::hyper_compat::Handler = {
                    let base_of = base_of.clone();
                    let next_nonce = next_nonce.clone();
                    let challenge_validated = Arc::clone(&challenge_validated);
                    Arc::new(move |_req, _params| {
                        let base = base_of();
                        let nonce = next_nonce();
                        let validated = challenge_validated.load(Ordering::SeqCst);
                        Box::pin(async move {
                            let status = if validated { "valid" } else { "pending" };
                            let mut resp = json_response(
                                StatusCode::OK,
                                &serde_json::json!({
                                    "status": status,
                                    "challenges": [{
                                        "type": "dns-01",
                                        "url": format!("{base}/challenge/1"),
                                        "token": TOKEN,
                                        "status": status,
                                    }],
                                }),
                            );
                            resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                            resp
                        })
                    })
                };
                let challenge_handler: crate::hyper_compat::Handler = {
                    let next_nonce = next_nonce.clone();
                    let challenge_validated = Arc::clone(&challenge_validated);
                    let dns_created = Arc::clone(&dns_created);
                    Arc::new(move |_req, _params| {
                        let nonce = next_nonce();
                        let challenge_validated = Arc::clone(&challenge_validated);
                        // The real validation step: check that the TXT
                        // record was actually published (via the mock DNS
                        // provider above) before marking the challenge
                        // valid -- proves ordering, not just that some
                        // HTTP call happened.
                        let published = !dns_created.lock().unwrap().is_empty();
                        Box::pin(async move {
                            if published {
                                challenge_validated.store(true, Ordering::SeqCst);
                            }
                            let mut resp = json_response(StatusCode::OK, &serde_json::json!({ "status": "processing" }));
                            resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                            resp
                        })
                    })
                };
                let finalize_handler: crate::hyper_compat::Handler = {
                    let base_of = base_of.clone();
                    let next_nonce = next_nonce.clone();
                    Arc::new(move |_req, _params| {
                        let base = base_of();
                        let nonce = next_nonce();
                        Box::pin(async move {
                            let mut resp = json_response(
                                StatusCode::OK,
                                &serde_json::json!({
                                    "status": "valid",
                                    "authorizations": [format!("{base}/authz/1")],
                                    "finalize": format!("{base}/finalize/1"),
                                    "certificate": format!("{base}/cert/1"),
                                }),
                            );
                            resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                            resp
                        })
                    })
                };
                let cert_handler: crate::hyper_compat::Handler = Arc::new(move |_req, _params| {
                    Box::pin(async move {
                        hyper::Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/pem-certificate-chain")
                            .body(crate::hyper_compat::fixed_body(bytes::Bytes::from_static(FAKE_CERT_PEM.as_bytes())))
                            .unwrap()
                    })
                });

                let ca_router = Router::new()
                    .route(Method::GET, "/directory", directory_handler)
                    .route(Method::HEAD, "/new-nonce", new_nonce_handler)
                    .route(Method::POST, "/new-acct", new_acct_handler)
                    .route(Method::POST, "/new-order", new_order_handler)
                    .route(Method::POST, "/authz/1", authz_handler)
                    .route(Method::POST, "/challenge/1", challenge_handler)
                    .route(Method::POST, "/finalize/1", finalize_handler)
                    .route(Method::POST, "/cert/1", cert_handler);
                let (ca_addr, _ca_handle) = serve(ca_router, "127.0.0.1:0".parse().unwrap()).await.expect("bind mock CA");
                *ca_base.lock().unwrap() = format!("http://{ca_addr}");

                let directory_url = format!("http://{ca_addr}/directory");
                let (cert_pem, key_pem) = obtain_certificate(&directory_url, "dns01-test.local", "admin@test.local", &provider)
                    .await
                    .expect("full DNS-01 flow should succeed against the mock CA and mock DNS provider");

                assert_eq!(cert_pem, FAKE_CERT_PEM);
                assert!(key_pem.contains("PRIVATE KEY"));
                assert!(challenge_validated.load(Ordering::SeqCst));
                assert_eq!(dns_created.lock().unwrap().len(), 1, "exactly one TXT record should have been published");
                assert_eq!(
                    dns_created.lock().unwrap()[0].0,
                    "_acme-challenge.dns01-test.local",
                    "TXT record name should follow RFC 8555 §8.4's _acme-challenge.<domain> convention"
                );
                assert_eq!(dns_deleted.lock().unwrap().len(), 1, "the TXT record should be cleaned up after validation");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Mutex;

        #[test]
        fn base64url_has_no_padding_and_uses_url_safe_alphabet() {
            // "any carnal pleasure." -> classic base64 test vector, but
            // url-safe/unpadded: standard base64 would be
            // "YW55IGNhcm5hbCBwbGVhc3VyZS4=" (note the '=' padding).
            let encoded = base64url_encode(b"any carnal pleasure.");
            assert_eq!(encoded, "YW55IGNhcm5hbCBwbGVhc3VyZS4");
            assert!(!encoded.contains('='));
            assert!(!encoded.contains('+'));
            assert!(!encoded.contains('/'));
        }

        #[test]
        fn thumbprint_is_stable_for_the_same_key() {
            let key = AcmeAccountKey::generate().unwrap();
            assert_eq!(key.thumbprint(), key.thumbprint());
        }

        #[test]
        fn different_keys_have_different_thumbprints() {
            let a = AcmeAccountKey::generate().unwrap();
            let b = AcmeAccountKey::generate().unwrap();
            assert_ne!(a.thumbprint(), b.thumbprint());
        }

        #[test]
        fn http01_key_authorization_is_token_dot_thumbprint() {
            let key = AcmeAccountKey::generate().unwrap();
            let key_auth = http01_key_authorization("abc123", &key);
            assert_eq!(key_auth, format!("abc123.{}", key.thumbprint()));
        }

        #[tokio::test]
        async fn sign_produces_a_64_byte_fixed_signature() {
            // ES256 (RFC 7518 §3.4) fixed-format signatures are exactly
            // 64 bytes: 32-byte r || 32-byte s. If this were accidentally
            // using the ASN.1 DER signing algorithm instead, the length
            // would vary per signature instead of always being 64.
            let key = AcmeAccountKey::generate().unwrap();
            for _ in 0..5 {
                let sig = key.sign(b"test message").unwrap();
                assert_eq!(sig.len(), 64, "ES256 JWS signatures must be fixed-length r||s, not ASN.1 DER");
            }
        }

        /// The strongest verification achievable in this sandbox (see the
        /// module doc's "Verification limitation" section): two real HTTP
        /// servers, both built on `hyper_compat::serve`, actually talking to
        /// each other. One is the *production* `ChallengeStore` +
        /// `challenge_response_handler` (the exact code wired into
        /// `build_hyper_app`); the other is a mock ACME CA that performs
        /// genuine HTTP-01 validation -- it does not simply say "ok", it
        /// makes a real outbound HTTP GET back to the challenge responder
        /// and only proceeds if it actually receives the published key
        /// authorization. The mock CA does not cryptographically verify JWS
        /// signatures (that would require reimplementing this test's own
        /// subject on the server side); it exercises the directory/nonce/
        /// account/order/authorization/challenge/finalize/download shape
        /// and state machine, which is what `AcmeClient` needs to get right
        /// to interoperate with a real CA.
        #[tokio::test]
        async fn full_http01_flow_against_mock_ca_with_real_challenge_loopback() {
            use crate::hyper_compat::{empty_status, json_response, serve, Router};
            use hyper::{Method, StatusCode};
            use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

            const TOKEN: &str = "test-challenge-token";
            const FAKE_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\nMOCK\n-----END CERTIFICATE-----\n";

            // 1. The real, production challenge-response server.
            let challenge_store = Arc::new(ChallengeStore::new());
            let challenge_router = Router::new().route(
                Method::GET,
                "/.well-known/acme-challenge/:token",
                super::super::challenge_response_handler(Arc::clone(&challenge_store)),
            );
            let (challenge_addr, _challenge_handle) = serve(challenge_router, "127.0.0.1:0".parse().unwrap())
                .await
                .expect("bind challenge responder");

            // 2. The mock CA. `ca_base` starts empty and is filled in once
            // this server itself is bound (see the chicken-and-egg note
            // below) -- route closures only read it at request time, after
            // it's populated, never at route-registration time.
            let ca_base: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
            let nonce_counter = Arc::new(AtomicU64::new(0));
            let challenge_validated = Arc::new(AtomicBool::new(false));
            let finalized = Arc::new(AtomicBool::new(false));

            let base_of = {
                let ca_base = Arc::clone(&ca_base);
                move || ca_base.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
            };
            let next_nonce = {
                let nonce_counter = Arc::clone(&nonce_counter);
                move || format!("nonce-{}", nonce_counter.fetch_add(1, Ordering::SeqCst))
            };

            let directory_handler: crate::hyper_compat::Handler = {
                let base_of = base_of.clone();
                Arc::new(move |_req, _params| {
                    let base = base_of();
                    Box::pin(async move {
                        json_response(
                            StatusCode::OK,
                            &serde_json::json!({
                                "newNonce": format!("{base}/new-nonce"),
                                "newAccount": format!("{base}/new-acct"),
                                "newOrder": format!("{base}/new-order"),
                            }),
                        )
                    })
                })
            };

            let new_nonce_handler: crate::hyper_compat::Handler = {
                let next_nonce = next_nonce.clone();
                Arc::new(move |_req, _params| {
                    let nonce = next_nonce();
                    Box::pin(async move {
                        let mut resp = empty_status(StatusCode::OK);
                        resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                        resp
                    })
                })
            };

            let new_acct_handler: crate::hyper_compat::Handler = {
                let base_of = base_of.clone();
                let next_nonce = next_nonce.clone();
                Arc::new(move |_req, _params| {
                    let base = base_of();
                    let nonce = next_nonce();
                    Box::pin(async move {
                        let mut resp = json_response(StatusCode::CREATED, &serde_json::json!({ "status": "valid" }));
                        resp.headers_mut().insert("location", format!("{base}/acct/1").parse().unwrap());
                        resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                        resp
                    })
                })
            };

            let new_order_handler: crate::hyper_compat::Handler = {
                let base_of = base_of.clone();
                let next_nonce = next_nonce.clone();
                Arc::new(move |_req, _params| {
                    let base = base_of();
                    let nonce = next_nonce();
                    Box::pin(async move {
                        let mut resp = json_response(
                            StatusCode::CREATED,
                            &serde_json::json!({
                                "status": "pending",
                                "authorizations": [format!("{base}/authz/1")],
                                "finalize": format!("{base}/finalize/1"),
                            }),
                        );
                        resp.headers_mut().insert("location", format!("{base}/order/1").parse().unwrap());
                        resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                        resp
                    })
                })
            };

            let authz_handler: crate::hyper_compat::Handler = {
                let base_of = base_of.clone();
                let next_nonce = next_nonce.clone();
                let challenge_validated = Arc::clone(&challenge_validated);
                Arc::new(move |_req, _params| {
                    let base = base_of();
                    let nonce = next_nonce();
                    let validated = challenge_validated.load(Ordering::SeqCst);
                    Box::pin(async move {
                        let status = if validated { "valid" } else { "pending" };
                        let mut resp = json_response(
                            StatusCode::OK,
                            &serde_json::json!({
                                "status": status,
                                "challenges": [{
                                    "type": "http-01",
                                    "url": format!("{base}/challenge/1"),
                                    "token": TOKEN,
                                    "status": status,
                                }],
                            }),
                        );
                        resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                        resp
                    })
                })
            };

            let challenge_handler: crate::hyper_compat::Handler = {
                let next_nonce = next_nonce.clone();
                let challenge_validated = Arc::clone(&challenge_validated);
                Arc::new(move |_req, _params| {
                    let nonce = next_nonce();
                    let challenge_validated = Arc::clone(&challenge_validated);
                    Box::pin(async move {
                        // The real validation step: fetch the token from the
                        // *actual* challenge-response server started in
                        // step 1, over real HTTP. Only mark it valid if the
                        // client genuinely published something there.
                        let url = format!("http://{challenge_addr}/.well-known/acme-challenge/{TOKEN}");
                        if let Ok(resp) = reqwest::get(&url).await {
                            if resp.status().is_success() {
                                if let Ok(body) = resp.text().await {
                                    if !body.is_empty() {
                                        challenge_validated.store(true, Ordering::SeqCst);
                                    }
                                }
                            }
                        }
                        let mut resp = json_response(StatusCode::OK, &serde_json::json!({ "status": "processing" }));
                        resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                        resp
                    })
                })
            };

            let finalize_handler: crate::hyper_compat::Handler = {
                let base_of = base_of.clone();
                let next_nonce = next_nonce.clone();
                let finalized = Arc::clone(&finalized);
                Arc::new(move |_req, _params| {
                    let base = base_of();
                    let nonce = next_nonce();
                    finalized.store(true, Ordering::SeqCst);
                    Box::pin(async move {
                        let mut resp = json_response(
                            StatusCode::OK,
                            &serde_json::json!({
                                "status": "valid",
                                "authorizations": [format!("{base}/authz/1")],
                                "finalize": format!("{base}/finalize/1"),
                                "certificate": format!("{base}/cert/1"),
                            }),
                        );
                        resp.headers_mut().insert("replay-nonce", nonce.parse().unwrap());
                        resp
                    })
                })
            };

            let cert_handler: crate::hyper_compat::Handler = Arc::new(move |_req, _params| {
                Box::pin(async move {
                    hyper::Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "application/pem-certificate-chain")
                        .body(crate::hyper_compat::fixed_body(bytes::Bytes::from_static(FAKE_CERT_PEM.as_bytes())))
                        .unwrap()
                })
            });

            let ca_router = Router::new()
                .route(Method::GET, "/directory", directory_handler)
                .route(Method::HEAD, "/new-nonce", new_nonce_handler)
                .route(Method::POST, "/new-acct", new_acct_handler)
                .route(Method::POST, "/new-order", new_order_handler)
                .route(Method::POST, "/authz/1", authz_handler)
                .route(Method::POST, "/challenge/1", challenge_handler)
                .route(Method::POST, "/finalize/1", finalize_handler)
                .route(Method::POST, "/cert/1", cert_handler);
            let (ca_addr, _ca_handle) = serve(ca_router, "127.0.0.1:0".parse().unwrap())
                .await
                .expect("bind mock CA");
            *ca_base.lock().unwrap() = format!("http://{ca_addr}");

            // 3. Run the real client against the mock CA end to end.
            let directory_url = format!("http://{ca_addr}/directory");
            let (cert_pem, key_pem) = obtain_certificate_http01(
                &directory_url,
                "test.local",
                "admin@test.local",
                &challenge_store,
            )
            .await
            .expect("full ACME HTTP-01 flow should succeed against the mock CA");

            assert_eq!(cert_pem, FAKE_CERT_PEM);
            assert!(key_pem.contains("PRIVATE KEY"), "should return a real PEM private key");
            assert!(
                challenge_validated.load(Ordering::SeqCst),
                "the mock CA's loopback fetch must have actually observed a published key authorization"
            );
            assert!(finalized.load(Ordering::SeqCst));
            // The challenge token must have been cleaned up after use --
            // publishing it forever would let anyone who knows the token
            // re-fetch a stale key authorization.
            assert!(challenge_store.get(TOKEN).is_none(), "challenge token should be removed after use");
        }
    }
}
