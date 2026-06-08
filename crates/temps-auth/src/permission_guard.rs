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
