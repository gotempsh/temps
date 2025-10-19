/// A more elegant solution for permission-based handlers
///
/// Since we can't create true procedural macros without a separate crate,
/// this provides a clean pattern that achieves the same goal.
///
/// Usage:
/// ```ignore
/// #[permission(ApiKeysCreate)]
/// pub async fn create_api_key(
///     auth: AuthorizedFor<ApiKeysCreate>,
///     State(state): State<Arc<AppState>>,
///     Json(request): Json<CreateApiKeyRequest>,
/// ) -> impl IntoResponse {
///     // auth.context gives you the AuthContext
///     // auth.user_id() is a convenience method
/// }
/// ```
use crate::context::AuthContext;
use crate::permissions::Permission;
use temps_core::error_builder::ErrorBuilder;
use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    response::IntoResponse,
};
use std::marker::PhantomData;

/// Marker trait for permission requirements
pub trait PermissionRequirement: Send + Sync + 'static {
    const PERMISSION: Permission;
}

/// Extractor that ensures the user has a specific permission
pub struct AuthorizedFor<P: PermissionRequirement> {
    pub context: AuthContext,
    _phantom: PhantomData<P>,
}

impl<P: PermissionRequirement> AuthorizedFor<P> {
    /// Convenience method to get the user ID
    pub fn user_id(&self) -> i32 {
        self.context.user_id()
    }

    /// Get the underlying auth context
    pub fn auth(&self) -> &AuthContext {
        &self.context
    }
}

impl<S, P> FromRequestParts<S> for AuthorizedFor<P>
where
    S: Send + Sync,
    P: PermissionRequirement,
{
    type Rejection = axum::response::Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Extract the auth context from the request extensions
        let context = parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or_else(|| {
                ErrorBuilder::new(StatusCode::UNAUTHORIZED)
                    .type_("https://temps.sh/probs/authentication-required")
                    .title("Authentication Required")
                    .detail("This operation requires authentication")
                    .build()
                    .into_response()
            })?;

        // Check if the user has the required permission
        if !context.has_permission(&P::PERMISSION) {
            return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/insufficient-permissions")
                .title("Insufficient Permissions")
                .detail(format!(
                    "This operation requires the {} permission",
                    P::PERMISSION.to_string()
                ))
                .value("required_permission", P::PERMISSION.to_string())
                .value("user_role", context.effective_role.to_string())
                .build()
                .into_response());
        }

        Ok(AuthorizedFor {
            context,
            _phantom: PhantomData,
        })
    }
}

// Macro to generate permission requirement types
#[macro_export]
macro_rules! define_permissions {
    ($($name:ident => $permission:expr),* $(,)?) => {
        $(
            pub struct $name;
            impl $crate::permission_attribute::PermissionRequirement for $name {
                const PERMISSION: $crate::permissions::Permission = $permission;
            }
        )*
    };
}

// Define all permission requirement types
define_permissions! {
    // Projects
    ProjectsRead => Permission::ProjectsRead,
    ProjectsWrite => Permission::ProjectsWrite,
    ProjectsDelete => Permission::ProjectsDelete,
    ProjectsCreate => Permission::ProjectsCreate,

    // Deployments
    DeploymentsRead => Permission::DeploymentsRead,
    DeploymentsWrite => Permission::DeploymentsWrite,
    DeploymentsDelete => Permission::DeploymentsDelete,
    DeploymentsCreate => Permission::DeploymentsCreate,

    // Domains
    DomainsRead => Permission::DomainsRead,
    DomainsWrite => Permission::DomainsWrite,
    DomainsDelete => Permission::DomainsDelete,
    DomainsCreate => Permission::DomainsCreate,

    // Environments
    EnvironmentsRead => Permission::EnvironmentsRead,
    EnvironmentsWrite => Permission::EnvironmentsWrite,
    EnvironmentsDelete => Permission::EnvironmentsDelete,
    EnvironmentsCreate => Permission::EnvironmentsCreate,

    // Analytics
    AnalyticsRead => Permission::AnalyticsRead,
    AnalyticsWrite => Permission::AnalyticsWrite,

    // Users
    UsersRead => Permission::UsersRead,
    UsersWrite => Permission::UsersWrite,
    UsersDelete => Permission::UsersDelete,
    UsersCreate => Permission::UsersCreate,

    // System
    SystemAdmin => Permission::SystemAdmin,
    SystemRead => Permission::SystemRead,

    // MCP
    McpConnect => Permission::McpConnect,
    McpExecute => Permission::McpExecute,
    McpRead => Permission::McpRead,
    McpWrite => Permission::McpWrite,

    // API Keys
    ApiKeysRead => Permission::ApiKeysRead,
    ApiKeysWrite => Permission::ApiKeysWrite,
    ApiKeysDelete => Permission::ApiKeysDelete,
    ApiKeysCreate => Permission::ApiKeysCreate,

    // Audit
    AuditRead => Permission::AuditRead,

    // Backups
    BackupsRead => Permission::BackupsRead,
    BackupsWrite => Permission::BackupsWrite,
    BackupsDelete => Permission::BackupsDelete,
    BackupsCreate => Permission::BackupsCreate,
}
