// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use temps_auth::context::AuthContext;
use temps_auth::permissions::Role;
use temps_core::project_access::ProjectAccessChecker;
use tracing::error;

use crate::error::McpError;

/// Confine a deployment token to its bound project, independent of
/// `check_project_access`'s admin/checker logic below.
///
/// This is the MCP-crate equivalent of `temps_auth::project_scope_guard!`
/// (which can't be used directly here: it `return`s a `Problem`, not an
/// `McpError`, since it's designed for Axum handlers). It must run for
/// every tool that takes a `project_id`, called *before* or alongside
/// `check_project_access` — not folded into it — because
/// `check_project_access`'s deployment-token bypass returns `Ok(())`
/// immediately, which would otherwise skip tenant-boundary enforcement
/// entirely for the one principal type it exists to confine.
///
/// No-op (`Ok(())`) for user/API-key/session/CLI auth, matching
/// [`AuthContext::is_scoped_to_project`]'s semantics.
pub(crate) fn check_project_scope(auth: &AuthContext, project_id: i32) -> Result<(), McpError> {
    if auth.is_scoped_to_project(project_id) {
        Ok(())
    } else {
        Err(McpError::ProjectAccessDenied { project_id })
    }
}

/// Check whether `auth` may access `project_id` via the optional checker.
///
/// Mirrors the `resolve_hidden_projects` REST handler's admin / deployment-token
/// exemption: platform admins, instance admins, and deployment tokens are
/// allowed unconditionally without ever consulting the `ProjectAccessChecker`.
///
/// This function does NOT confine a deployment token to its bound project —
/// that is a distinct tenant-boundary check, enforced separately by
/// [`check_project_scope`], which every tool handler calls alongside this
/// function. Deployment tokens bypass `ProjectAccessChecker` here today only
/// because the current permission-mapping in `context.rs` excludes them from
/// `ProjectsRead`/`DeploymentsCreate` — an accident of that unrelated table,
/// not a guarantee. `check_project_scope` is what makes the bypass safe even
/// if that mapping changes.
///
/// - Admin bypass (`is_deployment_token`, `is_admin`, or `PlatformAdmin` role)
///   → return `Ok(())` immediately, checker is not called.
/// - `None` checker → no RBAC configured → allow (OSS default).
/// - `Ok(true)` from checker → explicitly allowed.
/// - `Ok(false)` from checker → denied.
/// - `Err(_)` from checker → infrastructure failure → fail closed (deny).
pub(crate) async fn check_project_access(
    checker: Option<&dyn ProjectAccessChecker>,
    auth: &AuthContext,
    project_id: i32,
) -> Result<(), McpError> {
    // Admin / deployment-token bypass: matches resolve_hidden_projects semantics.
    // Neither the checker nor the OSS-default path is consulted for these
    // principals — they see everything, unconditionally. Tenant confinement
    // for deployment tokens is enforced separately by `check_project_scope`.
    if auth.is_deployment_token() || auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(());
    }

    let Some(checker) = checker else {
        // No checker registered → OSS default: allow everything.
        return Ok(());
    };

    let user_id = auth.user_id();
    match checker.user_can_access_project(user_id, project_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(McpError::ProjectAccessDenied { project_id }),
        Err(e) => {
            // Fail closed: infrastructure failure must not silently widen access.
            error!(
                user_id,
                project_id, "MCP project access check failed (infra error): {}", e
            );
            Err(McpError::ProjectAccessDenied { project_id })
        }
    }
}

/// Check that `auth` holds `permission` **on** `project_id`, not only
/// instance-wide: the MCP equivalent of `temps_auth::project_permission_guard!`.
///
/// A team viewer can reach a project (so [`check_project_access`] passes) and
/// may still hold a global write permission through their instance role; the
/// project's own permission set is what must decide a write. Like the guard:
///
/// - deployment tokens, instance admins and platform admins bypass narrowing;
/// - no checker → allow (OSS default);
/// - `Ok(None)` → the checker has no permission-level opinion, so fall back to
///   [`check_project_access`];
/// - `Ok(Some(perms))` → `permission` must be in `perms`;
/// - `Err(_)` → fail closed.
///
/// The caller still checks the instance-wide permission and the deployment
/// token's project scope.
pub(crate) async fn check_project_permission(
    checker: Option<&dyn ProjectAccessChecker>,
    auth: &AuthContext,
    project_id: i32,
    permission: &temps_auth::permissions::Permission,
) -> Result<(), McpError> {
    if auth.is_deployment_token() || auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(());
    }
    let Some(checker) = checker else {
        return Ok(());
    };
    let user_id = auth.user_id();
    match checker
        .effective_project_permissions(user_id, project_id)
        .await
    {
        Ok(None) => check_project_access(Some(checker), auth, project_id).await,
        Ok(Some(perms)) => {
            let required = permission.to_string();
            if perms.iter().any(|p| p == &required) {
                Ok(())
            } else {
                Err(McpError::ProjectPermissionDenied {
                    project_id,
                    permission: required,
                })
            }
        }
        Err(e) => {
            error!(
                user_id,
                project_id,
                permission = %permission,
                "MCP project permission check failed (infra error): {}", e
            );
            Err(McpError::ProjectAccessDenied { project_id })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use temps_entities::users;

    // ── AuthContext helpers ───────────────────────────────────────────────────

    fn make_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
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
            created_at: now,
            updated_at: now,
        }
    }

    fn non_admin_auth() -> AuthContext {
        AuthContext::new_session(make_user(1), Role::User)
    }

    fn admin_auth() -> AuthContext {
        AuthContext::new_session(make_user(1), Role::Admin)
    }

    fn platform_admin_auth() -> AuthContext {
        AuthContext::new_session(make_user(1), Role::PlatformAdmin)
    }

    fn deployment_token_auth() -> AuthContext {
        AuthContext::new_deployment_token(1, None, None, 1, "test-token".to_string(), vec![])
    }

    // ── Checker mocks ─────────────────────────────────────────────────────────

    /// A checker that denies access to a specific project_id.
    struct DenyingChecker {
        denied_project_id: i32,
    }

    #[async_trait]
    impl ProjectAccessChecker for DenyingChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(project_id != self.denied_project_id)
        }
    }

    /// A checker that always returns an infra error.
    struct FailingChecker;

    #[async_trait]
    impl ProjectAccessChecker for FailingChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Err("infra failure".into())
        }
    }

    // ── Existing behaviour (non-admin principals) ─────────────────────────────

    #[tokio::test]
    async fn check_project_access_none_checker_allows() {
        // No checker registered → OSS default → allow.
        let result = check_project_access(None, &non_admin_auth(), 42).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn check_project_access_allows_permitted_project() {
        let checker = DenyingChecker {
            denied_project_id: 99,
        };
        let result = check_project_access(Some(&checker), &non_admin_auth(), 42).await;
        assert!(result.is_ok(), "project 42 must be allowed");
    }

    #[tokio::test]
    async fn check_project_access_denies_forbidden_project() {
        let checker = DenyingChecker {
            denied_project_id: 42,
        };
        let result = check_project_access(Some(&checker), &non_admin_auth(), 42).await;
        assert!(
            matches!(
                result,
                Err(McpError::ProjectAccessDenied { project_id: 42 })
            ),
            "project 42 must be denied"
        );
    }

    #[tokio::test]
    async fn check_project_access_infra_failure_fails_closed() {
        // Infrastructure failure must produce ProjectAccessDenied, not a silent allow.
        let checker = FailingChecker;
        let result = check_project_access(Some(&checker), &non_admin_auth(), 42).await;
        assert!(
            matches!(
                result,
                Err(McpError::ProjectAccessDenied { project_id: 42 })
            ),
            "infra failure must fail closed as ProjectAccessDenied"
        );
    }

    // ── Admin bypass (parity with resolve_hidden_projects REST handler) ────────

    /// A PlatformAdmin must bypass the `ProjectAccessChecker` entirely and be
    /// allowed unconditionally — even if the checker would have denied them.
    #[tokio::test]
    async fn check_project_access_platform_admin_bypasses_checker() {
        let checker = DenyingChecker {
            denied_project_id: 42,
        };
        let result = check_project_access(Some(&checker), &platform_admin_auth(), 42).await;
        assert!(
            result.is_ok(),
            "PlatformAdmin must bypass the checker and be allowed for any project"
        );
    }

    /// Instance admins (Role::Admin) must also bypass the checker.
    #[tokio::test]
    async fn check_project_access_admin_bypasses_checker() {
        let checker = DenyingChecker {
            denied_project_id: 42,
        };
        let result = check_project_access(Some(&checker), &admin_auth(), 42).await;
        assert!(
            result.is_ok(),
            "Role::Admin must bypass the checker and be allowed for any project"
        );
    }

    /// Deployment tokens also bypass the `ProjectAccessChecker` in this
    /// function. Tenant confinement to the token's own project is a separate
    /// concern, enforced by `check_project_scope` — see that function's tests
    /// below for the case where the requested project does NOT match.
    #[tokio::test]
    async fn check_project_access_deployment_token_bypasses_checker() {
        let checker = DenyingChecker {
            denied_project_id: 42,
        };
        let result = check_project_access(Some(&checker), &deployment_token_auth(), 42).await;
        assert!(
            result.is_ok(),
            "deployment token must bypass the checker and be allowed"
        );
    }

    // ── check_project_scope (tenant-boundary confinement) ──────────────────────

    #[test]
    fn check_project_scope_allows_deployment_token_for_its_own_project() {
        // deployment_token_auth() is bound to project 1 (see its constructor call).
        let result = check_project_scope(&deployment_token_auth(), 1);
        assert!(result.is_ok(), "token must access its own bound project");
    }

    #[test]
    fn check_project_scope_denies_deployment_token_for_a_different_project() {
        // deployment_token_auth() is bound to project 1; requesting project 42
        // must be denied even though check_project_access would bypass the
        // checker entirely for this same auth context.
        let result = check_project_scope(&deployment_token_auth(), 42);
        assert!(
            matches!(
                result,
                Err(McpError::ProjectAccessDenied { project_id: 42 })
            ),
            "deployment token scoped to project 1 must not reach project 42"
        );
    }

    #[test]
    fn check_project_scope_is_a_noop_for_non_deployment_token_auth() {
        // Non-admin session auth has no per-project confinement at this layer.
        let result = check_project_scope(&non_admin_auth(), 999);
        assert!(result.is_ok(), "session auth is not project-scoped");
    }

    // ── Project-scoped permissions ───────────────────────────────────────────

    /// A checker that grants a fixed permission set on every project.
    struct PermissionChecker {
        perms: Option<Vec<&'static str>>,
    }

    #[async_trait]
    impl ProjectAccessChecker for PermissionChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(true)
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self
                .perms
                .as_ref()
                .map(|p| p.iter().map(|s| s.to_string()).collect()))
        }
    }

    #[tokio::test]
    async fn project_permission_is_checked_against_the_project_role() {
        use temps_auth::permissions::Permission;
        // A team viewer: can reach the project, may read but not write.
        let viewer = PermissionChecker {
            perms: Some(vec!["projects:read", "edge_rules:read"]),
        };
        let auth = non_admin_auth();
        assert!(
            check_project_permission(Some(&viewer), &auth, 7, &Permission::EdgeRulesRead)
                .await
                .is_ok()
        );
        let denied =
            check_project_permission(Some(&viewer), &auth, 7, &Permission::EdgeRulesWrite).await;
        assert!(
            matches!(
                &denied,
                Err(McpError::ProjectPermissionDenied { project_id: 7, permission })
                    if permission == "edge_rules:write"
            ),
            "{denied:?}"
        );
        // Instance admins are not narrowed by team roles.
        assert!(check_project_permission(
            Some(&viewer),
            &admin_auth(),
            7,
            &Permission::EdgeRulesWrite
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn project_permission_without_an_opinion_falls_back_to_access() {
        use temps_auth::permissions::Permission;
        let checker = DenyingChecker {
            denied_project_id: 7,
        };
        let auth = non_admin_auth();
        assert!(matches!(
            check_project_permission(Some(&checker), &auth, 7, &Permission::EdgeRulesRead).await,
            Err(McpError::ProjectAccessDenied { project_id: 7 })
        ));
        assert!(
            check_project_permission(Some(&checker), &auth, 8, &Permission::EdgeRulesWrite)
                .await
                .is_ok()
        );
        assert!(matches!(
            check_project_permission(Some(&FailingChecker), &auth, 8, &Permission::EdgeRulesRead)
                .await,
            Err(McpError::ProjectAccessDenied { project_id: 8 })
        ));
    }
}
