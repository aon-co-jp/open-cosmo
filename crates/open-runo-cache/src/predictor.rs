//! Self-learning cache predictor — open-runo's built-in "AI" for cache
//! admission and TTL, with **no external LLM or paid service**: pure
//! online statistics that improve with every request.
//!
//! ## What it learns
//!
//! - **Per-key arrival rate**: an exponentially-weighted moving average
//!   (EWMA) of the interval between requests for each page. Pages that are
//!   re-requested quickly are worth caching; one-shot pages are not.
//! - **Per-pattern generalization (cold-start prediction)**: URLs are
//!   collapsed into patterns (`/page/123` → `/page/*`), and the same
//!   statistics are kept per pattern. A brand-new `/page/999` inherits the
//!   learned behaviour of `/page/*`, so it can be admitted on its very
//!   first request once the pattern is known to be hot.
//! - **Render cost**: how long each page takes to render. Expensive pages
//!   get a lower admission bar and a longer default TTL — the AI spends
//!   memory where it saves the most server load.
//! - **Volatility**: every purge (content update) feeds an EWMA of the
//!   update interval. The suggested TTL adapts: frequently-updated pages
//!   get short TTLs, static pages drift toward the maximum.
//! - **Outcome feedback**: cache hits/misses are recorded, so operators can
//!   watch the hit ratio climb as the model warms up
//!   (`GET /api/cache/ai-stats`).
//!
//! Memory is bounded: key/pattern tables are capped and evict the
//! least-recently-seen entries. All state is `serde`-serializable so a
//! deployment can snapshot the learned model and restore it on restart.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// EWMA smoothing factor: how fast new observations override old ones.
const ALPHA: f64 = 0.3;

/// Learned statistics for one key or one pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    /// Total requests observed.
    pub requests: u64,
    /// EWMA of seconds between consecutive requests (None until 2nd hit).
    pub arrival_interval_secs: Option<f64>,
    /// EWMA of seconds between content updates/purges.
    pub update_interval_secs: Option<f64>,
    /// Last time this entry was touched (for bounded eviction).
    pub last_seen: DateTime<Utc>,
    /// Last update/purge time.
    pub last_update: Option<DateTime<Utc>>,
    /// EWMA of seconds one render (handler execution) takes.
    #[serde(default)]
    pub render_cost_secs: Option<f64>,
}

impl Stats {
    fn new(now: DateTime<Utc>) -> Self {
        Self {
            requests: 0,
            arrival_interval_secs: None,
            update_interval_secs: None,
            last_seen: now,
            last_update: None,
            render_cost_secs: None,
        }
    }

    fn observe_request(&mut self, now: DateTime<Utc>) {
        if self.requests > 0 {
            let gap = (now - self.last_seen).num_milliseconds().max(0) as f64 / 1000.0;
            self.arrival_interval_secs = Some(match self.arrival_interval_secs {
                Some(prev) => prev * (1.0 - ALPHA) + gap * ALPHA,
                None => gap,
            });
        }
        self.requests = self.requests.saturating_add(1);
        self.last_seen = now;
    }

    fn observe_render_cost(&mut self, secs: f64) {
        let secs = secs.max(0.0);
        self.render_cost_secs = Some(match self.render_cost_secs {
            Some(prev) => prev * (1.0 - ALPHA) + secs * ALPHA,
            None => secs,
        });
    }

    fn observe_update(&mut self, now: DateTime<Utc>) {
        if let Some(last) = self.last_update {
            let gap = (now - last).num_milliseconds().max(0) as f64 / 1000.0;
            self.update_interval_secs = Some(match self.update_interval_secs {
                Some(prev) => prev * (1.0 - ALPHA) + gap * ALPHA,
                None => gap,
            });
        }
        self.last_update = Some(now);
    }
}

/// Tunables. Defaults are safe; everything is env-overridable via
/// [`PredictorConfig::from_env`].
#[derive(Debug, Clone)]
pub struct PredictorConfig {
    /// Cache a page when we expect at least this many requests within TTL.
    pub min_expected_hits: f64,
    /// TTL bounds for [`CachePredictor::suggest_ttl`].
    pub min_ttl: Duration,
    pub max_ttl: Duration,
    /// Fallback TTL before anything is learned.
    pub default_ttl: Duration,
    /// Max tracked keys / patterns (bounded memory).
    pub max_tracked: usize,
}

impl Default for PredictorConfig {
    fn default() -> Self {
        Self {
            min_expected_hits: 1.0,
            min_ttl: Duration::seconds(5),
            max_ttl: Duration::seconds(3600),
            default_ttl: Duration::seconds(60),
            max_tracked: 50_000,
        }
    }
}

impl PredictorConfig {
    /// `OPEN_RUNO_CACHE_AI_MIN_TTL_SECS` / `_MAX_TTL_SECS` /
    /// `_DEFAULT_TTL_SECS` / `_MAX_TRACKED` / `_MIN_EXPECTED_HITS`.
    pub fn from_env() -> Self {
        let d = Self::default();
        let secs = |k: &str, fallback: i64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(fallback)
                .max(1)
        };
        Self {
            min_expected_hits: std::env::var("OPEN_RUNO_CACHE_AI_MIN_EXPECTED_HITS")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(d.min_expected_hits),
            min_ttl: Duration::seconds(secs("OPEN_RUNO_CACHE_AI_MIN_TTL_SECS", 5)),
            max_ttl: Duration::seconds(secs("OPEN_RUNO_CACHE_AI_MAX_TTL_SECS", 3600)),
            default_ttl: Duration::seconds(secs("OPEN_RUNO_CACHE_AI_DEFAULT_TTL_SECS", 60)),
            max_tracked: std::env::var("OPEN_RUNO_CACHE_AI_MAX_TRACKED")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(d.max_tracked)
                .max(16),
        }
    }
}

/// Aggregate outcome counters (model quality observability).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Outcomes {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub admitted: u64,
    pub rejected: u64,
}

/// Snapshot of the learned model (persist + restore across restarts).
#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub keys: HashMap<String, Stats>,
    pub patterns: HashMap<String, Stats>,
    pub outcomes: Outcomes,
}

#[derive(Debug, Default)]
struct Tables {
    keys: HashMap<String, Stats>,
    patterns: HashMap<String, Stats>,
    outcomes: Outcomes,
}

/// The self-learning predictor. Cheap to call on every request.
#[derive(Debug)]
pub struct CachePredictor {
    config: PredictorConfig,
    tables: Mutex<Tables>,
}

/// Collapse a URL path into a learnable pattern:
/// numeric / UUID / long-hex / slug-with-id segments become `*`.
pub fn pattern_of(path: &str) -> String {
    let collapsed: Vec<String> = path
        .split('/')
        .map(|seg| {
            if seg.is_empty() {
                String::new()
            } else if is_variable_segment(seg) {
                "*".to_string()
            } else {
                seg.to_string()
            }
        })
        .collect();
    let joined = collapsed.join("/");
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

fn is_variable_segment(seg: &str) -> bool {
    let digits = seg.chars().filter(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    // all digits, digit-heavy (ids, dates, hashes, uuids), long tokens,
    // or slugs that mix words with a numeric id ("title-5").
    digits == seg.len()
        || digits * 2 >= seg.len()
        || seg.len() >= 16
        || seg.contains('-')
        || seg.contains('_')
}

impl CachePredictor {
    pub fn new(config: PredictorConfig) -> Self {
        Self {
            config,
            tables: Mutex::new(Tables::default()),
        }
    }

    pub fn from_env() -> Self {
        Self::new(PredictorConfig::from_env())
    }

    pub fn config(&self) -> &PredictorConfig {
        &self.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Tables> {
        self.tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Learn from a request and decide: should this page be cached now?
    ///
    /// Admission = expected requests within the suggested TTL ≥ an
    /// effective threshold that shrinks as the page's render cost grows,
    /// judged from the key's own stats or — for cold keys — the learned
    /// pattern stats (cold-start prediction).
    pub fn observe_and_decide(&self, key: &str, path: &str, now: DateTime<Utc>) -> bool {
        let pattern = pattern_of(path);
        let mut t = self.lock();

        Self::bound(&mut t.keys, self.config.max_tracked);
        Self::bound(&mut t.patterns, self.config.max_tracked);

        let entry = t
            .keys
            .entry(key.to_string())
            .or_insert_with(|| Stats::new(now));
        entry.observe_request(now);
        let key_interval = entry.arrival_interval_secs;
        let key_requests = entry.requests;

        let p = t.patterns.entry(pattern).or_insert_with(|| Stats::new(now));
        p.observe_request(now);
        let pattern_interval = p.arrival_interval_secs;

        // Prefer the key's own signal (needs ≥2 requests); otherwise fall
        // back to the pattern's learned arrival rate.
        let interval = match key_interval {
            Some(i) if key_requests >= 2 => Some(i),
            _ => pattern_interval,
        };

        // Cost-aware admission: the more expensive a page is to render, the
        // lower the traffic bar for caching it — the AI spends memory where
        // it saves the most server load.
        let render_cost = t
            .keys
            .get(key)
            .and_then(|s| s.render_cost_secs)
            .or_else(|| {
                t.patterns
                    .get(&pattern_of(path))
                    .and_then(|s| s.render_cost_secs)
            })
            .unwrap_or(0.0);
        let effective_threshold = self.config.min_expected_hits / (1.0 + render_cost);

        let decision = match interval {
            Some(secs) if secs > 0.0 => {
                let ttl = self.suggest_ttl_locked(&t, key, path).num_seconds() as f64;
                ttl / secs >= effective_threshold
            },
            // Burst faster than clock resolution → clearly hot.
            Some(_) => true,
            // Nothing learned yet for key or pattern → stay conservative.
            None => false,
        };

        if decision {
            t.outcomes.admitted += 1;
        } else {
            t.outcomes.rejected += 1;
        }
        decision
    }

    /// Adaptive TTL: half the learned update interval, clamped to
    /// `[min_ttl, max_ttl]`. Before any updates are observed the default
    /// TTL is used, stretched for expensive pages.
    pub fn suggest_ttl(&self, key: &str, path: &str) -> Duration {
        let t = self.lock();
        self.suggest_ttl_locked(&t, key, path)
    }

    fn suggest_ttl_locked(&self, t: &Tables, key: &str, path: &str) -> Duration {
        let update_interval = t
            .keys
            .get(key)
            .and_then(|s| s.update_interval_secs)
            .or_else(|| {
                t.patterns
                    .get(&pattern_of(path))
                    .and_then(|s| s.update_interval_secs)
            });

        let ttl = match update_interval {
            // Volatility known: cache for half the observed update interval.
            Some(secs) => Duration::seconds((secs / 2.0) as i64),
            // Volatility unknown: start from the default, stretched for
            // expensive pages (a heavy render is worth keeping longer).
            None => {
                let cost = t
                    .keys
                    .get(key)
                    .and_then(|s| s.render_cost_secs)
                    .or_else(|| {
                        t.patterns
                            .get(&pattern_of(path))
                            .and_then(|s| s.render_cost_secs)
                    })
                    .unwrap_or(0.0);
                let scaled = self.config.default_ttl.num_seconds() as f64 * (1.0 + cost);
                Duration::seconds(scaled as i64)
            },
        };
        ttl.clamp(self.config.min_ttl, self.config.max_ttl)
    }

    /// Learn how long a render took (server-load signal for cost-aware
    /// admission). Call after executing the real handler.
    pub fn observe_render(&self, key: &str, path: &str, secs: f64) {
        let pattern = pattern_of(path);
        let now = Utc::now();
        let mut t = self.lock();
        t.keys
            .entry(key.to_string())
            .or_insert_with(|| Stats::new(now))
            .observe_render_cost(secs);
        t.patterns
            .entry(pattern)
            .or_insert_with(|| Stats::new(now))
            .observe_render_cost(secs);
    }

    /// Learn from a content update / purge (volatility signal).
    pub fn observe_update(&self, key: &str, path: &str, now: DateTime<Utc>) {
        let pattern = pattern_of(path);
        let mut t = self.lock();
        t.keys
            .entry(key.to_string())
            .or_insert_with(|| Stats::new(now))
            .observe_update(now);
        t.patterns
            .entry(pattern)
            .or_insert_with(|| Stats::new(now))
            .observe_update(now);
    }

    /// Feed cache outcomes so the hit ratio is observable.
    pub fn record_hit(&self) {
        self.lock().outcomes.cache_hits += 1;
    }

    pub fn record_miss(&self) {
        self.lock().outcomes.cache_misses += 1;
    }

    /// Export the learned model (persist on shutdown, inspect via API).
    pub fn snapshot(&self) -> Snapshot {
        let t = self.lock();
        Snapshot {
            keys: t.keys.clone(),
            patterns: t.patterns.clone(),
            outcomes: t.outcomes.clone(),
        }
    }

    /// Restore a previously exported model (warm start).
    pub fn restore(&self, snapshot: Snapshot) {
        let mut t = self.lock();
        t.keys = snapshot.keys;
        t.patterns = snapshot.patterns;
        t.outcomes = snapshot.outcomes;
    }

    fn bound(map: &mut HashMap<String, Stats>, cap: usize) {
        if map.len() < cap {
            return;
        }
        // Evict the least-recently-seen half.
        let mut by_age: Vec<(String, DateTime<Utc>)> =
            map.iter().map(|(k, s)| (k.clone(), s.last_seen)).collect();
        by_age.sort_by_key(|(_, seen)| *seen);
        for (k, _) in by_age.into_iter().take(cap / 2) {
            map.remove(&k);
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn predictor() -> CachePredictor {
        CachePredictor::new(PredictorConfig {
            min_expected_hits: 1.0,
            min_ttl: Duration::seconds(5),
            max_ttl: Duration::seconds(3600),
            default_ttl: Duration::seconds(60),
            max_tracked: 1000,
        })
    }

    fn t0() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn patterns_collapse_variable_segments() {
        assert_eq!(pattern_of("/page/123"), "/page/*");
        assert_eq!(pattern_of("/blog/2026/07/03/title-5"), "/blog/*/*/*/*");
        assert_eq!(
            pattern_of("/item/550e8400-e29b-41d4-a716-446655440000"),
            "/item/*"
        );
        assert_eq!(pattern_of("/about"), "/about");
        assert_eq!(pattern_of("/"), "/");
    }

    #[test]
    fn one_shot_pages_are_rejected_hot_pages_admitted() {
        let p = predictor();
        let now = t0();

        // First-ever request for an unknown key + pattern → conservative.
        assert!(!p.observe_and_decide("/one-off", "/one-off", now));

        // A page requested every 2 seconds → admitted from the 2nd request.
        assert!(!p.observe_and_decide("/hot", "/hot", now));
        assert!(p.observe_and_decide("/hot", "/hot", now + Duration::seconds(2)));
    }

    #[test]
    fn cold_start_inherits_pattern_knowledge() {
        let p = predictor();
        let now = t0();

        // Train the /page/* pattern with several fast-repeating pages.
        for i in 0..5 {
            let key = format!("/page/{i}");
            p.observe_and_decide(&key, &key, now + Duration::seconds(i));
            p.observe_and_decide(
                &key,
                &key,
                now + Duration::seconds(i) + Duration::seconds(1),
            );
        }

        // A brand-new page under the same pattern: admitted on its FIRST
        // request (this is the prediction the min-hits filter cannot make).
        assert!(p.observe_and_decide("/page/999999", "/page/999999", now + Duration::seconds(30)));
    }

    #[test]
    fn rarely_requested_pages_stay_rejected() {
        let p = predictor();
        let now = t0();
        // Requested once per day: expected hits within max TTL (1h) < 1.
        assert!(!p.observe_and_decide("/rare", "/rare", now));
        assert!(!p.observe_and_decide("/rare", "/rare", now + Duration::days(1)));
        assert!(!p.observe_and_decide("/rare", "/rare", now + Duration::days(2)));
    }

    #[test]
    fn ttl_adapts_to_update_frequency() {
        let p = predictor();
        let now = t0();

        // Default before any updates are observed.
        assert_eq!(p.suggest_ttl("/page/1", "/page/1").num_seconds(), 60);

        // Content updated every ~100 s → TTL settles near 50 s.
        p.observe_update("/page/1", "/page/1", now);
        p.observe_update("/page/1", "/page/1", now + Duration::seconds(100));
        let ttl = p.suggest_ttl("/page/1", "/page/1").num_seconds();
        assert!((45..=55).contains(&ttl), "ttl={ttl}");

        // Very frequent updates → clamped to min_ttl.
        p.observe_update("/news", "/news", now);
        p.observe_update("/news", "/news", now + Duration::seconds(2));
        assert_eq!(p.suggest_ttl("/news", "/news").num_seconds(), 5);

        // Pattern-level volatility flows to sibling pages.
        let ttl_sibling = p.suggest_ttl("/page/42", "/page/42").num_seconds();
        assert!(
            (45..=55).contains(&ttl_sibling),
            "sibling ttl={ttl_sibling}"
        );
    }

    #[test]
    fn expensive_pages_are_admitted_at_lower_traffic() {
        let p = predictor();
        let now = t0();

        // Both pages are requested once every 3 minutes. With the default
        // 60 s TTL that's only 0.33 expected hits — below the base
        // threshold, so the cheap page is (correctly) not cached.
        assert!(!p.observe_and_decide("/cheap", "/cheap", now));
        assert!(!p.observe_and_decide("/cheap", "/cheap", now + Duration::minutes(3)));

        // The expensive page took 3 s to render. The AI both stretches its
        // TTL (60 s → 240 s) and lowers the admission bar (1.0 → 0.25), so
        // the same traffic IS worth caching to protect the server.
        p.observe_render("/heavy", "/heavy", 3.0);
        assert!(!p.observe_and_decide("/heavy", "/heavy", now));
        assert!(p.observe_and_decide("/heavy", "/heavy", now + Duration::minutes(3)));
    }

    #[test]
    fn snapshot_roundtrip_preserves_learning() {
        let p = predictor();
        let now = t0();
        p.observe_and_decide("/hot", "/hot", now);
        p.observe_and_decide("/hot", "/hot", now + Duration::seconds(1));
        p.record_hit();

        let snap = p.snapshot();
        let json = serde_json::to_string(&snap).unwrap();

        let restored = predictor();
        restored.restore(sonic_rs::from_str(&json).unwrap());
        assert_eq!(restored.snapshot().outcomes.cache_hits, 1);
        // Learned arrival rate survives → still admitted immediately.
        assert!(restored.observe_and_decide("/hot", "/hot", now + Duration::seconds(2)));
    }

    #[test]
    fn tracked_tables_stay_bounded() {
        let p = CachePredictor::new(PredictorConfig {
            max_tracked: 20,
            ..PredictorConfig::default()
        });
        let now = t0();
        for i in 0..100 {
            let key = format!("/k{i}");
            p.observe_and_decide(&key, &key, now + Duration::seconds(i));
        }
        assert!(p.snapshot().keys.len() <= 20);
    }
}
