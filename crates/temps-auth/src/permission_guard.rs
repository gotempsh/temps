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
