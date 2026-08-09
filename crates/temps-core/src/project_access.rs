//! Extension point for team-based project access enforcement.
//!
//! See ADR 028 for the full design rationale.

use async_trait::async_trait;

/// OSS extension point for team-based project access enforcement.
///
/// OSS itself never implements this trait and has no concept of what
/// governs project access — it just asks "may this user access this
/// project?" A plugin (e.g. one implementing team-based project access)
/// registers an implementation via `context.register_service(checker)`.
/// Callers retrieve it with `context.get_service::<dyn ProjectAccessChecker>()`
/// — never `require_service` — so the OSS binary is a strict no-op when
/// nothing is registered, identical in spirit to [`crate::DeploymentGate`].
///
/// The check is **fail-closed** on infrastructure errors: if the implementation
/// cannot reach its database it must return `Err`, never silently allow.
/// This is distinct from the "fail-open when unconfigured" behaviour, which
/// is a semantic property of the checker's own business logic when the plugin
/// is present but no access grants have been configured yet.
///
/// # Semantics ("project-level access-grant" model)
///
/// The recommended implementation follows a two-step check:
///
/// 1. Does this project have any team-access grant rows? If **no** → allow
///    (project is unrestricted; adding the first grant is the action that
///    makes it team-gated).
/// 2. Does this user belong to at least one team that has a grant for this
///    project? If **yes** → allow. Otherwise → deny.
///
/// This ensures instances with the plugin present but no grants configured
/// behave identically to a plain OSS binary (fail-open-when-unconfigured).
///
/// # Admin bypass
///
/// The [`project_access_guard!`](temps_auth::project_access_guard) macro
/// short-circuits before calling this trait when the caller holds an
/// instance-wide Admin or PlatformAdmin role. Implementations are not
/// required to duplicate that check.
#[async_trait]
pub trait ProjectAccessChecker: Send + Sync {
    /// Returns `Ok(true)` if `user_id` may access `project_id`, `Ok(false)`
    /// if they may not, or `Err` if the check could not be completed
    /// (infrastructure failure — the caller treats this as a denial).
    async fn user_can_access_project(
        &self,
        user_id: i32,
        project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;

    /// Returns the set of permission strings (matching
    /// `temps_auth::permissions::Permission::to_string()`, e.g.
    /// `"deployments:create"`) `user_id` holds *within* `project_id`, for
    /// callers that need a specific-action check rather than the coarse
    /// "can touch this project at all" answer `user_can_access_project`
    /// gives. See [`project_permission_guard!`](temps_auth::project_permission_guard).
    ///
    /// # Semantics
    ///
    /// - `Ok(None)` — this checker has no project-scoped, per-permission
    ///   opinion (the default): either no team-scoped permission model
    ///   applies, or the project has zero access grants. The guard falls
    ///   through to the instance-wide permission check only — this is what
    ///   keeps a binary with no checker registered, or a binary whose
    ///   checker hasn't overridden this method yet, behaviourally unchanged.
    /// - `Ok(Some(perms))` — the exact permission strings the user holds in
    ///   this project. An **empty** vec means "holds nothing here" — every
    ///   project-scoped permission check denies, which is how a
    ///   non-member's binary deny is expressed at this finer granularity.
    /// - `Err(_)` — infrastructure failure. The caller treats this as a
    ///   denial (fail-closed), never a silent allow — identical contract to
    ///   `user_can_access_project`.
    ///
    /// Defaults to `Ok(None)` so existing implementations of this trait
    /// (compiled against an older version of it) keep behaving exactly as
    /// they do today without needing to implement this method.
    async fn effective_project_permissions(
        &self,
        _user_id: i32,
        _project_id: i32,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(None)
    }

    /// Returns the project ids `user_id` must **not** see in listings —
    /// projects that are access-gated and that this user has not been
    /// granted.
    ///
    /// Per-resource guards alone are not enough for a multi-tenant
    /// instance: without list filtering, every user still sees every
    /// project's *name* in `GET /projects` and only discovers the denial
    /// on click. On an instance where different clients or teams are meant
    /// to be isolated, the list of project names is itself something worth
    /// keeping separate.
    ///
    /// The "hidden" (rather than "visible") direction is deliberate: the
    /// returned set is bounded by the number of *gated* projects, which is
    /// zero on an instance that hasn't configured any access grants, so
    /// the default posture costs nothing and cannot accidentally hide a
    /// project that no rule was written for.
    ///
    /// # Semantics
    ///
    /// - `Ok(None)` — no opinion; callers list everything, unchanged.
    /// - `Ok(Some(ids))` — exclude these ids. An empty vec means "nothing
    ///   is hidden from this user".
    /// - `Err(_)` — infrastructure failure. Callers must fail the request
    ///   rather than fall back to an unfiltered list, which would leak
    ///   exactly what this method exists to hide.
    ///
    /// Callers are responsible for skipping this for instance
    /// administrators, matching the admin bypass in
    /// [`project_access_guard!`](temps_auth::project_access_guard).
    async fn hidden_project_ids(
        &self,
        _user_id: i32,
    ) -> Result<Option<Vec<i32>>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(None)
    }

    /// Flushes any cache this checker keeps for `effective_project_permissions`
    /// (and, transitively, for whatever a registered
    /// [`MembershipPermissionResolver`] answers).
    ///
    /// A checker that caches per-`(user, project)` permission sets — as the
    /// recommended implementation does, since `effective_project_permissions`
    /// is on the hot path of every `project_permission_guard!` call — becomes
    /// a second cache layer sitting in front of a
    /// [`MembershipPermissionResolver`] plugin's own cache. When that plugin's
    /// answer for a membership changes (a role definition is edited, a
    /// membership's role assignment is created/updated/deleted), the plugin
    /// can invalidate its own cache, but it holds only `Arc<dyn
    /// ProjectAccessChecker>` — the trait, not this checker's concrete type —
    /// so it has no way to reach this cache without this method.
    ///
    /// Defaults to a no-op so existing implementations of this trait
    /// (compiled against an older version of it, or a checker that caches
    /// nothing) keep behaving exactly as they do today.
    ///
    /// Deliberately synchronous and coarse (whole-cache flush, no
    /// per-project or per-user granularity): the moka-backed reference
    /// implementation's own `invalidate_all` is lazy and cheap to call from a
    /// resolver plugin's write path without needing `.await` there, and a
    /// resolver plugin has no way to know which `(user, project)` pairs are
    /// affected by e.g. a role permission-set edit — that set is unbounded
    /// from the resolver's point of view, so a full flush is the only
    /// correct granularity available to it.
    fn invalidate_permissions_cache(&self) {}
}

/// Optional extension point for overriding what a single team membership
/// resolves to, in place of the fixed role stored on the membership row.
///
/// Core knows four fixed roles (`owner`/`admin`/`deployer`/`viewer`) and
/// resolves a member's permissions inside a project from those. A plugin
/// (e.g. one implementing admin-defined named permission sets) registers
/// an implementation via `context.register_service(resolver)` to answer
/// differently for memberships it knows about. Retrieved with
/// `context.get_service::<dyn MembershipPermissionResolver>()` — never
/// `require_service` — so a binary with nothing registered resolves purely
/// from the fixed roles, identical in spirit to [`crate::DeploymentGate`].
///
/// # The answer is a ceiling, not a grant
///
/// Whatever this returns is **intersected** with the permission set the
/// team's own grant on the project allows. It can therefore only ever
/// narrow a member below their team's access, never widen them past it.
/// That invariant belongs to core, not to the implementation: a team
/// granted `viewer` on a project must not end up with a member who can
/// deploy to it, no matter what a plugin says.
#[async_trait]
pub trait MembershipPermissionResolver: Send + Sync {
    /// Returns `Ok(Some(perms))` — permission strings (matching
    /// `Permission::to_string()`, e.g. `"deployments:create"`) to use for
    /// this membership instead of its fixed role — or `Ok(None)` to fall
    /// through to the fixed role.
    ///
    /// `Err(_)` is an infrastructure failure and is propagated: the caller
    /// fails the check rather than silently falling back, since falling
    /// back could widen a member the plugin meant to narrow.
    async fn membership_permissions(
        &self,
        team_member_id: i32,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>>;
}
