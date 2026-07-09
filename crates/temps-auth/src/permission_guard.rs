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

/// Guard that enforces a *specific* permission within a project's team-scoped
/// access grant, narrowing what [`permission_guard!`] alone allows.
///
/// `permission_guard!` proves the caller's **instance-wide** role has this
/// permission at all; `project_access_guard!` proves the caller can touch
/// this **project** at all, but not which actions. Neither answers "does
/// this user's *role on this specific project* include this permission" —
/// e.g. a project team member granted a read-only role should not be able to
/// deploy, even though their instance-wide role and their team membership
/// both otherwise check out. This macro is the seam that closes that gap.
///
/// This macro is **self-contained**: it performs the instance-wide
/// permission check *and* the project-scoped narrowing in one call, so a
/// single invocation is the complete gate for a project-scoped, permissioned
/// action. Do not also call [`permission_guard!`] for the same permission in
/// the same handler — that would be redundant, not incorrect, but this macro
/// already does it.
///
/// **Call order** in a handler that needs it (replaces the
/// `permission_guard!` + `project_access_guard!` pair for handlers that need
/// permission-specific project narrowing; `project_scope_guard!` for
/// deployment-token IDOR is unaffected and still runs separately):
///
/// ```ignore
/// project_permission_guard!(auth, DeploymentsCreate, project_id, checker); // 1+3 combined
/// project_scope_guard!(auth, project_id);                                 // 2. deployment-token IDOR
/// ```
///
/// `checker` is the same `Option<Arc<dyn temps_core::ProjectAccessChecker>>`
/// field `project_access_guard!` takes.
///
/// **Precedence — final = instance-wide ∩ project-scoped.** A project role
/// can only ever *remove* permissions the instance-wide role already grants;
/// it can never add one. Concretely:
///
/// 1. Instance-wide ceiling first — identical to `permission_guard!`. A
///    caller whose instance-wide role lacks the permission is rejected here,
///    before any project-scoped lookup — the existing instance-wide
///    behaviour (e.g. a `user`-role account getting 403 on delete
///    operations) is unchanged.
/// 2. Deployment tokens skip project-scoped narrowing (governed by
///    `project_scope_guard!` instead, and carry no user identity to resolve
///    project-scoped permissions for) — same as `project_access_guard!`.
/// 3. Instance Admin / PlatformAdmin bypass project-scoped narrowing
///    entirely — same bypass `project_access_guard!` grants, for the same
///    reason (an instance admin who is not a team member must not be locked
///    out).
/// 4. Otherwise, `checker.effective_project_permissions(user_id, project_id)`
///    is consulted:
///    - `Ok(None)` → unrestricted from this checker's perspective (no plugin
///      registered, or the project has no team grants) — the instance-wide
///      result from step 1 stands.
///    - `Ok(Some(perms))` → the permission must be present in `perms`, else
///      403. An empty `perms` denies every project-scoped permission.
///    - `Err(_)` → fail-closed, 500. Never a silent allow.
#[macro_export]
macro_rules! project_permission_guard {
    ($auth:expr, $permission:ident, $project_id:expr, $checker:expr) => {{
        // Step 1: instance-wide ceiling — identical to permission_guard!.
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

        // Deployment tokens are governed by project_scope_guard! instead and
        // carry no user identity — skip project-scoped narrowing entirely.
        if !$auth.is_deployment_token() {
            // Instance administrators are never restricted by team membership.
            if !($auth.is_admin()
                || $auth.has_role(&$crate::permissions::Role::PlatformAdmin))
            {
                if let Some(ref __checker) = $checker {
                    if let Some(__user_id) = $auth.user_id_opt() {
                        match __checker
                            .effective_project_permissions(__user_id, $project_id)
                            .await
                        {
                            // No opinion from this checker — instance-wide result stands.
                            Ok(None) => {}
                            Ok(Some(__perms)) => {
                                let __required =
                                    $crate::permissions::Permission::$permission.to_string();
                                if !__perms.iter().any(|__p| __p == &__required) {
                                    return Err(temps_core::error_builder::ErrorBuilder::new(
                                        ::axum::http::StatusCode::FORBIDDEN,
                                    )
                                    .type_("https://temps.sh/probs/project-permission-denied")
                                    .title("Project Permission Denied")
                                    .detail(format!(
                                        "Your role on this project does not include the {} \
                                         permission",
                                        __required
                                    ))
                                    .value("required_permission", __required)
                                    .build());
                                }
                            }
                            Err(__e) => {
                                ::tracing::error!(
                                    project_id = $project_id,
                                    user_id = __user_id,
                                    error = %__e,
                                    "ProjectAccessChecker::effective_project_permissions \
                                     infrastructure failure — denying access"
                                );
                                return Err(temps_core::error_builder::ErrorBuilder::new(
                                    ::axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                )
                                .type_("https://temps.sh/probs/project-permission-check-failed")
                                .title("Project Permission Check Failed")
                                .detail(
                                    "Could not verify project permissions; please try again",
                                )
                                .build());
                            }
                        }
                    } else {
                        // user_id_opt() == None here is impossible today:
                        // deployment tokens (the only non-user auth source) are
                        // filtered by the outer is_deployment_token() check
                        // above, and every other AuthSource carries a user. If
                        // a future AuthSource variant broke that invariant,
                        // fail closed rather than silently skipping the
                        // project-scoped check.
                        return Err(temps_core::error_builder::ErrorBuilder::new(
                            ::axum::http::StatusCode::FORBIDDEN,
                        )
                        .type_("https://temps.sh/probs/insufficient-permissions")
                        .title("Insufficient Permissions")
                        .detail(
                            "Could not resolve caller identity for project permission check",
                        )
                        .build());
                    }
                }
                // checker is None → no plugin registered → no-op beyond step 1.
            }
            // Admin / PlatformAdmin → unrestricted beyond step 1.
        }
    }};
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

    // ---------------------------------------------------------------------------
    // project_permission_guard! tests (permission-specific project narrowing)
    // ---------------------------------------------------------------------------

    type EffectivePermissionsResult =
        Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>>;

    /// A mock [`ProjectAccessChecker`] whose `effective_project_permissions`
    /// result is configurable, for `project_permission_guard!` tests.
    /// `user_can_access_project` is a trivial always-allow stub — these tests
    /// never exercise it.
    struct MockPermissionChecker {
        result: fn() -> EffectivePermissionsResult,
    }

    impl MockPermissionChecker {
        fn unrestricted() -> Arc<dyn ProjectAccessChecker> {
            Arc::new(MockPermissionChecker {
                result: || Ok(None),
            })
        }

        fn with_perms(perms: &'static [&'static str]) -> Arc<dyn ProjectAccessChecker> {
            // fn pointers can't capture `perms` by closure, so encode the
            // fixed sets this test file actually needs directly.
            if perms == ["deployments:create"] {
                Arc::new(MockPermissionChecker {
                    result: || Ok(Some(vec!["deployments:create".to_string()])),
                })
            } else if perms == ["deployments:delete"] {
                Arc::new(MockPermissionChecker {
                    result: || Ok(Some(vec!["deployments:delete".to_string()])),
                })
            } else {
                Arc::new(MockPermissionChecker {
                    result: || Ok(Some(vec!["projects:read".to_string()])),
                })
            }
        }

        fn empty() -> Arc<dyn ProjectAccessChecker> {
            Arc::new(MockPermissionChecker {
                result: || Ok(Some(vec![])),
            })
        }

        fn error() -> Arc<dyn ProjectAccessChecker> {
            Arc::new(MockPermissionChecker {
                result: || Err(Box::new(std::io::Error::other("simulated DB failure"))),
            })
        }
    }

    #[async_trait]
    impl ProjectAccessChecker for MockPermissionChecker {
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
            (self.result)()
        }
    }

    /// Runs `project_permission_guard!` requiring `DeploymentsCreate` and
    /// returns `Ok(())` or `Err(Problem)`.
    async fn run_permission_guard(
        auth: &AuthContext,
        project_id: i32,
        checker: Option<Arc<dyn ProjectAccessChecker>>,
    ) -> Result<(), Problem> {
        project_permission_guard!(auth, DeploymentsCreate, project_id, checker);
        Ok(())
    }

    #[tokio::test]
    async fn permission_guard_instance_ceiling_blocks_before_project_lookup() {
        // Role::User lacks DeploymentsDelete outright — the instance-wide
        // check must reject before any project-scoped lookup happens, exactly
        // like permission_guard! does today.
        let auth = user_auth(Role::User);
        async fn run_delete_guard(
            auth: &AuthContext,
            project_id: i32,
            checker: Option<Arc<dyn ProjectAccessChecker>>,
        ) -> Result<(), Problem> {
            project_permission_guard!(auth, DeploymentsDelete, project_id, checker);
            Ok(())
        }
        // Even a checker that would grant everything must not matter — the
        // instance ceiling is checked first.
        let checker = Some(MockPermissionChecker::unrestricted());
        let result = run_delete_guard(&auth, 1, checker).await;
        let err = result.expect_err("instance-wide ceiling must reject before project lookup");
        assert_eq!(err.status_code, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn permission_guard_project_grant_cannot_widen_past_instance_ceiling() {
        // Narrowing-only invariant: a project-scoped checker can only ever
        // remove permissions the instance-wide role grants, never add ones it
        // didn't. Role::User lacks DeploymentsDelete outright (see
        // permission_guard_instance_ceiling_blocks_before_project_lookup), so
        // even a checker that explicitly grants "deployments:delete" at the
        // project level must not resurrect it — step 1 rejects before the
        // checker is ever consulted.
        let auth = user_auth(Role::User);
        async fn run_delete_guard(
            auth: &AuthContext,
            project_id: i32,
            checker: Option<Arc<dyn ProjectAccessChecker>>,
        ) -> Result<(), Problem> {
            project_permission_guard!(auth, DeploymentsDelete, project_id, checker);
            Ok(())
        }
        let checker = Some(MockPermissionChecker::with_perms(&["deployments:delete"]));
        let result = run_delete_guard(&auth, 1, checker).await;
        let err = result.expect_err(
            "a project-level grant must never widen permissions past the instance ceiling",
        );
        assert_eq!(err.status_code, axum::http::StatusCode::FORBIDDEN);
        // Must be the instance-wide rejection, proving the checker was never
        // reached — not the project-scoped "project-permission-denied" type,
        // which would imply the checker was consulted and (correctly) denied
        // it there instead of at the ceiling.
        assert_eq!(
            err.body.get("type").and_then(|v| v.as_str()),
            Some("https://temps.sh/probs/insufficient-permissions")
        );
    }

    #[tokio::test]
    async fn permission_guard_viewer_blocked_from_deploy() {
        // The reproduced hole: instance role `user` (has DeploymentsCreate
        // instance-wide) + a project-scoped role whose resolved permission
        // set does NOT include deployments:create → must be denied.
        let auth = user_auth(Role::User);
        let checker = Some(MockPermissionChecker::with_perms(&["projects:read"]));
        let result = run_permission_guard(&auth, 1, checker).await;
        let err = result.expect_err("a project role without deployments:create must be denied");
        assert_eq!(err.status_code, axum::http::StatusCode::FORBIDDEN);
        assert_eq!(
            err.body.get("type").and_then(|v| v.as_str()),
            Some("https://temps.sh/probs/project-permission-denied")
        );
    }

    #[tokio::test]
    async fn permission_guard_deployer_allowed_deploy() {
        let auth = user_auth(Role::User);
        let checker = Some(MockPermissionChecker::with_perms(&["deployments:create"]));
        let result = run_permission_guard(&auth, 1, checker).await;
        assert!(
            result.is_ok(),
            "a project role that includes deployments:create must be allowed"
        );
    }

    #[tokio::test]
    async fn permission_guard_empty_perms_denies() {
        let auth = user_auth(Role::User);
        let checker = Some(MockPermissionChecker::empty());
        let result = run_permission_guard(&auth, 1, checker).await;
        assert!(
            result.is_err(),
            "an empty project-scoped permission set must deny every action"
        );
    }

    #[tokio::test]
    async fn permission_guard_unrestricted_falls_through_to_instance_wide() {
        // Ok(None) means "no opinion" — the instance-wide result (which
        // already passed) stands.
        let auth = user_auth(Role::User);
        let checker = Some(MockPermissionChecker::unrestricted());
        let result = run_permission_guard(&auth, 1, checker).await;
        assert!(
            result.is_ok(),
            "Ok(None) must fall through to the instance-wide result"
        );
    }

    #[tokio::test]
    async fn permission_guard_resolver_error_returns_500() {
        let auth = user_auth(Role::User);
        let checker = Some(MockPermissionChecker::error());
        let result = run_permission_guard(&auth, 1, checker).await;
        let err = result.expect_err("infrastructure error must deny, never allow");
        assert_eq!(
            err.status_code,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn permission_guard_no_checker_registered_is_instance_wide_only() {
        let auth = user_auth(Role::User);
        let result = run_permission_guard(&auth, 1, None).await;
        assert!(
            result.is_ok(),
            "no checker registered must reduce to the instance-wide check only"
        );
    }

    #[tokio::test]
    async fn permission_guard_admin_bypasses_project_narrowing() {
        let auth = user_auth(Role::Admin);
        // Even a deny-everything checker must not block an instance admin.
        let checker = Some(MockPermissionChecker::empty());
        let result = run_permission_guard(&auth, 1, checker).await;
        assert!(
            result.is_ok(),
            "instance Admin must bypass project narrowing"
        );
    }

    #[tokio::test]
    async fn permission_guard_platform_admin_bypasses_project_narrowing() {
        // PlatformAdmin's instance-wide role deliberately excludes
        // Deployments{Create,Write,Delete} (read-only on deployments) — use
        // DeploymentsRead here so this test isolates the bypass behaviour
        // instead of tripping the (correct, separate) instance-wide ceiling.
        async fn run_read_guard(
            auth: &AuthContext,
            project_id: i32,
            checker: Option<Arc<dyn ProjectAccessChecker>>,
        ) -> Result<(), Problem> {
            project_permission_guard!(auth, DeploymentsRead, project_id, checker);
            Ok(())
        }
        let auth = user_auth(Role::PlatformAdmin);
        let checker = Some(MockPermissionChecker::empty());
        let result = run_read_guard(&auth, 1, checker).await;
        assert!(
            result.is_ok(),
            "instance PlatformAdmin must bypass project narrowing"
        );
    }

    #[tokio::test]
    async fn permission_guard_deployment_token_bypasses_project_narrowing() {
        // `AuthContext::has_permission` deliberately only bridges deployment
        // tokens to a small explicit whitelist of `Permission` variants
        // (AnalyticsRead/AnalyticsWrite/EmailsSend) — DeploymentsCreate is
        // not one of them by design ("no implicit bridge from
        // deployment-token permissions to general control-plane
        // permissions"), so no deployment token can ever pass step 1 for
        // that permission, with or without this macro. Use AnalyticsRead
        // (one of the bridged permissions) so this test isolates what it's
        // actually testing: that project-scoped narrowing is skipped for
        // deployment tokens, once step 1 passes.
        async fn run_analytics_guard(
            auth: &AuthContext,
            project_id: i32,
            checker: Option<Arc<dyn ProjectAccessChecker>>,
        ) -> Result<(), Problem> {
            project_permission_guard!(auth, AnalyticsRead, project_id, checker);
            Ok(())
        }
        let auth = AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "deploy-token".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::AnalyticsRead],
        );
        let checker = Some(MockPermissionChecker::empty());
        let result = run_analytics_guard(&auth, 7, checker).await;
        assert!(
            result.is_ok(),
            "deployment tokens are governed by project_scope_guard! instead"
        );
    }

    // ---------------------------------------------------------------------------
    // ADR-028 Phase B: Coverage enumeration
    //
    // Every Rust source file that contains `project_scope_guard!` must also
    // contain `project_access_guard!` — the two guards are always paired.
    // This test scans the workspace at test time and fails if any file has one
    // without the other, catching handlers added in future crates that omit the
    // companion guard.
    // ---------------------------------------------------------------------------

    #[test]
    fn every_project_scope_guard_has_access_guard_companion() {
        // Locate the workspace crates/ directory relative to this crate.
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // temps-auth lives at <workspace>/crates/temps-auth
        let workspace_root = manifest_dir
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .expect("cannot resolve workspace root from CARGO_MANIFEST_DIR");
        let crates_dir = workspace_root.join("crates");

        let mut violations: Vec<String> = Vec::new();

        for entry in walkdir::WalkDir::new(&crates_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            let path = entry.path();
            let contents = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            // Use `!(` to match actual macro invocations and avoid matching
            // doc-comment references like `[`project_scope_guard!`]`.
            let has_scope_guard = contents.contains("project_scope_guard!(");
            let has_access_guard = contents.contains("project_access_guard!(");
            // Skip the file that defines the macros themselves.
            let is_definition_file = contents.contains("macro_rules! project_scope_guard")
                || contents.contains("macro_rules! project_access_guard");
            if is_definition_file {
                continue;
            }
            if has_scope_guard && !has_access_guard {
                violations.push(format!("{}", path.display()));
            }
        }

        assert!(
            violations.is_empty(),
            "The following files have `project_scope_guard!` but are missing \
             `project_access_guard!` (ADR-028 Phase B requires both to be paired):\n{}",
            violations.join("\n")
        );
    }

    /// ADR-028 Phase B — crate coverage snapshot.
    ///
    /// Documents which crates contain `project_access_guard!` usages as of the
    /// Phase B rollout. When a new crate with project-scoped handlers is added,
    /// this list must grow to maintain the inventory. Keep it sorted.
    ///
    /// The check is **bidirectional**:
    /// - Every crate in `expected_crates` must still have at least one
    ///   `project_access_guard!` call (catches accidental removal).
    /// - Every crate that has a `project_access_guard!` call must be in
    ///   `expected_crates` (catches new crates that add guard calls without
    ///   being registered in this inventory, closing the silent-omission gap
    ///   identified in the Phase B review).
    #[test]
    fn project_access_guard_coverage_snapshot() {
        let expected_crates: &[&str] = &[
            "temps-agents",
            "temps-ai-chat",
            "temps-analytics",
            "temps-analytics-events",
            "temps-analytics-funnels",
            "temps-analytics-session-replay",
            "temps-deployments",
            "temps-environments",
            "temps-error-tracking",
            "temps-log-aggregator",
            "temps-monitoring",
            "temps-observability",
            "temps-otel",
            "temps-projects",
            "temps-providers",
            "temps-revenue",
            "temps-status-page",
            "temps-vulnerability-scanner",
            "temps-webhooks",
        ];

        // Verify the snapshot is sorted (makes PR diffs easier to review).
        let mut sorted = expected_crates.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            expected_crates,
            sorted.as_slice(),
            "keep the coverage snapshot sorted alphabetically"
        );

        // Scan the workspace and collect crates that actually use project_access_guard!.
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("cannot resolve workspace root");
        let crates_dir = workspace_root.join("crates");

        let mut found_crates: std::collections::BTreeSet<String> = Default::default();

        for entry in walkdir::WalkDir::new(&crates_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            let path = entry.path();
            let contents = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            // Skip the file that defines the macro itself.
            if contents.contains("macro_rules! project_access_guard") {
                continue;
            }
            if contents.contains("project_access_guard!(") {
                // Derive the crate name from the directory that directly contains
                // this source file's Cargo.toml — walk up until we find it.
                let mut dir = path.parent();
                while let Some(d) = dir {
                    if d.join("Cargo.toml").exists() {
                        if let Some(name) = d.file_name().and_then(|n| n.to_str()) {
                            found_crates.insert(name.to_string());
                        }
                        break;
                    }
                    dir = d.parent();
                }
            }
        }

        // Direction 1: every expected crate must still have a guard call.
        for expected in expected_crates {
            assert!(
                found_crates.contains(*expected),
                "Crate `{}` is in the coverage snapshot but no `project_access_guard!` \
                 usage was found in its source — was it accidentally removed?",
                expected
            );
        }

        // Direction 2: every crate with a guard call must be in the snapshot.
        // This catches crates that add guard calls without registering in this
        // inventory, which would otherwise silently escape the coverage check.
        let expected_set: std::collections::BTreeSet<&str> =
            expected_crates.iter().copied().collect();
        let extra_found: Vec<&str> = found_crates
            .iter()
            .filter(|c| !expected_set.contains(c.as_str()))
            .map(|s| s.as_str())
            .collect();
        assert!(
            extra_found.is_empty(),
            "These crates use `project_access_guard!` but are not listed in the coverage \
             snapshot — add them to `expected_crates` (sorted alphabetically):\n{}",
            extra_found.join("\n")
        );
    }

    /// v1 coverage snapshot for `project_permission_guard!` (permission-specific
    /// project narrowing, distinct from `project_access_guard!`'s coarser
    /// "can touch this project at all" check).
    ///
    /// v1 deliberately covers only the highest-risk project-scoped actions
    /// (deployment create/trigger, project delete/settings, environment
    /// variable create/write/delete, domain create/write/delete) — see the
    /// ADR for the full rationale on what's deferred and why. This test is
    /// the "reviewed allowlist" that ADR calls for: it does NOT enumerate
    /// every handler function (that's noted as a mechanical follow-up once
    /// the seam is proven), but it does pin which CRATES are expected to use
    /// the guard, the same granularity `project_access_guard_coverage_snapshot`
    /// already uses. Bidirectional for the same reason that one is: catches
    /// both accidental removal and silent, unreviewed expansion.
    #[test]
    fn project_permission_guard_coverage_snapshot() {
        let expected_crates: &[&str] =
            &["temps-deployments", "temps-environments", "temps-projects"];

        let mut sorted = expected_crates.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            expected_crates,
            sorted.as_slice(),
            "keep the coverage snapshot sorted alphabetically"
        );

        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("cannot resolve workspace root");
        let crates_dir = workspace_root.join("crates");

        let mut found_crates: std::collections::BTreeSet<String> = Default::default();

        for entry in walkdir::WalkDir::new(&crates_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            let path = entry.path();
            let contents = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if contents.contains("macro_rules! project_permission_guard") {
                continue;
            }
            if contents.contains("project_permission_guard!(") {
                let mut dir = path.parent();
                while let Some(d) = dir {
                    if d.join("Cargo.toml").exists() {
                        if let Some(name) = d.file_name().and_then(|n| n.to_str()) {
                            found_crates.insert(name.to_string());
                        }
                        break;
                    }
                    dir = d.parent();
                }
            }
        }

        for expected in expected_crates {
            assert!(
                found_crates.contains(*expected),
                "Crate `{}` is in the project_permission_guard! coverage snapshot but no \
                 usage was found in its source — was it accidentally removed?",
                expected
            );
        }

        let expected_set: std::collections::BTreeSet<&str> =
            expected_crates.iter().copied().collect();
        let extra_found: Vec<&str> = found_crates
            .iter()
            .filter(|c| !expected_set.contains(c.as_str()))
            .map(|s| s.as_str())
            .collect();
        assert!(
            extra_found.is_empty(),
            "These crates use `project_permission_guard!` but are not listed in the coverage \
             snapshot — add them to `expected_crates` (sorted alphabetically) after reviewing \
             the new usage:\n{}",
            extra_found.join("\n")
        );
    }
}
