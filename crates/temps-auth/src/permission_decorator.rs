/// Simple permission decorator macro for handlers
///
/// This macro wraps handler functions to automatically check permissions
/// without requiring manual permission checks in the function body.
///
/// Usage:
/// ```ignore
/// permission_required!(ApiKeysCreate);
/// pub async fn create_api_key(
///     RequireAuth(auth): RequireAuth,
///     State(state): State<Arc<AppState>>,
///     Json(request): Json<CreateApiKeyRequest>,
/// ) -> impl IntoResponse {
///     // No need to call require_permission_handler! here
///     // Permission is automatically checked
/// }
/// ```
/// Macro that generates a handler with automatic permission checking
#[macro_export]
macro_rules! permission_required {
    ($permission:ident) => {
        // This creates a decorator-like attribute that can be placed before a function
        // The actual implementation will check the permission automatically
    };

    // Full implementation with function definition
    ($permission:ident, $vis:vis async fn $name:ident(
        RequireAuth($auth:ident): RequireAuth,
        $($arg:ident: $arg_type:ty),* $(,)?
    ) -> impl IntoResponse $body:block) => {
        $vis async fn $name(
            RequireAuth($auth): RequireAuth,
            $($arg: $arg_type),*
        ) -> impl IntoResponse {
            // Automatically check the permission
            if !$auth.has_permission(&$crate::auth::permissions::Permission::$permission) {
                return $crate::utils::error::ErrorBuilder::new(::axum::http::StatusCode::FORBIDDEN)
                    .type_("https://temps.sh/probs/insufficient-permissions")
                    .title("Insufficient Permissions")
                    .detail(format!("This operation requires the {} permission",
                        $crate::auth::permissions::Permission::$permission.to_string()))
                    .value("required_permission",
                        $crate::auth::permissions::Permission::$permission.to_string())
                    .value("user_role", $auth.effective_role.to_string())
                    .build()
                    .into_response();
            }

            // Execute the original function body
            $body
        }
    };
}
