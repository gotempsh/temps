use crate::context::AuthContext;
use crate::permissions::Permission;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use temps_core::error_builder::ErrorBuilder;

/// Helper function to check permission and return appropriate error
pub fn check_permission_or_error(
    auth: &AuthContext,
    permission: Permission,
) -> Result<(), impl IntoResponse> {
    if !auth.has_permission(&permission) {
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Insufficient Permissions")
            .detail(format!(
                "This operation requires the {} permission",
                permission.to_string()
            ))
            .value("required_permission", permission.to_string())
            .value("user_role", auth.effective_role.to_string())
            .build()
            .into_response());
    }
    Ok(())
}

/// Macro to create a permission-checked handler
///
/// Usage:
/// ```ignore
/// with_permission!(Permission::ApiKeysCreate, async fn create_api_key(
///     auth: RequireAuth,
///     State(state): State<Arc<AppState>>,
///     Json(request): Json<CreateApiKeyRequest>,
/// ) -> impl IntoResponse {
///     // Handler logic here
/// });
/// ```
#[macro_export]
macro_rules! with_permission {
    ($permission:expr, $vis:vis async fn $name:ident $args:tt -> $ret:ty $body:block) => {
        $vis async fn $name $args -> $ret {
            let RequireAuth(ref auth) = auth;
            $crate::auth::decorators::check_permission_or_error(auth, $permission)?;

            // Re-bind auth for the handler body
            let auth = RequireAuth(auth.clone());
            $body
        }
    };
}

/// Alternative macro that generates both the permission check and the handler
/// This provides a cleaner syntax at the function definition level
#[macro_export]
macro_rules! handler_with_permission {
    (
        #[permission = $permission:expr]
        $vis:vis async fn $name:ident(
            $auth:ident: RequireAuth,
            $($arg:ident: $arg_type:ty),* $(,)?
        ) -> impl IntoResponse $body:block
    ) => {
        $vis async fn $name(
            $auth: RequireAuth,
            $($arg: $arg_type),*
        ) -> impl IntoResponse {
            // Check permission first
            if !$auth.0.has_permission(&$permission) {
                return $crate::utils::error::ErrorBuilder::new(::axum::http::StatusCode::FORBIDDEN)
                    .type_("https://temps.sh/probs/insufficient-permissions")
                    .title("Insufficient Permissions")
                    .detail(format!("This operation requires the {} permission", $permission.to_string()))
                    .value("required_permission", $permission.to_string())
                    .value("user_role", $auth.0.effective_role.to_string())
                    .build()
                    .into_response();
            }

            // Execute the handler body with the authenticated context
            let auth = $auth.0;
            $body
        }
    };
}
