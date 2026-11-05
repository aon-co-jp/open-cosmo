//! Role-Based Access Control (Cosmo Enterprise parity, shipped as OSS).
//!
//! A [`RbacPolicy`] maps role names to sets of `(Resource, Action)` grants.
//! JWT callers carry role names in `Claims::roles`; API-key callers can be
//! assigned roles out of band. `is_allowed` / `check` answer whether any of
//! the caller's roles grants the requested action on the resource.

use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Things a caller can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    /// Federated graph composition and status.
    Federation,
    /// Schema registry entries and history.
    Schema,
    /// Persisted queries / trusted documents.
    PersistedQuery,
    /// AI routing configuration and calls.
    AiRouting,
    /// `/api/db/*` key-value access.
    Database,
    /// Namespaces / multi-graph management (Phase C).
    Namespace,
    /// Security administration: keys, roles, rate limits.
    Admin,
}

/// What the caller wants to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
    Publish,
    Delete,
}

/// A named set of grants. `all = true` short-circuits every check
/// (the built-in `admin` role).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub all: bool,
    grants: HashSet<(Resource, Action)>,
}

impl Role {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            all: false,
            grants: HashSet::new(),
        }
    }

    #[must_use]
    pub fn grant(mut self, resource: Resource, action: Action) -> Self {
        self.grants.insert((resource, action));
        self
    }

    #[must_use]
    pub fn grant_all(mut self) -> Self {
        self.all = true;
        self
    }

    pub fn allows(&self, resource: Resource, action: Action) -> bool {
        self.all || self.grants.contains(&(resource, action))
    }
}

/// The complete role table for a deployment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RbacPolicy {
    roles: HashMap<String, Role>,
}

impl RbacPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Built-in roles matching Cosmo Studio conventions:
    ///
    /// | Role        | Grants                                             |
    /// |-------------|----------------------------------------------------|
    /// | `admin`     | everything                                         |
    /// | `developer` | read everything + write/publish schemas, federation, persisted queries |
    /// | `viewer`    | read-only                                          |
    pub fn builtin() -> Self {
        let read_all = [
            Resource::Federation,
            Resource::Schema,
            Resource::PersistedQuery,
            Resource::AiRouting,
            Resource::Database,
            Resource::Namespace,
        ];

        let mut viewer = Role::new("viewer");
        let mut developer = Role::new("developer");
        for r in read_all {
            viewer = viewer.grant(r, Action::Read);
            developer = developer.grant(r, Action::Read);
        }
        developer = developer
            .grant(Resource::Schema, Action::Write)
            .grant(Resource::Schema, Action::Publish)
            .grant(Resource::Federation, Action::Write)
            .grant(Resource::Federation, Action::Publish)
            .grant(Resource::PersistedQuery, Action::Write)
            .grant(Resource::Database, Action::Write);

        let mut policy = Self::new();
        policy.add_role(Role::new("admin").grant_all());
        policy.add_role(developer);
        policy.add_role(viewer);
        policy
    }

    pub fn add_role(&mut self, role: Role) {
        self.roles.insert(role.name.clone(), role);
    }

    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles.get(name)
    }

    /// `true` if any of `role_names` grants `action` on `resource`.
    /// Unknown role names are ignored (deny by default).
    pub fn is_allowed(&self, role_names: &[String], resource: Resource, action: Action) -> bool {
        role_names
            .iter()
            .filter_map(|n| self.roles.get(n))
            .any(|r| r.allows(resource, action))
    }

    /// Like [`Self::is_allowed`] but returns a descriptive error for the
    /// HTTP 403 path.
    pub fn check(&self, role_names: &[String], resource: Resource, action: Action) -> Result<()> {
        if self.is_allowed(role_names, resource, action) {
            Ok(())
        } else {
            Err(AppError::Validation(format!(
                "forbidden: roles {role_names:?} lack {action:?} on {resource:?}"
            )))
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn admin_can_do_everything() {
        let p = RbacPolicy::builtin();
        assert!(p.is_allowed(&roles(&["admin"]), Resource::Admin, Action::Delete));
        assert!(p.is_allowed(&roles(&["admin"]), Resource::Schema, Action::Publish));
    }

    #[test]
    fn viewer_is_read_only() {
        let p = RbacPolicy::builtin();
        let v = roles(&["viewer"]);
        assert!(p.is_allowed(&v, Resource::Schema, Action::Read));
        assert!(!p.is_allowed(&v, Resource::Schema, Action::Write));
        assert!(!p.is_allowed(&v, Resource::Admin, Action::Read));
    }

    #[test]
    fn developer_writes_schemas_but_not_admin() {
        let p = RbacPolicy::builtin();
        let d = roles(&["developer"]);
        assert!(p.is_allowed(&d, Resource::Schema, Action::Publish));
        assert!(p.is_allowed(&d, Resource::PersistedQuery, Action::Write));
        assert!(!p.is_allowed(&d, Resource::Admin, Action::Write));
        assert!(!p.is_allowed(&d, Resource::Schema, Action::Delete));
    }

    #[test]
    fn unknown_roles_are_denied() {
        let p = RbacPolicy::builtin();
        assert!(!p.is_allowed(&roles(&["ghost"]), Resource::Schema, Action::Read));
        assert!(!p.is_allowed(&[], Resource::Schema, Action::Read));
    }

    #[test]
    fn multiple_roles_are_unioned() {
        let p = RbacPolicy::builtin();
        let combo = roles(&["viewer", "developer"]);
        assert!(p.is_allowed(&combo, Resource::Schema, Action::Write));
    }

    #[test]
    fn custom_role_and_check_error_message() {
        let mut p = RbacPolicy::new();
        p.add_role(Role::new("pq-bot").grant(Resource::PersistedQuery, Action::Write));
        assert!(p
            .check(&roles(&["pq-bot"]), Resource::PersistedQuery, Action::Write)
            .is_ok());
        let err = p
            .check(&roles(&["pq-bot"]), Resource::Schema, Action::Read)
            .unwrap_err();
        assert!(err.to_string().contains("forbidden"));
    }
}
