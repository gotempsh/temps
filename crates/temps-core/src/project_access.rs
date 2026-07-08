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
}
