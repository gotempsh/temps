//! [`TeamProjectAccessChecker`] — the implementation of
//! [`temps_core::ProjectAccessChecker`] that makes the platform's
//! project-scoped enforcement seam real.
//!
//! `project_access_guard!` is called in every project-scoped handler and
//! asks "may `user_id` access `project_id`?" through the
//! `ProjectAccessChecker` trait. With no implementation registered the
//! guard is a no-op; this module is the implementation.
//!
//! # Two-step logic
//!
//! 1. Does `project_team_access` have **any** rows for `project_id`?
//!    If not → `Ok(true)` (project is unrestricted; a project becomes
//!    team-gated only once the first team is explicitly granted access).
//! 2. Does `team_members` have a row for `(user_id, team_id)` where
//!    `team_id` is one of the teams granted access to the project?
//!    If yes → `Ok(true)`. Else → `Ok(false)`.
//!
//! This "unrestricted until explicitly granted" shape is what makes
//! turning the feature on safe: an operator who creates teams but has not
//! yet granted any project access sees no behaviour change at all.
//!
//! # Fail-closed on infrastructure errors
//!
//! Any DB error propagates as `Err(...)`, never as a silent allow or deny.
//! The caller (`project_access_guard!`) maps `Err` to HTTP 500 + logs.
//!
//! # Admin bypass
//!
//! The `project_access_guard!` macro short-circuits before calling this
//! checker when the caller holds an instance-wide Admin or PlatformAdmin
//! role. This checker does NOT duplicate that check.
//!
//! # Permission-specific narrowing
//!
//! [`user_can_access_project`](ProjectAccessChecker::user_can_access_project)
//! is a binary check — it never looks at the specific `role`
//! (`owner|admin|deployer|viewer`) or `custom_role_id` value, so a project
//! role meant to be more restrictive than another has no effect once the
//! binary answer is yes. `effective_project_permissions` closes that gap:
//! it resolves the exact permission set a user holds *within* a project,
//! so `project_permission_guard!` can deny specific actions the coarser
//! check would allow.
//!
//! Resolution: for each team the user belongs to that has a grant on the
//! project, resolve that membership's permission set — `custom_role_id`
//! wins, else the intersection of `team_members.role` and
//! `project_team_access.role`'s fixed permission sets
//! ([`fixed_role_permissions`]) — and union across teams. Intersecting the
//! two fixed-role columns is deliberately *narrowing*: it cannot leak
//! privilege regardless of which column an admin actually populated.
//!
//! # Caching
//!
//! `moka::future::Cache<(user_id, project_id), bool>`, 60 s TTL,
//! 10 000-entry capacity, for the binary check. A second cache of the same
//! shape holds `Option<Vec<String>>` for the permission resolution.
//! Explicit invalidation (not just TTL) is required after writes:
//!
//! - [`invalidate_project`](TeamProjectAccessChecker::invalidate_project) —
//!   `grant_project_access` / `revoke_project_access`
//! - [`invalidate_user`](TeamProjectAccessChecker::invalidate_user) —
//!   `add_member` / `remove_member` / `set_member_role`
//! - [`invalidate_all`](TeamProjectAccessChecker::invalidate_all) —
//!   `delete_team`, and custom-role update/delete, where the set of
//!   affected `(user, project)` pairs is unbounded.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moka::future::Cache;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_auth::permissions::Permission;
use temps_core::ProjectAccessChecker;
use temps_entities::{project_team_access, team_members};

use crate::custom_role_service::CustomRoleService;
use crate::role_permissions::fixed_role_permissions;

/// Team membership and project grants change through deliberate admin
/// actions, so a short staleness window is acceptable; 60 s bounds how
/// long a revocation can lag if explicit invalidation is ever missed.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// 500 users × 20 projects = 10,000 unique pairs, which bounds memory to
/// a few hundred KB.
const CACHE_MAX_CAPACITY: u64 = 10_000;

/// Typed error for the checker's internal DB operations.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckerError {
    #[error("Failed to query access grants for project {project_id}: {source}")]
    AccessGrantQuery {
        project_id: i32,
        source: sea_orm::DbErr,
    },

    #[error(
        "Failed to check team membership for user {user_id} on project {project_id}: {source}"
    )]
    MembershipQuery {
        user_id: i32,
        project_id: i32,
        source: sea_orm::DbErr,
    },

    #[error(
        "Failed to resolve custom-role permissions for user {user_id} on project {project_id}: \
         {source}"
    )]
    CustomRoleResolution {
        user_id: i32,
        project_id: i32,
        source: crate::error::TeamError,
    },
}

/// Answers "may `user_id` access `project_id`?" and "what may they do
/// there?".
///
/// Registered as `Arc<dyn ProjectAccessChecker>` by
/// [`TeamsPlugin`](crate::TeamsPlugin) so project-scoped handlers
/// throughout the platform resolve it generically. Also held as a concrete
/// `Arc<TeamProjectAccessChecker>` by
/// [`DefaultTeamService`](crate::DefaultTeamService) so writes can call
/// the cache-invalidation methods.
pub struct TeamProjectAccessChecker {
    db: Arc<DatabaseConnection>,
    /// `(user_id, project_id) → is_allowed`.
    cache: Cache<(i32, i32), bool>,
    /// `(user_id, project_id) → effective permission strings`, `None` when
    /// unrestricted.
    permissions_cache: Cache<(i32, i32), Option<Vec<String>>>,
    custom_role_service: Arc<dyn CustomRoleService>,
}

impl TeamProjectAccessChecker {
    pub fn new(
        db: Arc<DatabaseConnection>,
        custom_role_service: Arc<dyn CustomRoleService>,
    ) -> Self {
        let cache = Cache::builder()
            .max_capacity(CACHE_MAX_CAPACITY)
            .time_to_live(CACHE_TTL)
            .build();
        let permissions_cache = Cache::builder()
            .max_capacity(CACHE_MAX_CAPACITY)
            .time_to_live(CACHE_TTL)
            .build();
        Self {
            db,
            cache,
            permissions_cache,
            custom_role_service,
        }
    }

    /// Explicit cache invalidation for all `(*, project_id)` entries, so
    /// an access change takes effect immediately rather than after the TTL.
    pub async fn invalidate_project(&self, project_id: i32) {
        // Flush moka's internal pending-write queue so that entries
        // inserted during the current request are visible to iter().
        self.cache.run_pending_tasks().await;
        self.permissions_cache.run_pending_tasks().await;

        let keys: Vec<(i32, i32)> = self
            .cache
            .iter()
            .filter_map(|(k, _v)| if k.1 == project_id { Some(*k) } else { None })
            .collect();
        for key in keys {
            self.cache.invalidate(&key).await;
        }

        let perm_keys: Vec<(i32, i32)> = self
            .permissions_cache
            .iter()
            .filter_map(|(k, _v)| if k.1 == project_id { Some(*k) } else { None })
            .collect();
        for key in perm_keys {
            self.permissions_cache.invalidate(&key).await;
        }
    }

    /// Explicit cache invalidation for all `(user_id, *)` entries, so a
    /// membership change takes effect immediately.
    pub async fn invalidate_user(&self, user_id: i32) {
        self.cache.run_pending_tasks().await;
        self.permissions_cache.run_pending_tasks().await;

        let keys: Vec<(i32, i32)> = self
            .cache
            .iter()
            .filter_map(|(k, _v)| if k.0 == user_id { Some(*k) } else { None })
            .collect();
        for key in keys {
            self.cache.invalidate(&key).await;
        }

        let perm_keys: Vec<(i32, i32)> = self
            .permissions_cache
            .iter()
            .filter_map(|(k, _v)| if k.0 == user_id { Some(*k) } else { None })
            .collect();
        for key in perm_keys {
            self.permissions_cache.invalidate(&key).await;
        }
    }

    /// Full cache flush, for writes affecting an unbounded set of
    /// `(user_id, project_id)` pairs — deleting a team cascades away its
    /// memberships and grants at once, and editing a custom role changes
    /// what's resolved for every member holding it.
    ///
    /// `moka::future::Cache::invalidate_all` is synchronous: it lazily
    /// marks every entry inserted before this call as expired rather than
    /// sweeping them immediately, so it's cheap even on a hot path.
    pub fn invalidate_all(&self) {
        self.cache.invalidate_all();
        self.permissions_cache.invalidate_all();
    }

    /// Two-step DB check — runs only on a cache miss.
    async fn check_uncached(&self, user_id: i32, project_id: i32) -> Result<bool, CheckerError> {
        // Step 1: does this project have ANY team-access grants? We fetch
        // the grants (rather than COUNT) so the team ids can be reused in
        // step 2 without an extra round trip.
        let grants = project_team_access::Entity::find()
            .filter(project_team_access::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::AccessGrantQuery { project_id, source })?;

        if grants.is_empty() {
            // No team has been granted access → unrestricted.
            return Ok(true);
        }

        let granted_team_ids: Vec<i32> = grants.into_iter().map(|g| g.team_id).collect();

        // Step 2: is this user a member of any team that has a grant?
        // `.one()` (LIMIT 1) stops at the first match.
        let any_membership = team_members::Entity::find()
            .filter(team_members::Column::UserId.eq(user_id))
            .filter(team_members::Column::TeamId.is_in(granted_team_ids))
            .one(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::MembershipQuery {
                user_id,
                project_id,
                source,
            })?;

        Ok(any_membership.is_some())
    }

    /// Gated projects `user_id` is not a member of any granted team for.
    ///
    /// Deliberately uncached: it runs once per project-list request (not
    /// per project-scoped request like the other two checks), and a stale
    /// entry here would keep showing a project in the list after access
    /// was revoked — the exact leak the filter exists to prevent. Two
    /// index-covered queries on small tables is the right trade.
    async fn hidden_project_ids_uncached(&self, user_id: i32) -> Result<Vec<i32>, CheckerError> {
        // Every gated project and the teams granted on it. A project with
        // no row here is unrestricted and never hidden.
        let grants = project_team_access::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::AccessGrantQuery {
                // Not scoped to one project — 0 reads as "all projects" in
                // the error message context.
                project_id: 0,
                source,
            })?;

        if grants.is_empty() {
            return Ok(Vec::new());
        }

        let memberships = team_members::Entity::find()
            .filter(team_members::Column::UserId.eq(user_id))
            .all(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::MembershipQuery {
                user_id,
                project_id: 0,
                source,
            })?;

        let user_team_ids: std::collections::HashSet<i32> =
            memberships.into_iter().map(|m| m.team_id).collect();

        // Group by project: a project is hidden only if the user is in
        // NONE of the teams granted on it.
        let mut allowed: std::collections::HashSet<i32> = std::collections::HashSet::new();
        let mut gated: std::collections::HashSet<i32> = std::collections::HashSet::new();
        for grant in grants {
            gated.insert(grant.project_id);
            if user_team_ids.contains(&grant.team_id) {
                allowed.insert(grant.project_id);
            }
        }

        Ok(gated.difference(&allowed).copied().collect())
    }

    /// Resolves the permission strings `user_id` holds within `project_id`
    /// — runs only on a permissions-cache miss.
    async fn check_permissions_uncached(
        &self,
        user_id: i32,
        project_id: i32,
    ) -> Result<Option<Vec<String>>, CheckerError> {
        // Step 1: same "does this project have any grants at all" check as
        // the binary path — unrestricted projects have no opinion here.
        let grants = project_team_access::Entity::find()
            .filter(project_team_access::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::AccessGrantQuery { project_id, source })?;

        if grants.is_empty() {
            return Ok(None);
        }

        let granted_team_ids: Vec<i32> = grants.iter().map(|g| g.team_id).collect();

        // Step 2: the user's membership rows on those granted teams.
        // Unlike the binary check we need the full rows (role,
        // custom_role_id), and there can be more than one — a user may
        // belong to several teams granted access to the same project.
        let memberships = team_members::Entity::find()
            .filter(team_members::Column::UserId.eq(user_id))
            .filter(team_members::Column::TeamId.is_in(granted_team_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::MembershipQuery {
                user_id,
                project_id,
                source,
            })?;

        if memberships.is_empty() {
            // Not a member of any granted team → holds nothing here. This
            // is the finer-grained expression of the binary check's false.
            return Ok(Some(Vec::new()));
        }

        let mut effective: std::collections::HashSet<String> = std::collections::HashSet::new();

        for membership in &memberships {
            // Every membership.team_id came from granted_team_ids, which
            // was built from these exact grants, so a missing match is
            // impossible — but resolve it without panicking anyway.
            let Some(grant) = grants.iter().find(|g| g.team_id == membership.team_id) else {
                continue;
            };

            let resolved: Vec<Permission> = if membership.custom_role_id.is_some() {
                match self
                    .custom_role_service
                    .effective_permissions(membership)
                    .await
                    .map_err(|source| CheckerError::CustomRoleResolution {
                        user_id,
                        project_id,
                        source,
                    })? {
                    Some(perms) => perms,
                    // custom_role_id was Some but resolution found nothing
                    // — fall back to the fixed-role intersection rather
                    // than silently granting zero permissions.
                    None => intersect_fixed_roles(&membership.role, &grant.role),
                }
            } else {
                intersect_fixed_roles(&membership.role, &grant.role)
            };

            effective.extend(resolved.into_iter().map(|p| p.to_string()));
        }

        Ok(Some(effective.into_iter().collect()))
    }
}

/// `fixed_role_permissions(member_role) ∩ fixed_role_permissions(grant_role)`
/// — whichever of the two columns is more restrictive wins. Malformed role
/// strings (which should not occur: both columns are always written via
/// `TeamRole::as_str()`) contribute no permissions rather than erroring the
/// whole resolution; other valid memberships in the union are unaffected.
fn intersect_fixed_roles(member_role: &str, grant_role: &str) -> Vec<Permission> {
    use std::str::FromStr;
    use temps_entities::TeamRole;

    let (Ok(member_role), Ok(grant_role)) = (
        TeamRole::from_str(member_role),
        TeamRole::from_str(grant_role),
    ) else {
        tracing::warn!(
            member_role,
            grant_role,
            "teams: unparsable role string during permission resolution — contributing no \
             permissions for this membership"
        );
        return Vec::new();
    };

    let member_set: std::collections::HashSet<Permission> =
        fixed_role_permissions(member_role).into_iter().collect();
    fixed_role_permissions(grant_role)
        .into_iter()
        .filter(|p| member_set.contains(p))
        .collect()
}

#[async_trait]
impl ProjectAccessChecker for TeamProjectAccessChecker {
    async fn user_can_access_project(
        &self,
        user_id: i32,
        project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let cache_key = (user_id, project_id);

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        // Cache miss: run the two-step DB check. Fail-closed — DB errors
        // propagate as Err, never a silent allow.
        let allowed = self
            .check_uncached(user_id, project_id)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        // Both allow and deny are cached.
        self.cache.insert(cache_key, allowed).await;

        Ok(allowed)
    }

    async fn hidden_project_ids(
        &self,
        user_id: i32,
    ) -> Result<Option<Vec<i32>>, Box<dyn std::error::Error + Send + Sync>> {
        self.hidden_project_ids_uncached(user_id)
            .await
            .map(Some)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    async fn effective_project_permissions(
        &self,
        user_id: i32,
        project_id: i32,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
        let cache_key = (user_id, project_id);

        if let Some(cached) = self.permissions_cache.get(&cache_key).await {
            return Ok(cached);
        }

        // Fail-closed: DB/resolution errors propagate as Err, never a
        // silent "unrestricted" or "allow everything".
        let resolved = self
            .check_permissions_uncached(user_id, project_id)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        self.permissions_cache
            .insert(cache_key, resolved.clone())
            .await;

        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::TeamRole;

    fn sample_grant(project_id: i32, team_id: i32) -> project_team_access::Model {
        project_team_access::Model {
            id: 1,
            project_id,
            team_id,
            role: "viewer".to_string(),
            granted_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn sample_member(team_id: i32, user_id: i32) -> team_members::Model {
        team_members::Model {
            id: 1,
            team_id,
            user_id,
            role: "viewer".to_string(),
            custom_role_id: None,
            added_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// A `CustomRoleService` whose `effective_permissions` returns a
    /// fixed, configurable set. `None` models "no custom role resolved";
    /// `Some(perms)` lets a test observe whether the custom-role branch
    /// was actually taken.
    struct StubCustomRoleService(Option<Vec<Permission>>);

    #[async_trait]
    impl CustomRoleService for StubCustomRoleService {
        async fn create_custom_role(
            &self,
            _actor_user_id: i32,
            _req: crate::custom_role_service::CreateCustomRoleRequest,
        ) -> Result<(temps_entities::custom_roles::Model, Vec<String>), crate::error::TeamError>
        {
            unreachable!("not exercised by checker tests")
        }
        async fn list_custom_roles(
            &self,
            _page: Option<u64>,
            _page_size: Option<u64>,
        ) -> Result<
            (Vec<(temps_entities::custom_roles::Model, Vec<String>)>, u64),
            crate::error::TeamError,
        > {
            unreachable!("not exercised by checker tests")
        }
        async fn get_custom_role(
            &self,
            _role_id: i32,
        ) -> Result<(temps_entities::custom_roles::Model, Vec<String>), crate::error::TeamError>
        {
            unreachable!("not exercised by checker tests")
        }
        async fn update_custom_role(
            &self,
            _role_id: i32,
            _req: crate::custom_role_service::UpdateCustomRoleRequest,
        ) -> Result<(temps_entities::custom_roles::Model, Vec<String>), crate::error::TeamError>
        {
            unreachable!("not exercised by checker tests")
        }
        async fn delete_custom_role(&self, _role_id: i32) -> Result<(), crate::error::TeamError> {
            unreachable!("not exercised by checker tests")
        }
        async fn effective_permissions(
            &self,
            member: &team_members::Model,
        ) -> Result<Option<Vec<Permission>>, crate::error::TeamError> {
            assert!(
                member.custom_role_id.is_some(),
                "effective_permissions should only be called when custom_role_id is set"
            );
            Ok(self.0.clone())
        }
    }

    fn new_checker(db: DatabaseConnection) -> TeamProjectAccessChecker {
        TeamProjectAccessChecker::new(Arc::new(db), Arc::new(StubCustomRoleService(None)))
    }

    /// Project with zero access-grant rows → allow regardless of team
    /// membership.
    #[tokio::test]
    async fn unrestricted_project_allows_any_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert!(
            checker.user_can_access_project(1, 42).await.unwrap(),
            "unrestricted project must allow any user"
        );
    }

    /// User in a team that has a grant → allow.
    #[tokio::test]
    async fn user_in_granted_team_is_allowed() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![vec![sample_member(10, 1)]])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.user_can_access_project(1, 42).await.unwrap());
    }

    /// User in Team A, project granted only to Team B → deny.
    #[tokio::test]
    async fn user_not_in_any_granted_team_is_denied() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert!(!checker.user_can_access_project(7, 42).await.unwrap());
    }

    /// Fail-closed: a DB error must propagate, never resolve to allow.
    #[tokio::test]
    async fn db_error_propagates_as_err_not_allow() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("simulated failure".into())])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.user_can_access_project(1, 42).await.is_err());
    }

    /// A second call with the same key must not hit the DB (only one pair
    /// of results is queued; a second DB hit panics MockDatabase).
    #[tokio::test]
    async fn second_call_hits_cache_not_db() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![vec![sample_member(10, 1)]])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.user_can_access_project(1, 42).await.unwrap());
        assert!(
            checker.user_can_access_project(1, 42).await.unwrap(),
            "cached result must still be true"
        );
    }

    /// Explicit cache invalidation on revoke — the new (denied) result
    /// must be visible immediately, without waiting out the TTL.
    #[tokio::test]
    async fn invalidate_project_evicts_cache_immediately() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Round 1 (allow).
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![vec![sample_member(10, 1)]])
            // Round 2 (deny after revocation).
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.user_can_access_project(1, 42).await.unwrap());

        checker.invalidate_project(42).await;

        assert!(
            !checker.user_can_access_project(1, 42).await.unwrap(),
            "after revoke + cache invalidation, access must be denied immediately"
        );
    }

    /// Removing a user from a team must take effect immediately.
    #[tokio::test]
    async fn invalidate_user_evicts_cache_immediately() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![vec![sample_member(10, 1)]])
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.user_can_access_project(1, 42).await.unwrap());

        checker.invalidate_user(1).await;

        assert!(
            !checker.user_can_access_project(1, 42).await.unwrap(),
            "after member removal + cache invalidation, access must be denied immediately"
        );
    }

    /// Different (user, project) pairs are cached independently.
    #[tokio::test]
    async fn cache_keys_are_scoped_to_user_and_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_query_results(vec![vec![sample_grant(99, 10)]])
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.user_can_access_project(1, 42).await.unwrap());
        assert!(!checker.user_can_access_project(2, 99).await.unwrap());
    }

    /// Team deletion cascades away memberships and grants for an unbounded
    /// set of users/projects — `invalidate_all` must clear every entry.
    #[tokio::test]
    async fn invalidate_all_evicts_every_entry() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        let _ = checker.user_can_access_project(1, 42).await.unwrap();
        let _ = checker.user_can_access_project(2, 99).await.unwrap();

        checker.invalidate_all();

        // Both pairs must re-hit the DB — MockDatabase panics on exhausted
        // results if either were still served from a stale entry.
        assert!(checker.user_can_access_project(1, 42).await.unwrap());
        assert!(checker.user_can_access_project(2, 99).await.unwrap());
    }

    /// invalidate_project leaves other projects' cache entries intact.
    #[tokio::test]
    async fn invalidate_project_is_scoped_to_that_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        let _ = checker.user_can_access_project(1, 42).await.unwrap();
        let _ = checker.user_can_access_project(1, 99).await.unwrap();

        checker.invalidate_project(42).await;

        // project 99 must still be cached (no more results queued for it).
        assert!(
            checker.user_can_access_project(1, 99).await.unwrap(),
            "project 99 cache must survive project 42 invalidation"
        );
        assert!(checker.user_can_access_project(1, 42).await.unwrap());
    }

    // -------------------------------------------------------------------
    // hidden_project_ids (project-list filtering)
    // -------------------------------------------------------------------

    /// No grants anywhere → nothing is hidden from anyone, so an instance
    /// that hasn't configured team access lists exactly what it did before.
    #[tokio::test]
    async fn hidden_project_ids_empty_when_no_grants_exist() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(
            checker.hidden_project_ids(1).await.unwrap(),
            Some(Vec::new())
        );
    }

    /// A gated project the user has no team on is hidden; a gated project
    /// one of their teams holds is not.
    #[tokio::test]
    async fn hidden_project_ids_hides_only_ungranted_gated_projects() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                sample_grant(42, 10), // user is in team 10 → visible
                sample_grant(99, 20), // user is not in team 20 → hidden
            ]])
            .append_query_results(vec![vec![sample_member(10, 1)]])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(checker.hidden_project_ids(1).await.unwrap(), Some(vec![99]));
    }

    /// A project granted to two teams is visible if the user is in either
    /// — the per-project answer is a union across its grants, not a
    /// per-row decision.
    #[tokio::test]
    async fn hidden_project_ids_unions_grants_on_the_same_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_grant(42, 10), sample_grant(42, 20)]])
            .append_query_results(vec![vec![sample_member(20, 1)]])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(
            checker.hidden_project_ids(1).await.unwrap(),
            Some(Vec::new()),
            "membership on any one granted team must make the project visible"
        );
    }

    /// A user in no teams sees no gated projects at all.
    #[tokio::test]
    async fn hidden_project_ids_hides_all_gated_projects_from_teamless_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(checker.hidden_project_ids(9).await.unwrap(), Some(vec![42]));
    }

    /// Fail-closed: a DB error must surface, never resolve to "hide
    /// nothing" — that would list every gated project to everyone.
    #[tokio::test]
    async fn hidden_project_ids_db_error_propagates_err() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("simulated failure".into())])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.hidden_project_ids(1).await.is_err());
    }

    // -------------------------------------------------------------------
    // effective_project_permissions
    // -------------------------------------------------------------------

    fn grant_with_role(project_id: i32, team_id: i32, role: &str) -> project_team_access::Model {
        project_team_access::Model {
            role: role.to_string(),
            ..sample_grant(project_id, team_id)
        }
    }

    fn member_with_role(team_id: i32, user_id: i32, role: &str) -> team_members::Model {
        team_members::Model {
            role: role.to_string(),
            ..sample_member(team_id, user_id)
        }
    }

    /// Project with zero access-grant rows → `Ok(None)`, mirroring the
    /// binary check's "unrestricted" case.
    #[tokio::test]
    async fn effective_permissions_unrestricted_project_returns_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(
            checker.effective_project_permissions(1, 42).await.unwrap(),
            None
        );
    }

    /// Project granted to a team the user is NOT in → holds nothing here.
    #[tokio::test]
    async fn effective_permissions_non_member_returns_empty_deny() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "viewer")]])
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(
            checker.effective_project_permissions(7, 42).await.unwrap(),
            Some(Vec::new())
        );
    }

    /// Both roles `deployer` → resolved set includes a deploy permission
    /// and excludes a delete permission.
    #[tokio::test]
    async fn effective_permissions_deployer_grants_deploy_not_delete() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "deployer")]])
            .append_query_results(vec![vec![member_with_role(10, 1, "deployer")]])
            .into_connection();
        let checker = new_checker(db);

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("deployer must resolve to Some(perms)");
        assert!(perms.contains(&Permission::DeploymentsCreate.to_string()));
        assert!(!perms.contains(&Permission::ProjectsDelete.to_string()));
    }

    /// member.role=`admin`, grant.role=`viewer` → the resolved set is the
    /// intersection, i.e. exactly the narrower `viewer` set.
    #[tokio::test]
    async fn effective_permissions_intersects_member_and_grant_roles() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "viewer")]])
            .append_query_results(vec![vec![member_with_role(10, 1, "admin")]])
            .into_connection();
        let checker = new_checker(db);

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("must resolve to Some(perms)");
        let viewer_set: std::collections::HashSet<String> =
            fixed_role_permissions(TeamRole::Viewer)
                .into_iter()
                .map(|p| p.to_string())
                .collect();
        let resolved_set: std::collections::HashSet<String> = perms.into_iter().collect();
        assert_eq!(
            resolved_set, viewer_set,
            "admin member role intersected with a viewer grant must equal the viewer set exactly"
        );
    }

    /// User in two granted teams — `viewer` on one, `deployer` on the
    /// other → the union contains the deploy permission.
    #[tokio::test]
    async fn effective_permissions_unions_across_teams() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                grant_with_role(42, 10, "viewer"),
                grant_with_role(42, 20, "deployer"),
            ]])
            .append_query_results(vec![vec![
                member_with_role(10, 1, "viewer"),
                member_with_role(20, 1, "deployer"),
            ]])
            .into_connection();
        let checker = new_checker(db);

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("must resolve to Some(perms)");
        assert!(
            perms.contains(&Permission::DeploymentsCreate.to_string()),
            "union across teams must include the deployer-team's deploy permission"
        );
    }

    /// Fail-closed: a DB error on the grants query propagates as `Err`,
    /// never a silent `Ok(None)` (unrestricted) or `Ok(Some(vec![]))`.
    #[tokio::test]
    async fn effective_permissions_db_error_propagates_err() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("simulated failure".into())])
            .into_connection();
        let checker = new_checker(db);

        assert!(checker.effective_project_permissions(1, 42).await.is_err());
    }

    /// `custom_role_id` set → the custom role's permission set is used,
    /// not the fixed `member.role ∩ grant.role` mapping.
    #[tokio::test]
    async fn effective_permissions_uses_custom_role_when_assigned() {
        let mut member = member_with_role(10, 1, "viewer");
        member.custom_role_id = Some(5);
        // A permission the "viewer" fixed set does NOT contain, so success
        // can only mean the custom-role branch actually ran.
        let custom_only_perm = Permission::WebhooksWrite;
        assert!(
            !fixed_role_permissions(TeamRole::Viewer).contains(&custom_only_perm),
            "test fixture invariant: WebhooksWrite must not be in the viewer set"
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "viewer")]])
            .append_query_results(vec![vec![member]])
            .into_connection();
        let checker = TeamProjectAccessChecker::new(
            Arc::new(db),
            Arc::new(StubCustomRoleService(Some(vec![custom_only_perm]))),
        );

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("must resolve to Some(perms)");
        assert_eq!(
            perms,
            vec![custom_only_perm.to_string()],
            "the custom role's set must be used verbatim, not intersected with the fixed mapping"
        );
    }

    /// `custom_role_id` set but the role resolved to nothing (e.g. it was
    /// concurrently emptied) → fall back to the fixed-role intersection
    /// rather than silently granting zero permissions.
    #[tokio::test]
    async fn effective_permissions_falls_back_when_custom_role_resolves_none() {
        let mut member = member_with_role(10, 1, "viewer");
        member.custom_role_id = Some(5);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "viewer")]])
            .append_query_results(vec![vec![member]])
            .into_connection();
        let checker =
            TeamProjectAccessChecker::new(Arc::new(db), Arc::new(StubCustomRoleService(None)));

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("must resolve to Some(perms)");
        let viewer_set: std::collections::HashSet<String> =
            fixed_role_permissions(TeamRole::Viewer)
                .into_iter()
                .map(|p| p.to_string())
                .collect();
        let resolved_set: std::collections::HashSet<String> = perms.into_iter().collect();
        assert_eq!(resolved_set, viewer_set);
    }

    /// Editing a custom role's permissions must be visible on the next
    /// resolution without waiting for the TTL — `invalidate_all` must
    /// clear the permissions cache, not just the binary-access cache.
    #[tokio::test]
    async fn invalidate_all_clears_permissions_cache_too() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .append_query_results(vec![Vec::<project_team_access::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(
            checker.effective_project_permissions(1, 42).await.unwrap(),
            None
        );

        checker.invalidate_all();

        // If the permissions cache weren't cleared, this would panic on
        // exhausted MockDatabase results instead of re-querying.
        assert_eq!(
            checker.effective_project_permissions(1, 42).await.unwrap(),
            None
        );
    }
}
