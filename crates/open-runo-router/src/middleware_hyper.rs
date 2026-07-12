//! Poem-free middleware for the hyper_compat stack: plain functions that
//! wrap a [`Handler`] and return a new [`Handler`] ("function in, function
//! out" composition — see `hyper_compat.rs` module doc), rather than
//! reimplementing `poem::Middleware`/`Endpoint` traits.

use crate::hyper_compat::{empty_status, fixed_body, json_response, Handler, Response};
use flate2::write::GzEncoder;
use flate2::Compression;
use http_body_util::BodyExt;
use hyper::StatusCode;
use open_runo_api_types::RateLimitedResponse;
use open_runo_security::RateLimiter;
use std::env;
use std::io::Write;
use std::sync::Arc;

/// Minimum response body size (in bytes) before we bother gzip-compressing
/// it. Small bodies (typical JSON error/status payloads) don't shrink
/// meaningfully after gzip's fixed framing overhead, so compressing them
/// just burns CPU for no benefit. Chosen pragmatically, not from a spec.
const COMPRESSION_MIN_SIZE: usize = 512;

/// Wrap `inner` so responses are gzip-compressed when the client sent
/// `Accept-Encoding: gzip` and the body is large enough to be worth it
/// (see [`COMPRESSION_MIN_SIZE`]). Poem-free port of Poem's `Compression`
/// middleware — gzip only for this first pass (see `with_compression`'s
/// doc for why brotli was left out).
///
/// Brotli tradeoff: Poem's `Compression` middleware also supports br/deflate
/// via the `async-compression` crate. We deliberately did not add a brotli
/// encoder here — the pure-Rust brotli encoder crates available (`brotli`,
/// `brotlic`) either pull in a C build step or have much less mileage than
/// `flate2`/zlib, and gzip alone already gets the bulk of the win for JSON
/// API responses (both are well within gzip's sweet spot for text). If a
/// pure-Rust, low-risk brotli encoder becomes available/needed later this
/// can be added as a second candidate, negotiated the same way, without any
/// dishonestly-quiet gzip removal.
pub fn with_compression(inner: Handler) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let accepts_gzip = req
            .headers()
            .get(hyper::header::ACCEPT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').any(|part| part.trim().starts_with("gzip")))
            .unwrap_or(false);

        Box::pin(async move {
            let resp = inner(req, params).await;
            if !accepts_gzip {
                return resp;
            }
            // Never double-compress (e.g. a handler that already streams
            // pre-encoded bytes, or a response some other middleware already
            // compressed).
            if resp.headers().contains_key(hyper::header::CONTENT_ENCODING) {
                return resp;
            }

            let (mut parts, body) = resp.into_parts();
            let collected = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(never) => match never {},
            };
            if collected.len() < COMPRESSION_MIN_SIZE {
                return Response::from_parts(parts, fixed_body(collected));
            }

            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            let compressed = match encoder.write_all(&collected).and_then(|_| encoder.finish()) {
                Ok(compressed) => compressed,
                // Should not happen for an in-memory Vec<u8> sink, but if it
                // ever does, fail open with the original uncompressed body
                // rather than dropping the response.
                Err(_) => return Response::from_parts(parts, fixed_body(collected)),
            };

            parts.headers.insert(hyper::header::CONTENT_ENCODING, "gzip".parse().unwrap());
            parts.headers.insert(
                hyper::header::CONTENT_LENGTH,
                compressed.len().to_string().parse().unwrap(),
            );
            parts.headers.remove(hyper::header::TRANSFER_ENCODING);
            Response::from_parts(parts, fixed_body(bytes::Bytes::from(compressed)))
        })
    })
}

/// Wrap `inner` so every response gets CORS headers, and `OPTIONS`
/// preflight requests are answered directly without reaching `inner`.
/// Poem-free port of `middleware::cors::build_cors`'s behavior.
pub fn with_cors(inner: Handler) -> Handler {
    let origins = env::var("OPEN_RUNO_CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let allowed: Vec<String> = origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let allow_origin_header = req
            .headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .map(|origin| {
                if allowed.is_empty() || allowed.iter().any(|a| a == origin) {
                    origin.to_string()
                } else {
                    "null".to_string()
                }
            })
            .unwrap_or_else(|| "*".to_string());
        let is_preflight = req.method() == hyper::Method::OPTIONS;

        Box::pin(async move {
            let mut resp: Response = if is_preflight {
                empty_status(StatusCode::OK)
            } else {
                inner(req, params).await
            };
            let headers = resp.headers_mut();
            headers.insert(
                "access-control-allow-origin",
                allow_origin_header.parse().unwrap_or_else(|_| "*".parse().unwrap()),
            );
            headers.insert(
                "access-control-allow-methods",
                "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
            );
            headers.insert(
                "access-control-allow-headers",
                "x-api-key, authorization, content-type".parse().unwrap(),
            );
            headers.insert("access-control-max-age", "3600".parse().unwrap());
            resp
        })
    })
}

/// Wrap `inner` so every request/response pair is logged via `tracing`,
/// tagged with a request ID that also comes back as the `X-Request-Id`
/// response header. If the caller already sent an `X-Request-Id` (e.g. a
/// load balancer, or a client chaining its own trace), that value is
/// reused instead of minting a new one, so a single request can be
/// correlated end-to-end across hops. Otherwise a fresh UUID v4 is
/// generated. Clients (the WASM frontend, `open-runo-cli`) surface this
/// ID in error messages so a user can hand it to whoever reads the
/// server's logs instead of describing "it just failed".
pub fn with_tracing(inner: Handler) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let request_id = req
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Box::pin(async move {
            let mut resp = inner(req, params).await;
            tracing::info!(%method, %path, status = %resp.status(), %request_id, "request");
            if let Ok(value) = request_id.parse() {
                resp.headers_mut().insert("x-request-id", value);
            }
            resp
        })
    })
}

/// Wrap `inner` so every request/response pair is recorded into `metrics`
/// (monthly request-count metering + per-operation latency/error-rate,
/// `docs/cosmo-parity.md` 4a). Deliberately its own combinator rather than
/// folded into [`with_tracing`] -- both need the same method/path/status/
/// duration tuple and are meant to sit next to each other in the `wrap`
/// stack, but tracing is a diagnostic side-channel while this one feeds a
/// queryable REST API (`/api/analytics/*`), so keeping them separate lets
/// either be removed/reordered independently.
pub fn with_metrics(inner: Handler, metrics: Arc<open_runo_observability::RequestMetrics>) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let metrics = Arc::clone(&metrics);
        let method = req.method().to_string();
        let path = req.uri().path().to_string();
        let start = std::time::Instant::now();
        Box::pin(async move {
            let resp = inner(req, params).await;
            let duration_ms = start.elapsed().as_millis() as u64;
            metrics.record(&method, &path, resp.status().as_u16(), duration_ms, chrono::Utc::now());
            resp
        })
    })
}

/// Wrap `inner` with a per-client rate limit backed by `limiter`. Shared
/// across every route in an app so the budget is global, not per-route —
/// build one `Arc<RateLimiter>` per app with [`build_rate_limiter`] and
/// pass clones of it to each route's wrapper.
pub fn with_shared_rate_limit(inner: Handler, limiter: Arc<RateLimiter>) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let limiter = Arc::clone(&limiter);
        let key = req
            .headers()
            .get("x-forwarded-for")
            .or_else(|| req.headers().get("x-real-ip"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| "anonymous".to_string());

        Box::pin(async move {
            let now = chrono::Utc::now();
            if limiter.check(&key, now).is_err() {
                let retry_after_secs = limiter.seconds_until_reset(&key, now);
                let mut resp = json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    &RateLimitedResponse {
                        error: "rate limit exceeded, see retry_after_secs".to_string(),
                        retry_after_secs,
                    },
                );
                if let Ok(value) = retry_after_secs.to_string().parse() {
                    resp.headers_mut().insert("retry-after", value);
                }
                return resp;
            }
            inner(req, params).await
        })
    })
}

/// Construct a rate limiter suitable for [`with_shared_rate_limit`].
pub fn build_rate_limiter(max_requests: u32, window_secs: i64) -> Arc<RateLimiter> {
    Arc::new(RateLimiter::new(max_requests, chrono::Duration::seconds(window_secs)))
}

/// Wrap `inner` with a per-client rate limit, keyed the same way as the
/// poem `RateLimit` middleware (`X-Forwarded-For` / `X-Real-IP`, falling
/// back to a single shared bucket). Convenience wrapper over
/// [`with_shared_rate_limit`] for callers that only need a single route
/// (e.g. tests); apps with multiple routes should build one limiter with
/// [`build_rate_limiter`] and share it via [`with_shared_rate_limit`].
pub fn with_rate_limit(inner: Handler, max_requests: u32, window_secs: i64) -> Handler {
    with_shared_rate_limit(inner, build_rate_limiter(max_requests, window_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{serve, Router};
    use hyper::Method;

    fn ok_handler() -> Handler {
        Arc::new(|_req, _params| Box::pin(async { empty_status(StatusCode::OK) }))
    }

    fn large_json_handler() -> Handler {
        Arc::new(|_req, _params| {
            Box::pin(async {
                let long_string = "x".repeat(2000);
                json_response(StatusCode::OK, &serde_json::json!({ "payload": long_string }))
            })
        })
    }

    #[tokio::test]
    async fn compression_gzips_large_body_when_accepted() {
        let router =
            Router::new().route(Method::GET, "/x", with_compression(large_json_handler()));
        let (addr, _handle) =
            serve(router, "127.0.0.1:0".parse().unwrap()).await.expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/x"))
            .header("accept-encoding", "gzip")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(
            resp.headers().get("content-encoding").expect("content-encoding header").to_str().unwrap(),
            "gzip"
        );
        let content_length: usize = resp
            .headers()
            .get("content-length")
            .expect("content-length header")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        // reqwest auto-decompresses if a `gzip` cargo feature were enabled,
        // but this crate's dev-dependency reqwest doesn't enable it, so the
        // raw (still-gzipped) bytes are what we get here.
        let raw_body = resp.bytes().await.unwrap();
        assert_eq!(raw_body.len(), content_length);
        assert!(
            raw_body.len() < 2000,
            "gzip-compressed body ({} bytes) should be much smaller than the 2000+ byte original",
            raw_body.len()
        );

        // Decompressing it should hand back the original JSON payload.
        use std::io::Read;
        let mut decoder = flate2::read::GzDecoder::new(&raw_body[..]);
        let mut decoded = String::new();
        decoder.read_to_string(&mut decoded).expect("valid gzip stream");
        let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(value["payload"].as_str().unwrap().len(), 2000);
    }

    #[tokio::test]
    async fn compression_is_skipped_without_accept_encoding() {
        let router =
            Router::new().route(Method::GET, "/x", with_compression(large_json_handler()));
        let (addr, _handle) =
            serve(router, "127.0.0.1:0".parse().unwrap()).await.expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/x"))
            .send()
            .await
            .expect("request should succeed");
        assert!(resp.headers().get("content-encoding").is_none());
        let body = resp.text().await.unwrap();
        assert!(body.len() > 2000);
    }

    #[tokio::test]
    async fn compression_skips_small_bodies_even_when_accepted() {
        let router = Router::new().route(Method::GET, "/x", with_compression(ok_handler()));
        let (addr, _handle) =
            serve(router, "127.0.0.1:0".parse().unwrap()).await.expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/x"))
            .header("accept-encoding", "gzip")
            .send()
            .await
            .expect("request should succeed");
        assert!(resp.headers().get("content-encoding").is_none());
    }

    #[tokio::test]
    async fn cors_adds_headers_and_answers_preflight() {
        let router = Router::new()
            .route(Method::GET, "/x", with_cors(ok_handler()))
            .route(Method::OPTIONS, "/x", with_cors(ok_handler()));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .request(reqwest::Method::OPTIONS, format!("http://{addr}/x"))
            .header("origin", "https://example.com")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(resp.headers().contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn rate_limit_blocks_after_threshold() {
        let router = Router::new().route(Method::GET, "/x", with_rate_limit(ok_handler(), 2, 60));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        assert_eq!(
            client.get(format!("http://{addr}/x")).send().await.unwrap().status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client.get(format!("http://{addr}/x")).send().await.unwrap().status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client.get(format!("http://{addr}/x")).send().await.unwrap().status(),
            reqwest::StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn rate_limit_response_has_retry_after_header_and_typed_body() {
        let router = Router::new().route(Method::GET, "/x", with_rate_limit(ok_handler(), 1, 60));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let _ = client.get(format!("http://{addr}/x")).send().await.unwrap();
        let blocked = client.get(format!("http://{addr}/x")).send().await.unwrap();
        assert_eq!(blocked.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        let retry_after_header: i64 = blocked
            .headers()
            .get("retry-after")
            .expect("retry-after header should be present")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=60).contains(&retry_after_header));

        let body: open_runo_api_types::RateLimitedResponse = blocked.json().await.unwrap();
        assert_eq!(body.retry_after_secs, retry_after_header);
    }

    #[tokio::test]
    async fn tracing_assigns_a_request_id_when_caller_sends_none() {
        let router = Router::new().route(Method::GET, "/x", with_tracing(ok_handler()));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new().get(format!("http://{addr}/x")).send().await.unwrap();
        let request_id = resp
            .headers()
            .get("x-request-id")
            .expect("x-request-id header should be present")
            .to_str()
            .unwrap();
        assert!(uuid::Uuid::parse_str(request_id).is_ok(), "should be a valid UUID: {request_id}");
    }

    #[tokio::test]
    async fn tracing_echoes_a_caller_supplied_request_id() {
        let router = Router::new().route(Method::GET, "/x", with_tracing(ok_handler()));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/x"))
            .header("x-request-id", "caller-chosen-id-123")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.headers().get("x-request-id").unwrap().to_str().unwrap(), "caller-chosen-id-123");
    }

    #[tokio::test]
    async fn rate_limit_separate_keys_get_separate_budgets() {
        let router = Router::new().route(Method::GET, "/x", with_rate_limit(ok_handler(), 1, 60));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("http://{addr}/x"))
                .header("x-forwarded-for", "1.1.1.1")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client
                .get(format!("http://{addr}/x"))
                .header("x-forwarded-for", "2.2.2.2")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client
                .get(format!("http://{addr}/x"))
                .header("x-forwarded-for", "1.1.1.1")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::TOO_MANY_REQUESTS
        );
    }
}
