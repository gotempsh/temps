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
//! (`owner|admin|deployer|viewer`), so a project role meant to be more
//! restrictive than another has no effect once the binary answer is yes. `effective_project_permissions` closes that gap:
//! it resolves the exact permission set a user holds *within* a project,
//! so `project_permission_guard!` can deny specific actions the coarser
//! check would allow.
//!
//! Resolution: for each team the user belongs to that has a grant on the
//! project, take the intersection of `team_members.role` and
//! `project_team_access.role`'s fixed permission sets
//! ([`fixed_role_permissions`]), then union across teams. Intersecting the
//! two role columns is deliberately *narrowing*: it cannot leak privilege
//! regardless of which column an admin actually populated.
//!
//! A registered [`MembershipPermissionResolver`] may replace the
//! *membership* half of that intersection for memberships it recognises —
//! but the grant half still applies, so a plugin can only narrow a member
//! below their team's access, never widen them past it.
//!
//! # Caching
//!
//! `moka::future::Cache<(user_id, project_id), bool>`, 60 s TTL,
//! 10 000-entry capacity, for the binary check. A second cache of the same
//! shape holds `Option<Vec<String>>` for the permission resolution.
//! Explicit invalidation (not just TTL) is required after writes. It is
//! best-effort rather than a hard guarantee: a check that read the DB
//! before a revocation but inserts into the cache after the eviction sweep
//! can still serve a stale allow until the TTL expires, and invalidation is
//! process-local, so on a multi-node control plane other nodes converge on
//! the TTL. Both windows are bounded by `CACHE_TTL`.
//!
//! - [`invalidate_project`](TeamProjectAccessChecker::invalidate_project) —
//!   `grant_project_access` / `revoke_project_access`
//! - [`invalidate_user`](TeamProjectAccessChecker::invalidate_user) —
//!   `add_member` / `remove_member` / `set_member_role`
//! - [`invalidate_all`](TeamProjectAccessChecker::invalidate_all) —
//!   `delete_team`, and any plugin-side change to what a
//!   [`MembershipPermissionResolver`] answers, where the set of affected
//!   `(user, project)` pairs is unbounded.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use moka::future::Cache;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect,
};
use temps_auth::permissions::Permission;
use temps_core::{MembershipPermissionResolver, ProjectAccessChecker};
use temps_entities::{project_team_access, team_members, users};

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
        "A registered MembershipPermissionResolver failed for team member {team_member_id} \
         (user {user_id}, project {project_id}): {source}"
    )]
    MembershipResolution {
        team_member_id: i32,
        user_id: i32,
        project_id: i32,
        source: Box<dyn std::error::Error + Send + Sync>,
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
    /// Populated once during route configuration, by which point every
    /// plugin's services are registered. `None` inside the `OnceLock` on a
    /// stock binary, where resolution is purely from the fixed roles;
    /// unset entirely before configuration, which reads the same way.
    membership_resolver: OnceLock<Option<Arc<dyn MembershipPermissionResolver>>>,
}

impl TeamProjectAccessChecker {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
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
            membership_resolver: OnceLock::new(),
        }
    }

    /// Attaches the optional [`MembershipPermissionResolver`].
    ///
    /// Called during route configuration rather than service registration:
    /// a resolver is registered by another plugin, and plugin registration
    /// order is not guaranteed, but *all* services are registered before
    /// *any* plugin configures routes. Resolving it earlier would silently
    /// miss a resolver whose plugin happens to load later.
    ///
    /// Idempotent — a second call is ignored.
    pub fn set_membership_resolver(&self, resolver: Option<Arc<dyn MembershipPermissionResolver>>) {
        let _ = self.membership_resolver.set(resolver);
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

    /// The registered [`MembershipPermissionResolver`], if any.
    ///
    /// Exposed so the service can run [`permissions_from_grants`] against
    /// its own transaction — resolving a caller's permissions from the very
    /// rows it holds locked, instead of from a second, unlocked read.
    pub(crate) fn membership_resolver(&self) -> Option<&Arc<dyn MembershipPermissionResolver>> {
        self.membership_resolver.get().and_then(|r| r.as_ref())
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
            // A soft-deleted user keeps their membership rows (deletion sets
            // `deleted_at`, it does not remove the row, so the FK cascade
            // never fires). Offboarding must revoke project access here even
            // if a credential outlives the account.
            .inner_join(users::Entity)
            .filter(users::Column::DeletedAt.is_null())
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
        // The user's teams first — bounded by how many teams one person is
        // in, which is small.
        let memberships = team_members::Entity::find()
            .filter(team_members::Column::UserId.eq(user_id))
            .inner_join(users::Entity)
            .filter(users::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::MembershipQuery {
                user_id,
                // Not scoped to one project — 0 reads as "all projects" in
                // the error message context.
                project_id: 0,
                source,
            })?;
        let user_team_ids: Vec<i32> = memberships.into_iter().map(|m| m.team_id).collect();

        // Two narrow queries instead of one wide one. The previous shape
        // read every (project_id, team_id) pair on the instance — the whole
        // grants table — and differenced them in memory, on a path that runs
        // for `GET /projects` *and* `GET /projects/statistics`, i.e. twice
        // per dashboard load. That is exactly the unbounded read CLAUDE.md
        // forbids, and the comment here used to claim it was bounded.
        //
        // Now: one DISTINCT column of gated project ids (one row per gated
        // project, not per grant), and one DISTINCT column of the projects
        // this user can reach. The result set is inherently O(gated
        // projects) — it is the answer — but nothing larger is transferred.
        #[derive(sea_orm::FromQueryResult)]
        struct ProjectId {
            project_id: i32,
        }

        let gated: Vec<ProjectId> = project_team_access::Entity::find()
            .select_only()
            .column(project_team_access::Column::ProjectId)
            .distinct()
            .into_model::<ProjectId>()
            .all(self.db.as_ref())
            .await
            .map_err(|source| CheckerError::AccessGrantQuery {
                project_id: 0,
                source,
            })?;

        if gated.is_empty() {
            return Ok(Vec::new());
        }

        // A project is hidden only if the user is in NONE of the teams
        // granted on it, so an empty team list hides every gated project.
        let allowed: std::collections::HashSet<i32> = if user_team_ids.is_empty() {
            std::collections::HashSet::new()
        } else {
            project_team_access::Entity::find()
                .select_only()
                .column(project_team_access::Column::ProjectId)
                .filter(project_team_access::Column::TeamId.is_in(user_team_ids))
                .distinct()
                .into_model::<ProjectId>()
                .all(self.db.as_ref())
                .await
                .map_err(|source| CheckerError::AccessGrantQuery {
                    project_id: 0,
                    source,
                })?
                .into_iter()
                .map(|p| p.project_id)
                .collect()
        };

        Ok(gated
            .into_iter()
            .map(|p| p.project_id)
            .filter(|id| !allowed.contains(id))
            .collect())
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

        permissions_from_grants(
            self.db.as_ref(),
            self.membership_resolver(),
            user_id,
            project_id,
            &grants,
        )
        .await
        .map(Some)
    }
}

/// Resolves what `user_id` may do on `project_id`, given that project's
/// access grants.
///
/// Generic over the connection so it can run either on the checker's own
/// pool (the cached read path) or **inside a caller's transaction** — the
/// service authorizes grant mutations by calling this with the same
/// transaction that holds those grant rows locked `FOR UPDATE`, so the
/// permissions it checks and the rows it is about to write cannot disagree.
///
/// `grants` must be this project's full grant set; passing a subset would
/// silently narrow the answer. The caller supplies it rather than this
/// function re-reading it, precisely so the locked rows are the ones used.
pub(crate) async fn permissions_from_grants<C: ConnectionTrait>(
    conn: &C,
    resolver: Option<&Arc<dyn MembershipPermissionResolver>>,
    user_id: i32,
    project_id: i32,
    grants: &[project_team_access::Model],
) -> Result<Vec<String>, CheckerError> {
    // No grants at all → the project is ungated and nobody holds
    // project-scoped permissions on it. Returning early also avoids
    // building an `IN ()` predicate from an empty id list.
    if grants.is_empty() {
        return Ok(Vec::new());
    }

    let granted_team_ids: Vec<i32> = grants.iter().map(|g| g.team_id).collect();

    // The user's membership rows on those granted teams. Unlike the binary
    // check we need the full rows (for the role), and there can be more
    // than one — a user may belong to several teams granted the same
    // project. Soft-deleted users are excluded: deletion does not remove
    // the membership row, so without this join a deleted account would
    // keep resolving to live permissions.
    let memberships = team_members::Entity::find()
        .filter(team_members::Column::UserId.eq(user_id))
        .filter(team_members::Column::TeamId.is_in(granted_team_ids))
        .inner_join(users::Entity)
        .filter(users::Column::DeletedAt.is_null())
        .all(conn)
        .await
        .map_err(|source| CheckerError::MembershipQuery {
            user_id,
            project_id,
            source,
        })?;

    // Not a member of any granted team → holds nothing here. This is the
    // finer-grained expression of the binary check's `false`.
    if memberships.is_empty() {
        return Ok(Vec::new());
    }

    let mut effective: std::collections::HashSet<String> = std::collections::HashSet::new();

    for membership in &memberships {
        // Every membership.team_id came from granted_team_ids, which was
        // built from these exact grants, so a missing match is impossible
        // — but resolve it without panicking anyway.
        let Some(grant) = grants.iter().find(|g| g.team_id == membership.team_id) else {
            continue;
        };

        // What the member's own role contributes. A registered resolver
        // may replace this half; the grant half below still applies.
        let member_side: Vec<Permission> = match resolve_member_side(resolver, membership).await {
            Ok(Some(perms)) => perms,
            Ok(None) => fixed_role_permissions_or_empty(&membership.role),
            Err(source) => {
                return Err(CheckerError::MembershipResolution {
                    team_member_id: membership.id,
                    user_id,
                    project_id,
                    source,
                })
            }
        };

        // The team's grant on this project is a hard ceiling: whatever the
        // member's side says, they cannot exceed what their team was
        // actually granted here.
        let grant_side: std::collections::HashSet<Permission> =
            fixed_role_permissions_or_empty(&grant.role)
                .into_iter()
                .collect();

        effective.extend(
            member_side
                .into_iter()
                .filter(|p| grant_side.contains(p))
                .map(|p| p.to_string()),
        );
    }

    Ok(effective.into_iter().collect())
}

/// Asks a registered [`MembershipPermissionResolver`], if any, what this
/// membership's own role should resolve to. `Ok(None)` means "use the
/// fixed role" — also the answer on a binary with no resolver registered.
async fn resolve_member_side(
    resolver: Option<&Arc<dyn MembershipPermissionResolver>>,
    membership: &team_members::Model,
) -> Result<Option<Vec<Permission>>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    let Some(strings) = resolver.membership_permissions(membership.id).await? else {
        return Ok(None);
    };
    // Unrecognised strings are dropped, not errored: narrowing is the safe
    // direction if a resolver is ahead of this binary's `Permission` enum.
    Ok(Some(
        strings
            .into_iter()
            .filter_map(|p| {
                let parsed = Permission::from_str(&p);
                if parsed.is_none() {
                    tracing::warn!(
                        team_member_id = membership.id,
                        permission = %p,
                        "teams: resolver returned an unrecognized permission — dropping it"
                    );
                }
                parsed
            })
            .collect(),
    ))
}

/// Fixed-role permission set for a stored role string.
///
/// A malformed value (which should not occur — both role columns are only
/// ever written via `TeamRole::as_str()`) contributes nothing rather than
/// erroring the whole resolution, so one bad row can't deny a user the
/// access their other memberships legitimately grant.
fn fixed_role_permissions_or_empty(role: &str) -> Vec<Permission> {
    use std::str::FromStr;
    use temps_entities::TeamRole;

    match TeamRole::from_str(role) {
        Ok(role) => fixed_role_permissions(role),
        Err(_) => {
            tracing::warn!(
                role,
                "teams: unparsable role string during permission resolution — contributing \
                 no permissions"
            );
            Vec::new()
        }
    }
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
            added_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// A resolver that answers with a fixed set for every membership,
    /// standing in for a plugin that has an opinion about this member.
    struct StubResolver(Vec<Permission>);

    #[async_trait]
    impl MembershipPermissionResolver for StubResolver {
        async fn membership_permissions(
            &self,
            _team_member_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(self.0.iter().map(|p| p.to_string()).collect()))
        }
    }

    /// A resolver that always declines, so resolution falls through to the
    /// fixed role — the same path a binary with no resolver takes.
    struct DecliningResolver;

    #[async_trait]
    impl MembershipPermissionResolver for DecliningResolver {
        async fn membership_permissions(
            &self,
            _team_member_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(None)
        }
    }

    /// A resolver whose backing store is down.
    struct FailingResolver;

    #[async_trait]
    impl MembershipPermissionResolver for FailingResolver {
        async fn membership_permissions(
            &self,
            _team_member_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Err("resolver backend unavailable".into())
        }
    }

    /// The stock shape: no plugin registered.
    fn new_checker(db: DatabaseConnection) -> TeamProjectAccessChecker {
        TeamProjectAccessChecker::new(Arc::new(db))
    }

    /// A checker with a resolver attached, as `configure_routes` would.
    fn checker_with_resolver(
        db: DatabaseConnection,
        resolver: Arc<dyn MembershipPermissionResolver>,
    ) -> TeamProjectAccessChecker {
        let checker = TeamProjectAccessChecker::new(Arc::new(db));
        checker.set_membership_resolver(Some(resolver));
        checker
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
            // memberships first, then grants
            .append_query_results(vec![Vec::<team_members::Model>::new()])
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
        // Three queries now: the user's memberships, the DISTINCT gated
        // project ids, then the DISTINCT ids the user can reach.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_member(10, 1)]])
            .append_query_results(vec![vec![
                sample_grant(42, 10), // gated
                sample_grant(99, 20), // gated
            ]])
            // user is in team 10 → reaches 42; not in team 20 → 99 hidden
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(checker.hidden_project_ids(1).await.unwrap(), Some(vec![99]));
    }

    /// A project granted to two teams is visible if the user is in either
    /// — the per-project answer is a union across its grants, not a
    /// per-row decision.
    /// A user in no teams reaches nothing gated. The second query is
    /// skipped entirely (an empty `IN ()` has no meaning), so only two
    /// results are queued — if it ran, MockDatabase would panic.
    #[tokio::test]
    async fn hidden_project_ids_hides_everything_gated_from_a_user_with_no_teams() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .append_query_results(vec![vec![sample_grant(42, 10), sample_grant(99, 20)]])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(
            checker.hidden_project_ids(1).await.unwrap(),
            Some(vec![42, 99])
        );
    }

    #[tokio::test]
    async fn hidden_project_ids_unions_grants_on_the_same_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_member(20, 1)]])
            .append_query_results(vec![vec![sample_grant(42, 10), sample_grant(42, 20)]])
            // Reached via team 20, even though team 10 also holds project 42.
            .append_query_results(vec![vec![sample_grant(42, 20)]])
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
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .append_query_results(vec![vec![sample_grant(42, 10)]])
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

    /// A registered resolver replaces the *membership* half of the
    /// intersection: a member whose resolver answer is narrower than their
    /// fixed role gets the narrower set.
    #[tokio::test]
    async fn resolver_replaces_the_membership_half() {
        // Team is granted `admin`, member's fixed role is `admin`, but the
        // resolver says they only hold one permission here.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "admin")]])
            .append_query_results(vec![vec![member_with_role(10, 1, "admin")]])
            .into_connection();
        let checker = checker_with_resolver(
            db,
            Arc::new(StubResolver(vec![Permission::DeploymentsRead])),
        );

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("must resolve to Some(perms)");
        assert_eq!(perms, vec![Permission::DeploymentsRead.to_string()]);
    }

    /// The grant role is a hard ceiling on whatever a resolver answers.
    /// A plugin must not be able to hand a member permissions the team was
    /// never granted on this project.
    #[tokio::test]
    async fn resolver_cannot_exceed_the_team_grant() {
        let over_reach = Permission::DeploymentsCreate;
        assert!(
            !fixed_role_permissions(TeamRole::Viewer).contains(&over_reach),
            "fixture invariant: DeploymentsCreate must not be in the viewer set"
        );

        // Team holds only `viewer` on this project, but the resolver tries
        // to grant a deploy permission.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "viewer")]])
            .append_query_results(vec![vec![member_with_role(10, 1, "admin")]])
            .into_connection();
        let checker = checker_with_resolver(
            db,
            Arc::new(StubResolver(vec![over_reach, Permission::DeploymentsRead])),
        );

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("must resolve to Some(perms)");
        assert!(
            !perms.contains(&over_reach.to_string()),
            "the team's viewer grant must cap what the resolver can hand out"
        );
        assert!(
            perms.contains(&Permission::DeploymentsRead.to_string()),
            "permissions inside the grant ceiling still come through"
        );
    }

    /// A resolver that declines falls through to the fixed role — same
    /// result as a binary with no resolver registered.
    #[tokio::test]
    async fn declining_resolver_falls_through_to_fixed_role() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "deployer")]])
            .append_query_results(vec![vec![member_with_role(10, 1, "deployer")]])
            .into_connection();
        let checker = checker_with_resolver(db, Arc::new(DecliningResolver));

        let perms = checker
            .effective_project_permissions(1, 42)
            .await
            .unwrap()
            .expect("must resolve to Some(perms)");
        assert!(perms.contains(&Permission::DeploymentsCreate.to_string()));
    }

    /// Fail-closed: a resolver error propagates rather than silently
    /// falling back to the fixed role, which could widen a member the
    /// plugin meant to narrow.
    #[tokio::test]
    async fn failing_resolver_propagates_err() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "admin")]])
            .append_query_results(vec![vec![member_with_role(10, 1, "admin")]])
            .into_connection();
        let checker = checker_with_resolver(db, Arc::new(FailingResolver));

        assert!(
            checker.effective_project_permissions(1, 42).await.is_err(),
            "a resolver failure must not fall back to the fixed role"
        );
    }

    /// A plugin-side change to what a resolver answers must be visible on
    /// the next resolution without waiting for the TTL — `invalidate_all`
    /// must clear the permissions cache, not just the binary-access cache.
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

    // -------------------------------------------------------------------
    // Soft-deleted users
    // -------------------------------------------------------------------

    /// Deleting a user sets `deleted_at`; it does not remove the row, so
    /// the FK cascade never fires and their `team_members` rows survive.
    /// Authorization must not treat them as a member — otherwise a
    /// credential that outlives the account keeps its project access.
    /// The membership query joins `users` and filters `deleted_at IS NULL`,
    /// so the DB returns no membership row for a deleted user.
    #[tokio::test]
    async fn soft_deleted_user_is_denied_even_with_a_surviving_membership_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_grant(42, 10)]])
            // The join filters the deleted user out, so the membership
            // lookup comes back empty.
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert!(
            !checker.user_can_access_project(1, 42).await.unwrap(),
            "a soft-deleted user must not retain team-derived project access"
        );
    }

    /// Same for the finer-grained resolution: no membership → holds
    /// nothing here, not "unrestricted".
    #[tokio::test]
    async fn soft_deleted_user_resolves_to_no_permissions() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![grant_with_role(42, 10, "admin")]])
            .append_query_results(vec![Vec::<team_members::Model>::new()])
            .into_connection();
        let checker = new_checker(db);

        assert_eq!(
            checker.effective_project_permissions(1, 42).await.unwrap(),
            Some(Vec::new())
        );
    }
}
