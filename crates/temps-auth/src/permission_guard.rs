/// Guard function that checks permission and returns early if not authorized
///
/// Usage in handler:
/// ```ignore
/// pub async fn create_api_key(
///     RequireAuth(auth): RequireAuth,
///     State(state): State<Arc<AppState>>,
///     Json(request): Json<CreateApiKeyRequest>,
/// ) -> impl IntoResponse {
///     permission_guard!(auth, ApiKeysCreate);
///
///     // Your handler logic here
/// }
/// ```
#[macro_export]
macro_rules! permission_guard {
    ($auth:expr, $permission:ident) => {
        if !$auth.has_permission(&$crate::permissions::Permission::$permission) {
            return Err(temps_core::error_builder::ErrorBuilder::new(
                ::axum::http::StatusCode::FORBIDDEN,
            )
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Insufficient Permissions")
            .detail(format!(
                "This operation requires the {} permission",
                $crate::permissions::Permission::$permission.to_string()
            ))
            .value(
                "required_permission",
                $crate::permissions::Permission::$permission.to_string(),
            )
            .value("user_role", $auth.effective_role.to_string())
            .build());
        }
    };
}

/// Guard that confines a deployment token to its bound project.
///
/// `permission_guard!` proves the caller holds a permission; it does NOT prove
/// the resource they're touching is theirs. A deployment token carrying
/// `FullAccess` satisfies every `permission_guard!`, so without this check it
/// can read/modify another tenant's project by passing a different `project_id`
/// in the path (cross-project IDOR). Call this immediately after the relevant
/// `permission_guard!` in every handler that takes a `project_id` and may be
/// reached by a deployment token.
///
/// For user/API-key/session/CLI auth this is a no-op (returns Ok), matching the
/// semantics of [`AuthContext::is_scoped_to_project`].
///
/// Usage in handler:
/// ```ignore
/// pub async fn get_environment_variables(
///     RequireAuth(auth): RequireAuth,
///     State(state): State<Arc<AppState>>,
///     Path(project_id): Path<i32>,
/// ) -> Result<impl IntoResponse, Problem> {
///     permission_guard!(auth, EnvironmentsRead);
///     project_scope_guard!(auth, project_id);
///
///     // Your handler logic here
/// }
/// ```
#[macro_export]
macro_rules! project_scope_guard {
    ($auth:expr, $project_id:expr) => {
        if !$auth.is_scoped_to_project($project_id) {
            return Err(temps_core::error_builder::ErrorBuilder::new(
                ::axum::http::StatusCode::FORBIDDEN,
            )
            .type_("https://temps.sh/probs/cross-project-access-denied")
            .title("Cross-Project Access Denied")
            .detail(
                "This deployment token is scoped to a different project and \
                 cannot access this resource",
            )
            .build());
        }
    };
}

/// Guard that rejects deployment-token auth entirely (403).
///
/// Use on endpoints that take a resource id with no `project_id` in scope to
/// confine it to the caller's project — e.g. analytics "by visitor/session id"
/// reads. A deployment token is a project-scoped machine credential; without a
/// `project_id` to compare against, the safe default is to require a real user
/// or API-key session for these by-id reads (which the console already uses).
/// No-op for user/API-key/session/CLI auth.
///
/// Usage in handler:
/// ```ignore
/// pub async fn get_visitor_by_id(
///     RequireAuth(auth): RequireAuth,
///     State(state): State<Arc<AppState>>,
///     Path(id): Path<i32>,
/// ) -> Result<impl IntoResponse, Problem> {
///     permission_guard!(auth, AnalyticsRead);
///     deny_deployment_token!(auth);
///     // ...
/// }
/// ```
#[macro_export]
macro_rules! deny_deployment_token {
    ($auth:expr) => {
        if $auth.is_deployment_token() {
            return Err(temps_core::error_builder::ErrorBuilder::new(
                ::axum::http::StatusCode::FORBIDDEN,
            )
            .type_("https://temps.sh/probs/deployment-token-not-allowed")
            .title("Deployment Token Not Allowed")
            .detail(
                "This endpoint requires user or API-key authentication; \
                 deployment tokens are not permitted",
            )
            .build());
        }
    };
}

/// Guard that enforces team-based project access for human sessions.
///
/// This is **distinct from** and **additional to** [`project_scope_guard!`], which
/// handles deployment-token cross-project IDOR and is a no-op for human
/// sessions. This macro enforces team-based access for those same human
/// sessions, but only when a [`temps_core::ProjectAccessChecker`] has been
/// registered (i.e. only when an optional plugin implementing that check is
/// present).
///
/// **Call order** in every project-scoped handler:
///
/// ```ignore
/// permission_guard!(auth, SomePermission);              // 1. instance-wide role
/// project_scope_guard!(auth, project_id);               // 2. deployment-token IDOR
/// project_access_guard!(auth, project_id, checker);     // 3. team-based access
/// ```
///
/// `checker` is the `Option<Arc<dyn temps_core::ProjectAccessChecker>>` field
/// stored on each handler plugin's `AppState`. It is resolved once during the
/// plugin's `configure_routes` phase via
/// `context.get_service::<dyn temps_core::ProjectAccessChecker>()`.
///
/// When `checker` is `None` (no plugin has registered a checker) this macro is
/// a **synchronous no-op** with zero overhead — OSS-only binaries are unaffected.
///
/// **Admin bypass:** callers with `Role::Admin` or `Role::PlatformAdmin` skip
/// the check entirely; instance administrators are never restricted by team
/// membership.
///
/// **Deployment tokens** bypass this guard — they are already confined to their
/// bound project by `project_scope_guard!` above and carry no user identity
/// with which to look up team membership.
///
/// **Fail semantics:**
/// - `Ok(true)` → allowed, continues.
/// - `Ok(false)` → returns `Err` with HTTP 403 (`project-access-denied`).
/// - `Err(_)` → infrastructure failure; returns `Err` with HTTP 500
///   (`project-access-check-failed`). Fail-closed: a broken check must never
///   silently allow access.
#[macro_export]
macro_rules! project_access_guard {
    ($auth:expr, $project_id:expr, $checker:expr) => {
        // Deployment tokens are already confined by project_scope_guard! and carry
        // no user identity — skip the team-membership check entirely.
        if !$auth.is_deployment_token() {
            // Instance administrators are never restricted by team membership.
            if !($auth.is_admin()
                || $auth.has_role(&$crate::permissions::Role::PlatformAdmin))
            {
                if let Some(ref __checker) = $checker {
                    if let Some(__user_id) = $auth.user_id_opt() {
                        match __checker
                            .user_can_access_project(__user_id, $project_id)
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => {
                                return Err(temps_core::error_builder::ErrorBuilder::new(
                                    ::axum::http::StatusCode::FORBIDDEN,
                                )
                                .type_("https://temps.sh/probs/project-access-denied")
                                .title("Project Access Denied")
                                .detail(
                                    "Your team membership does not include access to \
                                     this project",
                                )
                                .build());
                            }
                            Err(__e) => {
                                ::tracing::error!(
                                    project_id = $project_id,
                                    user_id = __user_id,
                                    error = %__e,
                                    "ProjectAccessChecker infrastructure failure \
                                     — denying access"
                                );
                                return Err(temps_core::error_builder::ErrorBuilder::new(
                                    ::axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                )
                                .type_("https://temps.sh/probs/project-access-check-failed")
                                .title("Project Access Check Failed")
                                .detail(
                                    "Could not verify project access; please try again",
                                )
                                .build());
                            }
                        }
                    }
                    // user_id_opt() == None here is impossible: deployment tokens
                    // (the only non-user auth source) are filtered by the outer
                    // is_deployment_token() check above.
                }
                // checker is None → no plugin registered → no-op.
            }
            // Admin / PlatformAdmin → unrestricted, no check performed.
        }
    };
}

/// Alias for permission_guard! macro for backwards compatibility
///
/// Usage in handler:
/// ```ignore
/// pub async fn delete_provider(
///     RequireAuth(auth): RequireAuth,
///     State(state): State<Arc<AppState>>,
///     Path(provider_id): Path<i32>,
/// ) -> impl IntoResponse {
///     permission_check!(auth, GitProvidersDelete);
///
///     // Your handler logic here
/// }
/// ```
#[macro_export]
macro_rules! permission_check {
    ($auth:expr, $permission:expr) => {
        if !$auth.has_permission(&$permission) {
            return Err(temps_core::error_builder::ErrorBuilder::new(
                ::axum::http::StatusCode::FORBIDDEN,
            )
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Insufficient Permissions")
            .detail(format!(
                "This operation requires the {} permission",
                $permission.to_string()
            ))
            .value("required_permission", $permission.to_string())
            .value("user_role", $auth.effective_role.to_string())
            .build());
        }
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use chrono::Utc;
    use temps_core::problemdetails::Problem;
    use temps_core::ProjectAccessChecker;
    use temps_entities::users;

    use crate::context::AuthContext;
    use crate::permissions::Role;

    // ---------------------------------------------------------------------------
    // Test helpers
    // ---------------------------------------------------------------------------

    fn test_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{}@example.com", id),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
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

    fn user_auth(role: Role) -> AuthContext {
        AuthContext::new_session(test_user(42), role)
    }

    fn deployment_token_auth() -> AuthContext {
        AuthContext::new_deployment_token(
            7,    // project_id
            None, // environment_id
            None, // deployment_id
            1,    // token_id
            "deploy-token".to_string(),
            vec![],
        )
    }

    /// A mock [`ProjectAccessChecker`] that returns a fixed outcome.
    struct MockChecker {
        result: fn() -> Result<bool, Box<dyn std::error::Error + Send + Sync>>,
    }

    impl MockChecker {
        fn allow() -> Arc<dyn ProjectAccessChecker> {
            Arc::new(MockChecker {
                result: || Ok(true),
            })
        }

        fn deny() -> Arc<dyn ProjectAccessChecker> {
            Arc::new(MockChecker {
                result: || Ok(false),
            })
        }

        fn error() -> Arc<dyn ProjectAccessChecker> {
            Arc::new(MockChecker {
                result: || Err(Box::new(std::io::Error::other("simulated DB failure"))),
            })
        }
    }

    #[async_trait]
    impl ProjectAccessChecker for MockChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            (self.result)()
        }
    }

    /// Runs the guard macro and returns `Ok(())` or `Err(Problem)`.
    async fn run_guard(
        auth: &AuthContext,
        project_id: i32,
        checker: Option<Arc<dyn ProjectAccessChecker>>,
    ) -> Result<(), Problem> {
        project_access_guard!(auth, project_id, checker);
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // (a) No checker registered → no-op, request proceeds
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn no_checker_registered_is_noop() {
        let auth = user_auth(Role::User);
        let result = run_guard(&auth, 1, None).await;
        assert!(result.is_ok(), "no checker should be a no-op");
    }

    // ---------------------------------------------------------------------------
    // (b) Checker registered, checker returns Ok(true) → proceeds
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn checker_allow_proceeds() {
        let auth = user_auth(Role::User);
        let checker = Some(MockChecker::allow());
        let result = run_guard(&auth, 1, checker).await;
        assert!(result.is_ok(), "checker returning Ok(true) should allow");
    }

    // ---------------------------------------------------------------------------
    // (c) Checker returns Ok(false) → 403
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn checker_deny_returns_403() {
        let auth = user_auth(Role::User);
        let checker = Some(MockChecker::deny());
        let result = run_guard(&auth, 1, checker).await;
        let err = result.expect_err("checker returning Ok(false) should deny");
        assert_eq!(
            err.status_code,
            axum::http::StatusCode::FORBIDDEN,
            "denial should be HTTP 403"
        );
    }

    // ---------------------------------------------------------------------------
    // (d) Checker returns Err → fail-closed, 500
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn checker_error_returns_500() {
        let auth = user_auth(Role::User);
        let checker = Some(MockChecker::error());
        let result = run_guard(&auth, 1, checker).await;
        let err = result.expect_err("infrastructure error should deny");
        assert_eq!(
            err.status_code,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "infrastructure failure should be HTTP 500"
        );
    }

    // ---------------------------------------------------------------------------
    // (e) Admin bypass — Role::Admin skips the checker entirely
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn admin_bypasses_checker() {
        let auth = user_auth(Role::Admin);
        // Even with a deny-all checker, Admin must not be blocked.
        let checker = Some(MockChecker::deny());
        let result = run_guard(&auth, 1, checker).await;
        assert!(result.is_ok(), "Admin should bypass the checker");
    }

    // ---------------------------------------------------------------------------
    // (f) PlatformAdmin bypass
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn platform_admin_bypasses_checker() {
        let auth = user_auth(Role::PlatformAdmin);
        let checker = Some(MockChecker::deny());
        let result = run_guard(&auth, 1, checker).await;
        assert!(result.is_ok(), "PlatformAdmin should bypass the checker");
    }

    // ---------------------------------------------------------------------------
    // (g) Deployment token bypass — no checker call, no user identity lookup
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn deployment_token_bypasses_checker() {
        let auth = deployment_token_auth();
        // Deployment tokens are governed by project_scope_guard! instead;
        // this guard must leave them untouched even with a deny-all checker.
        let checker = Some(MockChecker::deny());
        let result = run_guard(&auth, 7, checker).await;
        assert!(
            result.is_ok(),
            "deployment tokens should bypass project_access_guard!"
        );
    }
}
