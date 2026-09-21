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
    deploy_requires_instance_admin, granted_project_deploy_reason, granted_project_exec_reason,
    process_grant, DeployCaller, DockerSocketGrant, DOCKER_SOCKET_PROJECTS_ENV,
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

/// Refuse a shell inside a granted project's container unless the caller is an
/// instance admin.
///
/// A running granted container already has `/var/run/docker.sock` bound, so a
/// shell in it can drive the engine: the same host root a malicious deploy
/// would have obtained, reached without deploying anything. `ContainersExec`
/// on its own is therefore not sufficient for a declared project.
///
/// Unlike [`guard_deploy`] this **is** the enforcement point — there is no
/// builder or planner downstream to catch an omission — so it lives in the one
/// helper both exec routes already funnel through
/// (`container_exec::verify_container_exec_access`), not in each handler.
pub(crate) fn guard_exec(project_slug: &str, auth: &AuthContext) -> Result<(), Problem> {
    guard_exec_against(process_grant(), project_slug, auth)
}

/// [`guard_exec`] with the grant injected, for the same reason as
/// [`guard_deploy_against`].
///
/// Reuses `deploy_requires_instance_admin` deliberately: entering a container
/// that already holds the socket and deploying one that will are the same
/// privilege, so they must never be able to disagree about who may do it.
/// `DeployCaller::Platform` cannot occur here — an exec always has a request
/// behind it — so the shared predicate does not widen this check.
fn guard_exec_against(
    grant: &DockerSocketGrant,
    project_slug: &str,
    auth: &AuthContext,
) -> Result<(), Problem> {
    if !deploy_requires_instance_admin(grant, project_slug, deploy_caller(auth)) {
        return Ok(());
    }
    warn!(
        slug = %project_slug,
        user_id = auth.user_id(),
        env = DOCKER_SOCKET_PROJECTS_ENV,
        "Refused a non-admin exec into a project that holds host Docker access (ADR 045)"
    );
    Err(problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Host Docker Access Exec Requires An Admin")
        .with_detail(granted_project_exec_reason(project_slug)))
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

    /// A running granted container already has the socket bound, so a shell in
    /// it reaches the same host root a malicious deploy would have — without
    /// deploying anything. `ContainersExec` alone must not be enough.
    #[test]
    fn exec_into_a_declared_project_is_admin_only() {
        let problem = guard_exec_against(&granted(), "node-daemon", &auth(Role::User))
            .expect_err("a non-admin must not get a shell in a host-root container");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        for role in [Role::Admin, Role::PlatformAdmin] {
            assert!(
                guard_exec_against(&granted(), "node-daemon", &auth(role.clone())).is_ok(),
                "{role:?} is an instance admin and may exec into a granted project"
            );
        }
        for grant in [granted(), DockerSocketGrant::default()] {
            for role in [Role::User, Role::Admin] {
                assert!(
                    guard_exec_against(&grant, "my-app", &auth(role.clone())).is_ok(),
                    "{role:?} exec'ing into an undeclared project must be untouched by ADR 045"
                );
            }
        }
    }

    /// The two refusals must not read alike. Nothing is being deployed here,
    /// so "ask an admin to deploy it" would be the wrong instruction — and the
    /// operator reading it has nobody to ask what it meant.
    #[test]
    fn the_exec_refusal_explains_something_different_from_the_deploy_refusal() {
        let detail = |problem: Problem| {
            problem
                .body
                .get("detail")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let exec = guard_exec_against(&granted(), "node-daemon", &auth(Role::User))
            .expect_err("expected an exec refusal");
        let deploy = guard_deploy_against(&granted(), "node-daemon", &auth(Role::User))
            .expect_err("expected a deploy refusal");
        assert_ne!(exec.body.get("title"), deploy.body.get("title"));
        let exec_detail = detail(exec);
        assert_ne!(exec_detail, detail(deploy));
        assert!(exec_detail.contains("ADR 045"), "{exec_detail}");
        assert!(
            exec_detail.contains(DOCKER_SOCKET_PROJECTS_ENV),
            "{exec_detail}"
        );
        assert!(
            exec_detail.contains("Ask an admin to run the command"),
            "{exec_detail}"
        );
    }
}
