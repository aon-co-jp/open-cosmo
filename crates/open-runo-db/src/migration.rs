//! Schema migration helpers for the open-runo kv_store table.
//!
//! Run these at startup before any reads or writes.
//! Both PostgreSQL and aruaru-db (pgwire) use the same DDL.

/// DDL for the shared `kv_store` table used by all open-runo crates.
pub const KV_STORE_DDL: &str = "
CREATE TABLE IF NOT EXISTS kv_store (
    table_name TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (table_name, key)
);

CREATE INDEX IF NOT EXISTS kv_store_table_idx ON kv_store (table_name);
";

/// DDL for per-table updated_at trigger (optional, PostgreSQL only).
pub const UPDATED_AT_TRIGGER_DDL: &str = "
CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger WHERE tgname = 'kv_store_updated_at'
    ) THEN
        CREATE TRIGGER kv_store_updated_at
        BEFORE UPDATE ON kv_store
        FOR EACH ROW EXECUTE FUNCTION set_updated_at();
    END IF;
END $$;
";

#[cfg(feature = "postgres")]
pub mod postgres {
    use super::KV_STORE_DDL;
    use open_runo_core::{AppError, Result};
    use sqlx::PgPool;

    /// Apply migrations to a PostgreSQL database.
    /// Safe to call on every startup (idempotent).
    pub async fn run(pool: &PgPool) -> Result<()> {
        sqlx::query(KV_STORE_DDL)
            .execute(pool)
            .await
            .map_err(|e| AppError::Internal(format!("PostgreSQL migration failed: {e}")))?;
        tracing::info!("PostgreSQL migrations applied");
        Ok(())
    }
}

/// DDL for the `kv_store` table as stored inside aruaru-db specifically.
///
/// aruaru-db's own SQL engine (`aruaru_query::QueryEngine`, the thing that
/// actually backs its pgwire endpoint) is a minimal subset: `WHERE` only
/// supports a single `col = 'val'` equality predicate (no `AND`), and a
/// table's Git-on-SQL primary key is always the value of its *first*
/// declared column. The shared [`KV_STORE_DDL`] above declares
/// `PRIMARY KEY (table_name, key)` and relies on two-predicate
/// `WHERE table_name=$1 AND key=$2` reads/deletes — both PostgreSQL-only
/// assumptions that don't hold against aruaru-db's real engine (discovered
/// while wiring the commit-ID `AS OF COMMIT` read API, 2026-07-13).
///
/// To keep every existing single-key operation (`get`/`delete`/upsert
/// conflict target) *and* the new `AS OF COMMIT` read expressible as a
/// single first-column equality, aruaru-db's copy of `kv_store` carries an
/// explicit synthetic `pk` column (`table_name || '' || key`) as its
/// first column. `table_name`/`key` remain as separate columns so
/// `list()` (`WHERE table_name=$1`, single predicate, unaffected) keeps
/// working unchanged.
pub const KV_STORE_DDL_ARUARU: &str = "
CREATE TABLE IF NOT EXISTS kv_store (pk TEXT, table_name TEXT, key TEXT, value TEXT)
";

/// Build the synthetic single-column primary key used by aruaru-db's
/// `kv_store` copy: `table_name || '' || key`. `` (a control
/// character that cannot appear in a table/key string supplied over the
/// REST API's `:table`/`:key` path segments) keeps the composite
/// unambiguous.
pub fn aruaru_pk(table: &str, key: &str) -> String {
    format!("{table}\u{1}{key}")
}

#[cfg(feature = "aruaru")]
pub mod aruaru {
    use super::KV_STORE_DDL_ARUARU;
    use open_runo_core::{AppError, Result};
    use sqlx::PgPool;

    /// Apply migrations to aruaru-db (via its pgwire interface).
    /// Safe to call on every startup (idempotent). Uses
    /// [`KV_STORE_DDL_ARUARU`], not the PostgreSQL-oriented
    /// [`super::KV_STORE_DDL`] — see that constant's doc comment for why.
    pub async fn run(pool: &PgPool) -> Result<()> {
        sqlx::query(KV_STORE_DDL_ARUARU)
            .execute(pool)
            .await
            .map_err(|e| AppError::Internal(format!("aruaru-db migration failed: {e}")))?;
        tracing::info!("aruaru-db migrations applied");
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use crate::migration::aruaru_pk;

        #[test]
        fn pk_is_stable_and_distinguishes_key_boundaries() {
            // Without a separator, table="ab" key="c" would collide with
            // table="a" key="bc". The \u{1} separator prevents that.
            assert_ne!(aruaru_pk("ab", "c"), aruaru_pk("a", "bc"));
            assert_eq!(aruaru_pk("items", "sword"), aruaru_pk("items", "sword"));
        }
    }
}
