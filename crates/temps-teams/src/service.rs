use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    sea_query::OnConflict, ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use temps_entities::{custom_roles, project_team_access, team_members, teams, users, TeamRole};
use utoipa::ToSchema;

use crate::checker::TeamProjectAccessChecker;
use crate::error::TeamError;

/// A team membership enriched with the member's user identity.
///
/// `team_members` only stores `user_id`; the name/email live in `users`.
/// The service joins them so the API can return a human-readable member
/// without the frontend making N extra calls.
#[derive(Debug, Clone)]
pub struct TeamMember {
    pub member: team_members::Model,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
}

/// Maximum page size for list endpoints (CLAUDE.md §pagination:
/// default 20, max 100).
pub const MAX_PAGE_SIZE: u64 = 100;
pub const DEFAULT_PAGE_SIZE: u64 = 20;

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateTeamRequest {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateTeamRequest {
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateTeamMemberRequest {
    pub user_id: i32,
    pub role: TeamRole,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateProjectAccessRequest {
    pub team_id: i32,
    pub role: TeamRole,
}

/// `role` and `custom_role_id` are mutually exclusive — exactly one must
/// be set. Setting `role` clears any existing `custom_role_id`; setting
/// `custom_role_id` leaves the fixed `role` column as-is (it becomes
/// irrelevant once `custom_role_id` is non-null, per the precedence rule
/// in [`CustomRoleService::effective_permissions`](crate::CustomRoleService::effective_permissions)).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateMemberRoleRequest {
    pub role: Option<TeamRole>,
    pub custom_role_id: Option<i32>,
}

// ---------------------------------------------------------------------------
// Trait surface
// ---------------------------------------------------------------------------

#[async_trait]
pub trait TeamService: Send + Sync {
    async fn create_team(
        &self,
        actor_user_id: i32,
        req: CreateTeamRequest,
    ) -> Result<teams::Model, TeamError>;

    async fn list_teams(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<teams::Model>, u64), TeamError>;

    async fn get_team(&self, team_id: i32) -> Result<teams::Model, TeamError>;

    async fn update_team(
        &self,
        team_id: i32,
        req: UpdateTeamRequest,
    ) -> Result<teams::Model, TeamError>;

    async fn delete_team(&self, team_id: i32) -> Result<(), TeamError>;

    async fn add_member(
        &self,
        actor_user_id: i32,
        team_id: i32,
        req: CreateTeamMemberRequest,
    ) -> Result<TeamMember, TeamError>;

    async fn list_members(&self, team_id: i32) -> Result<Vec<TeamMember>, TeamError>;

    async fn remove_member(&self, team_id: i32, user_id: i32) -> Result<(), TeamError>;

    /// Assigns a fixed `role` or a `custom_role_id` to an existing
    /// membership.
    async fn set_member_role(
        &self,
        team_id: i32,
        user_id: i32,
        req: UpdateMemberRoleRequest,
    ) -> Result<TeamMember, TeamError>;

    async fn grant_project_access(
        &self,
        actor_user_id: i32,
        project_id: i32,
        req: CreateProjectAccessRequest,
    ) -> Result<project_team_access::Model, TeamError>;

    async fn list_project_access(
        &self,
        project_id: i32,
    ) -> Result<Vec<project_team_access::Model>, TeamError>;

    /// Every project a team has been granted access to — the mirror of
    /// `list_project_access` keyed by team rather than project.
    async fn list_team_projects(
        &self,
        team_id: i32,
    ) -> Result<Vec<project_team_access::Model>, TeamError>;

    async fn revoke_project_access(&self, project_id: i32, team_id: i32) -> Result<(), TeamError>;

    /// Team ids `user_id` belongs to. Used to scope the project list to
    /// what the caller can actually reach.
    async fn team_ids_for_user(&self, user_id: i32) -> Result<Vec<i32>, TeamError>;
}

// ---------------------------------------------------------------------------
// Default implementation
// ---------------------------------------------------------------------------

pub struct DefaultTeamService {
    db: Arc<DatabaseConnection>,
    /// Optional reference to the shared access checker. When `Some`, write
    /// operations that could change the access-control answer call the
    /// checker's invalidation methods so the cache reflects the new state
    /// immediately rather than after the TTL.
    ///
    /// `None` in unit-test contexts where no checker is wired up.
    checker: Option<Arc<TeamProjectAccessChecker>>,
}

impl DefaultTeamService {
    /// Creates a service without a cache-invalidation hook (tests only —
    /// the plugin always attaches one via [`Self::with_checker`]).
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db, checker: None }
    }

    /// Attaches the shared [`TeamProjectAccessChecker`] so that writes
    /// trigger immediate cache invalidation.
    pub fn with_checker(self, checker: Arc<TeamProjectAccessChecker>) -> Self {
        Self {
            checker: Some(checker),
            ..self
        }
    }

    fn validate_slug(slug: &str) -> Result<(), TeamError> {
        if slug.is_empty() {
            return Err(TeamError::Validation {
                message: "Team slug cannot be empty".into(),
            });
        }
        if slug.len() > 64 {
            return Err(TeamError::Validation {
                message: format!("Team slug must be ≤ 64 chars (got {})", slug.len()),
            });
        }
        if !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(TeamError::Validation {
                message: "Team slug must match [a-z0-9-]+".into(),
            });
        }
        Ok(())
    }

    fn validate_name(name: &str) -> Result<(), TeamError> {
        if name.trim().is_empty() {
            return Err(TeamError::Validation {
                message: "Team name cannot be empty".into(),
            });
        }
        if name.len() > 255 {
            return Err(TeamError::Validation {
                message: format!("Team name must be ≤ 255 chars (got {})", name.len()),
            });
        }
        Ok(())
    }

    fn normalize_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
        let page = page.unwrap_or(1).max(1);
        let size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        (page, size)
    }

    /// Batch-fetch users by id, returned as an id→user map.
    ///
    /// One `IN (...)` query — used to enrich team memberships with the
    /// member's name/email without an N+1. An empty input short-circuits
    /// (Sea-ORM would otherwise emit `IN ()`).
    async fn fetch_users(
        &self,
        ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, users::Model>, TeamError> {
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let users = users::Entity::find()
            .filter(users::Column::Id.is_in(ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        Ok(users.into_iter().map(|u| (u.id, u)).collect())
    }
}

#[async_trait]
impl TeamService for DefaultTeamService {
    async fn create_team(
        &self,
        actor_user_id: i32,
        req: CreateTeamRequest,
    ) -> Result<teams::Model, TeamError> {
        Self::validate_name(&req.name)?;
        Self::validate_slug(&req.slug)?;

        let now = Utc::now();
        let model = teams::ActiveModel {
            name: Set(req.name.trim().to_string()),
            slug: Set(req.slug.clone()),
            description: Set(req.description),
            created_by: Set(actor_user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        match model.insert(self.db.as_ref()).await {
            Ok(team) => Ok(team),
            Err(
                sea_orm::DbErr::Exec(_)
                | sea_orm::DbErr::Query(_)
                | sea_orm::DbErr::RecordNotInserted,
            ) => Err(TeamError::SlugConflict { slug: req.slug }),
            Err(other) => Err(TeamError::Database(other)),
        }
    }

    async fn list_teams(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<teams::Model>, u64), TeamError> {
        let (page, size) = Self::normalize_page(page, page_size);
        let paginator = teams::Entity::find()
            .order_by_desc(teams::Column::CreatedAt)
            .paginate(self.db.as_ref(), size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;
        Ok((items, total))
    }

    async fn get_team(&self, team_id: i32) -> Result<teams::Model, TeamError> {
        teams::Entity::find_by_id(team_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(TeamError::NotFound { team_id })
    }

    async fn update_team(
        &self,
        team_id: i32,
        req: UpdateTeamRequest,
    ) -> Result<teams::Model, TeamError> {
        let existing = self.get_team(team_id).await?;

        if let Some(ref name) = req.name {
            Self::validate_name(name)?;
        }

        let mut active: teams::ActiveModel = existing.into();
        if let Some(name) = req.name {
            active.name = Set(name.trim().to_string());
        }
        if let Some(description) = req.description {
            active.description = Set(Some(description));
        }
        active.updated_at = Set(Utc::now());

        Ok(active.update(self.db.as_ref()).await?)
    }

    async fn delete_team(&self, team_id: i32) -> Result<(), TeamError> {
        let result = teams::Entity::delete_by_id(team_id)
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(TeamError::NotFound { team_id });
        }
        // DB-level CASCADE removes every `team_members` and
        // `project_team_access` row for this team in the same delete, but
        // that can touch an unbounded set of users and projects at once —
        // cheaper to flush the whole cache than to enumerate the cascade.
        // Without this, a just-deleted team's former members would keep
        // their stale "allowed" cache entries for up to the TTL.
        if let Some(ref checker) = self.checker {
            checker.invalidate_all();
        }
        Ok(())
    }

    async fn add_member(
        &self,
        actor_user_id: i32,
        team_id: i32,
        req: CreateTeamMemberRequest,
    ) -> Result<TeamMember, TeamError> {
        // Make sure the team exists; surface NotFound rather than letting
        // the FK violate-and-rollback path produce a Database error.
        self.get_team(team_id).await?;

        let now = Utc::now();
        let model = team_members::ActiveModel {
            team_id: Set(team_id),
            user_id: Set(req.user_id),
            role: Set(req.role.to_string()),
            added_by: Set(actor_user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        let member = match model.insert(self.db.as_ref()).await {
            Ok(member) => member,
            Err(
                sea_orm::DbErr::Exec(_)
                | sea_orm::DbErr::Query(_)
                | sea_orm::DbErr::RecordNotInserted,
            ) => {
                return Err(TeamError::DuplicateMember {
                    team_id,
                    user_id: req.user_id,
                })
            }
            Err(other) => return Err(TeamError::Database(other)),
        };

        // Invalidate every cached `(user_id, *)` entry: the user's access
        // to all team-gated projects may have changed now that they're a
        // member of a new team.
        if let Some(ref checker) = self.checker {
            checker.invalidate_user(req.user_id).await;
        }

        // Enrich with the user's identity so the response matches the
        // shape `list_members` returns.
        let user_map = self.fetch_users(&[member.user_id]).await?;
        let user = user_map.get(&member.user_id);
        Ok(TeamMember {
            user_name: user.map(|u| u.name.clone()),
            user_email: user.map(|u| u.email.clone()),
            member,
        })
    }

    async fn list_members(&self, team_id: i32) -> Result<Vec<TeamMember>, TeamError> {
        self.get_team(team_id).await?;

        let members = team_members::Entity::find()
            .filter(team_members::Column::TeamId.eq(team_id))
            .order_by_asc(team_members::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        // Batch-fetch the referenced users in one `IN (...)` query (no N+1).
        let user_ids: Vec<i32> = members.iter().map(|m| m.user_id).collect();
        let user_map = self.fetch_users(&user_ids).await?;

        Ok(members
            .into_iter()
            .map(|member| {
                let user = user_map.get(&member.user_id);
                TeamMember {
                    user_name: user.map(|u| u.name.clone()),
                    user_email: user.map(|u| u.email.clone()),
                    member,
                }
            })
            .collect())
    }

    async fn remove_member(&self, team_id: i32, user_id: i32) -> Result<(), TeamError> {
        let result = team_members::Entity::delete_many()
            .filter(team_members::Column::TeamId.eq(team_id))
            .filter(team_members::Column::UserId.eq(user_id))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(TeamError::MemberNotFound { team_id, user_id });
        }
        // The user is no longer a member of `team_id`, which may have
        // withdrawn their access to one or more team-gated projects.
        if let Some(ref checker) = self.checker {
            checker.invalidate_user(user_id).await;
        }
        Ok(())
    }

    async fn set_member_role(
        &self,
        team_id: i32,
        user_id: i32,
        req: UpdateMemberRoleRequest,
    ) -> Result<TeamMember, TeamError> {
        match (&req.role, &req.custom_role_id) {
            (Some(_), Some(_)) => {
                return Err(TeamError::Validation {
                    message: "role and custom_role_id are mutually exclusive".into(),
                })
            }
            (None, None) => {
                return Err(TeamError::Validation {
                    message: "must specify exactly one of role or custom_role_id".into(),
                })
            }
            _ => {}
        }

        let existing = team_members::Entity::find()
            .filter(team_members::Column::TeamId.eq(team_id))
            .filter(team_members::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(TeamError::MemberNotFound { team_id, user_id })?;

        let mut active: team_members::ActiveModel = existing.into();
        if let Some(role) = req.role {
            active.role = Set(role.to_string());
            active.custom_role_id = Set(None);
        } else if let Some(custom_role_id) = req.custom_role_id {
            // Existence check up front — 404 rather than letting the FK
            // constraint fail as an opaque 500 for an unknown role id.
            custom_roles::Entity::find_by_id(custom_role_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(TeamError::CustomRoleNotFound {
                    role_id: custom_role_id,
                })?;
            active.custom_role_id = Set(Some(custom_role_id));
        }
        active.updated_at = Set(Utc::now());
        let member = active.update(self.db.as_ref()).await?;

        // The checker's permissions cache is keyed on exactly what this
        // call just changed — `role` and `custom_role_id` — so a stale
        // entry would keep serving the member's OLD resolved permissions
        // (e.g. still `admin` after a demotion to `viewer`) until the TTL.
        // Unlike the binary access cache, which only depends on membership
        // existence, this one depends on the role value itself, so this
        // write path needs its own invalidation call.
        if let Some(ref checker) = self.checker {
            checker.invalidate_user(user_id).await;
        }

        let user_map = self.fetch_users(&[member.user_id]).await?;
        let user = user_map.get(&member.user_id);
        Ok(TeamMember {
            user_name: user.map(|u| u.name.clone()),
            user_email: user.map(|u| u.email.clone()),
            member,
        })
    }

    async fn grant_project_access(
        &self,
        actor_user_id: i32,
        project_id: i32,
        req: CreateProjectAccessRequest,
    ) -> Result<project_team_access::Model, TeamError> {
        self.get_team(req.team_id).await?;

        let now = Utc::now();
        let model = project_team_access::ActiveModel {
            project_id: Set(project_id),
            team_id: Set(req.team_id),
            role: Set(req.role.to_string()),
            granted_by: Set(actor_user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        // Upsert: if a grant already exists, update the role instead of
        // failing. "Set team X to deployer on this project" should be
        // idempotent.
        let inserted = project_team_access::Entity::insert(model)
            .on_conflict(
                OnConflict::columns([
                    project_team_access::Column::ProjectId,
                    project_team_access::Column::TeamId,
                ])
                .update_columns([
                    project_team_access::Column::Role,
                    project_team_access::Column::GrantedBy,
                    project_team_access::Column::UpdatedAt,
                ])
                .to_owned(),
            )
            .exec_with_returning(self.db.as_ref())
            .await?;

        // Granting access changes which users may reach this project.
        if let Some(ref checker) = self.checker {
            checker.invalidate_project(project_id).await;
        }

        Ok(inserted)
    }

    async fn list_project_access(
        &self,
        project_id: i32,
    ) -> Result<Vec<project_team_access::Model>, TeamError> {
        Ok(project_team_access::Entity::find()
            .filter(project_team_access::Column::ProjectId.eq(project_id))
            .order_by_asc(project_team_access::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?)
    }

    async fn list_team_projects(
        &self,
        team_id: i32,
    ) -> Result<Vec<project_team_access::Model>, TeamError> {
        // Surface a clear NotFound for an unknown team rather than an
        // empty list, mirroring `list_members`.
        self.get_team(team_id).await?;
        Ok(project_team_access::Entity::find()
            .filter(project_team_access::Column::TeamId.eq(team_id))
            .order_by_asc(project_team_access::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?)
    }

    async fn revoke_project_access(&self, project_id: i32, team_id: i32) -> Result<(), TeamError> {
        let result = project_team_access::Entity::delete_many()
            .filter(project_team_access::Column::ProjectId.eq(project_id))
            .filter(project_team_access::Column::TeamId.eq(team_id))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(TeamError::ProjectAccessNotFound {
                project_id,
                team_id,
            });
        }
        // All users who were allowed via `team_id` must now be denied.
        if let Some(ref checker) = self.checker {
            checker.invalidate_project(project_id).await;
        }
        Ok(())
    }

    async fn team_ids_for_user(&self, user_id: i32) -> Result<Vec<i32>, TeamError> {
        let rows = team_members::Entity::find()
            .filter(team_members::Column::UserId.eq(user_id))
            .all(self.db.as_ref())
            .await?;
        Ok(rows.into_iter().map(|m| m.team_id).collect())
    }
}

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TeamMemberResponse {
    pub id: i32,
    pub team_id: i32,
    pub user_id: i32,
    /// Used when `custom_role_id` is `None` — and in that case also the
    /// source of this member's project-scoped permissions (intersected
    /// with `project_team_access.role`).
    pub role: TeamRole,
    /// When `Some`, this member's effective project-scoped permissions
    /// come from this custom role's permission set instead of `role`.
    pub custom_role_id: Option<i32>,
    pub added_by: i32,
    /// The member's display name, joined from `users`. `None` if the
    /// referenced user no longer exists.
    pub user_name: Option<String>,
    /// The member's email, joined from `users`.
    pub user_email: Option<String>,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl TeamMemberResponse {
    /// Project an enriched [`TeamMember`] into the API response.
    pub fn from_team_member(tm: TeamMember) -> Result<Self, TeamError> {
        let TeamMember {
            member,
            user_name,
            user_email,
        } = tm;
        let role: TeamRole = member.role.parse()?;
        Ok(Self {
            id: member.id,
            team_id: member.team_id,
            user_id: member.user_id,
            role,
            custom_role_id: member.custom_role_id,
            added_by: member.added_by,
            user_name,
            user_email,
            created_at: member.created_at,
            updated_at: member.updated_at,
        })
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectAccessResponse {
    pub id: i32,
    pub project_id: i32,
    pub team_id: i32,
    pub role: TeamRole,
    pub granted_by: i32,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ProjectAccessResponse {
    pub fn from_model(m: project_team_access::Model) -> Result<Self, TeamError> {
        let role: TeamRole = m.role.parse()?;
        Ok(Self {
            id: m.id,
            project_id: m.project_id,
            team_id: m.team_id,
            role,
            granted_by: m.granted_by,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn sample_team(id: i32) -> teams::Model {
        let now = Utc::now();
        teams::Model {
            id,
            name: "Platform".into(),
            slug: "platform".into(),
            description: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn create_team_rejects_empty_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .create_team(
                1,
                CreateTeamRequest {
                    name: "   ".into(),
                    slug: "ok".into(),
                    description: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::Validation { .. }));
    }

    #[tokio::test]
    async fn create_team_rejects_bad_slug() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .create_team(
                1,
                CreateTeamRequest {
                    name: "Platform".into(),
                    slug: "Has Spaces".into(),
                    description: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::Validation { .. }));
    }

    #[tokio::test]
    async fn get_team_returns_not_found_for_missing_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<teams::Model>::new()])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc.get_team(999).await.unwrap_err();
        assert!(matches!(err, TeamError::NotFound { team_id: 999 }));
    }

    #[tokio::test]
    async fn delete_team_returns_not_found_when_zero_rows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc.delete_team(42).await.unwrap_err();
        assert!(matches!(err, TeamError::NotFound { team_id: 42 }));
    }

    #[tokio::test]
    async fn remove_member_returns_not_found_when_zero_rows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc.remove_member(1, 5).await.unwrap_err();
        assert!(matches!(
            err,
            TeamError::MemberNotFound {
                team_id: 1,
                user_id: 5
            }
        ));
    }

    #[tokio::test]
    async fn list_members_propagates_team_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<teams::Model>::new()])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc.list_members(7).await.unwrap_err();
        assert!(matches!(err, TeamError::NotFound { team_id: 7 }));
    }

    #[tokio::test]
    async fn get_team_happy_path() {
        let team = sample_team(1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![team]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let out = svc.get_team(1).await.unwrap();
        assert_eq!(out.id, 1);
        assert_eq!(out.slug, "platform");
    }

    #[test]
    fn page_normalization_caps_size() {
        let (page, size) = DefaultTeamService::normalize_page(Some(0), Some(10_000));
        assert_eq!(page, 1);
        assert_eq!(size, MAX_PAGE_SIZE);
    }

    #[test]
    fn page_normalization_defaults() {
        let (page, size) = DefaultTeamService::normalize_page(None, None);
        assert_eq!(page, 1);
        assert_eq!(size, DEFAULT_PAGE_SIZE);
    }

    #[tokio::test]
    async fn set_member_role_rejects_both_role_and_custom_role_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .set_member_role(
                1,
                5,
                UpdateMemberRoleRequest {
                    role: Some(TeamRole::Viewer),
                    custom_role_id: Some(9),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::Validation { .. }));
    }

    #[tokio::test]
    async fn set_member_role_rejects_neither_role_nor_custom_role_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .set_member_role(
                1,
                5,
                UpdateMemberRoleRequest {
                    role: None,
                    custom_role_id: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::Validation { .. }));
    }

    #[tokio::test]
    async fn set_member_role_returns_not_found_for_missing_member() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .set_member_role(
                1,
                5,
                UpdateMemberRoleRequest {
                    role: Some(TeamRole::Viewer),
                    custom_role_id: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::MemberNotFound {
                team_id: 1,
                user_id: 5
            }
        ));
    }

    /// An unknown custom_role_id returns 404 up front rather than letting
    /// the FK constraint fail as an opaque 500.
    #[tokio::test]
    async fn set_member_role_returns_not_found_for_missing_custom_role() {
        let now = Utc::now();
        let member = team_members::Model {
            id: 1,
            team_id: 1,
            user_id: 5,
            role: "viewer".into(),
            custom_role_id: None,
            added_by: 1,
            created_at: now,
            updated_at: now,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![member]])
            .append_query_results(vec![Vec::<custom_roles::Model>::new()])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .set_member_role(
                1,
                5,
                UpdateMemberRoleRequest {
                    role: None,
                    custom_role_id: Some(999),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::CustomRoleNotFound { role_id: 999 }
        ));
    }
}
