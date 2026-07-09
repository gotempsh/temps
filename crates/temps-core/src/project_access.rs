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
}
