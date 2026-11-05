//! Migration engine — お引越しを「転送 + 検証」の 2 ステップにする。
//!
//! Every store open-runo supports (PostgreSQL, MySQL, SQLite, aruaru-db,
//! CockroachDB, YugabyteDB, MongoDB, Redis, ClickHouse, in-memory) sits
//! behind the same [`DbBackend`] trait, so **converting between engines is
//! a transfer through the trait**: MySQL → PostgreSQL, PostgreSQL →
//! CockroachDB, anything → anything.
//!
//! ```rust,ignore
//! let report = migrate::transfer(&*mysql, &*postgres, ALL_TABLES).await?;
//! let issues = migrate::verify(&*mysql, &*postgres, ALL_TABLES).await?;
//! assert!(issues.is_empty());
//! ```
//!
//! For engines outside the trait (Snowflake, BigQuery, spreadsheets), the
//! router exposes SQL-dump and CSV exports built on the same table walk.

use crate::DbBackend;
use open_runo_core::Result;
use serde::Serialize;

/// Per-table outcome of a transfer.
#[derive(Debug, Clone, Serialize)]
pub struct TableReport {
    pub table: String,
    pub copied: usize,
}

/// Outcome of a full transfer.
#[derive(Debug, Clone, Serialize, Default)]
pub struct TransferReport {
    pub tables: Vec<TableReport>,
    pub total: usize,
}

/// A record that differs between source and target after a transfer.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyIssue {
    pub table: String,
    pub key: String,
    /// `missing_in_target` or `value_mismatch`.
    pub kind: String,
}

/// Copy every record of `tables` from `source` into `target`.
/// Existing records in the target are overwritten (idempotent re-runs).
pub async fn transfer(
    source: &dyn DbBackend,
    target: &dyn DbBackend,
    tables: &[&str],
) -> Result<TransferReport> {
    let mut report = TransferReport::default();
    for table in tables {
        let rows = source.list(table).await?;
        let mut copied = 0usize;
        for row in rows {
            target.put(table, &row.key, &row.value).await?;
            copied += 1;
        }
        if copied > 0 {
            report.total += copied;
            report.tables.push(TableReport {
                table: (*table).to_string(),
                copied,
            });
        }
    }
    Ok(report)
}

/// Compare `tables` between source and target; empty result = perfect copy.
pub async fn verify(
    source: &dyn DbBackend,
    target: &dyn DbBackend,
    tables: &[&str],
) -> Result<Vec<VerifyIssue>> {
    let mut issues = Vec::new();
    for table in tables {
        for row in source.list(table).await? {
            match target.get(table, &row.key).await? {
                None => issues.push(VerifyIssue {
                    table: (*table).to_string(),
                    key: row.key,
                    kind: "missing_in_target".into(),
                }),
                Some(v) if v != row.value => issues.push(VerifyIssue {
                    table: (*table).to_string(),
                    key: row.key,
                    kind: "value_mismatch".into(),
                }),
                Some(_) => {},
            }
        }
    }
    Ok(issues)
}

/// Transfer then verify in one call — the "簡単お引越し" primitive.
pub async fn transfer_verified(
    source: &dyn DbBackend,
    target: &dyn DbBackend,
    tables: &[&str],
) -> Result<(TransferReport, Vec<VerifyIssue>)> {
    let report = transfer(source, target, tables).await?;
    let issues = verify(source, target, tables).await?;
    Ok((report, issues))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryBackend;

    const TABLES: &[&str] = &["schemas", "scim_users"];

    #[tokio::test]
    async fn transfer_copies_everything_and_verifies_clean() {
        let source = InMemoryBackend::new();
        let target = InMemoryBackend::new();
        source.put("schemas", "a", r#"{"sdl":"A"}"#).await.unwrap();
        source.put("schemas", "b", r#"{"sdl":"B"}"#).await.unwrap();
        source
            .put("scim_users", "u1", r#"{"userName":"alice"}"#)
            .await
            .unwrap();

        let (report, issues) = transfer_verified(&source, &target, TABLES).await.unwrap();
        assert_eq!(report.total, 3);
        assert!(issues.is_empty());
        assert_eq!(
            target.get("schemas", "a").await.unwrap().unwrap(),
            r#"{"sdl":"A"}"#
        );
    }

    #[tokio::test]
    async fn transfer_is_idempotent_and_overwrites_stale_target() {
        let source = InMemoryBackend::new();
        let target = InMemoryBackend::new();
        source.put("schemas", "a", "new").await.unwrap();
        target.put("schemas", "a", "stale").await.unwrap();

        transfer(&source, &target, TABLES).await.unwrap();
        assert_eq!(target.get("schemas", "a").await.unwrap().unwrap(), "new");
        // Second run: still clean.
        let issues = verify(&source, &target, TABLES).await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn verify_reports_missing_and_mismatched() {
        let source = InMemoryBackend::new();
        let target = InMemoryBackend::new();
        source.put("schemas", "gone", "x").await.unwrap();
        source.put("schemas", "diff", "left").await.unwrap();
        target.put("schemas", "diff", "right").await.unwrap();

        let mut issues = verify(&source, &target, TABLES).await.unwrap();
        issues.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].kind, "value_mismatch");
        assert_eq!(issues[1].kind, "missing_in_target");
    }
}
