//! 最小のJWT層。署名・検証の暗号は`ring`に任せ、自前では実装しない。
//!
//! 対応は HS256(署名・検証)と RS256(検証。テスト用に署名も)だけ。
//! `rsa`クレートや`crypto-bigint`を引かない(RUSTSEC-2023-0071を依存から外すため)。
//! ヘッダーの`alg`は期待値と完全一致しなければ拒否する(`none`や鍵種のすり替えを許さない)。

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use ring::{hmac, signature};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error(m.into()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub alg: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub kid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub typ: Option<String>,
}

/// 検証の条件。`exp`は必須で、`leeway`秒の時計ずれだけ許す。
#[derive(Debug, Clone, Default)]
pub struct Validation {
    pub leeway: u64,
    pub issuer: Option<String>,
    pub audience: Option<String>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn split(token: &str) -> Result<(&str, &str, &str)> {
    let mut it = token.split('.');
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() => Ok((h, p, s)),
        _ => err("malformed token"),
    }
}

fn b64(s: &str) -> Result<Vec<u8>> {
    B64.decode(s).map_err(|e| Error(format!("invalid base64url: {e}")))
}

pub fn decode_header(token: &str) -> Result<Header> {
    let (h, _, _) = split(token)?;
    serde_json::from_slice(&b64(h)?).map_err(|e| Error(format!("invalid header: {e}")))
}

fn sign_input<T: Serialize>(claims: &T, alg: &str, kid: Option<&str>) -> Result<String> {
    let header = Header { alg: alg.into(), kid: kid.map(str::to_string), typ: Some("JWT".into()) };
    let h = serde_json::to_vec(&header).map_err(|e| Error(e.to_string()))?;
    let p = serde_json::to_vec(claims).map_err(|e| Error(e.to_string()))?;
    Ok(format!("{}.{}", B64.encode(h), B64.encode(p)))
}

pub fn encode_hs256<T: Serialize>(claims: &T, secret: &[u8]) -> Result<String> {
    let input = sign_input(claims, "HS256", None)?;
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret), input.as_bytes());
    Ok(format!("{input}.{}", B64.encode(tag.as_ref())))
}

/// 署名を確かめたあとで、`exp`・`nbf`・`iss`・`aud`を検査して本文を返す。
fn check_claims<T: DeserializeOwned>(payload_b64: &str, v: &Validation) -> Result<T> {
    let value: serde_json::Value =
        serde_json::from_slice(&b64(payload_b64)?).map_err(|e| Error(format!("invalid claims: {e}")))?;
    let t = now();
    let exp = value.get("exp").and_then(|x| x.as_u64()).ok_or_else(|| Error("missing exp".into()))?;
    if exp + v.leeway <= t {
        return err("token expired");
    }
    if let Some(nbf) = value.get("nbf").and_then(|x| x.as_u64()) {
        if nbf > t + v.leeway {
            return err("token not yet valid");
        }
    }
    if let Some(iss) = &v.issuer {
        if value.get("iss").and_then(|x| x.as_str()) != Some(iss.as_str()) {
            return err("invalid issuer");
        }
    }
    if let Some(aud) = &v.audience {
        let ok = match value.get("aud") {
            Some(serde_json::Value::String(s)) => s == aud,
            Some(serde_json::Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(aud.as_str())),
            _ => false,
        };
        if !ok {
            return err("invalid audience");
        }
    }
    serde_json::from_value(value).map_err(|e| Error(format!("invalid claims: {e}")))
}

pub fn decode_hs256<T: DeserializeOwned>(token: &str, secret: &[u8], v: &Validation) -> Result<T> {
    let (h, p, s) = split(token)?;
    if decode_header(token)?.alg != "HS256" {
        return err("unexpected algorithm");
    }
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::verify(&key, format!("{h}.{p}").as_bytes(), &b64(s)?).map_err(|_| Error("invalid signature".into()))?;
    check_claims(p, v)
}

/// RS256の検証鍵(JWKの`n`と`e`)。
#[derive(Debug, Clone)]
pub struct RsaPublicKey {
    n: Vec<u8>,
    e: Vec<u8>,
}

impl RsaPublicKey {
    pub fn from_components(n_b64: &str, e_b64: &str) -> Result<Self> {
        let (n, e) = (b64(n_b64)?, b64(e_b64)?);
        if n.is_empty() || e.is_empty() {
            return err("empty RSA component");
        }
        Ok(Self { n, e })
    }
}

pub fn decode_rs256<T: DeserializeOwned>(token: &str, key: &RsaPublicKey, v: &Validation) -> Result<T> {
    let (h, p, s) = split(token)?;
    if decode_header(token)?.alg != "RS256" {
        return err("unexpected algorithm");
    }
    let pk = signature::RsaPublicKeyComponents { n: key.n.as_slice(), e: key.e.as_slice() };
    pk.verify(&signature::RSA_PKCS1_2048_8192_SHA256, format!("{h}.{p}").as_bytes(), &b64(s)?)
        .map_err(|_| Error("invalid signature".into()))?;
    check_claims(p, v)
}

/// テスト用のRS256署名(PKCS#8のDER鍵)。本番の発行には使わない想定。
pub fn encode_rs256<T: Serialize>(claims: &T, kid: Option<&str>, pkcs8_der: &[u8]) -> Result<String> {
    let kp = signature::RsaKeyPair::from_pkcs8(pkcs8_der).map_err(|e| Error(format!("invalid RSA key: {e}")))?;
    let input = sign_input(claims, "RS256", kid)?;
    let mut sig = vec![0u8; kp.public().modulus_len()];
    kp.sign(&signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), input.as_bytes(), &mut sig)
        .map_err(|_| Error("signing failed".into()))?;
    Ok(format!("{input}.{}", B64.encode(sig)))
}

/// PEMのPKCS#8秘密鍵をDERへ直す(テスト用)。
pub fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    use base64::engine::general_purpose::STANDARD;
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    STANDARD.decode(body.trim()).map_err(|e| Error(format!("invalid PEM: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn exp(secs: i64) -> u64 {
        (now() as i64 + secs) as u64
    }

    #[test]
    fn hs256_round_trip_and_rejections() {
        let t = encode_hs256(&json!({"sub":"a","exp":exp(60)}), b"k").unwrap();
        let c: serde_json::Value = decode_hs256(&t, b"k", &Validation::default()).unwrap();
        assert_eq!(c["sub"], "a");
        assert!(decode_hs256::<serde_json::Value>(&t, b"other", &Validation::default()).is_err());
        let old = encode_hs256(&json!({"sub":"a","exp":exp(-1)}), b"k").unwrap();
        assert!(decode_hs256::<serde_json::Value>(&old, b"k", &Validation::default()).is_err());
        let noexp = encode_hs256(&json!({"sub":"a"}), b"k").unwrap();
        assert!(decode_hs256::<serde_json::Value>(&noexp, b"k", &Validation::default()).is_err());
        // 署名の改ざん・alg=none
        let (h, p, _) = split(&t).unwrap();
        assert!(decode_hs256::<serde_json::Value>(&format!("{h}.{p}.AAAA"), b"k", &Validation::default()).is_err());
        let none = format!("{}.{p}.", B64.encode(br#"{"alg":"none"}"#));
        assert!(decode_hs256::<serde_json::Value>(&none, b"k", &Validation::default()).is_err());
    }

    #[test]
    fn issuer_and_audience_are_enforced() {
        let t = encode_hs256(&json!({"exp":exp(60),"iss":"i","aud":["x","y"]}), b"k").unwrap();
        let ok = Validation { issuer: Some("i".into()), audience: Some("y".into()), ..Default::default() };
        assert!(decode_hs256::<serde_json::Value>(&t, b"k", &ok).is_ok());
        let bad = Validation { issuer: Some("z".into()), ..Default::default() };
        assert!(decode_hs256::<serde_json::Value>(&t, b"k", &bad).is_err());
        let bad = Validation { audience: Some("z".into()), ..Default::default() };
        assert!(decode_hs256::<serde_json::Value>(&t, b"k", &bad).is_err());
    }

    #[test]
    fn an_hs256_token_is_not_accepted_as_rs256() {
        let t = encode_hs256(&json!({"exp":exp(60)}), b"k").unwrap();
        let key = RsaPublicKey::from_components("AQAB", "AQAB").unwrap();
        assert!(decode_rs256::<serde_json::Value>(&t, &key, &Validation::default()).is_err());
    }
}
