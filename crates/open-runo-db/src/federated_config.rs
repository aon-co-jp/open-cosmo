//! TOML-declared configuration for [`crate::federated::FederatedBackend`].
//!
//! Building a federation by hand (`FederatedBackend::builder().member(...)`)
//! is the right shape for tests and small embeddings, but an operator wiring
//! up several real databases across offices/teams shouldn't have to write
//! Rust to do it. This module lets the whole federation — members, table
//! routing, broadcast tables, default member — be declared in one TOML file
//! and loaded at startup.
//!
//! ```toml
//! # federation.toml
//! # Top-level scalars (default_member, broadcast) must come first: once a
//! # table header like `[[members]]` or `[routes]` is open, bare
//! # `key = value` lines belong to that table, not to the document root.
//! default_member = "tokyo-pg"
//! broadcast = ["schemas"]
//!
//! [[members]]
//! name = "tokyo-pg"
//! kind = "postgres"
//! url  = "postgres://localhost/tokyo"
//!
//! [[members]]
//! name = "osaka-my"
//! kind = "mysql"
//! url  = "mysql://localhost/osaka"
//!
//! [[members]]
//! name = "archive"
//! kind = "clickhouse"
//! url  = "http://localhost:8123"
//!
//! [routes]
//! orders    = "osaka-my"
//! audit_log = "archive"
//! ```
//!
//! ```rust,ignore
//! let config = FederatedConfig::from_file("federation.toml")?;
//! let fed = config.connect().await?; // dials every member, then builds
//! ```
//!
//! Each member's `kind` selects which [`crate`] backend module handles it
//! (`postgres`, `mysql`, `sqlite`, `aruaru`, `cockroach`, `yugabyte`,
//! `mongodb`/`mongo`, `redis`, `clickhouse`), gated by the matching
//! `open-runo-db` Cargo feature — the same features already used for
//! programmatic construction. `in-memory` (no feature required) is also
//! accepted, mainly for tests and quick local trials of a routing table
//! before pointing it at real databases.

use crate::federated::FederatedBackend;
use crate::{DbBackend, InMemoryBackend};
use open_runo_core::{AppError, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// One `[[members]]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct MemberConfig {
    /// Name this member is referred to by in `routes`/`default_member`.
    pub name: String,
    /// Backend kind: `postgres`, `mysql`, `sqlite`, `aruaru`, `cockroach`,
    /// `yugabyte`, `mongodb`, `redis`, `clickhouse`, or `in-memory`.
    pub kind: String,
    /// Connection string / URL passed to the backend's `connect()`.
    /// Ignored for `kind = "in-memory"`.
    #[serde(default)]
    pub url: String,
    /// MongoDB database name (only meaningful for `kind = "mongodb"`;
    /// defaults to `open_runo` when omitted).
    #[serde(default)]
    pub database: Option<String>,
}

/// Whole-file shape of a federation TOML config.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FederatedConfig {
    pub members: Vec<MemberConfig>,
    /// `table = "member name"` routing overrides.
    #[serde(default)]
    pub routes: HashMap<String, String>,
    /// Tables that should be written to and readable from every member.
    #[serde(default)]
    pub broadcast: Vec<String>,
    /// Member that owns tables without an explicit route
    /// (defaults to the first declared member, same as the builder).
    #[serde(default)]
    pub default_member: Option<String>,
}

impl FederatedConfig {
    /// Parse a federation config from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| AppError::Config(format!("invalid federation config: {e}")))
    }

    /// Load and parse a federation config from a file on disk.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            AppError::Config(format!(
                "cannot read federation config '{}': {e}",
                path.display()
            ))
        })?;
        Self::from_toml_str(&text)
    }

    /// Connect every declared member (dialing out over the network for
    /// real backends) and assemble the resulting [`FederatedBackend`].
    ///
    /// Async because most backend kinds require an async connection step;
    /// members are connected sequentially so a failure names exactly which
    /// member's `url` is bad.
    pub async fn connect(&self) -> Result<FederatedBackend> {
        if self.members.is_empty() {
            return Err(AppError::Validation(
                "federation config declares no [[members]]".into(),
            ));
        }
        let mut builder = FederatedBackend::builder();
        for member in &self.members {
            let backend = connect_member(member).await?;
            builder = builder.member(member.name.clone(), backend);
        }
        for (table, member) in &self.routes {
            builder = builder.route(table.clone(), member.clone());
        }
        for table in &self.broadcast {
            builder = builder.broadcast(table.clone());
        }
        if let Some(default) = &self.default_member {
            builder = builder.default_member(default.clone());
        }
        builder.build()
    }
}

/// Dispatches on `member.kind` to connect the matching backend type.
/// Kinds whose Cargo feature isn't enabled in this build fall through to
/// the final "unknown or feature-disabled" error, naming which feature to
/// enable.
async fn connect_member(member: &MemberConfig) -> Result<Arc<dyn DbBackend>> {
    match member.kind.as_str() {
        "in-memory" | "memory" => return Ok(Arc::new(InMemoryBackend::new())),
        #[cfg(feature = "postgres")]
        "postgres" | "postgresql" => {
            return Ok(Arc::new(
                crate::postgres::PostgresBackend::connect(&member.url).await?,
            ));
        },
        #[cfg(feature = "mysql")]
        "mysql" => {
            return Ok(Arc::new(
                crate::mysql::MySqlBackend::connect(&member.url).await?,
            ));
        },
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            return Ok(Arc::new(
                crate::sqlite::SqliteBackend::connect(&member.url).await?,
            ));
        },
        #[cfg(feature = "aruaru")]
        "aruaru-db" | "aruaru" => {
            return Ok(Arc::new(
                crate::aruaru::AruaruDbBackend::connect(&member.url).await?,
            ));
        },
        #[cfg(feature = "cockroach")]
        "cockroachdb" | "cockroach" => {
            return Ok(Arc::new(
                crate::cockroach::CockroachBackend::connect(&member.url).await?,
            ));
        },
        #[cfg(feature = "yugabyte")]
        "yugabytedb" | "yugabyte" => {
            return Ok(Arc::new(
                crate::yugabyte::YugabyteBackend::connect(&member.url).await?,
            ));
        },
        #[cfg(feature = "mongodb")]
        "mongodb" | "mongo" => {
            let db_name = member.database.as_deref().unwrap_or("open_runo");
            return Ok(Arc::new(
                crate::mongo::MongoBackend::connect(&member.url, db_name).await?,
            ));
        },
        #[cfg(feature = "redis")]
        "redis" => {
            return Ok(Arc::new(crate::redis_backend::RedisBackend::connect(
                &member.url,
            )?));
        },
        #[cfg(feature = "clickhouse")]
        "clickhouse" => {
            return Ok(Arc::new(
                crate::clickhouse_backend::ClickHouseBackend::connect(&member.url),
            ));
        },
        _ => {},
    }
    Err(AppError::Config(format!(
        "member '{}': unknown or feature-disabled backend kind '{}' \
         (enable the matching open-runo-db feature — postgres/mysql/sqlite/aruaru/\
         cockroach/yugabyte/mongodb/redis/clickhouse — or use 'in-memory')",
        member.name, member.kind
    )))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        default_member = "tokyo"
        broadcast = ["schemas"]

        [[members]]
        name = "tokyo"
        kind = "in-memory"

        [[members]]
        name = "osaka"
        kind = "in-memory"

        [[members]]
        name = "archive"
        kind = "in-memory"

        [routes]
        orders = "osaka"
        audit_log = "archive"
    "#;

    #[test]
    fn parses_members_routes_and_broadcast() {
        let cfg = FederatedConfig::from_toml_str(SAMPLE).unwrap();
        assert_eq!(cfg.members.len(), 3);
        assert_eq!(cfg.default_member.as_deref(), Some("tokyo"));
        assert_eq!(cfg.routes.get("orders").map(String::as_str), Some("osaka"));
        assert_eq!(cfg.broadcast, vec!["schemas".to_string()]);
    }

    #[tokio::test]
    async fn connects_in_memory_members_and_builds_a_working_federation() {
        let cfg = FederatedConfig::from_toml_str(SAMPLE).unwrap();
        let fed = cfg.connect().await.unwrap();

        fed.put("orders", "o1", "d1").await.unwrap();
        assert!(fed
            .member("osaka")
            .unwrap()
            .get("orders", "o1")
            .await
            .unwrap()
            .is_some());

        fed.put("schemas", "svc", "sdl").await.unwrap();
        for name in ["tokyo", "osaka", "archive"] {
            assert!(fed
                .member(name)
                .unwrap()
                .get("schemas", "svc")
                .await
                .unwrap()
                .is_some());
        }
    }

    #[test]
    fn rejects_empty_member_list() {
        let cfg = FederatedConfig::from_toml_str("members = []").unwrap();
        // Constructing the future is fine even without executing it, but we
        // want the empty-members check to fire before any connect attempt;
        // exercise it synchronously via a blocking runtime.
        let result = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(cfg.connect());
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_unknown_backend_kind() {
        let cfg = FederatedConfig::from_toml_str(
            r#"
                [[members]]
                name = "weird"
                kind = "not-a-real-backend"
                url = "irrelevant"
            "#,
        )
        .unwrap();
        let err = cfg.connect().await.unwrap_err();
        assert!(format!("{err}").contains("not-a-real-backend"));
    }

    #[test]
    fn rejects_malformed_toml() {
        assert!(FederatedConfig::from_toml_str("this is not [ valid toml").is_err());
    }

    #[test]
    fn from_file_reports_missing_path() {
        let err = FederatedConfig::from_file("/nonexistent/path/federation.toml").unwrap_err();
        assert!(format!("{err}").contains("federation.toml"));
    }
}
