//! Monthly request-count metering + per-operation latency/error-rate
//! analytics (Cosmo Launch/Scale parity, `docs/cosmo-parity.md` 4a:
//! "月間リクエスト数の計測" and "Analytics / Tracing (Studio 相当)").
//!
//! This is an **operational metric only** -- it is never used to throttle
//! or bill a caller (rate limiting already lives in
//! `open-runo-security::TokenBucketLimiter`/`RateLimiter`, entirely
//! separate from this module). [`RequestMetrics`] is meant to be hung off
//! `AppState` and fed from the router's request-logging hook (see
//! `open-runo-router::middleware_hyper::with_metrics`, which wraps every
//! route right next to `with_tracing` since both need the same
//! method/path/status/duration tuple).
//!
//! Two read paths are kept deliberately separate:
//! - **In-process aggregates** (`monthly_counts` / `operation_stats`):
//!   cheap `HashMap` counters updated synchronously on every request,
//!   queried directly by the `/api/analytics/*` REST handlers. These
//!   reset on restart -- they are a live view, not the durable record.
//! - **A buffered ClickHouse export** (`buffer` + [`MetricsSink`]): rows
//!   are appended to an in-memory `Vec` (never awaits, never blocks the
//!   request path) and periodically drained by a background flush task
//!   (see [`spawn_periodic_flush`]), matching the buffer-then-flush shape
//!   already used by `open-runo-cache`'s `CachePredictor` persistence.
//!   This is the durable, queryable-from-outside-the-process copy.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One recorded request, the unit written to the ClickHouse sink.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestMetricRow {
    /// RFC 3339 timestamp (kept as a string so this type has no
    /// ClickHouse-specific column type baked in -- the `clickhouse` crate
    /// row type, gated behind the `clickhouse` feature, converts it).
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
}

/// Aggregated per-(method,path) stats, as returned by
/// `GET /api/analytics/operations`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationStat {
    pub method: String,
    pub path: String,
    pub count: u64,
    pub error_count: u64,
    pub total_duration_ms: u64,
}

impl OperationStat {
    /// `error_count / count`, or `0.0` if the operation has never been
    /// called (avoids a division by zero rather than returning `NaN`).
    pub fn error_rate(&self) -> f64 {
        if self.count == 0 { 0.0 } else { self.error_count as f64 / self.count as f64 }
    }

    /// `total_duration_ms / count`, or `0.0` if never called.
    pub fn avg_duration_ms(&self) -> f64 {
        if self.count == 0 { 0.0 } else { self.total_duration_ms as f64 / self.count as f64 }
    }
}

/// One month's request count, as returned by
/// `GET /api/analytics/requests-per-month`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MonthlyCount {
    /// `YYYY-MM`.
    pub month: String,
    pub count: u64,
}

/// Where buffered [`RequestMetricRow`]s go once flushed. Implemented by
/// `InMemorySink` (tests / no-ClickHouse-configured deployments) and, when
/// the `clickhouse` feature is enabled, `ClickHouseSink`.
#[async_trait]
pub trait MetricsSink: Send + Sync + std::fmt::Debug {
    async fn write_batch(&self, rows: &[RequestMetricRow]) -> Result<(), String>;
}

/// A sink that just accumulates rows in memory -- the default when no
/// ClickHouse URL is configured, and what tests use to assert on flush
/// behavior without a live database.
#[derive(Debug, Default)]
pub struct InMemorySink {
    pub written: Mutex<Vec<RequestMetricRow>>,
}

impl InMemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rows(&self) -> Vec<RequestMetricRow> {
        self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl MetricsSink for InMemorySink {
    async fn write_batch(&self, rows: &[RequestMetricRow]) -> Result<(), String> {
        self.written
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(rows);
        Ok(())
    }
}

/// A key identifying one "operation" (Cosmo's term for a distinct
/// method+path pair, roughly analogous to a distinct GraphQL operation).
type OperationKey = (String, String);

/// Buffered, ClickHouse-exportable request metering, plus the in-process
/// aggregates the REST analytics endpoints read from directly. Cheap to
/// call `record` on: three `Mutex` locks, no I/O, no `.await`.
#[derive(Debug)]
pub struct RequestMetrics {
    monthly_counts: Mutex<HashMap<String, u64>>,
    operation_stats: Mutex<HashMap<OperationKey, OperationStat>>,
    buffer: Mutex<Vec<RequestMetricRow>>,
    sink: std::sync::Arc<dyn MetricsSink>,
    /// Cap on how many rows `buffer` may hold before `record` silently
    /// drops the oldest ones -- protects memory if the flush task falls
    /// behind or the sink is down for a long time. Metering is a
    /// best-effort operational aid, not a durability guarantee.
    max_buffered: usize,
}

const DEFAULT_MAX_BUFFERED: usize = 10_000;

impl RequestMetrics {
    pub fn new(sink: std::sync::Arc<dyn MetricsSink>) -> Self {
        Self {
            monthly_counts: Mutex::new(HashMap::new()),
            operation_stats: Mutex::new(HashMap::new()),
            buffer: Mutex::new(Vec::new()),
            sink,
            max_buffered: DEFAULT_MAX_BUFFERED,
        }
    }

    /// Convenience constructor for tests/local dev: metrics are recorded
    /// in-process and any flush just accumulates in an [`InMemorySink`].
    pub fn in_memory() -> (std::sync::Arc<Self>, std::sync::Arc<InMemorySink>) {
        let sink = std::sync::Arc::new(InMemorySink::new());
        let metrics = std::sync::Arc::new(Self::new(sink.clone()));
        (metrics, sink)
    }

    /// Record one completed request. Synchronous and non-blocking --
    /// intended to be called from the hot request path (see
    /// `open-runo-router::middleware_hyper::with_metrics`).
    pub fn record(&self, method: &str, path: &str, status: u16, duration_ms: u64, at: DateTime<Utc>) {
        let month = at.format("%Y-%m").to_string();
        {
            let mut monthly = self.monthly_counts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            *monthly.entry(month).or_insert(0) += 1;
        }
        {
            let mut ops = self.operation_stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = ops.entry((method.to_string(), path.to_string())).or_insert_with(|| OperationStat {
                method: method.to_string(),
                path: path.to_string(),
                count: 0,
                error_count: 0,
                total_duration_ms: 0,
            });
            entry.count += 1;
            if status >= 400 {
                entry.error_count += 1;
            }
            entry.total_duration_ms += duration_ms;
        }
        {
            let mut buf = self.buffer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if buf.len() >= self.max_buffered {
                buf.remove(0);
            }
            buf.push(RequestMetricRow {
                timestamp: at,
                method: method.to_string(),
                path: path.to_string(),
                status,
                duration_ms,
            });
        }
    }

    /// Drain the buffer and hand it to the sink. Returns the number of
    /// rows flushed. On sink failure the drained rows are **not**
    /// re-buffered (matching the "metering is best-effort" stance above)
    /// -- the error is returned so a caller (or the periodic task) can
    /// log it, but a transient ClickHouse outage does not grow the buffer
    /// without bound.
    pub async fn flush(&self) -> Result<usize, String> {
        let rows = {
            let mut buf = self.buffer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *buf)
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let count = rows.len();
        self.sink.write_batch(&rows).await?;
        Ok(count)
    }

    /// How many rows are currently buffered, awaiting the next flush.
    pub fn buffered_len(&self) -> usize {
        self.buffer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }

    /// `GET /api/analytics/requests-per-month` data, sorted oldest month first.
    pub fn requests_per_month(&self) -> Vec<MonthlyCount> {
        let monthly = self.monthly_counts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out: Vec<MonthlyCount> =
            monthly.iter().map(|(month, count)| MonthlyCount { month: month.clone(), count: *count }).collect();
        out.sort_by(|a, b| a.month.cmp(&b.month));
        out
    }

    /// `GET /api/analytics/operations` data, sorted by `total_duration_ms`
    /// descending (busiest/slowest-contributing operations first -- the
    /// most actionable ordering for a latency dashboard).
    pub fn operations_summary(&self) -> Vec<OperationStat> {
        let ops = self.operation_stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out: Vec<OperationStat> = ops.values().cloned().collect();
        out.sort_by(|a, b| b.total_duration_ms.cmp(&a.total_duration_ms));
        out
    }
}

/// Spawn a `tokio` task that calls `metrics.flush()` every `interval`,
/// forever (until the returned handle is dropped/aborted). Flush errors
/// are logged via `tracing::warn!` rather than propagated -- matching
/// `init_tracing_with_otlp`'s stance that a telemetry/metering sink being
/// unavailable must never affect request handling.
pub fn spawn_periodic_flush(
    metrics: std::sync::Arc<RequestMetrics>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            match metrics.flush().await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(rows = n, "flushed buffered request metrics"),
                Err(error) => tracing::warn!(%error, "failed to flush request metrics to sink"),
            }
        }
    })
}

// ── ClickHouse sink ────────────────────────────────────────────────────────

#[cfg(feature = "clickhouse")]
pub mod clickhouse_sink {
    use super::{MetricsSink, RequestMetricRow};
    use async_trait::async_trait;
    use clickhouse::Client;

    /// Writes [`RequestMetricRow`]s to a `request_metrics` table, reusing
    /// the same `clickhouse::Client::default().with_url(url)` connection
    /// pattern as `open_runo_db::clickhouse_backend::ClickHouseBackend`.
    pub struct ClickHouseSink {
        client: Client,
        table: String,
    }

    // Mirrors `ClickHouseBackend`'s manual `Debug` impl: `clickhouse::Client`
    // itself has no `Debug`, so this crate's `#![deny(missing_debug_implementations)]`-
    // style lint (see workspace lints) needs a hand-written stand-in.
    impl std::fmt::Debug for ClickHouseSink {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ClickHouseSink").field("table", &self.table).finish_non_exhaustive()
        }
    }

    #[derive(clickhouse::Row, serde::Serialize, serde::Deserialize)]
    struct ClickHouseRow {
        #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
        timestamp: chrono::DateTime<chrono::Utc>,
        method: String,
        path: String,
        status: u16,
        duration_ms: u64,
    }

    impl ClickHouseSink {
        /// `url` e.g. `http://localhost:8123`. `table` defaults to
        /// `request_metrics` if the caller has no reason to override it;
        /// the DDL to create it (`ENGINE = MergeTree ORDER BY timestamp`)
        /// is documented in `docs/database.md` rather than run here --
        /// this crate only ever inserts/selects, matching
        /// `ClickHouseBackend`'s stance that schema management is an
        /// operator concern.
        pub fn new(url: &str, table: impl Into<String>) -> Self {
            Self { client: Client::default().with_url(url), table: table.into() }
        }
    }

    #[async_trait]
    impl MetricsSink for ClickHouseSink {
        async fn write_batch(&self, rows: &[RequestMetricRow]) -> Result<(), String> {
            let mut insert = self
                .client
                .insert(&self.table)
                .map_err(|e| format!("ClickHouse insert init: {e}"))?;
            for row in rows {
                insert
                    .write(&ClickHouseRow {
                        timestamp: row.timestamp,
                        method: row.method.clone(),
                        path: row.path.clone(),
                        status: row.status,
                        duration_ms: row.duration_ms,
                    })
                    .await
                    .map_err(|e| format!("ClickHouse write: {e}"))?;
            }
            insert.end().await.map_err(|e| format!("ClickHouse end: {e}"))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, y, m, d, 12, 0, 0).unwrap()
    }

    #[test]
    fn record_updates_monthly_and_operation_aggregates() {
        let (metrics, _sink) = RequestMetrics::in_memory();
        metrics.record("GET", "/api/db/status", 200, 5, at(2026, 7, 1));
        metrics.record("GET", "/api/db/status", 500, 15, at(2026, 7, 2));
        metrics.record("GET", "/api/db/status", 200, 10, at(2026, 8, 1));

        let months = metrics.requests_per_month();
        assert_eq!(months, vec![
            MonthlyCount { month: "2026-07".into(), count: 2 },
            MonthlyCount { month: "2026-08".into(), count: 1 },
        ]);

        let ops = metrics.operations_summary();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].count, 3);
        assert_eq!(ops[0].error_count, 1);
        assert_eq!(ops[0].total_duration_ms, 30);
        assert!((ops[0].error_rate() - (1.0 / 3.0)).abs() < 1e-9);
        assert!((ops[0].avg_duration_ms() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn distinct_method_path_pairs_are_separate_operations() {
        let (metrics, _sink) = RequestMetrics::in_memory();
        metrics.record("GET", "/api/db/status", 200, 1, at(2026, 7, 1));
        metrics.record("POST", "/api/db/status", 200, 1, at(2026, 7, 1));
        metrics.record("GET", "/api/schemas/foo", 200, 1, at(2026, 7, 1));

        assert_eq!(metrics.operations_summary().len(), 3);
    }

    #[test]
    fn operation_with_zero_calls_has_zero_rates_not_nan() {
        let stat = OperationStat {
            method: "GET".into(),
            path: "/x".into(),
            count: 0,
            error_count: 0,
            total_duration_ms: 0,
        };
        assert_eq!(stat.error_rate(), 0.0);
        assert_eq!(stat.avg_duration_ms(), 0.0);
    }

    #[tokio::test]
    async fn flush_drains_buffer_into_sink_and_empties_it() {
        let (metrics, sink) = RequestMetrics::in_memory();
        metrics.record("GET", "/a", 200, 3, at(2026, 7, 1));
        metrics.record("GET", "/b", 404, 7, at(2026, 7, 1));
        assert_eq!(metrics.buffered_len(), 2);

        let flushed = metrics.flush().await.unwrap();
        assert_eq!(flushed, 2);
        assert_eq!(metrics.buffered_len(), 0);
        assert_eq!(sink.rows().len(), 2);

        // A second flush with nothing buffered is a cheap no-op.
        assert_eq!(metrics.flush().await.unwrap(), 0);
    }

    #[test]
    fn record_never_blocks_the_hot_path_even_past_the_buffer_cap() {
        // Regression guard: pushing past max_buffered must drop the
        // oldest row rather than growing unboundedly or panicking.
        let sink = Arc::new(InMemorySink::new());
        let metrics = RequestMetrics { max_buffered: 3, ..RequestMetrics::new(sink) };
        for i in 0..10u64 {
            metrics.record("GET", "/x", 200, i, at(2026, 7, 1));
        }
        assert_eq!(metrics.buffered_len(), 3);
    }

    /// Live-ClickHouse round trip. Not run by default -- this sandbox has
    /// no ClickHouse instance reachable (`docker-compose.yml` in this repo
    /// declares no `clickhouse` service, and no `:8123` port answers
    /// locally; verified by attempting a real connection before writing
    /// this test). Run explicitly against a real instance with:
    /// `OPEN_RUNO_CLICKHOUSE_URL=http://localhost:8123 cargo test -p
    /// open-runo-observability --features clickhouse -- --ignored
    /// request_metrics::tests::live_clickhouse_write_batch_round_trip`
    /// after creating the table:
    /// `CREATE TABLE request_metrics (timestamp DateTime64(3), method
    /// String, path String, status UInt16, duration_ms UInt64) ENGINE =
    /// MergeTree ORDER BY timestamp`.
    #[cfg(feature = "clickhouse")]
    #[tokio::test]
    #[ignore]
    async fn live_clickhouse_write_batch_round_trip() {
        let url = std::env::var("OPEN_RUNO_CLICKHOUSE_URL")
            .expect("set OPEN_RUNO_CLICKHOUSE_URL to run this ignored live-DB test");
        let sink = std::sync::Arc::new(clickhouse_sink::ClickHouseSink::new(&url, "request_metrics"));
        let metrics = RequestMetrics::new(sink);
        metrics.record("GET", "/live-test", 200, 42, Utc::now());
        let flushed = metrics.flush().await.expect("flush to a real ClickHouse instance");
        assert_eq!(flushed, 1);
    }
}
