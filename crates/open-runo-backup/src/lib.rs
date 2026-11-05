//! `open-runo-backup`: plans and tracks distributed backups (local storage,
//! remote VPS, S3-compatible object storage, other open-runo nodes, and
//! git-compatible archive repositories).
//!
//! This crate models backup *plans* and *runs*; actual transport
//! implementations (S3 client, rsync, etc.) are out of scope for Phase 1
//! and will be added as pluggable [`BackupTarget`] handlers.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use chrono::{DateTime, Utc};
use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupTarget {
    LocalStorage,
    RemoteVps { host: String },
    S3Compatible { bucket: String },
    PeerOpenRunoNode { node_id: String },
    GitArchive { repository_url: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupKind {
    Full,
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupStatus {
    Scheduled,
    Running,
    Succeeded,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupJob {
    pub target: BackupTarget,
    pub kind: BackupKind,
    pub scheduled_at: DateTime<Utc>,
    pub status: BackupStatus,
    pub encrypted: bool,
}

impl BackupJob {
    pub fn new(target: BackupTarget, kind: BackupKind, scheduled_at: DateTime<Utc>) -> Self {
        Self {
            target,
            kind,
            scheduled_at,
            status: BackupStatus::Scheduled,
            encrypted: true,
        }
    }

    pub fn mark_running(&mut self) -> Result<()> {
        if self.status != BackupStatus::Scheduled {
            return Err(AppError::Conflict(
                "only a Scheduled job can transition to Running".into(),
            ));
        }
        self.status = BackupStatus::Running;
        Ok(())
    }

    pub fn mark_succeeded(&mut self) -> Result<()> {
        if self.status != BackupStatus::Running {
            return Err(AppError::Conflict(
                "only a Running job can transition to Succeeded".into(),
            ));
        }
        self.status = BackupStatus::Succeeded;
        Ok(())
    }

    pub fn mark_failed(&mut self, reason: impl Into<String>) {
        self.status = BackupStatus::Failed {
            reason: reason.into(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_lifecycle_happy_path() {
        let mut job = BackupJob::new(BackupTarget::LocalStorage, BackupKind::Full, Utc::now());
        job.mark_running().unwrap();
        job.mark_succeeded().unwrap();
        assert_eq!(job.status, BackupStatus::Succeeded);
    }

    #[test]
    fn cannot_succeed_without_running() {
        let mut job = BackupJob::new(
            BackupTarget::LocalStorage,
            BackupKind::Incremental,
            Utc::now(),
        );
        assert!(job.mark_succeeded().is_err());
    }

    #[test]
    fn defaults_to_encrypted() {
        let job = BackupJob::new(
            BackupTarget::S3Compatible { bucket: "b".into() },
            BackupKind::Full,
            Utc::now(),
        );
        assert!(job.encrypted);
    }
}
