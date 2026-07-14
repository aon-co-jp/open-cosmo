//! `open-runo-db` — open-aruaru Database Coordination Layer
//!
//! ## 設計思想
//!
//! `DbBackend` トレイト 1 つで「どの DB を使っても同じコード」を実現する。
//! DUAL DATABASE（PostgreSQL + aruaru-db）が標準だが、
//! シングル DB でも、分散クラスタでも、NoSQL でも feature フラグ 1 つで切り替わる。
//!
//! ## サポート DB
//!
//! | feature      | Backend             | 接続先                             |
//! |--------------|---------------------|------------------------------------|
//! | (常時)        | `InMemoryBackend`   | メモリ (テスト / ローカル)           |
//! | `postgres`   | `PostgresBackend`   | PostgreSQL :5432                   |
//! | `mysql`      | `MySqlBackend`      | MySQL 8 / MariaDB 11 :3306         |
//! | `sqlite`     | `SqliteBackend`     | SQLite (file or :memory:)          |
//! | `aruaru`     | `AruaruDbBackend`   | aruaru-db :5433 (pgwire)           |
//! | `cockroach`  | `CockroachBackend`  | CockroachDB :26257 (pgwire 互換)   |
//! | `yugabyte`   | `YugabyteBackend`   | YugabyteDB (pgwire 互換)           |
//! | `mongodb`    | `MongoBackend`      | MongoDB :27017                     |
//! | `redis`      | `RedisBackend`      | Redis / KeyDB / DragonflyDB :6379  |
//! | `clickhouse` | `ClickHouseBackend` | ClickHouse :8123                   |
//!
//! ## 複合 feature プリセット
//!
//! | feature     | 内容                                          |
//! |-------------|-----------------------------------------------|
//! | `dual`      | `postgres` + `aruaru` — 標準 DUAL DATABASE    |
//! | `single-pg` | `postgres` のみ                               |
//! | `single-my` | `mysql` のみ — WordPress / Redmine 向け       |
//! | `dev`       | `sqlite` のみ — CI・ローカル開発               |
//! | `full`      | `dual` + `redis` + `clickhouse` — 本番         |
//! | `cluster`   | `cockroach` + `aruaru` — 分散クラスタ          |

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod dual;
pub mod federated;
pub mod federated_config;
pub mod migrate;
pub mod migration;

use async_trait::async_trait;
use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

// ── Core types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub value: String,
}

/// Backend-agnostic database interface.
#[async_trait]
pub trait DbBackend: Send + Sync + std::fmt::Debug {
    async fn put(&self, table: &str, key: &str, value: &str) -> Result<()>;
    async fn get(&self, table: &str, key: &str) -> Result<Option<String>>;
    async fn delete(&self, table: &str, key: &str) -> Result<()>;
    async fn list(&self, table: &str) -> Result<Vec<Record>>;
    fn backend_name(&self) -> &'static str;

    /// Verify (and self-heal) cross-database consistency. Single-store
    /// backends have nothing to compare — the default reports no issues.
    /// `DualBackend` overrides this with a real two-sided reconciliation.
    async fn consistency_check_and_heal(&self) -> Result<Vec<dual::Discrepancy>> {
        Ok(Vec::new())
    }

    /// VersionLessAPI + Git-on-SQL hybrid read side: fetch the value a
    /// `(table, key)` held as of a specific historical commit, not the
    /// current value. Only backends built on a Git-managed store (i.e.
    /// `AruaruDbBackend`) can honor this; every other backend keeps the
    /// default, which reports the operation as unsupported rather than
    /// silently returning the *current* value under a false pretense of
    /// historical accuracy.
    async fn get_at_commit(
        &self,
        _table: &str,
        _key: &str,
        _commit_id: &str,
    ) -> Result<Option<String>> {
        Err(AppError::Validation(format!(
            "AS OF COMMIT reads are not supported by the '{}' backend",
            self.backend_name()
        )))
    }

    /// RJSON server-side partial extraction (Phase 2, 2026-07-14): fetch
    /// just `path` (see `open_runo_rjson::extract_path`'s doc comment for
    /// the path language) out of `(table, key)`'s value, instead of the
    /// whole document — the network-bandwidth-savings benefit from the
    /// original RJSON proposal. Only backends that store values as
    /// well-formed JSON/RJSON can honor this (`RjsonBackend`); every
    /// other backend keeps the default, which reports the operation as
    /// unsupported rather than attempting to string-search a value that
    /// might not even be JSON.
    async fn get_field(
        &self,
        _table: &str,
        _key: &str,
        _path: &str,
    ) -> Result<Option<String>> {
        Err(AppError::Validation(format!(
            "field extraction is not supported by the '{}' backend",
            self.backend_name()
        )))
    }
}

// ── In-memory (常時コンパイル) ─────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct InMemoryBackend {
    store: Mutex<HashMap<(String, String), String>>,
}
impl InMemoryBackend {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl DbBackend for InMemoryBackend {
    fn backend_name(&self) -> &'static str { "in-memory" }
    async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
        self.store.lock().map_err(|_| AppError::Internal("lock".into()))?
            .insert((table.into(), key.into()), value.into());
        Ok(())
    }
    async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
        Ok(self.store.lock().map_err(|_| AppError::Internal("lock".into()))?
            .get(&(table.into(), key.into())).cloned())
    }
    async fn delete(&self, table: &str, key: &str) -> Result<()> {
        self.store.lock().map_err(|_| AppError::Internal("lock".into()))?
            .remove(&(table.into(), key.into()));
        Ok(())
    }
    async fn list(&self, table: &str) -> Result<Vec<Record>> {
        Ok(self.store.lock().map_err(|_| AppError::Internal("lock".into()))?
            .iter()
            .filter(|((t, _), _)| t == table)
            .map(|((_, k), v)| Record { key: k.clone(), value: v.clone() })
            .collect())
    }
}

// ── RJSON (Phase 1: in-memory proof, `rjson` feature) ─────────────────────────

#[cfg(feature = "rjson")]
pub mod rjson_backend {
    //! RJSON-aware `DbBackend`: proves, for real, the first of the three
    //! benefits described for a DB-level RJSON integration (2026-07-14,
    //! concept: 石塚正浩・aon CEO) —
    //! **automatic write-time validation against RJSON's grammar**. `put`
    //! parses the incoming value with [`open_runo_rjson::parse`] and
    //! rejects it outright if it isn't valid RJSON (which includes every
    //! valid strict-JSON document, since RJSON is a superset) — a
    //! malformed value never reaches storage, no matter which client
    //! sent it or how it was authored (trailing commas, comments, and
    //! the other RJSON conveniences are silently normalized away, since
    //! what's actually persisted is [`open_runo_rjson::to_string`]'s
    //! canonical strict-JSON form).
    //!
    //! **Scope of this Phase 1**: storage here is a plain in-memory
    //! `HashMap`, structurally identical to [`super::InMemoryBackend`] —
    //! there is no persistence, replication, or Git-on-SQL-style
    //! versioning yet, and no server-side partial-value extraction
    //! (the second and third benefits from the original proposal: network
    //! bandwidth savings via server-side key/array extraction, and a
    //! genuinely persistent/distributed storage engine comparable in
    //! scope to `aruaru-db`) are **not** implemented in this pass — both
    //! are large enough to warrant their own dedicated development passes,
    //! the same way `aruaru-db` itself grew incrementally across many
    //! sessions rather than being built in one. This module exists to
    //! prove the write-time-validation benefit end-to-end with a real,
    //! tested parser, and to give `open-runo-db` a concrete extension
    //! point (`DbBackend`) that a future persistent `RjsonDbBackend`
    //! (backed by its own storage engine, or by extending `aruaru-db`)
    //! can slot into without changing any call site above this trait.
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct RjsonBackend {
        store: Mutex<HashMap<(String, String), String>>,
    }

    impl RjsonBackend {
        pub fn new() -> Self {
            Self::default()
        }
    }

    #[async_trait]
    impl DbBackend for RjsonBackend {
        fn backend_name(&self) -> &'static str {
            "rjson"
        }

        /// Parses `value` as RJSON (accepting strict JSON too, since RJSON
        /// is a superset) and rejects the write with a validation error
        /// if it doesn't parse — the "DB-level automatic validation"
        /// benefit. What's actually stored is the canonical strict-JSON
        /// re-serialization (`open_runo_rjson::to_string`), not the raw
        /// input text, so every later `get` returns unambiguous strict
        /// JSON regardless of how leniently the value was originally
        /// authored.
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            let parsed = open_runo_rjson::parse(value)
                .map_err(|e| AppError::Validation(format!("invalid RJSON for {table}/{key}: {e}")))?;
            let canonical = open_runo_rjson::to_string(&parsed);
            self.store
                .lock()
                .map_err(|_| AppError::Internal("lock".into()))?
                .insert((table.into(), key.into()), canonical);
            Ok(())
        }

        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            Ok(self
                .store
                .lock()
                .map_err(|_| AppError::Internal("lock".into()))?
                .get(&(table.into(), key.into()))
                .cloned())
        }

        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            self.store
                .lock()
                .map_err(|_| AppError::Internal("lock".into()))?
                .remove(&(table.into(), key.into()));
            Ok(())
        }

        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            Ok(self
                .store
                .lock()
                .map_err(|_| AppError::Internal("lock".into()))?
                .iter()
                .filter(|((t, _), _)| t == table)
                .map(|((_, k), v)| Record { key: k.clone(), value: v.clone() })
                .collect())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn put_normalizes_lenient_rjson_to_canonical_strict_json() {
            let backend = RjsonBackend::new();
            // Trailing comma + comment + unquoted key + single-quoted
            // string -- all four RJSON extensions in one write.
            let lenient = "{ /* item */ name: 'sword', qty: 3, }";
            backend.put("items", "sword", lenient).await.expect("valid RJSON should be accepted");

            let stored = backend.get("items", "sword").await.unwrap().unwrap();
            assert!(!stored.contains("//"), "stored value must be canonical JSON, not the raw lenient input");
            assert!(!stored.contains("/*"));
            let value: serde_json::Value = serde_json::from_str(&stored).unwrap();
            assert_eq!(value, serde_json::json!({"name": "sword", "qty": 3}));
        }

        #[tokio::test]
        async fn put_rejects_genuinely_malformed_input() {
            let backend = RjsonBackend::new();
            let result = backend.put("items", "broken", "{ this is not json at all").await;
            assert!(result.is_err(), "malformed input must be rejected at write time, never silently stored");
        }

        #[tokio::test]
        async fn strict_json_is_accepted_unchanged_since_rjson_is_a_superset() {
            let backend = RjsonBackend::new();
            backend.put("items", "shield", r#"{"name":"shield","qty":1}"#).await.expect("strict JSON is valid RJSON");
            let stored = backend.get("items", "shield").await.unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(&stored).unwrap();
            assert_eq!(value, serde_json::json!({"name": "shield", "qty": 1}));
        }

        #[tokio::test]
        async fn list_and_delete_behave_like_every_other_backend() {
            let backend = RjsonBackend::new();
            backend.put("items", "a", "{x: 1}").await.unwrap();
            backend.put("items", "b", "{x: 2}").await.unwrap();
            backend.put("other", "c", "{x: 3}").await.unwrap();

            let items = backend.list("items").await.unwrap();
            assert_eq!(items.len(), 2, "list must only return the requested table's records");

            backend.delete("items", "a").await.unwrap();
            assert!(backend.get("items", "a").await.unwrap().is_none());
            assert_eq!(backend.list("items").await.unwrap().len(), 1);
        }
    }
}
// ── PostgreSQL :5432 ───────────────────────────────────────────────────────────

#[cfg(feature = "postgres")]
pub mod postgres {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use sqlx::PgPool;

    #[derive(Debug, Clone)]
    pub struct PostgresBackend { pool: PgPool }
    impl PostgresBackend {
        pub async fn connect(url: &str) -> Result<Self> {
            let pool = PgPool::connect(url).await
                .map_err(|e| AppError::Internal(format!("PostgreSQL connect: {e}")))?;
            tracing::info!(url, "connected to PostgreSQL :5432");
            Ok(Self { pool })
        }
        pub fn pool(&self) -> &PgPool { &self.pool }
    }
    #[async_trait]
    impl DbBackend for PostgresBackend {
        fn backend_name(&self) -> &'static str { "postgresql" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            sqlx::query(
                "INSERT INTO kv_store (table_name,key,value) VALUES ($1,$2,$3)
                 ON CONFLICT (table_name,key) DO UPDATE SET value=EXCLUDED.value",
            ).bind(table).bind(key).bind(value).execute(&self.pool).await
            .map_err(|e| AppError::Internal(format!("PostgreSQL put: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT value FROM kv_store WHERE table_name=$1 AND key=$2",
            ).bind(table).bind(key).fetch_optional(&self.pool).await
            .map_err(|e| AppError::Internal(format!("PostgreSQL get: {e}")))?;
            Ok(row.map(|(v,)| v))
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            sqlx::query("DELETE FROM kv_store WHERE table_name=$1 AND key=$2")
                .bind(table).bind(key).execute(&self.pool).await
                .map_err(|e| AppError::Internal(format!("PostgreSQL delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let rows: Vec<(String,String)> = sqlx::query_as(
                "SELECT key,value FROM kv_store WHERE table_name=$1 ORDER BY key",
            ).bind(table).fetch_all(&self.pool).await
            .map_err(|e| AppError::Internal(format!("PostgreSQL list: {e}")))?;
            Ok(rows.into_iter().map(|(key,value)| Record { key, value }).collect())
        }
    }
}

// ── MySQL 8 / MariaDB 11 :3306 ─────────────────────────────────────────────────

#[cfg(feature = "mysql")]
pub mod mysql {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use sqlx::MySqlPool;

    #[derive(Debug, Clone)]
    pub struct MySqlBackend { pool: MySqlPool }
    impl MySqlBackend {
        pub async fn connect(url: &str) -> Result<Self> {
            let pool = MySqlPool::connect(url).await
                .map_err(|e| AppError::Internal(format!("MySQL connect: {e}")))?;
            tracing::info!(url, "connected to MySQL/MariaDB :3306");
            Ok(Self { pool })
        }
        pub fn pool(&self) -> &MySqlPool { &self.pool }
    }
    #[async_trait]
    impl DbBackend for MySqlBackend {
        fn backend_name(&self) -> &'static str { "mysql" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            sqlx::query(
                "INSERT INTO kv_store (table_name,`key`,value) VALUES (?,?,?)
                 ON DUPLICATE KEY UPDATE value=VALUES(value)",
            ).bind(table).bind(key).bind(value).execute(&self.pool).await
            .map_err(|e| AppError::Internal(format!("MySQL put: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT value FROM kv_store WHERE table_name=? AND `key`=?",
            ).bind(table).bind(key).fetch_optional(&self.pool).await
            .map_err(|e| AppError::Internal(format!("MySQL get: {e}")))?;
            Ok(row.map(|(v,)| v))
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            sqlx::query("DELETE FROM kv_store WHERE table_name=? AND `key`=?")
                .bind(table).bind(key).execute(&self.pool).await
                .map_err(|e| AppError::Internal(format!("MySQL delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let rows: Vec<(String,String)> = sqlx::query_as(
                "SELECT `key`,value FROM kv_store WHERE table_name=? ORDER BY `key`",
            ).bind(table).fetch_all(&self.pool).await
            .map_err(|e| AppError::Internal(format!("MySQL list: {e}")))?;
            Ok(rows.into_iter().map(|(key,value)| Record { key, value }).collect())
        }
    }
}

// ── SQLite ─────────────────────────────────────────────────────────────────────

#[cfg(feature = "sqlite")]
pub mod sqlite {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use sqlx::SqlitePool;

    #[derive(Debug, Clone)]
    pub struct SqliteBackend { pool: SqlitePool }
    impl SqliteBackend {
        pub async fn connect(url: &str) -> Result<Self> {
            let pool = SqlitePool::connect(url).await
                .map_err(|e| AppError::Internal(format!("SQLite connect: {e}")))?;
            tracing::info!(url, "connected to SQLite");
            Ok(Self { pool })
        }
        pub fn pool(&self) -> &SqlitePool { &self.pool }
    }
    #[async_trait]
    impl DbBackend for SqliteBackend {
        fn backend_name(&self) -> &'static str { "sqlite" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            sqlx::query(
                "INSERT INTO kv_store (table_name,key,value) VALUES (?,?,?)
                 ON CONFLICT(table_name,key) DO UPDATE SET value=excluded.value",
            ).bind(table).bind(key).bind(value).execute(&self.pool).await
            .map_err(|e| AppError::Internal(format!("SQLite put: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT value FROM kv_store WHERE table_name=? AND key=?",
            ).bind(table).bind(key).fetch_optional(&self.pool).await
            .map_err(|e| AppError::Internal(format!("SQLite get: {e}")))?;
            Ok(row.map(|(v,)| v))
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            sqlx::query("DELETE FROM kv_store WHERE table_name=? AND key=?")
                .bind(table).bind(key).execute(&self.pool).await
                .map_err(|e| AppError::Internal(format!("SQLite delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let rows: Vec<(String,String)> = sqlx::query_as(
                "SELECT key,value FROM kv_store WHERE table_name=? ORDER BY key",
            ).bind(table).fetch_all(&self.pool).await
            .map_err(|e| AppError::Internal(format!("SQLite list: {e}")))?;
            Ok(rows.into_iter().map(|(key,value)| Record { key, value }).collect())
        }
    }
}

// ── aruaru-db :5433 (pgwire) ──────────────────────────────────────────────────

#[cfg(feature = "aruaru")]
pub mod aruaru {
    //! Pure Rust Git-on-SQL 分散 DB。全 put がバージョン付きコミットになる。

    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use sqlx::{PgPool, Row};

    /// Escape a value for interpolation into aruaru-db literal SQL text.
    /// Only used for `SELECT`s (see the doc comment on `get`/`list`/
    /// `get_at_commit` below for why those can't use bind parameters
    /// against this backend).
    fn sql_escape(s: &str) -> String {
        s.replace('\'', "''")
    }

    #[derive(Debug, Clone)]
    pub struct AruaruDbBackend { pool: PgPool }
    impl AruaruDbBackend {
        pub async fn connect(url: &str) -> Result<Self> {
            let pool = PgPool::connect(url).await
                .map_err(|e| AppError::Internal(format!("aruaru-db connect: {e}")))?;
            tracing::info!(url, "connected to aruaru-db (pgwire :5433)");
            Ok(Self { pool })
        }
        pub fn pool(&self) -> &PgPool { &self.pool }
        /// Build directly from an already-established pool (tests /
        /// callers that manage their own connection lifecycle).
        pub fn from_pool(pool: PgPool) -> Self { Self { pool } }
    }
    #[async_trait]
    impl DbBackend for AruaruDbBackend {
        fn backend_name(&self) -> &'static str { "aruaru-db" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            let pk = crate::migration::aruaru_pk(table, key);
            sqlx::query(
                "INSERT INTO kv_store (pk,table_name,key,value) VALUES ($1,$2,$3,$4)
                 ON CONFLICT (pk) DO UPDATE SET value=EXCLUDED.value",
            ).bind(pk).bind(table).bind(key).bind(value).execute(&self.pool).await
            .map_err(|e| AppError::Internal(format!("aruaru-db put: {e}")))?;
            Ok(())
        }
        /// **Why `raw_sql` (simple query protocol) instead of `query_as`/
        /// `.bind()` (extended/prepared protocol) for every read here**:
        /// aruaru-wire's `ExtendedQueryHandler::describe_portal` always
        /// returns an empty column list (`Ok(DescribePortalResponse::new(vec![]))`,
        /// `crates/aruaru-wire/src/lib.rs`) because aruaru-db's schema is
        /// dynamic — the real `RowDescription` is only known once the
        /// engine actually executes the query. `sqlx`'s prepared-statement
        /// path decodes rows using the column shape it got from Describe,
        /// so against this server it silently gets zero columns back for
        /// any `SELECT` that returns data (`ColumnIndexOutOfBounds`),
        /// discovered while writing this backend's real pgwire
        /// round-trip test (`open-runo-db/tests/aruaru_as_of_commit.rs`).
        /// Writes (`INSERT`/`DELETE`, which return only a command tag, no
        /// rows to decode) are unaffected and keep using bind parameters
        /// above. `sqlx::raw_sql` uses the simple query protocol, which
        /// aruaru-wire's `SimpleQueryHandler` answers with real column
        /// data — so every `SELECT` in this module escapes its literals
        /// manually via `sql_escape` instead of binding them.
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let pk = sql_escape(&crate::migration::aruaru_pk(table, key));
            let mut rows = sqlx::raw_sql(&format!("SELECT value FROM kv_store WHERE pk = '{pk}'"))
                .fetch_all(&self.pool).await
                .map_err(|e| AppError::Internal(format!("aruaru-db get: {e}")))?;
            Ok(match rows.pop() {
                Some(row) => Some(row.try_get::<String, _>(0)
                    .map_err(|e| AppError::Internal(format!("aruaru-db get: decode: {e}")))?),
                None => None,
            })
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            let pk = crate::migration::aruaru_pk(table, key);
            sqlx::query("DELETE FROM kv_store WHERE pk=$1")
                .bind(pk).execute(&self.pool).await
                .map_err(|e| AppError::Internal(format!("aruaru-db delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let table_esc = sql_escape(table);
            let rows = sqlx::raw_sql(&format!(
                "SELECT key,value FROM kv_store WHERE table_name = '{table_esc}' ORDER BY key"
            )).fetch_all(&self.pool).await
            .map_err(|e| AppError::Internal(format!("aruaru-db list: {e}")))?;
            rows.into_iter().map(|row| {
                let key = row.try_get::<String, _>(0)
                    .map_err(|e| AppError::Internal(format!("aruaru-db list: decode key: {e}")))?;
                let value = row.try_get::<String, _>(1)
                    .map_err(|e| AppError::Internal(format!("aruaru-db list: decode value: {e}")))?;
                Ok(Record { key, value })
            }).collect()
        }

        /// `SELECT value FROM kv_store WHERE pk = '<table\u{1}key>' AS OF
        /// COMMIT '<commit_id>'` — the commit-ID read-side query, wired
        /// through to aruaru-db's real `AS OF COMMIT` SQL support
        /// (`aruaru-query::engine::QueryEngine::select_as_of`, added
        /// 2026-07-13). Uses the same synthetic `pk` this backend's
        /// `put`/`get`/`delete` already rely on, since aruaru-db's engine
        /// only matches a single first-column equality predicate — see
        /// `migration::KV_STORE_DDL_ARUARU`'s doc comment.
        ///
        /// **`select_as_of` ignores the requested column list and always
        /// returns the full historical row** (`aruaru-query::engine::
        /// QueryEngine::select_as_of` reconstructs the row from the
        /// Prolly Tree and returns it verbatim — it doesn't re-run
        /// column projection the way a normal `SELECT` does). Discovered
        /// via this backend's real pgwire round-trip test
        /// (`tests/aruaru_as_of_commit.rs`): the query's own `SELECT
        /// value` clause is honored for column *naming* but not for
        /// projection, so the row that comes back is
        /// `(pk, table_name, key, value)` in `kv_store`'s declared column
        /// order — the `value` this method wants is therefore at index
        /// 3, not 0.
        async fn get_at_commit(&self, table: &str, key: &str, commit_id: &str) -> Result<Option<String>> {
            const VALUE_COLUMN_INDEX: usize = 3; // kv_store: pk, table_name, key, value
            let pk = sql_escape(&crate::migration::aruaru_pk(table, key));
            let commit_esc = sql_escape(commit_id);
            let sql = format!(
                "SELECT value FROM kv_store WHERE pk = '{pk}' AS OF COMMIT '{commit_esc}'"
            );
            let mut rows = sqlx::raw_sql(&sql)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| AppError::Internal(format!("aruaru-db get_at_commit: {e}")))?;
            Ok(match rows.pop() {
                Some(row) => Some(row.try_get::<String, _>(VALUE_COLUMN_INDEX)
                    .map_err(|e| AppError::Internal(format!("aruaru-db get_at_commit: decode: {e}")))?),
                None => None,
            })
        }
    }
}

// ── CockroachDB :26257 ────────────────────────────────────────────────────────

#[cfg(feature = "cockroach")]
pub mod cockroach {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use sqlx::PgPool;

    #[derive(Debug, Clone)]
    pub struct CockroachBackend { pool: PgPool }
    impl CockroachBackend {
        pub async fn connect(url: &str) -> Result<Self> {
            let pool = PgPool::connect(url).await
                .map_err(|e| AppError::Internal(format!("CockroachDB connect: {e}")))?;
            tracing::info!(url, "connected to CockroachDB :26257");
            Ok(Self { pool })
        }
        pub fn pool(&self) -> &PgPool { &self.pool }
    }
    #[async_trait]
    impl DbBackend for CockroachBackend {
        fn backend_name(&self) -> &'static str { "cockroachdb" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            sqlx::query("UPSERT INTO kv_store (table_name,key,value) VALUES ($1,$2,$3)")
                .bind(table).bind(key).bind(value).execute(&self.pool).await
                .map_err(|e| AppError::Internal(format!("CockroachDB put: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT value FROM kv_store WHERE table_name=$1 AND key=$2",
            ).bind(table).bind(key).fetch_optional(&self.pool).await
            .map_err(|e| AppError::Internal(format!("CockroachDB get: {e}")))?;
            Ok(row.map(|(v,)| v))
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            sqlx::query("DELETE FROM kv_store WHERE table_name=$1 AND key=$2")
                .bind(table).bind(key).execute(&self.pool).await
                .map_err(|e| AppError::Internal(format!("CockroachDB delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let rows: Vec<(String,String)> = sqlx::query_as(
                "SELECT key,value FROM kv_store WHERE table_name=$1 ORDER BY key",
            ).bind(table).fetch_all(&self.pool).await
            .map_err(|e| AppError::Internal(format!("CockroachDB list: {e}")))?;
            Ok(rows.into_iter().map(|(key,value)| Record { key, value }).collect())
        }
    }
}

// ── YugabyteDB (pgwire) ────────────────────────────────────────────────────────

#[cfg(feature = "yugabyte")]
pub mod yugabyte {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use sqlx::PgPool;

    #[derive(Debug, Clone)]
    pub struct YugabyteBackend { pool: PgPool }
    impl YugabyteBackend {
        pub async fn connect(url: &str) -> Result<Self> {
            let pool = PgPool::connect(url).await
                .map_err(|e| AppError::Internal(format!("YugabyteDB connect: {e}")))?;
            tracing::info!(url, "connected to YugabyteDB");
            Ok(Self { pool })
        }
        pub fn pool(&self) -> &PgPool { &self.pool }
    }
    #[async_trait]
    impl DbBackend for YugabyteBackend {
        fn backend_name(&self) -> &'static str { "yugabytedb" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            sqlx::query(
                "INSERT INTO kv_store (table_name,key,value) VALUES ($1,$2,$3)
                 ON CONFLICT (table_name,key) DO UPDATE SET value=EXCLUDED.value",
            ).bind(table).bind(key).bind(value).execute(&self.pool).await
            .map_err(|e| AppError::Internal(format!("YugabyteDB put: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT value FROM kv_store WHERE table_name=$1 AND key=$2",
            ).bind(table).bind(key).fetch_optional(&self.pool).await
            .map_err(|e| AppError::Internal(format!("YugabyteDB get: {e}")))?;
            Ok(row.map(|(v,)| v))
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            sqlx::query("DELETE FROM kv_store WHERE table_name=$1 AND key=$2")
                .bind(table).bind(key).execute(&self.pool).await
                .map_err(|e| AppError::Internal(format!("YugabyteDB delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let rows: Vec<(String,String)> = sqlx::query_as(
                "SELECT key,value FROM kv_store WHERE table_name=$1 ORDER BY key",
            ).bind(table).fetch_all(&self.pool).await
            .map_err(|e| AppError::Internal(format!("YugabyteDB list: {e}")))?;
            Ok(rows.into_iter().map(|(key,value)| Record { key, value }).collect())
        }
    }
}

// ── MongoDB :27017 ─────────────────────────────────────────────────────────────

#[cfg(feature = "mongodb")]
pub mod mongo {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use mongodb::{
        bson::{doc, Document},
        options::ClientOptions,
        Client, Collection,
    };

    #[derive(Debug, Clone)]
    pub struct MongoBackend { collection: Collection<Document> }
    impl MongoBackend {
        pub async fn connect(url: &str, db_name: &str) -> Result<Self> {
            let opts = ClientOptions::parse(url).await
                .map_err(|e| AppError::Internal(format!("MongoDB url: {e}")))?;
            let col = Client::with_options(opts)
                .map_err(|e| AppError::Internal(format!("MongoDB client: {e}")))?
                .database(db_name).collection::<Document>("kv_store");
            tracing::info!(url, db = db_name, "connected to MongoDB :27017");
            Ok(Self { collection: col })
        }
        fn doc_id(table: &str, key: &str) -> String { format!("{table}/{key}") }
    }
    #[async_trait]
    impl DbBackend for MongoBackend {
        fn backend_name(&self) -> &'static str { "mongodb" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            let id = Self::doc_id(table, key);
            let d = doc! { "_id": &id, "table": table, "key": key, "value": value };
            self.collection
                .replace_one(doc! { "_id": &id }, d)
                .upsert(true)
                .await
                .map_err(|e| AppError::Internal(format!("MongoDB put: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let id = Self::doc_id(table, key);
            Ok(self.collection.find_one(doc! { "_id": &id }).await
                .map_err(|e| AppError::Internal(format!("MongoDB get: {e}")))?
                .and_then(|d| d.get_str("value").ok().map(String::from)))
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            self.collection
                .delete_one(doc! { "_id": Self::doc_id(table, key) }).await
                .map_err(|e| AppError::Internal(format!("MongoDB delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let mut cur = self.collection
                .find(doc! { "table": table })
                .sort(doc! { "key": 1 })
                .await
                .map_err(|e| AppError::Internal(format!("MongoDB list: {e}")))?;
            let mut out = Vec::new();
            while cur.advance().await
                .map_err(|e| AppError::Internal(format!("MongoDB cursor: {e}")))? {
                let d = cur.deserialize_current()
                    .map_err(|e| AppError::Internal(format!("MongoDB deser: {e}")))?;
                if let (Ok(k), Ok(v)) = (d.get_str("key"), d.get_str("value")) {
                    out.push(Record { key: k.into(), value: v.into() });
                }
            }
            Ok(out)
        }
    }
}

// ── Redis :6379 (KeyDB / DragonflyDB 互換) ────────────────────────────────────

#[cfg(feature = "redis")]
pub mod redis_backend {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use deadpool_redis::{Config, Pool, Runtime};
    use redis::AsyncCommands;

    #[derive(Debug, Clone)]
    pub struct RedisBackend { pool: Pool }
    impl RedisBackend {
        pub fn connect(url: &str) -> Result<Self> {
            let pool = Config::from_url(url)
                .create_pool(Some(Runtime::Tokio1))
                .map_err(|e| AppError::Internal(format!("Redis pool: {e}")))?;
            tracing::info!(url, "created Redis connection pool");
            Ok(Self { pool })
        }
        fn rk(table: &str, key: &str) -> String { format!("open-runo:{table}:{key}") }
        fn sk(table: &str) -> String { format!("open-runo:{table}:_keys") }
    }
    #[async_trait]
    impl DbBackend for RedisBackend {
        fn backend_name(&self) -> &'static str { "redis" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            let mut c = self.pool.get().await
                .map_err(|e| AppError::Internal(format!("Redis conn: {e}")))?;
            c.set::<_,_,()>(Self::rk(table,key), value).await
                .map_err(|e| AppError::Internal(format!("Redis SET: {e}")))?;
            c.sadd::<_,_,()>(Self::sk(table), key).await
                .map_err(|e| AppError::Internal(format!("Redis SADD: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let mut c = self.pool.get().await
                .map_err(|e| AppError::Internal(format!("Redis conn: {e}")))?;
            Ok(c.get(Self::rk(table,key)).await
                .map_err(|e| AppError::Internal(format!("Redis GET: {e}")))?)
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            let mut c = self.pool.get().await
                .map_err(|e| AppError::Internal(format!("Redis conn: {e}")))?;
            c.del::<_,()>(Self::rk(table,key)).await
                .map_err(|e| AppError::Internal(format!("Redis DEL: {e}")))?;
            c.srem::<_,_,()>(Self::sk(table), key).await
                .map_err(|e| AppError::Internal(format!("Redis SREM: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let mut c = self.pool.get().await
                .map_err(|e| AppError::Internal(format!("Redis conn: {e}")))?;
            let keys: Vec<String> = c.smembers(Self::sk(table)).await
                .map_err(|e| AppError::Internal(format!("Redis SMEMBERS: {e}")))?;
            let mut out = Vec::new();
            for k in &keys {
                if let Some(v) = c.get::<_,Option<String>>(Self::rk(table,k)).await
                    .map_err(|e| AppError::Internal(format!("Redis GET: {e}")))? {
                    out.push(Record { key: k.clone(), value: v });
                }
            }
            out.sort_by(|a,b| a.key.cmp(&b.key));
            Ok(out)
        }
    }
}

// ── ClickHouse :8123 ──────────────────────────────────────────────────────────

#[cfg(feature = "clickhouse")]
pub mod clickhouse_backend {
    use super::{AppError, DbBackend, Record, Result};
    use async_trait::async_trait;
    use clickhouse::Client;
    use serde::{Deserialize, Serialize};

    #[derive(Clone)]
    pub struct ClickHouseBackend { client: Client }

    // `clickhouse::Client` doesn't implement `Debug`, so it can't be
    // `#[derive(Debug)]`d directly; a manual impl that just names the type
    // satisfies the workspace's `missing_debug_implementations` lint
    // without pretending to expose the client's internals.
    impl std::fmt::Debug for ClickHouseBackend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ClickHouseBackend").finish_non_exhaustive()
        }
    }

    #[derive(clickhouse::Row, Serialize, Deserialize, Debug)]
    struct KvRow { key: String, value: String }

    impl ClickHouseBackend {
        pub fn connect(url: &str) -> Self {
            tracing::info!(url, "created ClickHouse client");
            Self { client: Client::default().with_url(url) }
        }
    }
    #[async_trait]
    impl DbBackend for ClickHouseBackend {
        fn backend_name(&self) -> &'static str { "clickhouse" }
        async fn put(&self, table: &str, key: &str, value: &str) -> Result<()> {
            let mut ins = self.client.insert(table)
                .map_err(|e| AppError::Internal(format!("ClickHouse insert: {e}")))?;
            ins.write(&KvRow { key: key.into(), value: value.into() }).await
                .map_err(|e| AppError::Internal(format!("ClickHouse write: {e}")))?;
            ins.end().await
                .map_err(|e| AppError::Internal(format!("ClickHouse end: {e}")))?;
            Ok(())
        }
        async fn get(&self, table: &str, key: &str) -> Result<Option<String>> {
            let rows = self.client
                .query(&format!(
                    "SELECT key,value FROM `{table}` WHERE key=? \
                     ORDER BY _version DESC LIMIT 1"
                ))
                .bind(key).fetch_all::<KvRow>().await
                .map_err(|e| AppError::Internal(format!("ClickHouse get: {e}")))?;
            Ok(rows.into_iter().next().map(|r| r.value))
        }
        async fn delete(&self, table: &str, key: &str) -> Result<()> {
            self.client
                .query(&format!("ALTER TABLE `{table}` DELETE WHERE key=?"))
                .bind(key).execute().await
                .map_err(|e| AppError::Internal(format!("ClickHouse delete: {e}")))?;
            Ok(())
        }
        async fn list(&self, table: &str) -> Result<Vec<Record>> {
            let rows = self.client
                .query(&format!("SELECT key,value FROM `{table}` ORDER BY key"))
                .fetch_all::<KvRow>().await
                .map_err(|e| AppError::Internal(format!("ClickHouse list: {e}")))?;
            Ok(rows.into_iter().map(|r| Record { key: r.key, value: r.value }).collect())
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_delete_roundtrip() {
        let b = InMemoryBackend::new();
        b.put("users","u1",r#"{"name":"alice"}"#).await.unwrap();
        assert_eq!(b.get("users","u1").await.unwrap(), Some(r#"{"name":"alice"}"#.into()));
        b.delete("users","u1").await.unwrap();
        assert_eq!(b.get("users","u1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn list_returns_only_matching_table() {
        let b = InMemoryBackend::new();
        b.put("schemas","svc_a","{}").await.unwrap();
        b.put("schemas","svc_b","{}").await.unwrap();
        b.put("other","x","{}").await.unwrap();
        let recs = b.list("schemas").await.unwrap();
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().all(|r| r.key.starts_with("svc_")));
    }

    #[tokio::test]
    async fn backend_name() {
        assert_eq!(InMemoryBackend::new().backend_name(), "in-memory");
    }
}
