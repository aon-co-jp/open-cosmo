//! Webhook受信: 本文の読み取り上限・HMAC-SHA256署名検証・リプレイ対策。
//!
//! GitHub(`X-Hub-Signature-256`)、Slack(`X-Slack-Signature`+タイムスタンプ)、
//! 任意のヘッダ名の汎用方式に対応する。署名比較は `hmac` クレートの
//! `verify_slice`(定数時間)で行い、自前の比較は書かない。
//!
//! 署名は「生の本文」に対して計算されるため、JSONへ変換する前に検証する
//! 必要がある。`Request` の本文型は差し替えられないので、ミドルウェアでは
//! なく [`webhook_handler`] / [`read_verified_webhook`] をハンドラ側で使う。

#![allow(clippy::result_large_err)]

use crate::body_limit::read_body_limited;
use crate::hyper_compat::{empty_status, json_response, Handler, Request, Response};
use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use hyper::{HeaderMap, StatusCode};
use sha2::Sha256;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type HmacSha256 = Hmac<Sha256>;

/// 署名の方式。
#[derive(Debug, Clone)]
pub enum SignatureScheme {
    /// GitHub: `X-Hub-Signature-256: sha256=<hex>`、対象は本文そのまま。
    GitHub,
    /// Slack: `X-Slack-Signature: v0=<hex>`、対象は `v0:{timestamp}:{body}`、
    /// タイムスタンプは `X-Slack-Request-Timestamp`。
    Slack,
    /// 任意: `header` に `prefix<hex>`。`timestamp_header` があれば
    /// 対象は `{timestamp}.{body}`、無ければ本文そのまま。
    Generic {
        header: String,
        prefix: String,
        timestamp_header: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    /// 有効な秘密鍵。鍵の入れ替え(ローテーション)中は新旧を並べる。
    pub secrets: Vec<Vec<u8>>,
    pub scheme: SignatureScheme,
    /// 本文の上限(バイト)。
    pub max_body_bytes: usize,
    /// タイムスタンプ付き方式で許す時刻のずれ。
    pub tolerance: Duration,
    /// 配送IDのヘッダ名(例: `X-GitHub-Delivery`)。指定すると同じIDの
    /// 再送を `409` で拒否する。
    pub dedup_header: Option<String>,
}

impl WebhookConfig {
    fn base(secret: impl Into<Vec<u8>>, scheme: SignatureScheme) -> Self {
        Self {
            secrets: vec![secret.into()],
            scheme,
            max_body_bytes: 1024 * 1024,
            tolerance: Duration::from_secs(300),
            dedup_header: None,
        }
    }

    pub fn github(secret: impl Into<Vec<u8>>) -> Self {
        let mut c = Self::base(secret, SignatureScheme::GitHub);
        c.dedup_header = Some("x-github-delivery".to_string());
        c
    }

    pub fn slack(secret: impl Into<Vec<u8>>) -> Self {
        Self::base(secret, SignatureScheme::Slack)
    }

    pub fn generic(secret: impl Into<Vec<u8>>, header: &str, prefix: &str) -> Self {
        Self::base(
            secret,
            SignatureScheme::Generic {
                header: header.to_ascii_lowercase(),
                prefix: prefix.to_string(),
                timestamp_header: None,
            },
        )
    }

    pub fn with_old_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        self.secrets.push(secret.into());
        self
    }

    pub fn with_max_body_bytes(mut self, max: usize) -> Self {
        self.max_body_bytes = max;
        self
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WebhookError {
    MissingSignature,
    BadSignature,
    MissingTimestamp,
    StaleTimestamp,
    NoSecretConfigured,
}

impl WebhookError {
    pub fn status(&self) -> StatusCode {
        match self {
            WebhookError::NoSecretConfigured => StatusCode::INTERNAL_SERVER_ERROR,
            WebhookError::StaleTimestamp => StatusCode::BAD_REQUEST,
            _ => StatusCode::UNAUTHORIZED,
        }
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

fn check_timestamp(ts: &str, now_unix: i64, tolerance: Duration) -> Result<(), WebhookError> {
    let ts: i64 = ts.parse().map_err(|_| WebhookError::MissingTimestamp)?;
    if (now_unix - ts).unsigned_abs() > tolerance.as_secs() {
        return Err(WebhookError::StaleTimestamp);
    }
    Ok(())
}

/// 署名を検証する(純粋関数)。`now_unix` は現在のUNIX秒。
pub fn verify(
    config: &WebhookConfig,
    headers: &HeaderMap,
    body: &[u8],
    now_unix: i64,
) -> Result<(), WebhookError> {
    if config.secrets.is_empty() || config.secrets.iter().any(|s| s.is_empty()) {
        return Err(WebhookError::NoSecretConfigured);
    }

    // (署名ヘッダ名, 接頭辞, 署名対象の先頭に付ける文字列を作るためのタイムスタンプ)
    let (sig_header, prefix, signed_prefix): (&str, &str, Option<String>) = match &config.scheme {
        SignatureScheme::GitHub => ("x-hub-signature-256", "sha256=", None),
        SignatureScheme::Slack => {
            let ts = header_str(headers, "x-slack-request-timestamp")
                .ok_or(WebhookError::MissingTimestamp)?;
            check_timestamp(ts, now_unix, config.tolerance)?;
            ("x-slack-signature", "v0=", Some(format!("v0:{ts}:")))
        },
        SignatureScheme::Generic {
            header,
            prefix,
            timestamp_header,
        } => {
            let signed_prefix = match timestamp_header {
                Some(h) => {
                    let ts = header_str(headers, h).ok_or(WebhookError::MissingTimestamp)?;
                    check_timestamp(ts, now_unix, config.tolerance)?;
                    Some(format!("{ts}."))
                },
                None => None,
            };
            (header.as_str(), prefix.as_str(), signed_prefix)
        },
    };

    let provided = header_str(headers, sig_header).ok_or(WebhookError::MissingSignature)?;
    let hex_part = provided
        .strip_prefix(prefix)
        .ok_or(WebhookError::BadSignature)?;
    let expected = hex::decode(hex_part).map_err(|_| WebhookError::BadSignature)?;

    for secret in &config.secrets {
        // `new_from_slice` は任意長の鍵を受け付ける(HMACの仕様)。
        let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
            continue;
        };
        if let Some(p) = &signed_prefix {
            mac.update(p.as_bytes());
        }
        mac.update(body);
        if mac.verify_slice(&expected).is_ok() {
            return Ok(());
        }
    }
    Err(WebhookError::BadSignature)
}

/// 配送IDの重複を一定時間覚える入れ物(プロセス内)。件数に上限がある。
pub struct DeliveryDedup {
    seen: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
    max_entries: usize,
}

impl std::fmt::Debug for DeliveryDedup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliveryDedup")
            .field("ttl", &self.ttl)
            .field("max_entries", &self.max_entries)
            .finish()
    }
}

impl DeliveryDedup {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
            ttl,
            max_entries,
        }
    }

    /// 初めて見るIDなら `true`(記録する)。既知なら `false`。
    pub fn first_time(&self, id: &str) -> bool {
        let now = Instant::now();
        let mut seen = match self.seen.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        seen.retain(|_, t| now.duration_since(*t) < self.ttl);
        if seen.contains_key(id) {
            return false;
        }
        if seen.len() >= self.max_entries {
            // 満杯のときは一番古いものを捨てる。
            if let Some(oldest) = seen.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone()) {
                seen.remove(&oldest);
            }
        }
        seen.insert(id.to_string(), now);
        true
    }
}

/// 署名検証済みのWebhook。
#[derive(Debug)]
pub struct VerifiedWebhook {
    pub headers: HeaderMap,
    pub body: Bytes,
}

fn unauthorized(err: &WebhookError) -> Response {
    // 失敗の詳細(どの段階か)は攻撃者の手掛かりになるので応答には出さない。
    json_response(
        err.status(),
        &serde_json::json!({ "error": "webhook verification failed" }),
    )
}

/// 本文を上限つきで読み、署名を検証して返す。重複は `409`。
pub async fn read_verified_webhook(
    req: Request,
    config: &WebhookConfig,
    dedup: Option<&DeliveryDedup>,
) -> Result<VerifiedWebhook, Response> {
    let headers = req.headers().clone();
    let body = read_body_limited(req, config.max_body_bytes).await?;
    let now = chrono::Utc::now().timestamp();
    verify(config, &headers, &body, now).map_err(|e| unauthorized(&e))?;
    // 署名が正しいと確認できてから重複判定する(未認証の者がIDを
    // 先に使い切って正規の配送を弾く、という攻撃を避ける)。
    if let (Some(dedup), Some(name)) = (dedup, config.dedup_header.as_deref()) {
        if let Some(id) = header_str(&headers, name) {
            if !dedup.first_time(id) {
                return Err(json_response(
                    StatusCode::CONFLICT,
                    &serde_json::json!({ "error": "duplicate delivery" }),
                ));
            }
        }
    }
    Ok(VerifiedWebhook { headers, body })
}

/// Webhook受信用の [`Handler`] を作る。検証に通った場合だけ `f` を呼ぶ。
/// `f` が返した応答がそのまま返る(普通は `202`/`200`)。
pub fn webhook_handler<F, Fut>(config: WebhookConfig, f: F) -> Handler
where
    F: Fn(VerifiedWebhook) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let config = Arc::new(config);
    let dedup = Arc::new(DeliveryDedup::new(Duration::from_secs(3600), 10_000));
    let f = Arc::new(f);
    Arc::new(move |req, _params| {
        let config = Arc::clone(&config);
        let dedup = Arc::clone(&dedup);
        let f = Arc::clone(&f);
        Box::pin(async move {
            match read_verified_webhook(req, &config, Some(&dedup)).await {
                Ok(hook) => f(hook).await,
                Err(resp) => resp,
            }
        })
    })
}

/// 受け取ったことだけを返す標準応答(`202 Accepted`)。
pub fn accepted() -> Response {
    empty_status(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    fn hdr(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    /// GitHub公式ドキュメントの例(秘密鍵・本文)。署名値は実装とは独立に
    /// `openssl dgst -sha256 -hmac` で計算したもの。
    const GH_SECRET: &str = "It's a Secret to Everybody";
    const GH_BODY: &[u8] = b"Hello, World!";
    const GH_SIG: &str = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

    #[test]
    fn github_known_vector() {
        let cfg = WebhookConfig::github(GH_SECRET);
        let h = hdr(&[("x-hub-signature-256", GH_SIG)]);
        assert_eq!(verify(&cfg, &h, GH_BODY, 0), Ok(()));
        assert_eq!(
            verify(&cfg, &h, b"Hello, World?", 0),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn rejects_missing_malformed_and_wrong_prefix() {
        let cfg = WebhookConfig::github(GH_SECRET);
        assert_eq!(
            verify(&cfg, &HeaderMap::new(), GH_BODY, 0),
            Err(WebhookError::MissingSignature)
        );
        let h = hdr(&[("x-hub-signature-256", "sha256=zz")]);
        assert_eq!(
            verify(&cfg, &h, GH_BODY, 0),
            Err(WebhookError::BadSignature)
        );
        let h = hdr(&[("x-hub-signature-256", &GH_SIG["sha256=".len()..])]);
        assert_eq!(
            verify(&cfg, &h, GH_BODY, 0),
            Err(WebhookError::BadSignature)
        );
        // 短い/空の署名も拒否(長さ違いで通ってはならない)。
        let h = hdr(&[("x-hub-signature-256", "sha256=")]);
        assert_eq!(
            verify(&cfg, &h, GH_BODY, 0),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn empty_secret_is_refused() {
        let cfg = WebhookConfig::github(Vec::<u8>::new());
        let h = hdr(&[("x-hub-signature-256", GH_SIG)]);
        assert_eq!(
            verify(&cfg, &h, GH_BODY, 0),
            Err(WebhookError::NoSecretConfigured)
        );
    }

    #[test]
    fn secret_rotation_accepts_old_and_new() {
        let cfg = WebhookConfig::github("new-secret").with_old_secret(GH_SECRET);
        let h = hdr(&[("x-hub-signature-256", GH_SIG)]);
        assert_eq!(verify(&cfg, &h, GH_BODY, 0), Ok(()));
    }

    fn sign(secret: &[u8], payload: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(payload);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn slack_scheme_with_timestamp_window() {
        let cfg = WebhookConfig::slack("s3cret");
        let ts = "1700000000";
        let sig = format!(
            "v0={}",
            sign(b"s3cret", format!("v0:{ts}:payload").as_bytes())
        );
        let h = hdr(&[
            ("x-slack-request-timestamp", ts),
            ("x-slack-signature", &sig),
        ]);
        assert_eq!(verify(&cfg, &h, b"payload", 1_700_000_100), Ok(()));
        assert_eq!(
            verify(&cfg, &h, b"payload", 1_700_001_000),
            Err(WebhookError::StaleTimestamp)
        );
        assert_eq!(
            verify(
                &cfg,
                &hdr(&[("x-slack-signature", &sig)]),
                b"payload",
                1_700_000_100
            ),
            Err(WebhookError::MissingTimestamp)
        );
    }

    #[test]
    fn generic_scheme_with_and_without_timestamp() {
        let cfg = WebhookConfig::generic("k", "X-Sig", "sha256=");
        let sig = format!("sha256={}", sign(b"k", b"body"));
        assert_eq!(verify(&cfg, &hdr(&[("x-sig", &sig)]), b"body", 0), Ok(()));

        let mut cfg = WebhookConfig::generic("k", "X-Sig", "");
        if let SignatureScheme::Generic {
            timestamp_header, ..
        } = &mut cfg.scheme
        {
            *timestamp_header = Some("x-ts".to_string());
        }
        let sig = sign(b"k", b"100.body");
        let h = hdr(&[("x-sig", &sig), ("x-ts", "100")]);
        assert_eq!(verify(&cfg, &h, b"body", 150), Ok(()));
        assert_eq!(
            verify(&cfg, &h, b"body", 10_000),
            Err(WebhookError::StaleTimestamp)
        );
    }

    #[test]
    fn dedup_remembers_and_evicts() {
        let d = DeliveryDedup::new(Duration::from_secs(60), 2);
        assert!(d.first_time("a"));
        assert!(!d.first_time("a"));
        assert!(d.first_time("b"));
        assert!(d.first_time("c")); // 満杯 → 最古("a")を捨てる
        assert!(d.first_time("a"));
        let short = DeliveryDedup::new(Duration::from_millis(0), 10);
        assert!(short.first_time("x"));
        assert!(short.first_time("x")); // TTL切れ
    }

    #[tokio::test]
    async fn end_to_end_handler_over_real_tcp() {
        use crate::hyper_compat::{serve, Router};
        use hyper::Method;

        let handler = webhook_handler(
            WebhookConfig::github(GH_SECRET).with_max_body_bytes(1024),
            |hook| async move {
                assert_eq!(&hook.body[..], GH_BODY);
                accepted()
            },
        );
        let router = Router::new().route(Method::POST, "/hook", handler);
        let (addr, _h) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/hook");

        let ok = client
            .post(&url)
            .header("x-hub-signature-256", GH_SIG)
            .header("x-github-delivery", "d-1")
            .body(GH_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 202);

        // 同じ配送IDの再送は409。
        let dup = client
            .post(&url)
            .header("x-hub-signature-256", GH_SIG)
            .header("x-github-delivery", "d-1")
            .body(GH_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(dup.status(), 409);

        // 署名なし/不正は401、上限超過は413。
        let bad = client
            .post(&url)
            .body(GH_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 401);
        let big = client
            .post(&url)
            .header("x-hub-signature-256", GH_SIG)
            .body(vec![0u8; 4096])
            .send()
            .await
            .unwrap();
        assert_eq!(big.status(), 413);
    }
}
