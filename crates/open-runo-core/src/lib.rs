//! `open-runo-core`: shared foundation for every open-runo crate.
//!
//! This crate intentionally has no dependency on any other `open-runo-*`
//! crate. It provides:
//!
//! - [`AppError`] / [`Result`] — a common error type used across the workspace.
//! - [`Config`] — a minimal environment-driven configuration loader.
//! - [`Environment`] — the deployment environment enum (local/dev/staging/prod).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::env;
use std::fmt;

/// Workspace-wide error type. Individual crates should wrap domain-specific
/// errors into this type at their public boundary so callers only need to
/// handle one error type.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("internal error: {0}")]
    Internal(String),

    /// Catch-all for errors from third-party crates that don't warrant
    /// their own `AppError` variant. Enables `?` on any `anyhow::Error`
    /// (and, transitively, most library error types via `anyhow`'s own
    /// `From` impls) at a crate's public boundary.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias used throughout the open-runo workspace.
pub type Result<T> = std::result::Result<T, AppError>;

/// Deployment environment, matching the environments described in the
/// open-runo architecture (local / development / staging / production).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Local,
    Development,
    Staging,
    Production,
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Environment::Local => "local",
            Environment::Development => "development",
            Environment::Staging => "staging",
            Environment::Production => "production",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for Environment {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "local" => Ok(Environment::Local),
            "development" | "dev" => Ok(Environment::Development),
            "staging" | "stg" => Ok(Environment::Staging),
            "production" | "prod" => Ok(Environment::Production),
            other => Err(AppError::Config(format!("unknown environment: {other}"))),
        }
    }
}

/// Minimal process configuration, loaded from environment variables.
///
/// Real deployments are expected to layer a config file / secret manager on
/// top of this; this struct only covers the values every open-runo service
/// needs at boot (bind address, environment, log level).
#[derive(Debug, Clone)]
pub struct Config {
    pub environment: Environment,
    pub bind_addr: String,
    pub log_level: String,
    /// Requests allowed per client key within `rate_limit_window_secs`,
    /// enforced by `open-runo-router`'s rate-limiting middleware (which
    /// wraps `open_runo_security::RateLimiter`).
    pub rate_limit_max_requests: u32,
    /// Rolling window, in seconds, over which `rate_limit_max_requests`
    /// applies.
    pub rate_limit_window_secs: u64,
    /// OTLP HTTP endpoint (e.g. `http://localhost:4318`) to export traces
    /// to via `open-runo-observability`. `None` means console-only tracing
    /// (the default — no collector required for local development).
    pub otlp_endpoint: Option<String>,
}

impl Config {
    /// Load configuration from environment variables, falling back to
    /// sane local-development defaults for anything unset.
    ///
    /// - `OPEN_RUNO_ENV` (default: `local`)
    /// - `OPEN_RUNO_BIND_ADDR` (default: `0.0.0.0:8080`)
    /// - `OPEN_RUNO_LOG_LEVEL` (default: `info`)
    /// - `OPEN_RUNO_RATE_LIMIT_MAX_REQUESTS` (default: `120`)
    /// - `OPEN_RUNO_RATE_LIMIT_WINDOW_SECS` (default: `60`)
    /// - `OPEN_RUNO_OTLP_ENDPOINT` (default: unset, i.e. console-only tracing)
    pub fn from_env() -> Result<Self> {
        let environment = match env::var("OPEN_RUNO_ENV") {
            Ok(v) => v.parse()?,
            Err(_) => Environment::Local,
        };
        let bind_addr =
            env::var("OPEN_RUNO_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
        let log_level = env::var("OPEN_RUNO_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let rate_limit_max_requests = parse_env_or("OPEN_RUNO_RATE_LIMIT_MAX_REQUESTS", 120)?;
        let rate_limit_window_secs = parse_env_or("OPEN_RUNO_RATE_LIMIT_WINDOW_SECS", 60)?;
        let otlp_endpoint = env::var("OPEN_RUNO_OTLP_ENDPOINT").ok();

        Ok(Config {
            environment,
            bind_addr,
            log_level,
            rate_limit_max_requests,
            rate_limit_window_secs,
            otlp_endpoint,
        })
    }
}

/// Parses an environment variable into `T`, falling back to `default` when
/// the variable is unset, and returning `AppError::Config` when it is set
/// but fails to parse.
fn parse_env_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(v) => v
            .parse()
            .map_err(|e| AppError::Config(format!("invalid value for {key}: {e}"))),
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use std::sync::Mutex;

    /// `std::env::set_var`/`remove_var` mutate global process state, but
    /// Rust's test harness runs tests in parallel threads within one
    /// process by default. Every test below that touches `OPEN_RUNO_*` env
    /// vars must hold this lock for its duration, or it can race with
    /// another such test and read a value the other test just set/cleared.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn environment_roundtrip() {
        assert_eq!(
            Environment::from_str("production").unwrap(),
            Environment::Production
        );
        assert_eq!(Environment::Production.to_string(), "production");
    }

    #[test]
    fn environment_rejects_unknown() {
        assert!(Environment::from_str("nonsense").is_err());
    }

    #[test]
    fn anyhow_error_converts_via_from() {
        let err: AppError = anyhow::anyhow!("boom").into();
        assert!(matches!(err, AppError::Other(_)));
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn config_defaults_when_env_unset() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        env::remove_var("OPEN_RUNO_ENV");
        env::remove_var("OPEN_RUNO_BIND_ADDR");
        env::remove_var("OPEN_RUNO_LOG_LEVEL");
        env::remove_var("OPEN_RUNO_RATE_LIMIT_MAX_REQUESTS");
        env::remove_var("OPEN_RUNO_RATE_LIMIT_WINDOW_SECS");
        env::remove_var("OPEN_RUNO_OTLP_ENDPOINT");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.environment, Environment::Local);
        assert_eq!(cfg.bind_addr, "0.0.0.0:8080");
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.rate_limit_max_requests, 120);
        assert_eq!(cfg.rate_limit_window_secs, 60);
        assert_eq!(cfg.otlp_endpoint, None);
    }

    #[test]
    fn config_reads_otlp_endpoint_when_set() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        env::set_var("OPEN_RUNO_OTLP_ENDPOINT", "http://localhost:4318");
        let cfg = Config::from_env().unwrap();
        env::remove_var("OPEN_RUNO_OTLP_ENDPOINT");
        assert_eq!(cfg.otlp_endpoint.as_deref(), Some("http://localhost:4318"));
    }

    #[test]
    fn config_rejects_invalid_rate_limit_value() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        env::set_var("OPEN_RUNO_RATE_LIMIT_MAX_REQUESTS", "not-a-number");
        let result = Config::from_env();
        env::remove_var("OPEN_RUNO_RATE_LIMIT_MAX_REQUESTS");
        assert!(result.is_err());
    }
}
