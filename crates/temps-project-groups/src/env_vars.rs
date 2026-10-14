// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Environment variables set on a project group and inherited by every
//! project in it.
//!
//! Precedence at deploy time (lowest to highest): linked-service defaults,
//! group variables, the project's own variables, platform-owned variables.
//! A project can therefore always override what its group provides.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set, SqlErr, TransactionTrait,
};
use serde::Deserialize;
use temps_core::EncryptionService;
use temps_entities::{environments, project_group_env_vars};
use utoipa::ToSchema;

use crate::error::ProjectGroupError;
use crate::service::ProjectGroupService;

const MAX_KEY_LEN: usize = 255;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateGroupEnvVarRequest {
    #[schema(example = "SENTRY_ENVIRONMENT")]
    pub key: String,
    pub value: String,
    /// Write-only: the value can never be read back through the API.
    #[serde(default)]
    pub is_secret: bool,
    /// Environment slug (e.g. `production`, `staging`) this applies to.
    /// Omit to apply to every environment.
    pub environment_slug: Option<String>,
    /// Whether an unscoped variable also reaches preview environments.
    /// Defaults to `false` — opt-in, matching the per-service variable default
    /// so that sensitive group-level values don't leak into previews unless
    /// deliberately enabled.
    #[serde(default)]
    pub include_in_preview: bool,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateGroupEnvVarRequest {
    pub value: Option<String>,
    pub is_secret: Option<bool>,
    pub include_in_preview: Option<bool>,
}

fn validate_key(key: &str) -> Result<String, ProjectGroupError> {
    let key = key.trim();
    let mut chars = key.chars();
    let valid = match chars.next() {
        Some(first) => {
            (first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    };
    if !valid || key.len() > MAX_KEY_LEN {
        return Err(ProjectGroupError::Validation {
            message: format!(
                "Variable name '{key}' must start with a letter or underscore, contain only \
                 letters, digits and underscores, and be at most {MAX_KEY_LEN} characters"
            ),
        });
    }
    Ok(key.to_string())
}

fn normalize_scope(slug: Option<String>) -> Result<Option<String>, ProjectGroupError> {
    let Some(slug) = slug.map(|s| s.trim().to_lowercase()) else {
        return Ok(None);
    };
    if slug.is_empty() {
        return Ok(None);
    }
    let valid = slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid || slug.len() > 100 {
        return Err(ProjectGroupError::Validation {
            message: format!(
                "Environment '{slug}' is not a valid environment slug (lowercase letters, \
                 digits and dashes, e.g. 'production')"
            ),
        });
    }
    Ok(Some(slug))
}

pub use temps_entities::project_group_env_vars::select_inherited;

/// The decrypted variables `project_id` inherits from its group in
/// `environment`. Empty when the project is ungrouped.
pub async fn inherited_env_vars(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    project_id: i32,
    environment: &environments::Model,
) -> Result<HashMap<String, String>, ProjectGroupError> {
    let Some((group_id, selected)) =
        project_group_env_vars::inherited_for_project(db, project_id, environment)
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading variables inherited by project {project_id}"
            )))?
    else {
        return Ok(HashMap::new());
    };
    let mut resolved = HashMap::new();
    for (key, variable) in selected {
        let value = encryption.decrypt_string(&variable.value).map_err(|e| {
            ProjectGroupError::Encryption {
                group_id,
                key: key.clone(),
                reason: e.to_string(),
            }
        })?;
        resolved.insert(key, value);
    }
    Ok(resolved)
}

pub struct GroupEnvVarService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
}

impl GroupEnvVarService {
    pub fn new(db: Arc<DatabaseConnection>, encryption: Arc<EncryptionService>) -> Self {
        Self { db, encryption }
    }

    pub async fn list(
        &self,
        group_id: i32,
    ) -> Result<Vec<project_group_env_vars::Model>, ProjectGroupError> {
        project_group_env_vars::Entity::find()
            .filter(project_group_env_vars::Column::GroupId.eq(group_id))
            .order_by_asc(project_group_env_vars::Column::Key)
            .order_by_asc(project_group_env_vars::Column::EnvironmentSlug)
            .all(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "listing variables of project group {group_id}"
            )))
    }

    fn encrypt(&self, group_id: i32, key: &str, value: &str) -> Result<String, ProjectGroupError> {
        self.encryption
            .encrypt_string(value)
            .map_err(|e| ProjectGroupError::Encryption {
                group_id,
                key: key.to_string(),
                reason: e.to_string(),
            })
    }

    /// Creates a variable in a group the caller owns outright (a group the
    /// instantiation engine is assembling). Handlers that authorize the
    /// caller against the group's members use [`Self::create_checked`].
    pub async fn create(
        &self,
        group_id: i32,
        req: CreateGroupEnvVarRequest,
    ) -> Result<project_group_env_vars::Model, ProjectGroupError> {
        self.insert(self.db.as_ref(), group_id, req).await
    }

    /// Creates a variable that reaches every member of the group, for a
    /// caller whose access was checked on exactly `authorized_member_ids`.
    /// The group is locked and its members read again inside the
    /// transaction that writes; one that joined since fails with
    /// [`ProjectGroupError::MembersChanged`] and nothing is written.
    pub async fn create_checked(
        &self,
        group_id: i32,
        authorized_member_ids: &HashSet<i32>,
        req: CreateGroupEnvVarRequest,
    ) -> Result<project_group_env_vars::Model, ProjectGroupError> {
        let txn = self.begin_locked(group_id, authorized_member_ids).await?;
        let result = self.insert(&txn, group_id, req).await;
        ProjectGroupService::end_transaction(
            txn,
            result,
            format!("creation of a variable in project group {group_id}"),
        )
        .await
    }

    /// Starts a transaction that holds the group locked and has verified that
    /// its members are all in `authorized_member_ids`. The transaction is
    /// rolled back (releasing the lock) when that is not so.
    async fn begin_locked(
        &self,
        group_id: i32,
        authorized_member_ids: &HashSet<i32>,
    ) -> Result<sea_orm::DatabaseTransaction, ProjectGroupError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(ProjectGroupError::db(format!(
                "starting transaction to change the variables of project group {group_id}"
            )))?;
        match ProjectGroupService::lock_group_members(&txn, group_id, authorized_member_ids, None)
            .await
        {
            Ok(_) => Ok(txn),
            Err(error) => {
                if let Err(rollback) = txn.rollback().await {
                    tracing::warn!(
                        group_id,
                        error = %rollback,
                        "project groups: rollback failed after {error}"
                    );
                }
                Err(error)
            }
        }
    }

    async fn insert<C: ConnectionTrait>(
        &self,
        conn: &C,
        group_id: i32,
        req: CreateGroupEnvVarRequest,
    ) -> Result<project_group_env_vars::Model, ProjectGroupError> {
        let key = validate_key(&req.key)?;
        let environment_slug = normalize_scope(req.environment_slug)?;
        let encrypted = self.encrypt(group_id, &key, &req.value)?;
        let now = Utc::now();
        let scope_label = environment_slug
            .clone()
            .unwrap_or_else(|| "all environments".into());
        project_group_env_vars::ActiveModel {
            group_id: Set(group_id),
            key: Set(key.clone()),
            value: Set(encrypted),
            is_secret: Set(req.is_secret),
            environment_slug: Set(environment_slug),
            include_in_preview: Set(req.include_in_preview),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(conn)
        .await
        .map_err(|err| match err.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(_)) => ProjectGroupError::EnvVarConflict {
                group_id,
                key: key.clone(),
                scope: scope_label,
            },
            _ => ProjectGroupError::Database {
                operation: format!("creating variable '{key}' in project group {group_id}"),
                source: err,
            },
        })
    }

    async fn find<C: ConnectionTrait>(
        &self,
        conn: &C,
        group_id: i32,
        var_id: i32,
    ) -> Result<project_group_env_vars::Model, ProjectGroupError> {
        project_group_env_vars::Entity::find_by_id(var_id)
            .filter(project_group_env_vars::Column::GroupId.eq(group_id))
            .one(conn)
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading variable {var_id} of project group {group_id}"
            )))?
            .ok_or(ProjectGroupError::EnvVarNotFound { group_id, var_id })
    }

    /// Updates a variable, for a caller whose access was checked on exactly
    /// `authorized_member_ids` (see [`Self::create_checked`]).
    pub async fn update_checked(
        &self,
        group_id: i32,
        var_id: i32,
        authorized_member_ids: &HashSet<i32>,
        req: UpdateGroupEnvVarRequest,
    ) -> Result<project_group_env_vars::Model, ProjectGroupError> {
        let txn = self.begin_locked(group_id, authorized_member_ids).await?;
        let result = self.apply_update(&txn, group_id, var_id, req).await;
        ProjectGroupService::end_transaction(
            txn,
            result,
            format!("update of variable {var_id} of project group {group_id}"),
        )
        .await
    }

    async fn apply_update<C: ConnectionTrait>(
        &self,
        conn: &C,
        group_id: i32,
        var_id: i32,
        req: UpdateGroupEnvVarRequest,
    ) -> Result<project_group_env_vars::Model, ProjectGroupError> {
        let current = self.find(conn, group_id, var_id).await?;
        if current.is_secret && req.is_secret == Some(false) && req.value.is_none() {
            return Err(ProjectGroupError::Validation {
                message: format!(
                    "Variable '{}' is secret; provide a new value to make it readable",
                    current.key
                ),
            });
        }
        let mut active: project_group_env_vars::ActiveModel = current.clone().into();
        if let Some(value) = req.value.as_deref() {
            active.value = Set(self.encrypt(group_id, &current.key, value)?);
        }
        if let Some(is_secret) = req.is_secret {
            active.is_secret = Set(is_secret);
        }
        if let Some(include) = req.include_in_preview {
            active.include_in_preview = Set(include);
        }
        active.updated_at = Set(Utc::now());
        active
            .update(conn)
            .await
            .map_err(ProjectGroupError::db(format!(
                "updating variable {var_id} of project group {group_id}"
            )))
    }

    /// Keys the project defines itself (any scope), so the UI can show which
    /// inherited variables that project overrides.
    pub async fn project_variable_keys(
        &self,
        project_id: i32,
    ) -> Result<std::collections::HashSet<String>, ProjectGroupError> {
        use sea_orm::QuerySelect;
        let keys: Vec<String> = temps_entities::env_vars::Entity::find()
            .select_only()
            .column(temps_entities::env_vars::Column::Key)
            .filter(temps_entities::env_vars::Column::ProjectId.eq(project_id))
            .into_tuple()
            .all(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "listing variable names of project {project_id}"
            )))?;
        Ok(keys.into_iter().collect())
    }

    /// Deletes a variable, for a caller whose access was checked on exactly
    /// `authorized_member_ids` (see [`Self::create_checked`]).
    pub async fn delete_checked(
        &self,
        group_id: i32,
        var_id: i32,
        authorized_member_ids: &HashSet<i32>,
    ) -> Result<project_group_env_vars::Model, ProjectGroupError> {
        let txn = self.begin_locked(group_id, authorized_member_ids).await?;
        let result = async {
            let current = self.find(&txn, group_id, var_id).await?;
            project_group_env_vars::Entity::delete_by_id(var_id)
                .exec(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "deleting variable {var_id} of project group {group_id}"
                )))?;
            Ok::<_, ProjectGroupError>(current)
        }
        .await;
        ProjectGroupService::end_transaction(
            txn,
            result,
            format!("deletion of variable {var_id} of project group {group_id}"),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(id: i32, key: &str, slug: Option<&str>, preview: bool) -> project_group_env_vars::Model {
        project_group_env_vars::Model {
            id,
            group_id: 1,
            key: key.into(),
            value: String::new(),
            is_secret: false,
            environment_slug: slug.map(Into::into),
            include_in_preview: preview,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn scoped_variable_beats_unscoped_regardless_of_order() {
        for vars in [
            vec![
                var(1, "A", None, true),
                var(2, "A", Some("production"), true),
            ],
            vec![
                var(2, "A", Some("production"), true),
                var(1, "A", None, true),
            ],
        ] {
            let selected = select_inherited(vars, "production", false);
            assert_eq!(selected["A"].id, 2);
        }
    }

    #[test]
    fn other_environments_scope_is_ignored() {
        let selected = select_inherited(
            vec![var(1, "A", Some("staging"), true), var(2, "B", None, true)],
            "production",
            false,
        );
        assert!(!selected.contains_key("A"));
        assert!(selected.contains_key("B"));
    }

    #[test]
    fn previews_only_get_unscoped_vars_marked_for_preview() {
        let selected = select_inherited(
            vec![var(1, "A", None, true), var(2, "B", None, false)],
            "pr-42",
            true,
        );
        assert!(selected.contains_key("A"));
        assert!(!selected.contains_key("B"));
    }

    #[test]
    fn key_and_scope_validation() {
        assert_eq!(validate_key(" API_URL ").unwrap(), "API_URL");
        for bad in ["", "1ABC", "A-B", "A B"] {
            assert!(validate_key(bad).is_err(), "'{bad}' should be rejected");
        }
        assert_eq!(
            normalize_scope(Some(" Production ".into()))
                .unwrap()
                .as_deref(),
            Some("production")
        );
        assert_eq!(normalize_scope(Some("".into())).unwrap(), None);
        assert!(normalize_scope(Some("prod/x".into())).is_err());
    }

    // Database-backed: skips when no container runtime is available.
    use crate::service::{CreateProjectGroupRequest, ProjectGroupService};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    use temps_database::test_utils::TestDatabase;

    async fn insert_project_with_env(
        conn: &impl ConnectionTrait,
        id: i32,
        env_slug: &str,
        preview: bool,
    ) -> environments::Model {
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO projects (id, name, repo_name, repo_owner, directory, main_branch, preset,
                 created_at, updated_at, slug, is_deleted, is_public_repo, attack_mode,
                 enable_preview_environments, source_type)
               VALUES ($1, 'svc', 'r', 'o', '.', 'main', 'nextjs', NOW(), NOW(), $2, FALSE, TRUE,
                 FALSE, FALSE, 'git')"#,
            [id.into(), format!("svc-{id}").into()],
        ))
        .await
        .expect("insert project");
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, is_preview, created_at, updated_at)
               VALUES ($1, $1, $2, $2, '[]'::json, $3, $4, NOW(), NOW())"#,
            [env_slug.into(), format!("svc-{id}-{env_slug}").into(), id.into(), preview.into()],
        ))
        .await
        .expect("insert environment");
        environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(id))
            .one(conn)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn inherited_vars_follow_membership_scope_and_encryption() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Docker/TimescaleDB unavailable, skipping: {e}");
                return;
            }
        };
        let conn = db.connection_arc();
        let prod = insert_project_with_env(conn.as_ref(), 9701, "production", false).await;
        let preview = insert_project_with_env(conn.as_ref(), 9702, "pr-7", true).await;
        let loner = insert_project_with_env(conn.as_ref(), 9703, "production", false).await;
        let encryption = Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );

        let groups = ProjectGroupService::new(conn.clone());
        let group = groups
            .create_group(CreateProjectGroupRequest {
                name: "Shop".into(),
                slug: None,
                description: None,
                project_ids: vec![9701, 9702],
            })
            .await
            .unwrap();
        let svc = GroupEnvVarService::new(conn.clone(), encryption.clone());
        let add =
            |key: &str, value: &str, slug: Option<&str>, preview: bool| CreateGroupEnvVarRequest {
                key: key.into(),
                value: value.into(),
                is_secret: false,
                environment_slug: slug.map(Into::into),
                include_in_preview: preview,
            };
        let stored = svc
            .create(
                group.group.id,
                add("API_URL", "https://api.example.test", None, true),
            )
            .await
            .unwrap();
        assert_ne!(
            stored.value, "https://api.example.test",
            "values must be encrypted at rest"
        );
        svc.create(
            group.group.id,
            add(
                "API_URL",
                "https://prod.example.test",
                Some("production"),
                true,
            ),
        )
        .await
        .unwrap();
        svc.create(group.group.id, add("ANALYTICS", "on", None, false))
            .await
            .unwrap();
        let dup = svc
            .create(
                group.group.id,
                add("API_URL", "x", Some("production"), true),
            )
            .await
            .unwrap_err();
        assert!(matches!(dup, ProjectGroupError::EnvVarConflict { .. }));

        let on_prod = inherited_env_vars(&conn, &encryption, 9701, &prod)
            .await
            .unwrap();
        assert_eq!(on_prod["API_URL"], "https://prod.example.test");
        assert_eq!(on_prod["ANALYTICS"], "on");

        let on_preview = inherited_env_vars(&conn, &encryption, 9702, &preview)
            .await
            .unwrap();
        assert_eq!(on_preview["API_URL"], "https://api.example.test");
        assert!(!on_preview.contains_key("ANALYTICS"));

        assert!(inherited_env_vars(&conn, &encryption, 9703, &loner)
            .await
            .unwrap()
            .is_empty());
    }

    fn test_encryption() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    fn plain(key: &str, value: &str) -> CreateGroupEnvVarRequest {
        CreateGroupEnvVarRequest {
            key: key.into(),
            value: value.into(),
            is_secret: false,
            environment_slug: None,
            include_in_preview: false,
        }
    }

    #[tokio::test]
    async fn checked_writes_refuse_a_member_that_joined_after_authorization() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Docker/TimescaleDB unavailable, skipping: {e}");
                return;
            }
        };
        let conn = db.connection_arc();
        for id in [9711, 9712, 9713] {
            insert_project_with_env(conn.as_ref(), id, "production", false).await;
        }
        let groups = ProjectGroupService::new(conn.clone());
        let group = groups
            .create_group(CreateProjectGroupRequest {
                name: "Checked".into(),
                slug: None,
                description: None,
                project_ids: vec![9711, 9712],
            })
            .await
            .unwrap()
            .group
            .id;
        let svc = GroupEnvVarService::new(conn.clone(), test_encryption());
        let checked: HashSet<i32> = [9711, 9712].into_iter().collect();

        // Unchanged members: every write goes through.
        let created = svc
            .create_checked(group, &checked, plain("FIRST", "one"))
            .await
            .unwrap();
        let updated = svc
            .update_checked(
                group,
                created.id,
                &checked,
                UpdateGroupEnvVarRequest {
                    value: Some("two".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_ne!(updated.value, created.value);

        // A restricted project joins after the caller's access was checked.
        groups.add_project(group, 9713).await.unwrap();
        let before = svc.list(group).await.unwrap();

        let err = svc
            .create_checked(group, &checked, plain("SECOND", "x"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProjectGroupError::MembersChanged { group_id, ref project_ids } if group_id == group && *project_ids == vec![9713]),
            "got {err:?}"
        );
        let err = svc
            .update_checked(
                group,
                created.id,
                &checked,
                UpdateGroupEnvVarRequest {
                    value: Some("three".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectGroupError::MembersChanged { .. }));
        let err = svc
            .delete_checked(group, created.id, &checked)
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectGroupError::MembersChanged { .. }));

        // Nothing was written.
        let after = svc.list(group).await.unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before[0].value, after[0].value);
        assert_eq!(before[0].updated_at, after[0].updated_at);

        // Authorized on the members it has now, the same writes succeed.
        let checked: HashSet<i32> = [9711, 9712, 9713].into_iter().collect();
        svc.create_checked(group, &checked, plain("SECOND", "x"))
            .await
            .unwrap();
        let deleted = svc
            .delete_checked(group, created.id, &checked)
            .await
            .unwrap();
        assert_eq!(deleted.key, "FIRST");
        assert_eq!(svc.list(group).await.unwrap().len(), 1);

        // The error kinds the handlers relied on are unchanged.
        let err = svc
            .delete_checked(group, 424242, &checked)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProjectGroupError::EnvVarNotFound { var_id: 424242, .. }
        ));
        let err = svc
            .create_checked(999_999, &HashSet::new(), plain("X", "y"))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProjectGroupError::NotFound { group_id: 999_999 }
        ));
    }

    /// The member is joining in a transaction that has not committed when the
    /// write starts: the write waits for it, then sees the member and refuses.
    #[tokio::test]
    async fn checked_create_waits_for_a_member_that_is_joining() {
        use sea_orm::TransactionTrait;
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Docker/TimescaleDB unavailable, skipping: {e}");
                return;
            }
        };
        let conn = db.connection_arc();
        for id in [9721, 9722] {
            insert_project_with_env(conn.as_ref(), id, "production", false).await;
        }
        let groups = ProjectGroupService::new(conn.clone());
        let group = groups
            .create_group(CreateProjectGroupRequest {
                name: "Racing".into(),
                slug: None,
                description: None,
                project_ids: vec![9721],
            })
            .await
            .unwrap()
            .group
            .id;
        let svc = Arc::new(GroupEnvVarService::new(conn.clone(), test_encryption()));

        let joining = conn.begin().await.unwrap();
        ProjectGroupService::lock_projects(&joining, &[9722])
            .await
            .unwrap();
        ProjectGroupService::place_membership(&joining, group, 9722, None, None)
            .await
            .unwrap();

        let write = {
            let svc = svc.clone();
            let checked: HashSet<i32> = [9721].into_iter().collect();
            tokio::spawn(async move {
                svc.create_checked(group, &checked, plain("LATE", "x"))
                    .await
                    .map(|v| v.id)
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !write.is_finished(),
            "the write must wait for the join instead of reading around it"
        );
        joining.commit().await.unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_secs(20), write)
            .await
            .expect("write still blocked after the join committed")
            .unwrap();
        assert!(
            matches!(result, Err(ProjectGroupError::MembersChanged { ref project_ids, .. }) if *project_ids == vec![9722]),
            "got {result:?}"
        );
        assert!(svc.list(group).await.unwrap().is_empty());
    }

    // ── include_in_preview default-false (item R2.1) ────────────────────────

    #[test]
    fn unscoped_group_var_does_not_reach_preview_without_include_in_preview() {
        // An unscoped group var with include_in_preview = FALSE must NOT be
        // selected for a preview environment. This verifies the default is
        // opt-in (false), matching the per-service variable default.
        let vars = vec![
            var(1, "SENTRY_DSN", None, false), // include_in_preview = false
            var(2, "PUBLIC_KEY", None, true),  // include_in_preview = true
        ];
        let selected = select_inherited(vars, "pr-42", true); // preview env
        assert!(
            !selected.contains_key("SENTRY_DSN"),
            "SENTRY_DSN (include_in_preview=false) must not reach preview"
        );
        assert!(
            selected.contains_key("PUBLIC_KEY"),
            "PUBLIC_KEY (include_in_preview=true) must reach preview"
        );
    }

    #[test]
    fn create_group_env_var_request_defaults_include_in_preview_to_false() {
        // Verify that deserializing a request without `include_in_preview`
        // produces false (the correct opt-in default).
        let req: CreateGroupEnvVarRequest = serde_json::from_str(r#"{"key":"FOO","value":"bar"}"#)
            .expect("minimal request must deserialize");
        assert!(
            !req.include_in_preview,
            "include_in_preview must default to false (opt-in)"
        );
    }
}
