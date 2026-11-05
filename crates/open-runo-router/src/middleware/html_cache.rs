//! HTML page cache middleware: serves repeat-render pages from memory,
//! with a built-in self-learning admission model (no external LLM).
//!
//! Rust+Poem re-renders the same HTML far cheaper than a JS runtime, and for
//! pages that render identically on every request, not rendering at all is
//! cheaper still. This middleware detects such pages and serves them from an
//! in-process TTL cache.
//!
//! ## Safety-first decision rules (in order)
//!
//! 1. **Method**: only `GET` is ever cached.
//! 2. **Identity**: any `Cookie` / `Authorization` / `X-Api-Key` header →
//!    bypass. Per-user pages can never leak into (or out of) the cache.
//! 3. **Path**: API-ish prefixes (`/api/`, `/scim/`, `/graphql`, `/health`)
//!    are never cached; they have their own caching layers.
//! 4. **Handler opt-out**: a response carrying `Cache-Control: private` or
//!    `no-store` is never stored.
//! 5. **Status**: only `200 OK` responses are stored.
//!
//! ## Self-learning behaviour (AI mode, default on)
//!
//! [`open_runo_cache::predictor::CachePredictor`] learns per-URL and
//! per-pattern arrival rates, render costs, and update frequencies:
//!
//! - admission decisions improve with traffic (cold-start prediction via
//!   URL patterns, cost-aware bias toward expensive pages);
//! - TTLs adapt to each page's observed update frequency;
//! - near-expiry hits trigger **refresh-ahead** (stale-while-revalidate):
//!   the cached page is served instantly and re-rendered in the background,
//!   so visitors never wait on a hot page again.
//!
//! ## Robustness
//!
//! - **Key normalization**: tracking params (`utm_*`, `fbclid`, `gclid`) are
//!   stripped and the remaining query is sorted, so `/p?utm_source=x` and
//!   `/p` share one entry (crawler-proof).
//! - **Singleflight**: on a cache miss, one request renders while identical
//!   concurrent requests wait on a per-key lock and then read the fresh
//!   entry — the backend sees exactly one render (thundering-herd guard).
//! - **Bounded memory**: backed by `open_runo_cache::InMemoryTtlCache`
//!   `with_capacity`, TTL and capacity both configurable via env.
//!
//! Config (all overridable, never hard-coded):
//! `OPEN_RUNO_HTML_CACHE=on`, `OPEN_RUNO_HTML_CACHE_TTL_SECS` (default 60),
//! `OPEN_RUNO_HTML_CACHE_MAX_ENTRIES` (default 10000),
//! `OPEN_RUNO_HTML_CACHE_MIN_HITS` (default 2, used when AI off),
//! `OPEN_RUNO_HTML_CACHE_AI=off` (AI on by default),
//! `OPEN_RUNO_HTML_CACHE_REFRESH_RATIO` (default 0.8).

use open_runo_cache::predictor::CachePredictor;
use open_runo_cache::{Cache, InMemoryTtlCache};
use poem::{
    http::{Method, StatusCode},
    Endpoint, IntoResponse, Middleware, Request, Response, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Query parameters that never affect page content (tracking noise).
const IGNORED_QUERY_PREFIXES: &[&str] = &["utm_", "fbclid", "gclid", "msclkid"];

/// Path prefixes that are never page-cached.
const BYPASS_PREFIXES: &[&str] = &["/api/", "/scim/", "/graphql", "/health", "/healthz"];

#[derive(Debug, Clone)]
pub struct HtmlCacheConfig {
    pub enabled: bool,
    pub ttl: chrono::Duration,
    pub max_entries: usize,
    /// Cache only from the N-th request for a key (bot/crawler filter).
    /// Used when `ai` is off.
    pub min_hits: u32,
    /// Self-learning admission + adaptive TTL (no external LLM; the
    /// built-in [`CachePredictor`] gets smarter with every request).
    pub ai: bool,
    /// Stale-while-revalidate: when a hit's age exceeds this fraction of
    /// its TTL, serve it instantly and re-render in the background so the
    /// next visitor gets fresh content without ever waiting (0.0–1.0;
    /// ≥1.0 disables refresh-ahead).
    pub refresh_ratio: f64,
}

impl HtmlCacheConfig {
    pub fn from_env() -> Self {
        let enabled = matches!(
            std::env::var("OPEN_RUNO_HTML_CACHE").as_deref(),
            Ok("on") | Ok("true") | Ok("1")
        );
        let ttl_secs = std::env::var("OPEN_RUNO_HTML_CACHE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(60);
        let max_entries = std::env::var("OPEN_RUNO_HTML_CACHE_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10_000);
        let min_hits = std::env::var("OPEN_RUNO_HTML_CACHE_MIN_HITS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(2);
        // AI admission is on by default; OPEN_RUNO_HTML_CACHE_AI=off falls
        // back to the fixed min-hits filter.
        let ai = !matches!(
            std::env::var("OPEN_RUNO_HTML_CACHE_AI").as_deref(),
            Ok("off") | Ok("false") | Ok("0")
        );
        let refresh_ratio = std::env::var("OPEN_RUNO_HTML_CACHE_REFRESH_RATIO")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.8);
        Self {
            enabled,
            ttl: chrono::Duration::seconds(ttl_secs.max(1)),
            max_entries: max_entries.max(1),
            min_hits: min_hits.max(1),
            ai,
            refresh_ratio,
        }
    }

    /// Test/embedder helper: enabled, deterministic min-hits mode (AI off),
    /// cache from the first request, refresh-ahead off.
    pub fn enabled_for_tests(ttl_secs: i64) -> Self {
        Self {
            enabled: true,
            ttl: chrono::Duration::seconds(ttl_secs.max(1)),
            max_entries: 1_000,
            min_hits: 1,
            ai: false,
            refresh_ratio: 2.0,
        }
    }
}

/// What we persist per page.
#[derive(Debug, Serialize, Deserialize)]
struct CachedPage {
    content_type: String,
    body: String,
    stored_at: chrono::DateTime<chrono::Utc>,
    ttl_secs: i64,
}

impl CachedPage {
    /// Age as a fraction of TTL (0.0 = fresh, 1.0 = expiring).
    fn age_fraction(&self) -> f64 {
        let age = (chrono::Utc::now() - self.stored_at)
            .num_milliseconds()
            .max(0) as f64
            / 1000.0;
        age / (self.ttl_secs.max(1) as f64)
    }
}

/// Shared page-cache state. Also injected via `Data` so purge handlers and
/// content-update handlers can invalidate entries pin-pointedly.
#[derive(Debug)]
pub struct HtmlPageCache {
    store: Arc<dyn Cache>,
    config: HtmlCacheConfig,
    /// Request counters for the min-hits filter (bounded sweep).
    hit_counts: Mutex<HashMap<String, u32>>,
    /// Per-key render locks (singleflight).
    locks: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Self-learning admission/TTL model (present when `config.ai`).
    predictor: Option<Arc<CachePredictor>>,
}

impl HtmlPageCache {
    pub fn new(config: HtmlCacheConfig) -> Self {
        let store: Arc<dyn Cache> = Arc::new(InMemoryTtlCache::with_capacity(config.max_entries));
        Self::with_store(config, store)
    }

    /// Swap in a different backend (e.g. Redis via `open-runo-cache`'s
    /// `redis-backend` feature) without touching the middleware logic.
    pub fn with_store(config: HtmlCacheConfig, store: Arc<dyn Cache>) -> Self {
        let predictor = config.ai.then(|| Arc::new(CachePredictor::from_env()));
        Self {
            store,
            config,
            hit_counts: Mutex::new(HashMap::new()),
            locks: tokio::sync::Mutex::new(HashMap::new()),
            predictor,
        }
    }

    pub fn config(&self) -> &HtmlCacheConfig {
        &self.config
    }

    /// The learned model, when AI admission is enabled
    /// (snapshot/restore + `/api/cache/ai-stats`).
    pub fn predictor(&self) -> Option<&Arc<CachePredictor>> {
        self.predictor.as_ref()
    }

    /// Pin-point purge: drop one page (call after content updates).
    /// Also feeds the volatility signal to the learning model, so pages
    /// that update often converge to shorter TTLs automatically.
    pub async fn purge(&self, path_and_query: &str) {
        let key = normalize_key(path_and_query);
        let _ = self.store.invalidate(&key).await;
        if let Some(p) = &self.predictor {
            let path = key.split('?').next().unwrap_or(&key).to_string();
            p.observe_update(&key, &path, chrono::Utc::now());
        }
    }

    /// Drop every cached page (e.g. after a template/theme change).
    pub async fn purge_all(&self) {
        let _ = self.store.clear().await;
        self.hit_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// Increment and return the request count for `key` (min-hits filter).
    fn count_hit(&self, key: &str) -> u32 {
        let mut counts = self
            .hit_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Bounded: reset the counter map rather than growing forever.
        if counts.len() >= self.config.max_entries.saturating_mul(4) {
            counts.clear();
        }
        let n = counts.entry(key.to_string()).or_insert(0);
        *n = n.saturating_add(1);
        *n
    }

    async fn render_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().await;
        // Bounded: sweep completed locks opportunistically.
        if locks.len() >= 1024 {
            locks.retain(|_, l| Arc::strong_count(l) > 1);
        }
        Arc::clone(locks.entry(key.to_string()).or_default())
    }
}

/// Normalize a path+query into a cache key: strip tracking params, sort the
/// rest, drop the query entirely when nothing meaningful remains.
pub fn normalize_key(path_and_query: &str) -> String {
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => return path_and_query.to_string(),
    };
    let mut params: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name = p.split('=').next().unwrap_or(p);
            !IGNORED_QUERY_PREFIXES
                .iter()
                .any(|ig| name == ig.trim_end_matches('_') || name.starts_with(ig))
        })
        .collect();
    if params.is_empty() {
        return path.to_string();
    }
    params.sort_unstable();
    format!("{path}?{}", params.join("&"))
}

fn is_bypassed_path(path: &str) -> bool {
    BYPASS_PREFIXES
        .iter()
        .any(|p| path == p.trim_end_matches('/') || path.starts_with(p))
}

fn has_identity(req: &Request) -> bool {
    let h = req.headers();
    h.contains_key("cookie") || h.contains_key("authorization") || h.contains_key("x-api-key")
}

fn response_opts_out(resp: &Response) -> bool {
    resp.headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("private") || v.contains("no-store")
        })
        .unwrap_or(false)
}

/// The poem middleware. Wrap any HTML-serving route tree with it:
/// `Route::new().at("/page/:id", get(render)).with(HtmlCacheMiddleware(cache))`.
#[derive(Debug, Clone)]
pub struct HtmlCacheMiddleware(pub Arc<HtmlPageCache>);

impl<E: Endpoint + 'static> Middleware<E> for HtmlCacheMiddleware {
    type Output = HtmlCacheEndpoint<E>;

    fn transform(&self, ep: E) -> Self::Output {
        HtmlCacheEndpoint {
            inner: Arc::new(ep),
            cache: Arc::clone(&self.0),
        }
    }
}

#[derive(Debug)]
pub struct HtmlCacheEndpoint<E> {
    inner: Arc<E>,
    cache: Arc<HtmlPageCache>,
}

impl<E: Endpoint + 'static> Endpoint for HtmlCacheEndpoint<E> {
    type Output = Response;

    async fn call(&self, req: Request) -> Result<Self::Output> {
        let cfg = &self.cache.config;

        // ── Decision rules (cheap checks first) ─────────────────────────
        if !cfg.enabled
            || req.method() != Method::GET
            || is_bypassed_path(req.uri().path())
            || has_identity(&req)
        {
            return self.inner.call(req).await.map(IntoResponse::into_response);
        }

        let key = normalize_key(
            req.uri()
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or_else(|| req.uri().path()),
        );

        // ── Learn + admission decision ───────────────────────────────────
        // AI mode: the predictor learns from every request and decides
        // whether this page is worth caching (pattern-based cold-start
        // prediction included). Fallback: fixed min-hits counter.
        let path = req.uri().path().to_string();
        let admit = match self.cache.predictor.as_ref() {
            Some(p) => p.observe_and_decide(&key, &path, chrono::Utc::now()),
            None => self.cache.count_hit(&key) >= cfg.min_hits,
        };

        // ── Cache hit: skip the handler entirely ────────────────────────
        if let Some((hit, age_fraction)) = self.lookup(&key).await {
            if let Some(p) = self.cache.predictor.as_ref() {
                p.record_hit();
            }
            // Refresh-ahead (stale-while-revalidate): the entry is close to
            // expiry — serve it instantly and let the AI re-render it in
            // the background, so nobody ever waits on this page again.
            if age_fraction >= cfg.refresh_ratio {
                self.spawn_background_refresh(key.clone(), path.clone(), req.uri().clone());
            }
            return Ok(hit);
        }

        // ── Not admitted (yet): render normally, uncached ────────────────
        if !admit {
            let resp = self
                .inner
                .call(req)
                .await
                .map(IntoResponse::into_response)?;
            return Ok(with_cache_header(resp, "COLD"));
        }

        if let Some(p) = self.cache.predictor.as_ref() {
            p.record_miss();
        }

        // ── Singleflight: one renderer per key ──────────────────────────
        let lock = self.cache.render_lock(&key).await;
        let _guard = lock.lock().await;

        // Double-check: a concurrent leader may have filled the cache
        // while we waited on the lock.
        if let Some((hit, _)) = self.lookup(&key).await {
            return Ok(hit);
        }

        let started = std::time::Instant::now();
        let resp = self
            .inner
            .call(req)
            .await
            .map(IntoResponse::into_response)?;
        // Server-load signal: how expensive was this render?
        if let Some(p) = self.cache.predictor.as_ref() {
            p.observe_render(&key, &path, started.elapsed().as_secs_f64());
        }

        // ── Store (safety checks on the response side) ──────────────────
        if resp.status() == StatusCode::OK && !response_opts_out(&resp) {
            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/html; charset=utf-8")
                .to_string();
            let (parts, body) = resp.into_parts();
            match body.into_string().await {
                Ok(text) => {
                    // AI mode: TTL adapts to the page's learned update
                    // frequency; otherwise the configured fixed TTL.
                    let ttl = match self.cache.predictor.as_ref() {
                        Some(p) => p.suggest_ttl(&key, &path),
                        None => cfg.ttl,
                    };
                    let page = CachedPage {
                        content_type,
                        body: text.clone(),
                        stored_at: chrono::Utc::now(),
                        ttl_secs: ttl.num_seconds().max(1),
                    };
                    if let Ok(json) = serde_json::to_string(&page) {
                        let _ = self.cache.store.set(&key, &json, ttl).await;
                    }
                    let rebuilt = Response::from_parts(parts, poem::Body::from_string(text));
                    return Ok(with_cache_header(rebuilt, "MISS"));
                },
                // Non-UTF-8 body (images etc.): pass through uncached.
                Err(_) => {
                    return Ok(with_cache_header(
                        Response::from_parts(parts, poem::Body::empty()),
                        "UNCACHEABLE",
                    ));
                },
            }
        }

        Ok(resp)
    }
}

impl<E: Endpoint + 'static> HtmlCacheEndpoint<E> {
    async fn lookup(&self, key: &str) -> Option<(Response, f64)> {
        let raw = self.cache.store.get(key).await.ok().flatten()?;
        let page: CachedPage = sonic_rs::from_str(&raw).ok()?;
        let age = page.age_fraction();
        Some((
            Response::builder()
                .header("content-type", page.content_type)
                .header("x-cache", "HIT")
                .body(page.body),
            age,
        ))
    }

    /// Re-render `key` in the background (refresh-ahead). The per-key
    /// singleflight lock guarantees at most one refresh at a time; if a
    /// refresh is already running we simply skip.
    fn spawn_background_refresh(&self, key: String, path: String, uri: poem::http::Uri) {
        let inner = Arc::clone(&self.inner);
        let cache = Arc::clone(&self.cache);

        tokio::spawn(async move {
            let lock = cache.render_lock(&key).await;
            let Ok(_guard) = lock.try_lock() else {
                return; // another render is already in flight
            };

            let req = Request::builder().method(Method::GET).uri(uri).finish();
            let started = std::time::Instant::now();
            let Ok(resp) = inner.call(req).await.map(IntoResponse::into_response) else {
                return;
            };
            if let Some(p) = cache.predictor.as_ref() {
                p.observe_render(&key, &path, started.elapsed().as_secs_f64());
            }
            if resp.status() != StatusCode::OK || response_opts_out(&resp) {
                return;
            }

            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/html; charset=utf-8")
                .to_string();
            if let Ok(text) = resp.into_body().into_string().await {
                let ttl = match cache.predictor.as_ref() {
                    Some(p) => p.suggest_ttl(&key, &path),
                    None => cache.config.ttl,
                };
                let page = CachedPage {
                    content_type,
                    body: text,
                    stored_at: chrono::Utc::now(),
                    ttl_secs: ttl.num_seconds().max(1),
                };
                if let Ok(json) = serde_json::to_string(&page) {
                    let _ = cache.store.set(&key, &json, ttl).await;
                }
            }
        });
    }
}

fn with_cache_header(mut resp: Response, value: &'static str) -> Response {
    resp.headers_mut()
        .insert("x-cache", poem::http::HeaderValue::from_static(value));
    resp
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use poem::{get, handler, test::TestClient, EndpointExt, Route};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // One counter per test: the test binary runs tests in parallel, so a
    // shared counter would race across tests.
    macro_rules! counting_page {
        ($fn_name:ident, $counter:ident) => {
            static $counter: AtomicUsize = AtomicUsize::new(0);

            #[handler]
            async fn $fn_name() -> Response {
                $counter.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .header("content-type", "text/html; charset=utf-8")
                    .body("<html><body>hello</body></html>")
            }
        };
    }

    #[handler]
    async fn render_private() -> Response {
        Response::builder()
            .header("content-type", "text/html")
            .header("cache-control", "private")
            .body("<html>my cart</html>")
    }

    fn fresh_cache(min_hits: u32) -> Arc<HtmlPageCache> {
        let mut cfg = HtmlCacheConfig::enabled_for_tests(60);
        cfg.min_hits = min_hits;
        Arc::new(HtmlPageCache::new(cfg))
    }

    #[tokio::test]
    async fn second_request_is_served_from_cache() {
        counting_page!(page_a, RENDERS_A);
        let client = TestClient::new(
            Route::new()
                .at("/page", get(page_a))
                .with(HtmlCacheMiddleware(fresh_cache(1))),
        );

        let r1 = client.get("/page").send().await;
        r1.assert_status_is_ok();
        r1.assert_header("x-cache", "MISS");

        let r2 = client.get("/page").send().await;
        r2.assert_status_is_ok();
        r2.assert_header("x-cache", "HIT");
        r2.assert_text("<html><body>hello</body></html>").await;

        assert_eq!(RENDERS_A.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn identity_headers_bypass_the_cache() {
        counting_page!(page_b, RENDERS_B);
        let client = TestClient::new(
            Route::new()
                .at("/page", get(page_b))
                .with(HtmlCacheMiddleware(fresh_cache(1))),
        );

        for _ in 0..3 {
            let r = client
                .get("/page")
                .header("cookie", "session=abc")
                .send()
                .await;
            r.assert_status_is_ok();
            // No x-cache header at all: middleware fully bypassed.
            assert!(r.0.headers().get("x-cache").is_none());
        }
        assert_eq!(RENDERS_B.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn min_hits_two_skips_cold_first_request() {
        counting_page!(page_c, RENDERS_C);
        let client = TestClient::new(
            Route::new()
                .at("/page", get(page_c))
                .with(HtmlCacheMiddleware(fresh_cache(2))),
        );

        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "COLD");
        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "MISS");
        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "HIT");
        assert_eq!(RENDERS_C.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn private_responses_are_never_stored() {
        let client = TestClient::new(
            Route::new()
                .at("/private", get(render_private))
                .with(HtmlCacheMiddleware(fresh_cache(1))),
        );
        client.get("/private").send().await.assert_status_is_ok();
        // Second request must NOT be a HIT.
        let r = client.get("/private").send().await;
        assert_ne!(
            r.0.headers().get("x-cache").and_then(|v| v.to_str().ok()),
            Some("HIT")
        );
    }

    #[tokio::test]
    async fn singleflight_renders_once_under_burst() {
        static RENDERS_D: AtomicUsize = AtomicUsize::new(0);

        #[handler]
        async fn slow_page() -> Response {
            RENDERS_D.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            Response::builder()
                .header("content-type", "text/html")
                .body("<html>slow</html>")
        }

        let cache = fresh_cache(1);
        let app = Arc::new(
            Route::new()
                .at("/slow", get(slow_page))
                .with(HtmlCacheMiddleware(cache)),
        );

        let mut tasks = Vec::new();
        for _ in 0..10 {
            let app = Arc::clone(&app);
            tasks.push(tokio::spawn(async move {
                let req = Request::builder().uri("/slow".parse().unwrap()).finish();
                let resp = app.call(req).await.unwrap();
                resp.into_body().into_string().await.unwrap()
            }));
        }
        for t in tasks {
            assert_eq!(t.await.unwrap(), "<html>slow</html>");
        }
        // The 150 ms render ran exactly once for 10 concurrent requests.
        assert_eq!(RENDERS_D.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn purge_forces_a_fresh_render() {
        counting_page!(page_e, RENDERS_E);
        let cache = fresh_cache(1);
        let client = TestClient::new(
            Route::new()
                .at("/page", get(page_e))
                .with(HtmlCacheMiddleware(Arc::clone(&cache))),
        );

        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "MISS");
        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "HIT");

        cache.purge("/page").await;

        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "MISS");
        assert_eq!(RENDERS_E.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn key_normalization_strips_tracking_params() {
        assert_eq!(normalize_key("/p?utm_source=x&utm_medium=y"), "/p");
        assert_eq!(normalize_key("/p?b=2&a=1"), "/p?a=1&b=2");
        assert_eq!(normalize_key("/p?fbclid=zzz&page=3"), "/p?page=3");
        assert_eq!(normalize_key("/p"), "/p");
    }

    #[test]
    fn bypass_prefixes_cover_api_surface() {
        assert!(is_bypassed_path("/api/schemas"));
        assert!(is_bypassed_path("/scim/v2/Users"));
        assert!(is_bypassed_path("/graphql"));
        assert!(is_bypassed_path("/health"));
        assert!(!is_bypassed_path("/page/123"));
        assert!(!is_bypassed_path("/"));
    }

    #[tokio::test]
    async fn ai_mode_learns_and_starts_caching() {
        counting_page!(page_f, RENDERS_F);
        let mut cfg = HtmlCacheConfig::enabled_for_tests(60);
        cfg.ai = true;
        let cache = Arc::new(HtmlPageCache::new(cfg));
        let client = TestClient::new(
            Route::new()
                .at("/page", get(page_f))
                .with(HtmlCacheMiddleware(Arc::clone(&cache))),
        );

        // 1st request: nothing learned yet → conservative COLD.
        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "COLD");
        // 2nd request arrives immediately → arrival rate is clearly hot,
        // the predictor admits it → rendered once more and stored.
        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "MISS");
        // 3rd request: served from memory.
        client
            .get("/page")
            .send()
            .await
            .assert_header("x-cache", "HIT");

        assert_eq!(RENDERS_F.load(Ordering::SeqCst), 2);

        // The model recorded outcomes (observable via /api/cache/ai-stats).
        let snap = cache.predictor().unwrap().snapshot();
        assert_eq!(snap.outcomes.cache_hits, 1);
        assert!(snap.outcomes.admitted >= 1);
    }

    #[tokio::test]
    async fn refresh_ahead_updates_cache_in_background() {
        static VERSION: AtomicUsize = AtomicUsize::new(0);

        #[handler]
        async fn versioned_page() -> Response {
            let v = VERSION.fetch_add(1, Ordering::SeqCst);
            Response::builder()
                .header("content-type", "text/html")
                .body(format!("<html>v{v}</html>"))
        }

        let mut cfg = HtmlCacheConfig::enabled_for_tests(60);
        cfg.refresh_ratio = 0.0; // every hit triggers a background refresh
        let cache = Arc::new(HtmlPageCache::new(cfg));
        let client = TestClient::new(
            Route::new()
                .at("/v", get(versioned_page))
                .with(HtmlCacheMiddleware(cache)),
        );

        // MISS renders v0 and stores it.
        let r = client.get("/v").send().await;
        r.assert_header("x-cache", "MISS");
        r.assert_text("<html>v0</html>").await;

        // HIT serves v0 instantly, background refresh renders v1.
        let r = client.get("/v").send().await;
        r.assert_header("x-cache", "HIT");
        r.assert_text("<html>v0</html>").await;

        // Give the background task a moment.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Next hit serves the freshly re-rendered v1 — still from cache,
        // no user ever waited for the render.
        let r = client.get("/v").send().await;
        r.assert_header("x-cache", "HIT");
        r.assert_text("<html>v1</html>").await;
    }
}
