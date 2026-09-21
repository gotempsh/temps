// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request-side ADR-045 refusals for the deployment handlers.
//!
//! **This module is not the enforcement point, and must never be the only
//! check.** Enforcement lives where every deployment is structurally forced
//! through — `WorkflowPlanner::create_deployment_jobs` for the queue path and
//! `DeployImageJobBuilder::build` for the direct-build path — both of which
//! take the [`DeployCaller`] as a required argument, exactly as they take
//! `project_slug`. Handler checks are what was missed when this rule was first
//! added: three remote-deployment handlers built the deployment row and its
//! jobs themselves and never touched the gated service methods, so a check
//! that lived only in handlers was a check a new route could omit.
//!
//! What this module adds is the part enforcement cannot: a 403 that explains
//! itself *before* a deployment row is persisted, instead of a 500 from a
//! planner that refused after the row already existed. Forgetting to call
//! [`guard_deploy`] in a new handler therefore costs a worse error message —
//! never host root.

use axum::http::StatusCode;
use temps_auth::AuthContext;
use temps_core::docker_socket_grant::{
    deploy_requires_instance_admin, granted_project_deploy_reason, process_grant, DeployCaller,
    DockerSocketGrant, DOCKER_SOCKET_PROJECTS_ENV,
};
use temps_core::problemdetails::{self, Problem};
use tracing::warn;

/// The deploy authority of the principal behind this request (ADR 045).
///
/// The one place a handler turns `auth` into the value the planner and the
/// builder require, so every deployment route derives it identically.
pub(crate) fn deploy_caller(auth: &AuthContext) -> DeployCaller {
    DeployCaller::from_instance_admin(auth.is_instance_admin())
}

/// Refuse a deployment of a project that holds host Docker access unless the
/// caller is an instance admin.
///
/// Call it straight after the project row is loaded and before anything is
/// written. 403, not 400: an admin sending the identical request succeeds.
pub(crate) fn guard_deploy(project_slug: &str, auth: &AuthContext) -> Result<(), Problem> {
    guard_deploy_against(process_grant(), project_slug, auth)
}

/// [`guard_deploy`] with the grant injected.
///
/// Pure, mirroring `ProjectService::guard_reserved_slug_against`: the process
/// grant is a `OnceLock` frozen at first use, so the rule would otherwise be
/// untestable without mutating process-global environment state from parallel
/// tests.
fn guard_deploy_against(
    grant: &DockerSocketGrant,
    project_slug: &str,
    auth: &AuthContext,
) -> Result<(), Problem> {
    if !deploy_requires_instance_admin(grant, project_slug, deploy_caller(auth)) {
        return Ok(());
    }
    // No audit event covers a write that never happened, and this one matters:
    // it is an attempt to run code as host root. Logged with the principal so
    // an operator reviewing host logs can see who tried.
    warn!(
        slug = %project_slug,
        user_id = auth.user_id(),
        env = DOCKER_SOCKET_PROJECTS_ENV,
        "Refused a non-admin deployment of a project that holds host Docker access (ADR 045)"
    );
    Err(problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Host Docker Access Deployment Requires An Admin")
        .with_detail(granted_project_deploy_reason(project_slug)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::Role;

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 7,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: Some("hashed".to_string()),
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn auth(role: Role) -> AuthContext {
        AuthContext::new_session(user(), role)
    }

    fn granted() -> DockerSocketGrant {
        DockerSocketGrant::parse(Some("node-daemon"))
    }

    /// The bypass this guard exists to close: `Role::User` holds
    /// `DeploymentsCreate` by default, so permission alone would let an
    /// ordinary account deploy its own image and command into a container
    /// that receives `/var/run/docker.sock`.
    #[test]
    fn an_ordinary_user_is_refused_on_a_declared_project() {
        let problem = guard_deploy_against(&granted(), "node-daemon", &auth(Role::User))
            .expect_err("a non-admin must not deploy a project that holds host Docker access");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }

    /// Both roles the instance-admin check accepts, so a future change to
    /// `is_instance_admin` cannot silently narrow this to one of them.
    #[test]
    fn an_instance_admin_is_allowed_on_a_declared_project() {
        for role in [Role::Admin, Role::PlatformAdmin] {
            assert!(
                guard_deploy_against(&granted(), "node-daemon", &auth(role.clone())).is_ok(),
                "{role:?} is an instance admin and may deploy a granted project"
            );
        }
    }

    /// Every other project on a host that *does* set the variable, and every
    /// project on the overwhelming majority of installs that never set it.
    #[test]
    fn an_undeclared_project_is_unaffected_for_either_role() {
        for grant in [granted(), DockerSocketGrant::default()] {
            for role in [Role::User, Role::Admin] {
                assert!(
                    guard_deploy_against(&grant, "my-app", &auth(role.clone())).is_ok(),
                    "{role:?} deploying an undeclared project must be untouched by ADR 045"
                );
            }
        }
    }

    /// The refusal has to be actionable on its own: the operator reading it is
    /// usually the person who set the variable.
    #[test]
    fn the_refusal_names_the_adr_the_variable_and_the_slug() {
        let problem = guard_deploy_against(&granted(), "node-daemon", &auth(Role::User))
            .expect_err("expected a refusal");
        let detail = problem
            .body
            .get("detail")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        assert!(detail.contains("ADR 045"), "{detail}");
        assert!(detail.contains(DOCKER_SOCKET_PROJECTS_ENV), "{detail}");
        assert!(detail.contains("node-daemon"), "{detail}");
    }
}
