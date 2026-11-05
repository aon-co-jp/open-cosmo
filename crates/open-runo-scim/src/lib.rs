//! `open-runo-scim`: SCIM 2.0 user provisioning core (RFC 7643 / RFC 7644).
//!
//! Cosmo offers SCIM only on Enterprise; open-runo ships it as OSS.
//! IdPs (Entra ID, Okta, Keycloak) push user lifecycle events to
//! `/scim/v2/Users`; this crate implements the resource model and storage,
//! `open-runo-router` exposes the HTTP surface.
//!
//! Scope: the `User` resource with the attributes IdPs actually send
//! (userName, displayName, active, emails, roles). Groups land with the
//! multi-graph/namespace work (Phase C).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use chrono::{DateTime, Utc};
use open_runo_core::{AppError, Result};
use open_runo_db::DbBackend;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Logical table for SCIM users (DUAL routing: PostgreSQL by default).
pub const TABLE: &str = "scim_users";

/// SCIM `User` resource schema URI (RFC 7643 §4.1).
pub const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";

/// RFC 7643 §4.1.2 `emails` entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Email {
    pub value: String,
    #[serde(default)]
    pub primary: bool,
}

/// SCIM 2.0 `User` (the subset real IdPs provision with).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(default = "user_schemas")]
    pub schemas: Vec<String>,
    pub id: String,
    #[serde(rename = "userName")]
    pub user_name: String,
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    #[serde(default = "default_active")]
    pub active: bool,
    #[serde(default)]
    pub emails: Vec<Email>,
    /// open-runo extension: RBAC role names for `open-runo-security::rbac`.
    #[serde(default)]
    pub roles: Vec<String>,
    pub meta: Meta,
}

/// RFC 7643 §3.1 common `meta` attribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "resourceType")]
    pub resource_type: String,
    pub created: DateTime<Utc>,
    #[serde(rename = "lastModified")]
    pub last_modified: DateTime<Utc>,
}

fn user_schemas() -> Vec<String> {
    vec![USER_SCHEMA.to_string()]
}

fn default_active() -> bool {
    true
}

/// Attributes accepted on create/replace (id/meta are server-controlled).
#[derive(Debug, Clone, Deserialize)]
pub struct UserInput {
    #[serde(rename = "userName")]
    pub user_name: String,
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    #[serde(default = "default_active")]
    pub active: bool,
    #[serde(default)]
    pub emails: Vec<Email>,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// RFC 7644 §3.4.2 list response envelope.
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub schemas: Vec<String>,
    #[serde(rename = "totalResults")]
    pub total_results: usize,
    #[serde(rename = "Resources")]
    pub resources: Vec<User>,
}

/// SCIM user store on top of any [`DbBackend`].
#[derive(Debug, Clone)]
pub struct ScimUserStore {
    db: Arc<dyn DbBackend>,
}

impl ScimUserStore {
    pub fn new(db: Arc<dyn DbBackend>) -> Self {
        Self { db }
    }

    /// Create a user (POST /scim/v2/Users). Rejects duplicate userName.
    pub async fn create(&self, input: UserInput) -> Result<User> {
        if input.user_name.trim().is_empty() {
            return Err(AppError::Validation("userName must not be empty".into()));
        }
        if self.find_by_user_name(&input.user_name).await?.is_some() {
            return Err(AppError::Validation(format!(
                "userName already exists: {}",
                input.user_name
            )));
        }

        let now = Utc::now();
        let user = User {
            schemas: user_schemas(),
            id: uuid::Uuid::new_v4().to_string(),
            user_name: input.user_name,
            display_name: input.display_name,
            active: input.active,
            emails: input.emails,
            roles: input.roles,
            meta: Meta {
                resource_type: "User".into(),
                created: now,
                last_modified: now,
            },
        };
        self.put(&user).await?;
        Ok(user)
    }

    /// Fetch by server-assigned id (GET /scim/v2/Users/:id).
    pub async fn get(&self, id: &str) -> Result<Option<User>> {
        match self.db.get(TABLE, id).await? {
            None => Ok(None),
            Some(raw) => sonic_rs::from_str(&raw)
                .map(Some)
                .map_err(|e| AppError::Internal(format!("corrupt SCIM record: {e}"))),
        }
    }

    /// Replace all mutable attributes (PUT /scim/v2/Users/:id).
    pub async fn replace(&self, id: &str, input: UserInput) -> Result<User> {
        let existing = self
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("user not found: {id}")))?;

        // userName change must not collide with another user.
        if input.user_name != existing.user_name {
            if let Some(other) = self.find_by_user_name(&input.user_name).await? {
                if other.id != id {
                    return Err(AppError::Validation(format!(
                        "userName already exists: {}",
                        input.user_name
                    )));
                }
            }
        }

        let user = User {
            schemas: user_schemas(),
            id: existing.id,
            user_name: input.user_name,
            display_name: input.display_name,
            active: input.active,
            emails: input.emails,
            roles: input.roles,
            meta: Meta {
                last_modified: Utc::now(),
                ..existing.meta
            },
        };
        self.put(&user).await?;
        Ok(user)
    }

    /// Deprovision: IdPs usually PATCH `active: false`, but hard delete is
    /// also part of RFC 7644 (DELETE /scim/v2/Users/:id).
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.db.delete(TABLE, id).await
    }

    /// List all users (GET /scim/v2/Users), optional exact userName filter
    /// (`?filter=userName eq "alice"` — the only filter IdPs send in practice).
    pub async fn list(&self, user_name_filter: Option<&str>) -> Result<Vec<User>> {
        let mut users = Vec::new();
        for rec in self.db.list(TABLE).await? {
            let user: User = sonic_rs::from_str(&rec.value)
                .map_err(|e| AppError::Internal(format!("corrupt SCIM record: {e}")))?;
            if user_name_filter.is_none_or(|f| user.user_name == f) {
                users.push(user);
            }
        }
        users.sort_by(|a, b| a.user_name.cmp(&b.user_name));
        Ok(users)
    }

    /// Parse the RFC 7644 filter grammar subset IdPs use:
    /// `userName eq "value"`.
    pub fn parse_user_name_filter(filter: &str) -> Option<String> {
        let rest = filter.trim().strip_prefix("userName")?.trim_start();
        let rest = rest.strip_prefix("eq")?.trim_start();
        let rest = rest.strip_prefix('"')?;
        rest.strip_suffix('"').map(str::to_string)
    }

    async fn find_by_user_name(&self, user_name: &str) -> Result<Option<User>> {
        Ok(self.list(Some(user_name)).await?.into_iter().next())
    }

    async fn put(&self, user: &User) -> Result<()> {
        let json = serde_json::to_string(user)
            .map_err(|e| AppError::Internal(format!("serialize SCIM user: {e}")))?;
        self.db.put(TABLE, &user.id, &json).await
    }
}

// ── Groups (RFC 7643 §4.2) ──────────────────────────────────────────────────

/// Logical table for SCIM groups.
pub const GROUP_TABLE: &str = "scim_groups";

/// SCIM `Group` resource schema URI.
pub const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";

/// RFC 7643 §4.2 `members` entry (references a User id).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupMember {
    /// The member User's `id`.
    pub value: String,
    #[serde(default)]
    pub display: Option<String>,
}

/// SCIM 2.0 `Group`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    #[serde(default = "group_schemas")]
    pub schemas: Vec<String>,
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub members: Vec<GroupMember>,
    pub meta: Meta,
}

fn group_schemas() -> Vec<String> {
    vec![GROUP_SCHEMA.to_string()]
}

/// Attributes accepted on create/replace.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupInput {
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub members: Vec<GroupMember>,
}

/// RFC 7644 list envelope for groups.
#[derive(Debug, Serialize)]
pub struct GroupListResponse {
    pub schemas: Vec<String>,
    #[serde(rename = "totalResults")]
    pub total_results: usize,
    #[serde(rename = "Resources")]
    pub resources: Vec<Group>,
}

/// SCIM group store on top of any [`DbBackend`].
#[derive(Debug, Clone)]
pub struct ScimGroupStore {
    db: Arc<dyn DbBackend>,
}

impl ScimGroupStore {
    pub fn new(db: Arc<dyn DbBackend>) -> Self {
        Self { db }
    }

    /// Create a group. Rejects duplicate displayName.
    pub async fn create(&self, input: GroupInput) -> Result<Group> {
        if input.display_name.trim().is_empty() {
            return Err(AppError::Validation("displayName must not be empty".into()));
        }
        if self
            .list(Some(&input.display_name))
            .await?
            .into_iter()
            .next()
            .is_some()
        {
            return Err(AppError::Validation(format!(
                "displayName already exists: {}",
                input.display_name
            )));
        }

        let now = Utc::now();
        let group = Group {
            schemas: group_schemas(),
            id: uuid::Uuid::new_v4().to_string(),
            display_name: input.display_name,
            members: input.members,
            meta: Meta {
                resource_type: "Group".into(),
                created: now,
                last_modified: now,
            },
        };
        self.put(&group).await?;
        Ok(group)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Group>> {
        match self.db.get(GROUP_TABLE, id).await? {
            None => Ok(None),
            Some(raw) => sonic_rs::from_str(&raw)
                .map(Some)
                .map_err(|e| AppError::Internal(format!("corrupt SCIM group record: {e}"))),
        }
    }

    /// Replace displayName + full member list (how IdPs sync group membership).
    pub async fn replace(&self, id: &str, input: GroupInput) -> Result<Group> {
        let existing = self
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("group not found: {id}")))?;

        let group = Group {
            schemas: group_schemas(),
            id: existing.id,
            display_name: input.display_name,
            members: input.members,
            meta: Meta {
                last_modified: Utc::now(),
                ..existing.meta
            },
        };
        self.put(&group).await?;
        Ok(group)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        self.db.delete(GROUP_TABLE, id).await
    }

    /// List groups, optional exact displayName filter
    /// (`?filter=displayName eq "engineering"`).
    pub async fn list(&self, display_name_filter: Option<&str>) -> Result<Vec<Group>> {
        let mut groups = Vec::new();
        for rec in self.db.list(GROUP_TABLE).await? {
            let group: Group = sonic_rs::from_str(&rec.value)
                .map_err(|e| AppError::Internal(format!("corrupt SCIM group record: {e}")))?;
            if display_name_filter.is_none_or(|f| group.display_name == f) {
                groups.push(group);
            }
        }
        groups.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        Ok(groups)
    }

    /// Parse `displayName eq "value"` (the filter IdPs send for groups).
    pub fn parse_display_name_filter(filter: &str) -> Option<String> {
        let rest = filter.trim().strip_prefix("displayName")?.trim_start();
        let rest = rest.strip_prefix("eq")?.trim_start();
        let rest = rest.strip_prefix('"')?;
        rest.strip_suffix('"').map(str::to_string)
    }

    async fn put(&self, group: &Group) -> Result<()> {
        let json = serde_json::to_string(group)
            .map_err(|e| AppError::Internal(format!("serialize SCIM group: {e}")))?;
        self.db.put(GROUP_TABLE, &group.id, &json).await
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_db::InMemoryBackend;

    fn store() -> ScimUserStore {
        ScimUserStore::new(Arc::new(InMemoryBackend::new()))
    }

    fn alice() -> UserInput {
        UserInput {
            user_name: "alice@example.com".into(),
            display_name: Some("Alice".into()),
            active: true,
            emails: vec![Email {
                value: "alice@example.com".into(),
                primary: true,
            }],
            roles: vec!["developer".into()],
        }
    }

    #[tokio::test]
    async fn create_get_roundtrip_with_scim_meta() {
        let s = store();
        let created = s.create(alice()).await.unwrap();
        assert_eq!(created.schemas, vec![USER_SCHEMA.to_string()]);
        assert_eq!(created.meta.resource_type, "User");

        let fetched = s.get(&created.id).await.unwrap().unwrap();
        assert_eq!(fetched.user_name, "alice@example.com");
        assert_eq!(fetched.roles, vec!["developer".to_string()]);
    }

    #[tokio::test]
    async fn duplicate_user_name_is_rejected() {
        let s = store();
        s.create(alice()).await.unwrap();
        assert!(s.create(alice()).await.is_err());
    }

    #[tokio::test]
    async fn replace_updates_attributes_and_last_modified() {
        let s = store();
        let created = s.create(alice()).await.unwrap();

        let mut input = alice();
        input.active = false; // deprovision
        input.roles = vec!["viewer".into()];
        let updated = s.replace(&created.id, input).await.unwrap();

        assert!(!updated.active);
        assert_eq!(updated.roles, vec!["viewer".to_string()]);
        assert_eq!(updated.meta.created, created.meta.created);
        assert!(updated.meta.last_modified >= created.meta.last_modified);
    }

    #[tokio::test]
    async fn replace_unknown_id_is_not_found() {
        let s = store();
        assert!(s.replace("missing", alice()).await.is_err());
    }

    #[tokio::test]
    async fn delete_removes_user() {
        let s = store();
        let created = s.create(alice()).await.unwrap();
        s.delete(&created.id).await.unwrap();
        assert!(s.get(&created.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_filters_by_user_name() {
        let s = store();
        s.create(alice()).await.unwrap();
        let mut bob = alice();
        bob.user_name = "bob@example.com".into();
        s.create(bob).await.unwrap();

        assert_eq!(s.list(None).await.unwrap().len(), 2);
        let filtered = s.list(Some("bob@example.com")).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].user_name, "bob@example.com");
    }

    #[test]
    fn filter_parser_handles_idp_grammar() {
        assert_eq!(
            ScimUserStore::parse_user_name_filter(r#"userName eq "alice@example.com""#),
            Some("alice@example.com".to_string())
        );
        assert_eq!(
            ScimUserStore::parse_user_name_filter("active eq true"),
            None
        );
    }

    // ── Groups ─────────────────────────────────────────────────────────

    fn group_store() -> ScimGroupStore {
        ScimGroupStore::new(Arc::new(InMemoryBackend::new()))
    }

    fn engineering() -> GroupInput {
        GroupInput {
            display_name: "engineering".into(),
            members: vec![GroupMember {
                value: "user-1".into(),
                display: Some("Alice".into()),
            }],
        }
    }

    #[tokio::test]
    async fn group_create_get_roundtrip() {
        let s = group_store();
        let created = s.create(engineering()).await.unwrap();
        assert_eq!(created.schemas, vec![GROUP_SCHEMA.to_string()]);
        assert_eq!(created.meta.resource_type, "Group");

        let fetched = s.get(&created.id).await.unwrap().unwrap();
        assert_eq!(fetched.display_name, "engineering");
        assert_eq!(fetched.members.len(), 1);
    }

    #[tokio::test]
    async fn group_duplicate_display_name_rejected() {
        let s = group_store();
        s.create(engineering()).await.unwrap();
        assert!(s.create(engineering()).await.is_err());
    }

    #[tokio::test]
    async fn group_replace_syncs_membership() {
        let s = group_store();
        let created = s.create(engineering()).await.unwrap();

        let updated = s
            .replace(
                &created.id,
                GroupInput {
                    display_name: "engineering".into(),
                    members: vec![
                        GroupMember {
                            value: "user-1".into(),
                            display: None,
                        },
                        GroupMember {
                            value: "user-2".into(),
                            display: None,
                        },
                    ],
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.members.len(), 2);
        assert_eq!(updated.meta.created, created.meta.created);
    }

    #[tokio::test]
    async fn group_delete_and_list_filter() {
        let s = group_store();
        let g1 = s.create(engineering()).await.unwrap();
        s.create(GroupInput {
            display_name: "sales".into(),
            members: vec![],
        })
        .await
        .unwrap();

        let filtered = s.list(Some("engineering")).await.unwrap();
        assert_eq!(filtered.len(), 1);

        s.delete(&g1.id).await.unwrap();
        assert!(s.get(&g1.id).await.unwrap().is_none());
        assert_eq!(s.list(None).await.unwrap().len(), 1);
    }

    #[test]
    fn group_filter_parser() {
        assert_eq!(
            ScimGroupStore::parse_display_name_filter(r#"displayName eq "engineering""#),
            Some("engineering".to_string())
        );
        assert_eq!(
            ScimGroupStore::parse_display_name_filter("id eq \"x\""),
            None
        );
    }
}
