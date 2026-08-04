use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    sea_query::{LockType, OnConflict},
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_auth::permissions::Permission;
use temps_entities::{project_team_access, projects, team_members, teams, users, TeamRole};
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

/// The new fixed role for an existing membership.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateMemberRoleRequest {
    pub role: TeamRole,
}

/// Who is asking for a grant mutation.
///
/// Deliberately *only* the instance-admin flag. The caller's project-scoped
/// permissions are **not** passed in: they are resolved by the service
/// inside its own transaction, from the same grant rows it holds locked
/// (see [`ResolvedAuthz::resolve`]). Handing them in from the handler would
/// reintroduce the defect this design exists to prevent — a decision made
/// against a snapshot taken before the lock, which a concurrent revoke can
/// invalidate between the read and the write.
#[derive(Debug, Clone, Copy)]
pub struct GrantAuthz {
    /// Instance admins bypass every project-scoped rule, as they do
    /// everywhere else in the platform.
    pub is_instance_admin: bool,
}

impl GrantAuthz {
    /// Unrestricted, for internal callers with no user context.
    pub fn instance_admin() -> Self {
        Self {
            is_instance_admin: true,
        }
    }

    /// A caller subject to the project-scoped rules.
    pub fn project_scoped() -> Self {
        Self {
            is_instance_admin: false,
        }
    }
}

/// [`GrantAuthz`] plus the permissions the caller actually holds on the
/// project, resolved under the transaction's row lock.
struct ResolvedAuthz {
    is_instance_admin: bool,
    held: std::collections::HashSet<String>,
}

impl ResolvedAuthz {
    /// Resolves the caller's project permissions **on `txn`**, against
    /// `grants` — the rows the caller has already locked `FOR UPDATE`.
    ///
    /// Both halves matter. Running on the transaction means the read sees
    /// the same snapshot as the write. Passing the locked `grants` rather
    /// than re-reading them means no concurrent mutation can change the
    /// grant set out from under the answer.
    async fn resolve(
        txn: &sea_orm::DatabaseTransaction,
        checker: Option<&TeamProjectAccessChecker>,
        authz: &GrantAuthz,
        user_id: i32,
        project_id: i32,
        grants: &[project_team_access::Model],
    ) -> Result<Self, TeamError> {
        if authz.is_instance_admin {
            return Ok(Self {
                is_instance_admin: true,
                held: std::collections::HashSet::new(),
            });
        }

        let held = crate::checker::permissions_from_grants(
            txn,
            checker.and_then(|c| c.membership_resolver()),
            user_id,
            project_id,
            grants,
        )
        .await
        .map_err(|e| TeamError::PermissionResolution {
            user_id,
            project_id,
            reason: e.to_string(),
        })?;

        Ok(Self {
            is_instance_admin: false,
            held: held.into_iter().collect(),
        })
    }

    fn holds(&self, permission: &Permission) -> bool {
        self.held.contains(&permission.to_string())
    }

    /// The role a caller may not exceed when writing or removing a grant.
    ///
    /// Checked against the role being *written* and the role being
    /// *overwritten*: without the latter, a project-admin could demote or
    /// delete an `owner` grant and become the highest authority on the
    /// project, having removed everyone above them.
    fn check_role_ceiling(&self, role: TeamRole) -> Result<(), TeamError> {
        if let Some(excess) = crate::role_permissions::fixed_role_permissions(role)
            .into_iter()
            .find(|p| !self.holds(p))
        {
            return Err(TeamError::RoleCeilingExceeded {
                role: role.to_string(),
                permission: excess.to_string(),
            });
        }
        Ok(())
    }
}

/// True only for a Postgres unique-constraint violation (SQLSTATE 23505).
///
/// `DbErr::Exec`/`Query`/`RecordNotInserted` also cover connection resets,
/// statement timeouts and permission-denied-on-table. Mapping that whole
/// class to "already exists" tells an operator debugging an outage to go
/// look for a duplicate row that does not exist.
fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    matches!(err, sea_orm::DbErr::RecordNotInserted)
        || err
            .sql_err()
            .is_some_and(|e| matches!(e, sea_orm::SqlErr::UniqueConstraintViolation(_)))
}

/// Parses a `role` column, reporting corruption with the row it came from.
///
/// Stored roles are never caller input — the API takes roles as a typed
/// enum — so a parse failure here is a data-integrity fault, and the error
/// says which row to go look at.
fn parse_stored_role(entity: &'static str, id: i32, role: &str) -> Result<TeamRole, TeamError> {
    role.parse().map_err(|_| TeamError::CorruptStoredRole {
        entity,
        id,
        role: role.to_string(),
    })
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

    /// Deletes a team, returning the access grants the cascade removed so
    /// the caller can audit them.
    async fn delete_team(
        &self,
        team_id: i32,
        authz: &GrantAuthz,
    ) -> Result<Vec<project_team_access::Model>, TeamError>;

    async fn add_member(
        &self,
        actor_user_id: i32,
        team_id: i32,
        req: CreateTeamMemberRequest,
    ) -> Result<TeamMember, TeamError>;

    async fn list_members(&self, team_id: i32) -> Result<Vec<TeamMember>, TeamError>;

    async fn remove_member(&self, team_id: i32, user_id: i32) -> Result<(), TeamError>;

    /// Changes the fixed role on an existing membership.
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
        authz: &GrantAuthz,
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

    async fn revoke_project_access(
        &self,
        actor_user_id: i32,
        project_id: i32,
        team_id: i32,
        authz: &GrantAuthz,
    ) -> Result<(), TeamError>;

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
            Err(e) if is_unique_violation(&e) => Err(TeamError::SlugConflict { slug: req.slug }),
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

    async fn delete_team(
        &self,
        team_id: i32,
        authz: &GrantAuthz,
    ) -> Result<Vec<project_team_access::Model>, TeamError> {
        let txn = self.db.begin().await?;

        // `fk_project_team_access_team` is ON DELETE CASCADE, so deleting a
        // team silently removes every grant it held. Where that was a
        // project's *last* grant, the project becomes ungated — reachable
        // by every user on the instance. That is the same transition
        // `revoke_project_access` refuses without an instance admin, so it
        // cannot be left as an unguarded side effect of a team delete.
        let cascaded = project_team_access::Entity::find()
            .filter(project_team_access::Column::TeamId.eq(team_id))
            .lock(LockType::Update)
            .all(&txn)
            .await?;

        if !cascaded.is_empty() && !authz.is_instance_admin {
            let affected: Vec<i32> = {
                let mut ids: Vec<i32> = cascaded.iter().map(|g| g.project_id).collect();
                ids.sort_unstable();
                ids.dedup();
                ids
            };

            // Every grant on the affected projects, locked, so a concurrent
            // revoke cannot remove the grant we are counting on to keep a
            // project gated.
            let siblings = project_team_access::Entity::find()
                .filter(project_team_access::Column::ProjectId.is_in(affected.clone()))
                .lock(LockType::Update)
                .all(&txn)
                .await?;

            let mut would_ungate: Vec<i32> = affected
                .into_iter()
                .filter(|pid| {
                    !siblings
                        .iter()
                        .any(|g| g.project_id == *pid && g.team_id != team_id)
                })
                .collect();
            would_ungate.sort_unstable();

            if !would_ungate.is_empty() {
                return Err(TeamError::TeamDeletionWouldUngate {
                    team_id,
                    projects: would_ungate
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        }

        let result = teams::Entity::delete_by_id(team_id).exec(&txn).await?;
        if result.rows_affected == 0 {
            return Err(TeamError::NotFound { team_id });
        }

        txn.commit().await?;

        // DB-level CASCADE removes every `team_members` and
        // `project_team_access` row for this team in the same delete, but
        // that can touch an unbounded set of users and projects at once —
        // cheaper to flush the whole cache than to enumerate the cascade.
        // Without this, a just-deleted team's former members would keep
        // their stale "allowed" cache entries for up to the TTL.
        if let Some(ref checker) = self.checker {
            checker.invalidate_all();
        }

        // Returned so the handler can audit each cascaded revocation:
        // otherwise the trail shows only TEAM_DELETED and never says which
        // projects stopped being reachable through it.
        Ok(cascaded)
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
        // Same for the user. Without this, a non-existent user_id violates
        // `fk_team_members_user` and the catch-all below reports it as
        // "already a member" — an error that actively misdirects whoever is
        // debugging it.
        //
        // Deleting a user is a *soft* delete, so the row survives and the FK
        // is satisfied — the checker excludes soft-deleted users from every
        // resolution path, so adding one here would create a membership that
        // grants nothing and shows up in the UI as a live member. Reject it
        // at the door and report it as not existing, which is what a deleted
        // user is from the caller's point of view.
        if users::Entity::find_by_id(req.user_id)
            .filter(users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .is_none()
        {
            return Err(TeamError::Validation {
                message: format!("User {} does not exist", req.user_id),
            });
        }

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
            Err(e) if is_unique_violation(&e) => {
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
        let existing = team_members::Entity::find()
            .filter(team_members::Column::TeamId.eq(team_id))
            .filter(team_members::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(TeamError::MemberNotFound { team_id, user_id })?;

        let mut active: team_members::ActiveModel = existing.into();
        active.role = Set(req.role.to_string());
        active.updated_at = Set(Utc::now());
        let member = active.update(self.db.as_ref()).await?;

        // The checker's permissions cache is keyed on exactly what this
        // call just changed — the member's `role` — so a stale
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
        authz: &GrantAuthz,
    ) -> Result<project_team_access::Model, TeamError> {
        let txn = self.db.begin().await?;

        // Lock this project's grants for the duration. Every rule below is
        // evaluated against the locked set, so a concurrent mutation can't
        // change the answer between the check and the write.
        let grants = project_team_access::Entity::find()
            .filter(project_team_access::Column::ProjectId.eq(project_id))
            .lock(LockType::Update)
            .all(&txn)
            .await?;

        // Resolved on `txn`, from the rows just locked above — so the
        // permissions this decision rests on and the rows it is about to
        // write are the same snapshot.
        let authz = ResolvedAuthz::resolve(
            &txn,
            self.checker.as_deref(),
            authz,
            actor_user_id,
            project_id,
            &grants,
        )
        .await?;

        if !authz.is_instance_admin {
            // Adding the first grant gates a previously-open project and
            // locks out everyone who isn't in the named team.
            if grants.is_empty() {
                return Err(TeamError::GatingRequiresAdmin);
            }
            if !authz.holds(&Permission::ProjectsWrite) {
                return Err(TeamError::ProjectPermissionDenied {
                    project_id,
                    required: Permission::ProjectsWrite.to_string(),
                });
            }
            // The role being written...
            authz.check_role_ceiling(req.role)?;
            // ...and the role being overwritten, if this replaces a grant.
            // Without this a project-admin could demote an `owner` team to
            // `viewer` and become the highest authority on the project.
            if let Some(existing) = grants.iter().find(|g| g.team_id == req.team_id) {
                authz.check_role_ceiling(parse_stored_role(
                    "project_team_access",
                    existing.id,
                    &existing.role,
                )?)?;
            }
        }

        // Reporting "no such team" here is not the enumeration oracle that
        // `list_team_projects` was: every check that could deny is above
        // this line, so only an instance admin or someone who already holds
        // `projects:write` on this project gets here — and they can tell an
        // existing team from a missing one anyway, because for an existing
        // one the grant simply succeeds. Answering generically would buy no
        // secrecy and would tell a project admin they lack a permission they
        // hold, sending them to look for a problem that isn't there.
        // Keep this lookup last.
        if teams::Entity::find_by_id(req.team_id)
            .one(&txn)
            .await?
            .is_none()
        {
            return Err(TeamError::NotFound {
                team_id: req.team_id,
            });
        }
        // A non-existent project_id violates `fk_project_team_access_project`
        // and would otherwise surface as an opaque 500.
        if projects::Entity::find_by_id(project_id)
            .one(&txn)
            .await?
            .is_none()
        {
            return Err(TeamError::Validation {
                message: format!("Project {project_id} does not exist"),
            });
        }

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
            .exec_with_returning(&txn)
            .await?;

        txn.commit().await?;

        // Granting access changes which users may reach this project.
        //
        // Deliberately after the commit and outside the transaction: the
        // cache is not a source of truth, so it must never be invalidated
        // for a write that then rolls back. The cost is that a crash in
        // this window leaves the read caches stale until the 60 s TTL
        // expires. That is bounded and read-only — and the *write* gate
        // does not consult the cache at all (see `ResolvedAuthz::resolve`),
        // so a stale entry cannot be escalated into a privilege change.
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

    async fn revoke_project_access(
        &self,
        actor_user_id: i32,
        project_id: i32,
        team_id: i32,
        authz: &GrantAuthz,
    ) -> Result<(), TeamError> {
        let txn = self.db.begin().await?;

        // Lock the project's grants before counting them. Two concurrent
        // revokes against a two-grant project would otherwise each read
        // "two grants, not the last one", both authorize, and between them
        // remove every grant — silently re-opening the project to every
        // user, which is exactly the transition this rule exists to gate.
        let grants = project_team_access::Entity::find()
            .filter(project_team_access::Column::ProjectId.eq(project_id))
            .lock(LockType::Update)
            .all(&txn)
            .await?;

        // Resolved on `txn`, from the rows just locked above.
        let authz = ResolvedAuthz::resolve(
            &txn,
            self.checker.as_deref(),
            authz,
            actor_user_id,
            project_id,
            &grants,
        )
        .await?;

        // Authorize *before* revealing whether the target grant exists, so
        // this endpoint does not distinguish "no such grant" (404) from
        // "grant you may not touch" (403) for a caller who cannot manage
        // the project at all. The grant path orders itself the same way.
        if !authz.is_instance_admin && !authz.holds(&Permission::ProjectsWrite) {
            return Err(TeamError::ProjectPermissionDenied {
                project_id,
                required: Permission::ProjectsWrite.to_string(),
            });
        }

        let Some(target) = grants.iter().find(|g| g.team_id == team_id) else {
            return Err(TeamError::ProjectAccessNotFound {
                project_id,
                team_id,
            });
        };

        if !authz.is_instance_admin {
            // Removing the last grant re-opens the project to everyone.
            if grants.len() == 1 {
                return Err(TeamError::GatingRequiresAdmin);
            }
            // You may not remove a grant you could not have created.
            authz.check_role_ceiling(parse_stored_role(
                "project_team_access",
                target.id,
                &target.role,
            )?)?;
        }

        let result = project_team_access::Entity::delete_many()
            .filter(project_team_access::Column::ProjectId.eq(project_id))
            .filter(project_team_access::Column::TeamId.eq(team_id))
            .exec(&txn)
            .await?;
        if result.rows_affected == 0 {
            return Err(TeamError::ProjectAccessNotFound {
                project_id,
                team_id,
            });
        }

        txn.commit().await?;
        // All users who were allowed via `team_id` must now be denied.
        // Post-commit and non-transactional for the reasons in
        // `grant_project_access`.
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
    /// The source of this member's project-scoped permissions, intersected
    /// with `project_team_access.role`.
    pub role: TeamRole,
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
        let role: TeamRole = parse_stored_role("team_members", member.id, &member.role)?;
        Ok(Self {
            id: member.id,
            team_id: member.team_id,
            user_id: member.user_id,
            role,
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
        let role: TeamRole = parse_stored_role("team_members", m.id, &m.role)?;
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
            // The team's cascaded grants are read first; this team holds none.
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .delete_team(42, &GrantAuthz::instance_admin())
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::NotFound { team_id: 42 }));
    }

    /// `fk_project_team_access_team` is ON DELETE CASCADE, so deleting a
    /// team drops every grant it held. Where that was a project's last
    /// grant the project silently becomes reachable by everyone — the
    /// transition `revoke_project_access` refuses. Deleting a team must not
    /// be a way around that rule.
    #[tokio::test]
    async fn delete_team_will_not_ungate_a_project_without_instance_admin() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Team 7 holds a grant on project 42...
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Admin)]])
            // ...and it is the only grant there.
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Admin)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .delete_team(7, &GrantAuthz::project_scoped())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::TeamDeletionWouldUngate { team_id: 7, ref projects } if projects == "42"
        ));
    }

    #[tokio::test]
    async fn delete_team_is_allowed_when_another_team_still_holds_the_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Admin)]])
            // Team 9 also holds project 42, so it stays gated.
            .append_query_results(vec![vec![
                grant_row(1, 7, TeamRole::Admin),
                grant_row(2, 9, TeamRole::Owner),
            ]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let cascaded = svc
            .delete_team(7, &GrantAuthz::project_scoped())
            .await
            .expect("deletion leaves project 42 gated by team 9");
        // Returned so the handler can audit each removed grant.
        assert_eq!(cascaded.len(), 1);
        assert_eq!(cascaded[0].project_id, 42);
    }

    /// Revoke must not distinguish "no such grant" (404) from "a grant you
    /// may not touch" (403) for a caller who cannot manage the project.
    #[tokio::test]
    async fn revoke_denies_a_caller_without_project_write_before_revealing_the_target() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Admin)]])
            .append_query_results(vec![vec![member_row(1, 7, TeamRole::Viewer)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        // Target team 9999 has no grant here; the caller must still be told
        // only that they lack permission.
        let err = svc
            .revoke_project_access(ACTOR, 42, 9999, &GrantAuthz::project_scoped())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::ProjectPermissionDenied { project_id: 42, .. }
        ));
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
                    role: TeamRole::Viewer,
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

    // -----------------------------------------------------------------
    // Grant/revoke authorization
    //
    // These rules used to live in the handler, deciding against an
    // unlocked snapshot and a *cached* permission lookup. Each test below
    // pins one of the escalations that made possible.
    //
    // The caller's permissions are no longer injected: the service
    // resolves them from the membership rows, on the transaction, against
    // the locked grants. So every test appends the grants first and the
    // actor's memberships second — the order the service reads them.
    // -----------------------------------------------------------------

    /// The actor in every test below.
    const ACTOR: i32 = 1;

    fn grant_row(id: i32, team_id: i32, role: TeamRole) -> project_team_access::Model {
        let now = Utc::now();
        project_team_access::Model {
            id,
            project_id: 42,
            team_id,
            role: role.to_string(),
            granted_by: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// A membership giving [`ACTOR`] `role` within `team_id`. Their
    /// effective permissions on the project are this role intersected with
    /// that team's grant role, so the two together decide the ceiling.
    fn member_row(id: i32, team_id: i32, role: TeamRole) -> team_members::Model {
        let now = Utc::now();
        team_members::Model {
            id,
            team_id,
            user_id: ACTOR,
            role: role.to_string(),
            added_by: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// A caller whose resolved permissions are exactly those of `role` —
    /// for the pure ceiling tests, which don't go through the service.
    fn resolved_for(role: TeamRole) -> ResolvedAuthz {
        ResolvedAuthz {
            is_instance_admin: false,
            held: crate::role_permissions::fixed_role_permissions(role)
                .into_iter()
                .map(|p| p.to_string())
                .collect(),
        }
    }

    fn grant_req(team_id: i32, role: TeamRole) -> CreateProjectAccessRequest {
        CreateProjectAccessRequest { team_id, role }
    }

    #[test]
    fn ceiling_admits_the_callers_own_role_and_everything_below() {
        let authz = resolved_for(TeamRole::Admin);
        for role in [TeamRole::Viewer, TeamRole::Deployer, TeamRole::Admin] {
            assert!(
                authz.check_role_ceiling(role).is_ok(),
                "an admin must be able to hand out {role}"
            );
        }
    }

    #[test]
    fn ceiling_rejects_a_role_the_caller_does_not_hold() {
        let err = resolved_for(TeamRole::Admin)
            .check_role_ceiling(TeamRole::Owner)
            .unwrap_err();
        // `owner` is `admin` + ProjectsDelete, so that is the excess.
        assert!(matches!(
            err,
            TeamError::RoleCeilingExceeded { ref role, ref permission }
                if role == "owner" && permission == &Permission::ProjectsDelete.to_string()
        ));
    }

    #[tokio::test]
    async fn grant_will_not_gate_an_open_project_without_instance_admin() {
        // No grants yet: adding the first one locks out every user who is
        // not in the named team, which is an instance-admin decision.
        // Resolution short-circuits on the empty grant set, so no
        // membership query is issued.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .grant_project_access(
                ACTOR,
                42,
                grant_req(7, TeamRole::Viewer),
                &GrantAuthz::project_scoped(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::GatingRequiresAdmin));
    }

    #[tokio::test]
    async fn grant_requires_project_scoped_write_not_just_instance_write() {
        // Instance-level ProjectsWrite got them into the handler; within
        // the project they are only a viewer.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Viewer)]])
            .append_query_results(vec![vec![member_row(1, 7, TeamRole::Viewer)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .grant_project_access(
                ACTOR,
                42,
                grant_req(9, TeamRole::Viewer),
                &GrantAuthz::project_scoped(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::ProjectPermissionDenied { project_id: 42, .. }
        ));
    }

    #[tokio::test]
    async fn grant_cannot_write_a_role_above_the_callers_ceiling() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Admin)]])
            .append_query_results(vec![vec![member_row(1, 7, TeamRole::Admin)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .grant_project_access(
                ACTOR,
                42,
                grant_req(9, TeamRole::Owner),
                &GrantAuthz::project_scoped(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::RoleCeilingExceeded { ref role, .. } if role == "owner"
        ));
    }

    /// The escalation the ceiling check missed while it only inspected the
    /// role being written: a project-admin demoting the `owner` team to
    /// `viewer`, leaving themselves the highest authority on the project.
    #[tokio::test]
    async fn grant_cannot_overwrite_a_grant_above_the_callers_ceiling() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                grant_row(1, 7, TeamRole::Owner),
                grant_row(2, 9, TeamRole::Admin),
            ]])
            .append_query_results(vec![vec![member_row(1, 9, TeamRole::Admin)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .grant_project_access(
                ACTOR,
                42,
                // The *incoming* role is below the caller's ceiling; only
                // the role being replaced is above it.
                grant_req(7, TeamRole::Viewer),
                &GrantAuthz::project_scoped(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::RoleCeilingExceeded { ref role, .. } if role == "owner"
        ));
    }

    /// The team lookup is deliberately last: every denial is decided before
    /// it, so reaching it means the caller could have granted the team had
    /// it existed. Moving it earlier would turn the endpoint into a team
    /// enumeration oracle for callers who cannot manage this project.
    #[tokio::test]
    async fn grant_reports_an_unknown_team_only_after_every_denial() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Owner)]])
            .append_query_results(vec![vec![member_row(1, 7, TeamRole::Owner)]])
            .append_query_results(vec![Vec::<teams::Model>::new()])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .grant_project_access(
                ACTOR,
                42,
                grant_req(9999, TeamRole::Viewer),
                &GrantAuthz::project_scoped(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::NotFound { team_id: 9999 }));
    }

    #[tokio::test]
    async fn grant_denies_a_caller_without_project_write_before_looking_up_the_team() {
        // Only the grants and the membership are queued. If the team lookup
        // ran before the permission check this would panic on the missing
        // third result instead of denying — which is the property being
        // pinned: a caller who cannot manage the project never learns
        // whether the team exists.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Admin)]])
            .append_query_results(vec![vec![member_row(1, 7, TeamRole::Viewer)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .grant_project_access(
                ACTOR,
                42,
                grant_req(9999, TeamRole::Viewer),
                &GrantAuthz::project_scoped(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::ProjectPermissionDenied { project_id: 42, .. }
        ));
    }

    /// The grant role caps the member role: `owner` within the team is
    /// still only `viewer` on a project the team holds `viewer` on.
    #[tokio::test]
    async fn grant_role_caps_the_member_role() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Viewer)]])
            .append_query_results(vec![vec![member_row(1, 7, TeamRole::Owner)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .grant_project_access(
                ACTOR,
                42,
                grant_req(9, TeamRole::Viewer),
                &GrantAuthz::project_scoped(),
            )
            .await
            .unwrap_err();
        // Team owner, but the team only holds `viewer` here — so not even
        // `projects:write`, let alone the ceiling for handing out a role.
        assert!(matches!(
            err,
            TeamError::ProjectPermissionDenied { project_id: 42, .. }
        ));
    }

    #[tokio::test]
    async fn revoke_reports_a_team_with_no_grant_as_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Owner)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .revoke_project_access(ACTOR, 42, 9, &GrantAuthz::instance_admin())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::ProjectAccessNotFound {
                project_id: 42,
                team_id: 9
            }
        ));
    }

    #[tokio::test]
    async fn revoke_will_not_ungate_a_project_without_instance_admin() {
        // Removing the last grant re-opens the project to every user.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_row(1, 7, TeamRole::Owner)]])
            .append_query_results(vec![vec![member_row(1, 7, TeamRole::Owner)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .revoke_project_access(ACTOR, 42, 7, &GrantAuthz::project_scoped())
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::GatingRequiresAdmin));
    }

    #[tokio::test]
    async fn revoke_cannot_remove_a_grant_above_the_callers_ceiling() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                grant_row(1, 7, TeamRole::Owner),
                grant_row(2, 9, TeamRole::Admin),
            ]])
            .append_query_results(vec![vec![member_row(1, 9, TeamRole::Admin)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .revoke_project_access(ACTOR, 42, 7, &GrantAuthz::project_scoped())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::RoleCeilingExceeded { ref role, .. } if role == "owner"
        ));
    }

    /// A `role` column the binary cannot parse is a data-integrity fault,
    /// not a bad request — and it must deny, never fall through.
    #[tokio::test]
    async fn corrupt_stored_role_denies_and_is_not_a_client_error() {
        let mut corrupt = grant_row(1, 7, TeamRole::Owner);
        corrupt.role = "sorcerer".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![corrupt, grant_row(2, 9, TeamRole::Admin)]])
            .append_query_results(vec![vec![member_row(1, 9, TeamRole::Admin)]])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .revoke_project_access(ACTOR, 42, 7, &GrantAuthz::project_scoped())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TeamError::CorruptStoredRole {
                entity: "project_team_access",
                id: 1,
                ref role
            } if role == "sorcerer"
        ));
        let problem: temps_core::problemdetails::Problem = err.into();
        assert_eq!(
            problem.status_code,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "a corrupt stored role is a server fault, not a client error"
        );
    }

    #[tokio::test]
    async fn add_member_rejects_a_soft_deleted_user() {
        // The row still satisfies `fk_team_members_user`, but the checker
        // excludes soft-deleted users from every resolution path, so the
        // membership would grant nothing while showing as live in the UI.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_team(3)]])
            .append_query_results(vec![Vec::<users::Model>::new()])
            .into_connection();
        let svc = DefaultTeamService::new(Arc::new(db));
        let err = svc
            .add_member(
                1,
                3,
                CreateTeamMemberRequest {
                    user_id: 77,
                    role: TeamRole::Viewer,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TeamError::Validation { .. }));
    }
}
