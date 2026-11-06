//! 管理操作の共通ロジック・第2段階(REST廃止計画、CLAUDE.md「2026-11-01追記」)。
//!
//! 第1段階の[`crate::admin_ops`]と同じ方式。認証/セッション/キー発行、DB操作、
//! バックアップ/移行/整合性、分析、AI経路を、RESTハンドラ(`handlers_hyper`)と
//! GraphQL(`open-runo-gateway::admin`)が同じ関数で提供する。戻り値はRESTの
//! 応答と同じ形(`Serialize`)で、GraphQL側はそれをJSONとして返す。

use crate::admin_ops::{AdminError, AdminResult};
use crate::keyring::{KeyDecision, KeyGuardian};
use crate::middleware::html_cache::HtmlPageCache;
use crate::session::SessionBackend;
use crate::state::AppState;
use hyper::StatusCode;
use open_runo_api_types::{
    DbDeleteResponse, DbRecordAtCommitResponse, DbRecordItem, DbRecordListResponse,
    DbRecordResponse, DbRoutingEntry, DbRoutingInfo, DbStatusResponse,
};
use serde::Serialize;
use std::sync::Arc;

fn internal(e: impl ToString) -> AdminError {
    AdminError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ── 認証 / セッション / キー発行 ─────────────────────────────────────────

const SELF_ISSUE_KEY_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, Serialize)]
pub struct SelfIssued {
    pub api_key: String,
    pub expires_at: String,
}

/// `POST /api/keys/self-issue`(認証不要、発行した鍵自体が資格情報)。
pub async fn self_issue_key(state: &AppState, guardian: &KeyGuardian) -> AdminResult<SelfIssued> {
    let owner = format!("wasm-frontend-{}", uuid::Uuid::new_v4());
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(SELF_ISSUE_KEY_TTL_HOURS);
    let api_key = guardian
        .issue(&owner, vec!["developer".to_string()], Some(expires_at))
        .await
        .map_err(internal)?;
    crate::audit::record(state, "key-guardian", "key.self_issue", owner).await;
    Ok(SelfIssued {
        api_key,
        expires_at: expires_at.to_rfc3339(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionLogin {
    pub owner: String,
    pub csrf_token: String,
    pub expires_in_secs: i64,
}

/// `POST /api/session/login`。有効な`X-Api-Key`をセッションCookie+CSRFへ交換する。
/// 戻り値の2番目は`Set-Cookie`ヘッダ値(HTTP層が付ける)。
pub async fn session_login(
    guardian: &KeyGuardian,
    sessions: &Arc<dyn SessionBackend>,
    api_key: &str,
) -> AdminResult<(SessionLogin, String)> {
    let unauthorized = || AdminError::new(StatusCode::UNAUTHORIZED, "unauthorized");
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(unauthorized());
    }
    let (owner, roles) = match guardian.verify(api_key, chrono::Utc::now()).await {
        KeyDecision::RegistryEmpty => ("dev".to_string(), Vec::new()),
        KeyDecision::Ok { owner, roles } => (owner, roles),
        KeyDecision::Rejected | KeyDecision::Suspended => return Err(unauthorized()),
    };
    let (session_id, csrf_token) = sessions.create(owner.clone(), roles).await;
    Ok((
        SessionLogin {
            owner,
            csrf_token,
            expires_in_secs: crate::session::SESSION_TTL_HOURS * 3600,
        },
        crate::session::set_cookie_header(&session_id),
    ))
}

/// `POST /api/session/logout`(冪等)。戻り値はCookieを消す`Set-Cookie`値。
pub async fn session_logout(
    sessions: &Arc<dyn SessionBackend>,
    session_id: Option<&str>,
) -> String {
    if let Some(id) = session_id {
        sessions.destroy(id).await;
    }
    crate::session::clear_cookie_header()
}

// ── DB操作 ──────────────────────────────────────────────────────────────

fn parse_value(raw: &str) -> serde_json::Value {
    sonic_rs::from_str(raw).unwrap_or(serde_json::Value::String(raw.to_string()))
}

pub fn db_status(state: &AppState) -> DbStatusResponse {
    DbStatusResponse {
        backend: state.db.backend_name().to_string(),
        status: "ok".to_string(),
    }
}

pub fn db_routing() -> DbRoutingInfo {
    let e = |table: &str, target: &str| DbRoutingEntry {
        table: table.into(),
        target: target.into(),
    };
    DbRoutingInfo {
        default_target: "postgresql".into(),
        entries: vec![
            e("sessions", "postgresql"),
            e("api_keys", "postgresql"),
            e("rate_limits", "postgresql"),
            e("schemas", "both"),
            e("backup_jobs", "both"),
            e("persisted_queries", "both"),
            e("schema_history", "aruaru-db"),
            e("change_records", "aruaru-db"),
            e("audit_log", "aruaru-db"),
        ],
    }
}

pub async fn db_list(state: &AppState, table: &str) -> AdminResult<DbRecordListResponse> {
    let records = state.db.list(table).await.map_err(internal)?;
    let items: Vec<DbRecordItem> = records
        .into_iter()
        .map(|r| DbRecordItem {
            key: r.key,
            value: parse_value(&r.value),
        })
        .collect();
    Ok(DbRecordListResponse {
        count: items.len(),
        table: table.to_string(),
        records: items,
    })
}

pub async fn db_get(state: &AppState, table: &str, key: &str) -> AdminResult<DbRecordResponse> {
    match state.db.get(table, key).await {
        Ok(Some(raw)) => Ok(DbRecordResponse {
            table: table.to_string(),
            key: key.to_string(),
            value: parse_value(&raw),
        }),
        Ok(None) => Err(AdminError::new(
            StatusCode::NOT_FOUND,
            format!("record not found: {table}/{key}"),
        )),
        Err(e) => Err(internal(e)),
    }
}

pub async fn db_get_at_commit(
    state: &AppState,
    table: &str,
    key: &str,
    commit_id: &str,
) -> AdminResult<DbRecordAtCommitResponse> {
    match state.db.get_at_commit(table, key, commit_id).await {
        Ok(Some(raw)) => Ok(DbRecordAtCommitResponse {
            table: table.to_string(),
            key: key.to_string(),
            commit_id: commit_id.to_string(),
            value: parse_value(&raw),
        }),
        Ok(None) => Err(AdminError::new(
            StatusCode::NOT_FOUND,
            format!(
                "no value for {table}/{key} as of commit {commit_id} \
                 (commit unknown, or the key did not exist yet at that point)"
            ),
        )),
        Err(open_runo_core::AppError::Validation(msg)) => {
            Err(AdminError::new(StatusCode::NOT_IMPLEMENTED, msg))
        },
        Err(e) => Err(internal(e)),
    }
}

pub async fn db_put(
    state: &AppState,
    actor: &str,
    table: &str,
    key: &str,
    value: serde_json::Value,
) -> AdminResult<DbRecordResponse> {
    let serialized =
        serde_json::to_string(&value).map_err(|e| internal(format!("serialize value: {e}")))?;
    state
        .db
        .put(table, key, &serialized)
        .await
        .map_err(internal)?;
    crate::audit::record(state, actor, "db.put", format!("{table}/{key}")).await;
    Ok(DbRecordResponse {
        table: table.to_string(),
        key: key.to_string(),
        value,
    })
}

pub async fn db_delete(
    state: &AppState,
    actor: &str,
    table: &str,
    key: &str,
) -> AdminResult<DbDeleteResponse> {
    state.db.delete(table, key).await.map_err(internal)?;
    crate::audit::record(state, actor, "db.delete", format!("{table}/{key}")).await;
    Ok(DbDeleteResponse {
        table: table.to_string(),
        key: key.to_string(),
        deleted: true,
    })
}

// ── バックアップ / 移行 / 整合性 ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct BackupExported {
    pub written: Vec<String>,
    pub records: usize,
}

pub async fn backup_export(
    state: &AppState,
    cache: &HtmlPageCache,
    actor: &str,
) -> AdminResult<BackupExported> {
    let config = crate::maintenance::BackupConfig::from_env();
    let (written, records) = crate::maintenance::export_backup(state, cache, &config)
        .await
        .map_err(internal)?;
    crate::audit::record(
        state,
        actor,
        "backup.export",
        format!("{records} records → {}", written.join(", ")),
    )
    .await;
    Ok(BackupExported { written, records })
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupImported {
    pub restored: usize,
}

pub async fn backup_import(
    state: &AppState,
    actor: &str,
    path: &str,
) -> AdminResult<BackupImported> {
    let restored = crate::maintenance::import_backup(state, path)
        .await
        .map_err(|e| AdminError::new(StatusCode::BAD_REQUEST, e))?;
    crate::audit::record(
        state,
        actor,
        "backup.import",
        format!("{restored} records ← {path}"),
    )
    .await;
    Ok(BackupImported { restored })
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupRestored {
    pub restored_from: String,
    pub restored: usize,
}

pub async fn backup_restore_latest(state: &AppState, actor: &str) -> AdminResult<BackupRestored> {
    let config = crate::maintenance::BackupConfig::from_env();
    let path = crate::maintenance::find_latest_backup_async(config)
        .await
        .ok_or_else(|| AdminError::new(StatusCode::NOT_FOUND, "no backup file found"))?;
    let path_str = path.display().to_string();
    let restored = crate::maintenance::import_backup(state, &path_str)
        .await
        .map_err(internal)?;
    crate::audit::record(
        state,
        actor,
        "backup.restore_latest",
        format!("{restored} records ← {path_str}"),
    )
    .await;
    Ok(BackupRestored {
        restored_from: path_str,
        restored,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Conversion {
    pub written: Vec<String>,
}

pub fn parse_dialect(s: &str) -> AdminResult<crate::maintenance::SqlDialect> {
    use crate::maintenance::SqlDialect;
    match s.to_lowercase().as_str() {
        "postgres" => Ok(SqlDialect::Postgres),
        "mysql" => Ok(SqlDialect::Mysql),
        "generic" => Ok(SqlDialect::Generic),
        other => Err(AdminError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unknown dialect: {other}"),
        )),
    }
}

pub async fn migrate_export_sql(
    state: &AppState,
    actor: &str,
    dialect: crate::maintenance::SqlDialect,
) -> AdminResult<Conversion> {
    let sql = crate::maintenance::export_sql(state, dialect)
        .await
        .map_err(internal)?;
    let name = format!(
        "open-runo-dump-{:?}-{}.sql",
        dialect,
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    )
    .to_lowercase();
    let written = crate::maintenance::write_to_backup_dirs_async(
        crate::maintenance::BackupConfig::from_env(),
        name,
        sql,
    )
    .await
    .map_err(internal)?;
    crate::audit::record(state, actor, "migrate.export_sql", written.join(", ")).await;
    Ok(Conversion { written })
}

pub async fn migrate_export_csv(state: &AppState, actor: &str) -> AdminResult<Conversion> {
    let csv = crate::maintenance::export_csv(state)
        .await
        .map_err(internal)?;
    let name = format!(
        "open-runo-dump-{}.csv",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let written = crate::maintenance::write_to_backup_dirs_async(
        crate::maintenance::BackupConfig::from_env(),
        name,
        csv,
    )
    .await
    .map_err(internal)?;
    crate::audit::record(state, actor, "migrate.export_csv", written.join(", ")).await;
    Ok(Conversion { written })
}

#[derive(Debug, Serialize)]
pub struct IntegrityReport {
    pub backend: &'static str,
    pub healed: usize,
    pub discrepancies: Vec<open_runo_db::dual::Discrepancy>,
}

pub async fn integrity_check(state: &AppState, actor: &str) -> AdminResult<IntegrityReport> {
    let discrepancies = state
        .db
        .consistency_check_and_heal()
        .await
        .map_err(internal)?;
    for d in &discrepancies {
        crate::audit::record(
            state,
            actor,
            "integrity.heal",
            format!("{}/{} {} (from {})", d.table, d.key, d.kind, d.healed_from),
        )
        .await;
    }
    Ok(IntegrityReport {
        backend: state.db.backend_name(),
        healed: discrepancies.len(),
        discrepancies,
    })
}

// ── 分析 ────────────────────────────────────────────────────────────────

pub fn requests_per_month(state: &AppState) -> serde_json::Value {
    serde_json::json!({ "months": state.request_metrics.requests_per_month() })
}

pub fn operations_summary(state: &AppState) -> serde_json::Value {
    let operations: Vec<serde_json::Value> = state
        .request_metrics
        .operations_summary()
        .into_iter()
        .map(|op| {
            serde_json::json!({
                "method": op.method,
                "path": op.path,
                "count": op.count,
                "error_count": op.error_count,
                "total_duration_ms": op.total_duration_ms,
                "avg_duration_ms": op.avg_duration_ms(),
                "error_rate": op.error_rate(),
            })
        })
        .collect();
    serde_json::json!({ "operations": operations })
}

#[derive(Debug, Serialize)]
pub struct PatternStat {
    pub pattern: String,
    pub requests: u64,
    pub arrival_interval_secs: Option<f64>,
    pub update_interval_secs: Option<f64>,
    pub render_cost_secs: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AiStats {
    pub ai_enabled: bool,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub admitted: u64,
    pub rejected: u64,
    pub hit_ratio: f64,
    pub tracked_keys: usize,
    pub tracked_patterns: usize,
    pub top_patterns: Vec<PatternStat>,
}

/// `GET /api/cache/ai-stats`
pub fn ai_stats(cache: &HtmlPageCache) -> AiStats {
    let Some(p) = cache.predictor() else {
        return AiStats {
            ai_enabled: false,
            cache_hits: 0,
            cache_misses: 0,
            admitted: 0,
            rejected: 0,
            hit_ratio: 0.0,
            tracked_keys: 0,
            tracked_patterns: 0,
            top_patterns: Vec::new(),
        };
    };
    let snap = p.snapshot();
    let total = snap.outcomes.cache_hits + snap.outcomes.cache_misses;
    let mut patterns: Vec<PatternStat> = snap
        .patterns
        .iter()
        .map(|(k, s)| PatternStat {
            pattern: k.clone(),
            requests: s.requests,
            arrival_interval_secs: s.arrival_interval_secs,
            update_interval_secs: s.update_interval_secs,
            render_cost_secs: s.render_cost_secs,
        })
        .collect();
    patterns.sort_by_key(|p| std::cmp::Reverse(p.requests));
    patterns.truncate(20);
    AiStats {
        ai_enabled: true,
        cache_hits: snap.outcomes.cache_hits,
        cache_misses: snap.outcomes.cache_misses,
        admitted: snap.outcomes.admitted,
        rejected: snap.outcomes.rejected,
        hit_ratio: if total == 0 {
            0.0
        } else {
            snap.outcomes.cache_hits as f64 / total as f64
        },
        tracked_keys: snap.keys.len(),
        tracked_patterns: snap.patterns.len(),
        top_patterns: patterns,
    }
}

// ── AI経路 ──────────────────────────────────────────────────────────────

fn parse_provider(s: &str) -> open_runo_ai_routing::Provider {
    use open_runo_ai_routing::Provider;
    match s.to_lowercase().replace('-', "_").as_str() {
        "openai" => Provider::OpenAi,
        "anthropic" | "anthropic_claude" => Provider::AnthropicClaude,
        "google" | "google_gemini" | "gemini" => Provider::GoogleGemini,
        "deepseek" => Provider::DeepSeek,
        "local" | "local_llm" => Provider::LocalLlm,
        _ => Provider::CustomOpenAiCompatible,
    }
}

fn parse_policy(s: &str) -> open_runo_ai_routing::RoutingPolicy {
    use open_runo_ai_routing::RoutingPolicy;
    match s.to_lowercase().as_str() {
        "latency" | "latency_optimized" => RoutingPolicy::LatencyOptimized,
        "local" | "local_first" => RoutingPolicy::LocalFirst,
        "privacy" | "privacy_first" => RoutingPolicy::PrivacyFirst,
        _ => RoutingPolicy::CostOptimized,
    }
}

fn provider_name(p: &open_runo_ai_routing::Provider) -> &'static str {
    use open_runo_ai_routing::Provider;
    match p {
        Provider::OpenAi => "openai",
        Provider::AnthropicClaude => "anthropic_claude",
        Provider::GoogleGemini => "google_gemini",
        Provider::DeepSeek => "deepseek",
        Provider::LocalLlm => "local_llm",
        Provider::CustomOpenAiCompatible => "custom_openai_compatible",
    }
}

#[derive(Debug, Clone)]
pub struct RouteCandidate {
    pub provider: String,
    pub estimated_cost_usd_per_1k_tokens: f64,
    pub estimated_latency_ms: u32,
    pub is_local: bool,
    pub context_length: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteChoice {
    pub selected_provider: String,
    pub is_local: bool,
    pub estimated_cost_usd_per_1k_tokens: f64,
    pub estimated_latency_ms: u32,
}

/// `POST /api/ai/route`
pub fn ai_route(
    policy: &str,
    min_context_length: Option<u32>,
    candidates: &[RouteCandidate],
) -> AdminResult<RouteChoice> {
    let candidates: Vec<open_runo_ai_routing::Candidate> = candidates
        .iter()
        .map(|c| open_runo_ai_routing::Candidate {
            provider: parse_provider(&c.provider),
            estimated_cost_usd_per_1k_tokens: c.estimated_cost_usd_per_1k_tokens,
            estimated_latency_ms: c.estimated_latency_ms,
            is_local: c.is_local,
            context_length: c.context_length,
        })
        .collect();
    let chosen = open_runo_ai_routing::route(
        &candidates,
        parse_policy(policy),
        min_context_length.unwrap_or(0),
    )
    .map_err(|e| AdminError::new(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    Ok(RouteChoice {
        selected_provider: provider_name(&chosen.provider).to_string(),
        is_local: chosen.is_local,
        estimated_cost_usd_per_1k_tokens: chosen.estimated_cost_usd_per_1k_tokens,
        estimated_latency_ms: chosen.estimated_latency_ms,
    })
}

// ── SCIMユーザー(管理画面用。IdP向けの/scim/v2/*標準プロトコルもこの関数を共有) ──

fn scim_user_store(state: &AppState) -> open_runo_scim::ScimUserStore {
    open_runo_scim::ScimUserStore::new(Arc::clone(&state.db))
}

/// `GET /scim/v2/Users`。`filter`はSCIMの`userName eq "..."`形式。
pub async fn scim_list_users(
    state: &AppState,
    filter: Option<&str>,
) -> AdminResult<open_runo_scim::ListResponse> {
    let filter = filter.and_then(open_runo_scim::ScimUserStore::parse_user_name_filter);
    let users = scim_user_store(state)
        .list(filter.as_deref())
        .await
        .map_err(internal)?;
    Ok(open_runo_scim::ListResponse {
        schemas: vec!["urn:ietf:params:scim:api:messages:2.0:ListResponse".into()],
        total_results: users.len(),
        resources: users,
    })
}

/// `POST /scim/v2/Users`。作成後にKeyGuardianが鍵を自動発行し、平文鍵を
/// `urn:open-runo:params:scim:api-key`に載せて返す(これが唯一の取得機会)。
pub async fn scim_create_user(
    state: &AppState,
    guardian: &KeyGuardian,
    actor: &str,
    input: open_runo_scim::UserInput,
) -> AdminResult<serde_json::Value> {
    let user = scim_user_store(state)
        .create(input)
        .await
        .map_err(|e| AdminError::new(StatusCode::CONFLICT, e.to_string()))?;
    crate::audit::record(state, actor, "scim.user.create", user.user_name.clone()).await;
    let mut body = serde_json::to_value(&user).map_err(internal)?;
    match guardian
        .issue(&user.user_name, user.roles.clone(), None)
        .await
    {
        Ok(plaintext) => {
            body["urn:open-runo:params:scim:api-key"] = serde_json::Value::String(plaintext);
            crate::audit::record(
                state,
                "key-guardian",
                "key.auto_issue",
                user.user_name.clone(),
            )
            .await;
        },
        Err(e) => {
            tracing::warn!(error = %e, owner = %user.user_name, "auto key issue failed");
        },
    }
    Ok(body)
}

/// `DELETE /scim/v2/Users/:id`。削除したユーザーの鍵は自動失効させる。
pub async fn scim_delete_user(
    state: &AppState,
    guardian: &KeyGuardian,
    actor: &str,
    id: &str,
) -> AdminResult<()> {
    let owner = scim_user_store(state)
        .get(id)
        .await
        .map_err(internal)?
        .map(|u| u.user_name);
    scim_user_store(state).delete(id).await.map_err(internal)?;
    crate::audit::record(state, actor, "scim.user.delete", id.to_string()).await;
    if let Some(owner) = owner {
        if let Ok(n) = guardian.revoke_owner(&owner).await {
            if n > 0 {
                crate::audit::record(
                    state,
                    "key-guardian",
                    "key.auto_revoke",
                    format!("{owner} ({n} keys, deleted)"),
                )
                .await;
            }
        }
    }
    Ok(())
}

/// 管理画面・GraphQL向けの簡易版(`userName`と`roles`だけで作成)。
pub async fn scim_create_user_named(
    state: &AppState,
    guardian: &KeyGuardian,
    actor: &str,
    user_name: &str,
    roles: Vec<String>,
) -> AdminResult<serde_json::Value> {
    let input: open_runo_scim::UserInput =
        serde_json::from_value(serde_json::json!({ "userName": user_name, "roles": roles }))
            .map_err(|e| AdminError::new(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    scim_create_user(state, guardian, actor, input).await
}
