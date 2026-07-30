//! Admin-defined, named permission sets that can be assigned to a team
//! membership in place of one of the four fixed [`TeamRole`]s.
//!
//! Privilege-escalation ceiling enforcement is deliberately NOT done in
//! this service — it lives in the handler layer
//! ([`crate::handlers::enforce_permission_ceiling`]), matching the pattern
//! established in `temps_auth::apikey_handler`'s
//! `enforce_custom_permission_ceiling`. The service only validates that
//! permission strings are well-formed `Permission` variants; it has no
//! `AuthContext` to check a ceiling against.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_auth::permissions::Permission;
use temps_entities::{custom_role_permissions, custom_roles, team_members};
use utoipa::ToSchema;

use crate::error::TeamError;
use crate::service::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateCustomRoleRequest {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    /// `Permission::to_string()` values, e.g. `"deployments:write"`.
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateCustomRoleRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    /// When `Some`, replaces the role's entire permission set.
    pub permissions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CustomRoleResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub created_by: i32,
    pub permissions: Vec<String>,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CustomRoleListResponse {
    pub roles: Vec<CustomRoleResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[async_trait]
pub trait CustomRoleService: Send + Sync {
    async fn create_custom_role(
        &self,
        actor_user_id: i32,
        req: CreateCustomRoleRequest,
    ) -> Result<(custom_roles::Model, Vec<String>), TeamError>;

    async fn list_custom_roles(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<(custom_roles::Model, Vec<String>)>, u64), TeamError>;

    async fn get_custom_role(
        &self,
        role_id: i32,
    ) -> Result<(custom_roles::Model, Vec<String>), TeamError>;

    /// Changing a role's permission set changes what's resolved for every
    /// member holding its `custom_role_id`, without any row the checker's
    /// `invalidate_user`/`invalidate_project` watch actually changing.
    /// This trait has no reference to the checker (it would create a
    /// circular `Arc` dependency — the checker needs `CustomRoleService`
    /// to resolve `custom_role_id` grants), so **the caller is responsible
    /// for calling [`TeamProjectAccessChecker::invalidate_all`](crate::TeamProjectAccessChecker::invalidate_all)
    /// after this returns `Ok`** — the HTTP handler does this today. Any
    /// new caller must do the same or reintroduce the staleness window.
    async fn update_custom_role(
        &self,
        role_id: i32,
        req: UpdateCustomRoleRequest,
    ) -> Result<(custom_roles::Model, Vec<String>), TeamError>;

    /// Same caller-responsibility note as [`Self::update_custom_role`]:
    /// deleting a role reverts every member holding its `custom_role_id`
    /// to their fixed `role` column (DB-level `ON DELETE SET NULL`), which
    /// this trait cannot invalidate itself.
    async fn delete_custom_role(&self, role_id: i32) -> Result<(), TeamError>;

    /// Precedence resolver: `custom_role_id` wins when non-null, resolving
    /// to that role's `Permission` set. Returns `None` when the member has
    /// no custom role assigned — the caller falls back to the fixed `role`
    /// column via [`fixed_role_permissions`](crate::fixed_role_permissions).
    /// This is the ONLY place the precedence rule is encoded; callers must
    /// not re-implement the nullable-check inline.
    async fn effective_permissions(
        &self,
        member: &team_members::Model,
    ) -> Result<Option<Vec<Permission>>, TeamError>;
}

/// Deliberately holds no reference to `TeamProjectAccessChecker` — that
/// would create a circular `Arc` dependency, since the checker holds a
/// `CustomRoleService` to resolve `custom_role_id` grants. See the
/// `update_custom_role`/`delete_custom_role` trait docs for the
/// caller-responsibility invalidation contract this implies.
pub struct DefaultCustomRoleService {
    db: Arc<DatabaseConnection>,
}

impl DefaultCustomRoleService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    fn validate_slug(slug: &str) -> Result<(), TeamError> {
        if slug.is_empty() {
            return Err(TeamError::Validation {
                message: "Custom role slug cannot be empty".into(),
            });
        }
        if slug.len() > 64 {
            return Err(TeamError::Validation {
                message: format!("Custom role slug must be ≤ 64 chars (got {})", slug.len()),
            });
        }
        if !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(TeamError::Validation {
                message: "Custom role slug must match [a-z0-9-]+".into(),
            });
        }
        Ok(())
    }

    fn validate_name(name: &str) -> Result<(), TeamError> {
        if name.trim().is_empty() {
            return Err(TeamError::Validation {
                message: "Custom role name cannot be empty".into(),
            });
        }
        if name.len() > 255 {
            return Err(TeamError::Validation {
                message: format!("Custom role name must be ≤ 255 chars (got {})", name.len()),
            });
        }
        Ok(())
    }

    /// Validates every string parses as a `Permission` and that the set is
    /// non-empty. Does NOT check a privilege ceiling — see module docs.
    fn validate_permissions(permissions: &[String]) -> Result<(), TeamError> {
        if permissions.is_empty() {
            return Err(TeamError::Validation {
                message: "Custom role requires at least one permission".into(),
            });
        }
        for perm_str in permissions {
            if Permission::from_str(perm_str).is_none() {
                return Err(TeamError::InvalidPermission {
                    permission: perm_str.clone(),
                });
            }
        }
        Ok(())
    }

    async fn load_permissions(&self, role_id: i32) -> Result<Vec<String>, TeamError> {
        Self::load_permissions_on(self.db.as_ref(), role_id).await
    }

    /// Same query as `load_permissions`, but against a caller-supplied
    /// connection — used inside `update_custom_role`'s transaction so the
    /// permissions returned (and audited) are read from the same snapshot
    /// as the metadata update, not a separate pool connection that could
    /// observe a concurrent write.
    async fn load_permissions_on(
        conn: &impl sea_orm::ConnectionTrait,
        role_id: i32,
    ) -> Result<Vec<String>, TeamError> {
        let rows = custom_role_permissions::Entity::find()
            .filter(custom_role_permissions::Column::RoleId.eq(role_id))
            .all(conn)
            .await?;
        Ok(rows.into_iter().map(|r| r.permission).collect())
    }

    async fn find_role(&self, role_id: i32) -> Result<custom_roles::Model, TeamError> {
        custom_roles::Entity::find_by_id(role_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(TeamError::CustomRoleNotFound { role_id })
    }
}

#[async_trait]
impl CustomRoleService for DefaultCustomRoleService {
    async fn create_custom_role(
        &self,
        actor_user_id: i32,
        req: CreateCustomRoleRequest,
    ) -> Result<(custom_roles::Model, Vec<String>), TeamError> {
        Self::validate_name(&req.name)?;
        Self::validate_slug(&req.slug)?;
        Self::validate_permissions(&req.permissions)?;

        let now = Utc::now();
        let model = custom_roles::ActiveModel {
            name: Set(req.name.trim().to_string()),
            slug: Set(req.slug.clone()),
            description: Set(req.description),
            created_by: Set(actor_user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        let txn = self.db.begin().await?;

        let inserted = match model.insert(&txn).await {
            Ok(role) => role,
            Err(
                sea_orm::DbErr::Exec(_)
                | sea_orm::DbErr::Query(_)
                | sea_orm::DbErr::RecordNotInserted,
            ) => {
                return Err(TeamError::CustomRoleSlugConflict {
                    slug: req.slug.clone(),
                })
            }
            Err(other) => return Err(TeamError::Database(other)),
        };

        for perm in &req.permissions {
            custom_role_permissions::ActiveModel {
                role_id: Set(inserted.id),
                permission: Set(perm.clone()),
            }
            .insert(&txn)
            .await?;
        }

        txn.commit().await?;

        Ok((inserted, req.permissions))
    }

    async fn list_custom_roles(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<(custom_roles::Model, Vec<String>)>, u64), TeamError> {
        let page = page.unwrap_or(1).max(1);
        let size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);

        let paginator = custom_roles::Entity::find()
            .order_by_desc(custom_roles::Column::CreatedAt)
            .paginate(self.db.as_ref(), size);
        let total = paginator.num_items().await?;
        let roles = paginator.fetch_page(page - 1).await?;

        // One `IN (...)` query for every role's permissions instead of one
        // query per role.
        let role_ids: Vec<i32> = roles.iter().map(|r| r.id).collect();
        let mut by_role: std::collections::HashMap<i32, Vec<String>> =
            std::collections::HashMap::new();
        if !role_ids.is_empty() {
            let rows = custom_role_permissions::Entity::find()
                .filter(custom_role_permissions::Column::RoleId.is_in(role_ids))
                .all(self.db.as_ref())
                .await?;
            for row in rows {
                by_role.entry(row.role_id).or_default().push(row.permission);
            }
        }

        let out = roles
            .into_iter()
            .map(|role| {
                let permissions = by_role.remove(&role.id).unwrap_or_default();
                (role, permissions)
            })
            .collect();
        Ok((out, total))
    }

    async fn get_custom_role(
        &self,
        role_id: i32,
    ) -> Result<(custom_roles::Model, Vec<String>), TeamError> {
        let role = self.find_role(role_id).await?;
        let permissions = self.load_permissions(role_id).await?;
        Ok((role, permissions))
    }

    async fn update_custom_role(
        &self,
        role_id: i32,
        req: UpdateCustomRoleRequest,
    ) -> Result<(custom_roles::Model, Vec<String>), TeamError> {
        let existing = self.find_role(role_id).await?;

        if let Some(ref name) = req.name {
            Self::validate_name(name)?;
        }
        if let Some(ref permissions) = req.permissions {
            Self::validate_permissions(permissions)?;
        }

        let txn = self.db.begin().await?;

        let mut active: custom_roles::ActiveModel = existing.into();
        if let Some(name) = req.name {
            active.name = Set(name.trim().to_string());
        }
        if let Some(description) = req.description {
            active.description = Set(Some(description));
        }
        active.updated_at = Set(Utc::now());
        let updated = active.update(&txn).await?;

        let permissions = if let Some(permissions) = req.permissions {
            custom_role_permissions::Entity::delete_many()
                .filter(custom_role_permissions::Column::RoleId.eq(role_id))
                .exec(&txn)
                .await?;
            for perm in &permissions {
                custom_role_permissions::ActiveModel {
                    role_id: Set(role_id),
                    permission: Set(perm.clone()),
                }
                .insert(&txn)
                .await?;
            }
            permissions
        } else {
            Self::load_permissions_on(&txn, role_id).await?
        };

        txn.commit().await?;

        Ok((updated, permissions))
    }

    async fn delete_custom_role(&self, role_id: i32) -> Result<(), TeamError> {
        let result = custom_roles::Entity::delete_by_id(role_id)
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(TeamError::CustomRoleNotFound { role_id });
        }
        // `custom_role_permissions` rows cascade-delete at the DB level.
        // `team_members.custom_role_id` references fall back to NULL
        // (ON DELETE SET NULL) — those members revert to their fixed
        // `role` column on their next permission check. This service has
        // no reference to the checker (circular Arc); the caller
        // invalidates the permission cache after this returns.
        Ok(())
    }

    async fn effective_permissions(
        &self,
        member: &team_members::Model,
    ) -> Result<Option<Vec<Permission>>, TeamError> {
        let Some(custom_role_id) = member.custom_role_id else {
            return Ok(None);
        };
        let permission_strings = self.load_permissions(custom_role_id).await?;
        Ok(Some(
            permission_strings
                .into_iter()
                .filter_map(|p| {
                    let parsed = Permission::from_str(&p);
                    if parsed.is_none() {
                        // Write-time validation rejects unknown strings, so
                        // this should only happen after a downgrade to a
                        // binary whose `Permission` enum doesn't recognise
                        // a string a newer version stored — silently
                        // narrowing is the safe direction, but make it
                        // visible.
                        tracing::warn!(
                            custom_role_id,
                            permission = %p,
                            "teams: unrecognized permission string on custom role — \
                             dropping it from the resolved set (narrowing, not widening)"
                        );
                    }
                    parsed
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn sample_role(id: i32) -> custom_roles::Model {
        let now = Utc::now();
        custom_roles::Model {
            id,
            name: "Reviewer".into(),
            slug: "reviewer".into(),
            description: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn create_custom_role_rejects_empty_permissions() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));
        let err = svc
            .create_custom_role(
                1,
                CreateCustomRoleRequest {
                    name: "Reviewer".into(),
                    slug: "reviewer".into(),
                    description: None,
                    permissions: vec![],
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::Validation { .. }));
    }

    #[tokio::test]
    async fn create_custom_role_rejects_invalid_permission_string() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));
        let err = svc
            .create_custom_role(
                1,
                CreateCustomRoleRequest {
                    name: "Reviewer".into(),
                    slug: "reviewer".into(),
                    description: None,
                    permissions: vec!["not-a-real-permission".into()],
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::InvalidPermission { permission } if permission == "not-a-real-permission"
        ));
    }

    #[tokio::test]
    async fn create_custom_role_rejects_bad_slug() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));
        let err = svc
            .create_custom_role(
                1,
                CreateCustomRoleRequest {
                    name: "Reviewer".into(),
                    slug: "Has Spaces".into(),
                    description: None,
                    permissions: vec!["projects:read".into()],
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::Validation { .. }));
    }

    #[tokio::test]
    async fn get_custom_role_returns_not_found_for_missing_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<custom_roles::Model>::new()])
            .into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));
        let err = svc.get_custom_role(999).await.unwrap_err();
        assert!(matches!(
            err,
            TeamError::CustomRoleNotFound { role_id: 999 }
        ));
    }

    #[tokio::test]
    async fn delete_custom_role_returns_not_found_when_zero_rows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));
        let err = svc.delete_custom_role(42).await.unwrap_err();
        assert!(matches!(err, TeamError::CustomRoleNotFound { role_id: 42 }));
    }

    #[tokio::test]
    async fn get_custom_role_happy_path_loads_permissions() {
        let role = sample_role(1);
        let perm_row = custom_role_permissions::Model {
            role_id: 1,
            permission: "projects:read".into(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![role]])
            .append_query_results(vec![vec![perm_row]])
            .into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));
        let (out_role, permissions) = svc.get_custom_role(1).await.unwrap();
        assert_eq!(out_role.id, 1);
        assert_eq!(permissions, vec!["projects:read".to_string()]);
    }

    /// Precedence rule: `custom_role_id` present → resolves to that role's
    /// permission set.
    #[tokio::test]
    async fn effective_permissions_resolves_custom_role_when_present() {
        let perm_row = custom_role_permissions::Model {
            role_id: 5,
            permission: "deployments:write".into(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![perm_row]])
            .into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));

        let member = team_members::Model {
            id: 1,
            team_id: 1,
            user_id: 1,
            role: "viewer".into(),
            custom_role_id: Some(5),
            added_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let perms = svc.effective_permissions(&member).await.unwrap();
        assert_eq!(perms, Some(vec![Permission::DeploymentsWrite]));
    }

    /// `custom_role_id` is `None` → `None`, with no DB query at all (no
    /// results are queued, so a query would panic MockDatabase).
    #[tokio::test]
    async fn effective_permissions_returns_none_when_no_custom_role() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultCustomRoleService::new(Arc::new(db));

        let member = team_members::Model {
            id: 1,
            team_id: 1,
            user_id: 1,
            role: "viewer".into(),
            custom_role_id: None,
            added_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let perms = svc.effective_permissions(&member).await.unwrap();
        assert_eq!(perms, None);
    }
}
