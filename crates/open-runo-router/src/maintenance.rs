//! Self-maintenance: the framework looks after its own data.
//!
//! - **Model persistence**: the AI cache predictor's learned model is
//!   restored from the `ai_model` table at startup and saved back
//!   periodically. `ai_model` is DUAL-routed to *Both*, so the learned
//!   intelligence lives in PostgreSQL **and** aruaru-db and survives the
//!   loss of either.
//! - **Portable backups**: every table plus the learned model is exported
//!   into ONE relocatable JSON file (引っ越し可能ファイル). With a mirror
//!   directory configured — point it at a Google Drive for Desktop synced
//!   folder — each backup is written to TWO places automatically.
//! - **Integrity loop**: `DbBackend::consistency_check_and_heal` is run on
//!   a timer; any divergence between the two databases is healed and
//!   reported to the audit log.
//!
//! Env (all optional):
//! `OPEN_RUNO_AI_PERSIST_SECS` (default 300, 0 = off),
//! `OPEN_RUNO_INTEGRITY_SECS` (default 3600, 0 = off),
//! `OPEN_RUNO_BACKUP_DIR` (default `./backups`),
//! `OPEN_RUNO_BACKUP_MIRROR_DIR` (e.g. your Google Drive folder),
//! `OPEN_RUNO_BACKUP_SECS` (default 0 = manual via `/api/backup/export`).

use crate::middleware::html_cache::HtmlPageCache;
use crate::state::AppState;
use chrono::Utc;
use open_runo_cache::predictor::Snapshot;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Table + key where the learned model is stored (DUAL-routed to Both).
pub const MODEL_TABLE: &str = "ai_model";
pub const MODEL_KEY: &str = "cache_predictor";

/// Every logical table the framework uses (for full exports).
pub const ALL_TABLES: &[&str] = &[
    "schemas",
    "schema_history",
    "change_records",
    "audit_log",
    "backup_jobs",
    "sessions",
    "api_keys",
    "rate_limits",
    "persisted_queries",
    "scim_users",
    "scim_groups",
    "ai_model",
];

// ── Model persistence ───────────────────────────────────────────────────────

/// Save the predictor's learned model into the database (both sides).
pub async fn save_model(state: &AppState, cache: &HtmlPageCache) -> bool {
    let Some(predictor) = cache.predictor() else {
        return false;
    };
    let snapshot = predictor.snapshot();
    match serde_json::to_string(&snapshot) {
        Ok(json) => match state.db.put(MODEL_TABLE, MODEL_KEY, &json).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "AI model save failed");
                false
            },
        },
        Err(e) => {
            tracing::warn!(error = %e, "AI model serialization failed");
            false
        },
    }
}

/// Restore the learned model from the database (warm start after reboot).
pub async fn restore_model(state: &AppState, cache: &HtmlPageCache) -> bool {
    let Some(predictor) = cache.predictor() else {
        return false;
    };
    match state.db.get(MODEL_TABLE, MODEL_KEY).await {
        Ok(Some(json)) => match sonic_rs::from_str::<Snapshot>(&json) {
            Ok(snapshot) => {
                predictor.restore(snapshot);
                tracing::info!("AI model restored: the framework woke up smart");
                true
            },
            Err(e) => {
                tracing::warn!(error = %e, "stored AI model unreadable; starting fresh");
                false
            },
        },
        _ => false,
    }
}

// ── Portable backup (引っ越し可能ファイル) ─────────────────────────────────

/// One self-contained, relocatable backup file.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupFile {
    pub format: String,
    pub created_at: String,
    /// table → key → raw value.
    pub tables: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct BackupConfig {
    pub dir: PathBuf,
    /// Second location — point this at a Google Drive for Desktop folder
    /// and every backup lands in the cloud too (二か所バックアップ).
    pub mirror_dir: Option<PathBuf>,
}

impl BackupConfig {
    pub fn from_env() -> Self {
        Self {
            dir: std::env::var("OPEN_RUNO_BACKUP_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./backups")),
            mirror_dir: std::env::var("OPEN_RUNO_BACKUP_MIRROR_DIR")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        }
    }
}

/// Export every table (data + learned model) into one portable JSON file,
/// written to the primary dir and, when configured, mirrored to the second
/// location. Returns the written paths.
pub async fn export_backup(
    state: &AppState,
    cache: &HtmlPageCache,
    config: &BackupConfig,
) -> std::result::Result<(Vec<String>, usize), String> {
    // Make sure the freshest learning is included in "全ての DATA".
    save_model(state, cache).await;

    let mut tables = HashMap::new();
    let mut records = 0usize;
    for table in ALL_TABLES {
        let rows = state
            .db
            .list(table)
            .await
            .map_err(|e| format!("list {table}: {e}"))?;
        if rows.is_empty() {
            continue;
        }
        let map: HashMap<String, String> = rows.into_iter().map(|r| (r.key, r.value)).collect();
        records += map.len();
        tables.insert((*table).to_string(), map);
    }

    let file = BackupFile {
        format: "open-runo-backup/v1".into(),
        created_at: Utc::now().to_rfc3339(),
        tables,
    };
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    let name = format!(
        "open-runo-backup-{}.json",
        Utc::now().format("%Y%m%d-%H%M%S")
    );

    // File I/O is genuinely blocking (create_dir_all + write, potentially to
    // two locations); running it inline on the async executor would stall
    // whichever tokio worker thread picked up this request, starving other
    // concurrent requests on that worker. Offload to the blocking thread
    // pool via `spawn_blocking` (tokio's dedicated pool for exactly this).
    let dirs: Vec<PathBuf> = std::iter::once(config.dir.clone())
        .chain(config.mirror_dir.clone())
        .collect();
    let written = tokio::task::spawn_blocking(move || {
        let mut written = Vec::new();
        for dir in &dirs {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!(dir = %dir.display(), error = %e, "backup dir unavailable");
                continue;
            }
            let path = dir.join(&name);
            match std::fs::write(&path, &json) {
                Ok(()) => written.push(path.display().to_string()),
                Err(e) => tracing::warn!(path = %path.display(), error = %e, "backup write failed"),
            }
        }
        written
    })
    .await
    .map_err(|e| format!("backup write task panicked: {e}"))?;

    if written.is_empty() {
        return Err("no backup location was writable".into());
    }
    Ok((written, records))
}

/// Import a portable backup file (restore after a move / disaster).
pub async fn import_backup(state: &AppState, path: &str) -> std::result::Result<usize, String> {
    // Same rationale as `export_backup`: keep synchronous disk I/O off the
    // async worker thread.
    let path_owned = path.to_string();
    let json = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path_owned))
        .await
        .map_err(|e| format!("backup read task panicked: {e}"))?
        .map_err(|e| format!("read {path}: {e}"))?;
    let file: BackupFile = sonic_rs::from_str(&json).map_err(|e| format!("parse: {e}"))?;
    if file.format != "open-runo-backup/v1" {
        return Err(format!("unknown backup format: {}", file.format));
    }
    let mut restored = 0usize;
    for (table, rows) in &file.tables {
        for (key, value) in rows {
            state
                .db
                .put(table, key, value)
                .await
                .map_err(|e| format!("restore {table}/{key}: {e}"))?;
            restored += 1;
        }
    }
    Ok(restored)
}

/// Async, non-blocking-executor-friendly wrapper around
/// [`find_latest_backup`] for callers already on a tokio worker thread
/// (e.g. HTTP handlers) — directory scanning + per-entry `metadata()` calls
/// are blocking syscalls, so this offloads them via `spawn_blocking`.
pub async fn find_latest_backup_async(config: BackupConfig) -> Option<PathBuf> {
    tokio::task::spawn_blocking(move || find_latest_backup(&config))
        .await
        .unwrap_or(None)
}

/// Find the newest portable backup across the primary and mirror dirs
/// (簡単復活: no need to remember file names). Synchronous — prefer
/// [`find_latest_backup_async`] from async/handler code.
pub fn find_latest_backup(config: &BackupConfig) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in std::iter::once(&config.dir).chain(config.mirror_dir.iter()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("open-runo-backup-") || !name.ends_with(".json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                newest = Some((modified, path));
            }
        }
    }
    newest.map(|(_, p)| p)
}

// ── Engine conversion exports (Snowflake / BI / any SQL engine) ────────────

/// SQL dialect for dump generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SqlDialect {
    /// PostgreSQL / CockroachDB / YugabyteDB (`ON CONFLICT` upsert).
    Postgres,
    /// MySQL / MariaDB (`ON DUPLICATE KEY UPDATE`).
    Mysql,
    /// Snowflake and other plain-INSERT engines (no upsert clause;
    /// bulk loads should prefer the CSV export + `COPY INTO`).
    Generic,
}

fn sql_escape(v: &str) -> String {
    v.replace('\'', "''")
}

/// Render every table into one loadable SQL script. The data lands in a
/// single generic KV table, so the script runs on any SQL engine.
pub async fn export_sql(
    state: &AppState,
    dialect: SqlDialect,
) -> std::result::Result<String, String> {
    let mut out = String::new();
    out.push_str("-- open-runo portable dump (open_runo_kv)\n");
    out.push_str(
        "CREATE TABLE IF NOT EXISTS open_runo_kv (\n  tbl VARCHAR(255) NOT NULL,\n  k   VARCHAR(512) NOT NULL,\n  v   TEXT,\n  PRIMARY KEY (tbl, k)\n);\n",
    );
    for table in ALL_TABLES {
        let rows = state.db.list(table).await.map_err(|e| e.to_string())?;
        for row in rows {
            let stmt = match dialect {
                SqlDialect::Postgres => format!(
                    "INSERT INTO open_runo_kv (tbl, k, v) VALUES ('{}', '{}', '{}') ON CONFLICT (tbl, k) DO UPDATE SET v = EXCLUDED.v;\n",
                    sql_escape(table), sql_escape(&row.key), sql_escape(&row.value)
                ),
                SqlDialect::Mysql => format!(
                    "INSERT INTO open_runo_kv (tbl, k, v) VALUES ('{}', '{}', '{}') ON DUPLICATE KEY UPDATE v = VALUES(v);\n",
                    sql_escape(table), sql_escape(&row.key), sql_escape(&row.value)
                ),
                SqlDialect::Generic => format!(
                    "INSERT INTO open_runo_kv (tbl, k, v) VALUES ('{}', '{}', '{}');\n",
                    sql_escape(table), sql_escape(&row.key), sql_escape(&row.value)
                ),
            };
            out.push_str(&stmt);
        }
    }
    Ok(out)
}

fn csv_field(v: &str) -> String {
    format!("\"{}\"", v.replace('\"', "\"\""))
}

/// Render every table into RFC 4180 CSV (`tbl,k,v` columns) — the format
/// Snowflake ingests directly with `COPY INTO ... FILE_FORMAT (TYPE=CSV)`.
pub async fn export_csv(state: &AppState) -> std::result::Result<String, String> {
    let mut out = String::from("tbl,k,v\r\n");
    for table in ALL_TABLES {
        let rows = state.db.list(table).await.map_err(|e| e.to_string())?;
        for row in rows {
            out.push_str(&format!(
                "{},{},{}\r\n",
                csv_field(table),
                csv_field(&row.key),
                csv_field(&row.value)
            ));
        }
    }
    Ok(out)
}

/// Write a conversion export to the primary + mirror dirs (二か所).
pub fn write_to_backup_dirs(
    config: &BackupConfig,
    name: &str,
    content: &str,
) -> std::result::Result<Vec<String>, String> {
    let mut written = Vec::new();
    for dir in std::iter::once(&config.dir).chain(config.mirror_dir.iter()) {
        if std::fs::create_dir_all(dir).is_err() {
            continue;
        }
        let path = dir.join(name);
        if std::fs::write(&path, content).is_ok() {
            written.push(path.display().to_string());
        }
    }
    if written.is_empty() {
        return Err("no export location was writable".into());
    }
    Ok(written)
}

/// Async wrapper around [`write_to_backup_dirs`] for handler call sites —
/// offloads the `create_dir_all`/`write` syscalls to `spawn_blocking` so an
/// export request does not block the tokio worker thread it landed on.
pub async fn write_to_backup_dirs_async(
    config: BackupConfig,
    name: String,
    content: String,
) -> std::result::Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || write_to_backup_dirs(&config, &name, &content))
        .await
        .map_err(|e| format!("export write task panicked: {e}"))?
}

// ── Background self-maintenance loops ───────────────────────────────────────

fn env_secs(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Spawn the self-maintenance tasks (no-ops outside a tokio runtime, e.g.
/// in sync unit tests). Called by `build_app`.
pub fn spawn(state: Arc<AppState>, cache: Arc<HtmlPageCache>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };

    // 1. Restore the learned model once, then save it periodically.
    let persist_secs = env_secs("OPEN_RUNO_AI_PERSIST_SECS", 300);
    if cache.predictor().is_some() && persist_secs > 0 {
        let state_c = Arc::clone(&state);
        let cache_c = Arc::clone(&cache);
        handle.spawn(async move {
            restore_model(&state_c, &cache_c).await;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(persist_secs));
            tick.tick().await; // first tick fires immediately; skip it
            loop {
                tick.tick().await;
                save_model(&state_c, &cache_c).await;
            }
        });
    }

    // 2. Cross-database integrity check + self-heal on a timer.
    let integrity_secs = env_secs("OPEN_RUNO_INTEGRITY_SECS", 3600);
    if integrity_secs > 0 {
        let state_c = Arc::clone(&state);
        handle.spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(integrity_secs));
            tick.tick().await;
            loop {
                tick.tick().await;
                match state_c.db.consistency_check_and_heal().await {
                    Ok(report) if !report.is_empty() => {
                        tracing::warn!(healed = report.len(), "integrity divergence healed");
                        for d in &report {
                            crate::audit::record(
                                &state_c,
                                "integrity-guardian",
                                "integrity.heal",
                                format!(
                                    "{}/{} {} (from {})",
                                    d.table, d.key, d.kind, d.healed_from
                                ),
                            )
                            .await;
                        }
                    },
                    Ok(_) => {},
                    Err(e) => tracing::warn!(error = %e, "integrity check failed"),
                }
            }
        });
    }

    // 3. Periodic portable backups (off unless OPEN_RUNO_BACKUP_SECS > 0).
    let backup_secs = env_secs("OPEN_RUNO_BACKUP_SECS", 0);
    if backup_secs > 0 {
        let config = BackupConfig::from_env();
        handle.spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(backup_secs));
            tick.tick().await;
            loop {
                tick.tick().await;
                match export_backup(&state, &cache, &config).await {
                    Ok((paths, records)) => {
                        tracing::info!(?paths, records, "periodic backup written");
                    },
                    Err(e) => tracing::warn!(error = %e, "periodic backup failed"),
                }
            }
        });
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::html_cache::HtmlCacheConfig;

    fn ai_cache() -> HtmlPageCache {
        let mut cfg = HtmlCacheConfig::enabled_for_tests(60);
        cfg.ai = true;
        HtmlPageCache::new(cfg)
    }

    #[tokio::test]
    async fn model_survives_a_restart() {
        let state = AppState::new();
        let cache = ai_cache();

        // Learn something, then persist.
        let p = cache.predictor().unwrap();
        let now = Utc::now();
        p.observe_and_decide("/hot", "/hot", now);
        p.observe_and_decide("/hot", "/hot", now + chrono::Duration::seconds(1));
        assert!(save_model(&state, &cache).await);

        // "Reboot": a fresh cache instance restores the learned model from
        // the database and is immediately smart again.
        let reborn = ai_cache();
        assert!(restore_model(&state, &reborn).await);
        assert!(reborn.predictor().unwrap().observe_and_decide(
            "/hot",
            "/hot",
            now + chrono::Duration::seconds(2)
        ));
    }

    #[tokio::test]
    async fn backup_exports_to_two_places_and_reimports() {
        let state = AppState::new();
        let cache = ai_cache();

        // Some data + some learning.
        state
            .db
            .put("schemas", "users", r#"{"sdl":"type U{}"}"#)
            .await
            .unwrap();
        cache
            .predictor()
            .unwrap()
            .observe_and_decide("/p", "/p", Utc::now());

        let base = std::env::temp_dir().join(format!("orn-bk-{}", uuid::Uuid::new_v4()));
        let config = BackupConfig {
            dir: base.join("primary"),
            mirror_dir: Some(base.join("mirror")), // = Google Drive folder in prod
        };

        let (paths, records) = export_backup(&state, &cache, &config).await.unwrap();
        assert_eq!(paths.len(), 2, "written to BOTH locations");
        assert!(records >= 2); // schemas + ai_model at least

        // Disaster: wipe, then restore from the mirror copy.
        state.db.delete("schemas", "users").await.unwrap();
        let restored = import_backup(&state, &paths[1]).await.unwrap();
        assert!(restored >= 2);
        assert!(state.db.get("schemas", "users").await.unwrap().is_some());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn import_rejects_unknown_format() {
        let state = AppState::new();
        let path = std::env::temp_dir().join(format!("orn-bad-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            r#"{"format":"other/v9","created_at":"x","tables":{}}"#,
        )
        .unwrap();
        assert!(import_backup(&state, path.to_str().unwrap()).await.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn latest_backup_is_discovered_for_one_click_restore() {
        let state = AppState::new();
        let cache = ai_cache();
        state.db.put("schemas", "s", "v").await.unwrap();

        let base = std::env::temp_dir().join(format!("orn-latest-{}", uuid::Uuid::new_v4()));
        let config = BackupConfig {
            dir: base.clone(),
            mirror_dir: None,
        };

        export_backup(&state, &cache, &config).await.unwrap();
        let found = find_latest_backup(&config).expect("backup discovered");
        assert!(found.to_string_lossy().contains("open-runo-backup-"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn sql_dump_speaks_each_dialect() {
        let state = AppState::new();
        state.db.put("schemas", "svc", "it's json").await.unwrap();

        let pg = export_sql(&state, SqlDialect::Postgres).await.unwrap();
        assert!(pg.contains("ON CONFLICT (tbl, k) DO UPDATE"));
        assert!(pg.contains("it''s json"), "quotes escaped");

        let my = export_sql(&state, SqlDialect::Mysql).await.unwrap();
        assert!(my.contains("ON DUPLICATE KEY UPDATE"));

        let sf = export_sql(&state, SqlDialect::Generic).await.unwrap();
        assert!(!sf.contains("ON CONFLICT") && !sf.contains("ON DUPLICATE"));
        assert!(sf.contains("CREATE TABLE IF NOT EXISTS open_runo_kv"));
    }

    #[tokio::test]
    async fn csv_export_is_rfc4180_quoted() {
        let state = AppState::new();
        state
            .db
            .put("schemas", "q", "say \"hi\", world")
            .await
            .unwrap();
        let csv = export_csv(&state).await.unwrap();
        assert!(csv.starts_with("tbl,k,v\r\n"));
        assert!(csv.contains("\"say \"\"hi\"\", world\""));
    }
}
