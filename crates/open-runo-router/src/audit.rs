//! Audit logging: every mutating operation is recorded in the `audit_log`
//! table, which the DUAL DATABASE routes to aruaru-db (Git-on-SQL) so the
//! trail is immutable-by-history (Cosmo Enterprise compliance parity).

use crate::state::AppState;
use serde::Serialize;

/// One audit trail entry, stored as JSON under a UUID key.
#[derive(Debug, Serialize)]
pub struct AuditRecord {
    pub actor: String,
    pub action: String,
    pub target: String,
    pub at: String,
}

/// Write an audit record. Failures are logged, never propagated — an audit
/// outage must not take the data path down.
pub async fn record(state: &AppState, actor: &str, action: &str, target: impl Into<String>) {
    let entry = AuditRecord {
        actor: actor.to_string(),
        action: action.to_string(),
        target: target.into(),
        at: chrono::Utc::now().to_rfc3339(),
    };
    let key = uuid::Uuid::new_v4().to_string();
    match serde_json::to_string(&entry) {
        Ok(json) => {
            if let Err(e) = state.db.put("audit_log", &key, &json).await {
                tracing::warn!(error = %e, action = %entry.action, "audit log write failed");
            }
        },
        Err(e) => tracing::warn!(error = %e, "audit record serialization failed"),
    }
}
