//! `open-runo-observability`: standardizes tracing/metrics setup across all
//! open-runo services. This crate wires up structured console logging via
//! `tracing-subscriber`, an optional OTLP (OpenTelemetry Protocol) trace
//! exporter for shipping spans to a collector (Jaeger, Tempo, Grafana,
//! any OTLP-compatible backend), and a minimal in-process counter registry
//! for tests and local development.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub mod request_metrics;
pub use request_metrics::{
    InMemorySink, MetricsSink, MonthlyCount, OperationStat, RequestMetricRow, RequestMetrics,
    spawn_periodic_flush,
};
#[cfg(feature = "clickhouse")]
pub use request_metrics::clickhouse_sink::ClickHouseSink;

/// Initialize a JSON-structured `tracing` subscriber reading its level
/// from `log_level` (e.g. `"info"`, `"debug"`). Safe to call once per
/// process at startup. Console-only; see [`init_tracing_with_otlp`] to
/// additionally export spans via OTLP.
pub fn init_tracing(log_level: &str) {
    init_tracing_with_otlp(log_level, None, "open-runo");
}

/// Same as [`init_tracing`], but when `otlp_endpoint` is `Some`, additionally
/// exports spans to that OTLP HTTP endpoint (e.g. `http://localhost:4318`,
/// the default port an OpenTelemetry Collector or Jaeger/Tempo listens on)
/// under `service_name`. If building the exporter fails (bad URL, etc.) this
/// falls back to console-only logging rather than failing startup —
/// telemetry export is a diagnostic aid, not a hard dependency for the
/// service to run.
pub fn init_tracing_with_otlp(log_level: &str, otlp_endpoint: Option<&str>, service_name: &str) {
    let env_filter = tracing_subscriber::EnvFilter::new(log_level.to_string());
    let fmt_layer = tracing_subscriber::fmt::layer().json();

    let Some(endpoint) = otlp_endpoint else {
        let _ = tracing_subscriber::registry().with(env_filter).with(fmt_layer).try_init();
        return;
    };

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build();

    let exporter = match exporter {
        Ok(exporter) => exporter,
        Err(error) => {
            let _ = tracing_subscriber::registry().with(env_filter).with(fmt_layer).try_init();
            tracing::warn!(%error, endpoint, "failed to build OTLP exporter; continuing with console-only tracing");
            return;
        }
    };

    let resource = opentelemetry_sdk::Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new("service.name", service_name.to_string()))
        .build();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();
    let tracer = provider.tracer(service_name.to_string());
    opentelemetry::global::set_tracer_provider(provider);

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .try_init();
}

/// A minimal monotonic counter registry, useful for unit tests and local
/// dashboards before a full Prometheus exporter is wired in.
#[derive(Debug, Default)]
pub struct Counters {
    values: Mutex<HashMap<String, AtomicU64>>,
}

impl Counters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&self, name: &str) {
        // A poisoned lock still holds a valid (if possibly inconsistent)
        // map; recovering it is preferable to panicking a whole service
        // over a metrics counter.
        let mut values = self.values.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        values
            .entry(name.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self, name: &str) -> u64 {
        let values = self.values.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        values.get(name).map(|c| c.load(Ordering::Relaxed)).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_increment_independently() {
        let counters = Counters::new();
        counters.increment("requests_total");
        counters.increment("requests_total");
        counters.increment("errors_total");

        assert_eq!(counters.get("requests_total"), 2);
        assert_eq!(counters.get("errors_total"), 1);
        assert_eq!(counters.get("never_incremented"), 0);
    }

    #[test]
    fn init_tracing_is_idempotent() {
        // Calling twice must not panic (try_init swallows the second error).
        init_tracing("info");
        init_tracing("debug");
    }

    #[test]
    fn init_tracing_with_otlp_falls_back_on_bad_endpoint() {
        // An unparseable endpoint must not panic the caller; it should log
        // a warning and fall back to console-only tracing.
        init_tracing_with_otlp("info", Some("not a valid url"), "test-service");
    }

    #[test]
    fn init_tracing_with_otlp_none_is_console_only() {
        init_tracing_with_otlp("info", None, "test-service");
    }
}
