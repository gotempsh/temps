use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    Extension,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use super::permission_guard;
use super::RequireAuth;
use crate::apikey_handler_types::{
    ApiKeyListResponse, ApiKeyResponse, CreateApiKeyRequest, CreateApiKeyResponse,
    UpdateApiKeyRequest,
};
use crate::audit::{ApiKeyCreatedAudit, ApiKeyRotatedAudit};
use crate::{
    apikey_service::ApiKeyService,
    apikey_types::{get_available_permissions, AvailablePermissions},
};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ListApiKeysQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

pub struct ApiKeyState {
    pub api_key_service: Arc<ApiKeyService>,
    /// Anonymous product telemetry reporter
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    /// Audit logger for write operations (e.g. key rotation)
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}

/// Privilege-escalation ceiling: a custom-permission API key must never
/// grant more than the creating user's own effective permissions.
///
/// Without this check, any role holding `ApiKeysCreate` (e.g. the standard
/// `Role::User`) could mint a key carrying `Permission::SystemAdmin` or any
/// other permission outside its own role — a full privilege escalation,
/// since `ApiKeyService::create_api_key` only validates that the requested
/// permission strings parse, not that the requester holds them. Unparsable
/// strings are left to that existing validation, which returns 400.
fn enforce_custom_permission_ceiling(
    auth: &crate::context::AuthContext,
    request: &CreateApiKeyRequest,
) -> Result<(), Problem> {
    if request.role_type != "custom" {
        return Ok(());
    }
    let Some(ref permissions) = request.permissions else {
        return Ok(());
    };
    for perm_str in permissions {
        if let Some(perm) = crate::permissions::Permission::from_str(perm_str) {
            if !auth.has_permission(&perm) {
                return Err(
                    temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                        .type_("https://temps.sh/probs/permission-ceiling-exceeded")
                        .title("Forbidden")
                        .detail(format!(
                            "Cannot grant permission '{perm_str}': it exceeds your own permissions"
                        ))
                        .value("error_code", "PERMISSION_CEILING_EXCEEDED")
                        .build(),
                );
            }
        }
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/api-keys",
    request_body = CreateApiKeyRequest,
    responses(
        (status = 201, description = "API key created successfully", body = CreateApiKeyResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 409, description = "Conflict - API key name already exists"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysCreate);

    enforce_custom_permission_ceiling(&auth, &request)?;

    // Capture audit fields before request is moved into the service call.
    let role_type = request.role_type.clone();
    let permissions = request.permissions.clone();

    let api_key = state
        .api_key_service
        .create_api_key(auth.user_id(), request.into())
        .await
        .map_err(|e| e.to_problem())?;

    let audit = ApiKeyCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        api_key_id: api_key.id,
        api_key_name: api_key.name.clone(),
        role_type,
        permissions,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for API key {} creation: {}",
            api_key.id, e
        );
    }

    state.telemetry.report(temps_core::TelemetryEvent::new(
        temps_core::TelemetryEventKind::ApiKeyCreated,
    ));

    Ok((StatusCode::CREATED, Json(api_key)))
}

#[utoipa::path(
    get,
    path = "/api-keys",
    params(
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Items per page (default: 20)")
    ),
    responses(
        (status = 200, description = "API keys retrieved successfully", body = ApiKeyListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_api_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Query(query): Query<ListApiKeysQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysRead);

    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);

    match state
        .api_key_service
        .list_api_keys(auth.user_id(), page, page_size)
        .await
    {
        Ok(response) => Ok(Json(response)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    get,
    path = "/api-keys/{id}",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key retrieved successfully", body = ApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysRead);

    match state
        .api_key_service
        .get_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    put,
    path = "/api-keys/{id}",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    request_body = UpdateApiKeyRequest,
    responses(
        (status = 200, description = "API key updated successfully", body = ApiKeyResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 409, description = "Conflict - API key name already exists"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
    Json(request): Json<UpdateApiKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    match state
        .api_key_service
        .update_api_key(auth.user_id(), api_key_id, request.into())
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    delete,
    path = "/api-keys/{id}",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 204, description = "API key deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysDelete);

    match state
        .api_key_service
        .delete_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    post,
    path = "/api-keys/{id}/deactivate",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key deactivated successfully", body = ApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn deactivate_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    match state
        .api_key_service
        .deactivate_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    post,
    path = "/api-keys/{id}/activate",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key activated successfully", body = ApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn activate_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    match state
        .api_key_service
        .activate_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    post,
    path = "/api-keys/{id}/rotate",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key rotated successfully; the response contains the new plaintext secret, shown only once", body = CreateApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn rotate_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    let rotated = state
        .api_key_service
        .rotate_api_key(auth.user_id(), api_key_id)
        .await
        .map_err(|e| e.to_problem())?;

    let audit = ApiKeyRotatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        api_key_id: rotated.id,
        api_key_name: rotated.name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for API key {} rotation: {}",
            api_key_id, e
        );
    }

    Ok(Json(rotated))
}

#[utoipa::path(
    get,
    path = "/api-keys/permissions",
    responses(
        (status = 200, description = "Available permissions and roles retrieved successfully", body = AvailablePermissions),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_api_key_permissions(RequireAuth(_auth): RequireAuth) -> impl IntoResponse {
    // No specific permission check needed - authenticated users can see available permissions
    Json(get_available_permissions())
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_api_key,
        list_api_keys,
        get_api_key,
        update_api_key,
        delete_api_key,
        activate_api_key,
        deactivate_api_key,
        rotate_api_key,
        get_api_key_permissions,
    ),
    components(
        schemas(
            CreateApiKeyRequest,
            UpdateApiKeyRequest,
            ApiKeyResponse,
            CreateApiKeyResponse,
            ApiKeyListResponse,
            ListApiKeysQuery,
            AvailablePermissions,
        )
    ),
    tags(
        (name = "API Keys", description = "API key management endpoints")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub struct ApiKeyApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use temps_entities::users;

    use crate::context::AuthContext;
    use crate::permissions::Role;

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

    fn custom_request(permissions: Vec<&str>) -> CreateApiKeyRequest {
        CreateApiKeyRequest {
            name: "test-key".to_string(),
            role_type: "custom".to_string(),
            permissions: Some(permissions.into_iter().map(String::from).collect()),
            expires_at: None,
        }
    }

    /// `Role::User` does not hold `Permission::SystemAdmin` — this is the
    /// exact escalation path that motivated this check (see finding logged
    /// during ADR 0008 research).
    #[test]
    fn custom_role_cannot_grant_permission_actor_does_not_hold() {
        let auth = user_auth(Role::User);
        let req = custom_request(vec!["system:admin"]);
        let err = enforce_custom_permission_ceiling(&auth, &req).unwrap_err();
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    /// `Role::User` does hold `ProjectsRead` — a custom key scoped to a
    /// subset of the actor's own permissions must succeed.
    #[test]
    fn custom_role_can_grant_permission_actor_holds() {
        let auth = user_auth(Role::User);
        let req = custom_request(vec!["projects:read"]);
        assert!(enforce_custom_permission_ceiling(&auth, &req).is_ok());
    }

    /// `Role::Admin` holds every permission, including `SystemAdmin` —
    /// confirms the check is a real ceiling, not a blanket denial.
    #[test]
    fn admin_can_grant_any_permission() {
        let auth = user_auth(Role::Admin);
        let req = custom_request(vec!["system:admin", "users:delete"]);
        assert!(enforce_custom_permission_ceiling(&auth, &req).is_ok());
    }

    /// Non-custom role types (predefined roles) bypass the ceiling check
    /// entirely — they don't carry an arbitrary permission list to bound.
    #[test]
    fn predefined_role_type_skips_ceiling_check() {
        let auth = user_auth(Role::User);
        let req = CreateApiKeyRequest {
            name: "test-key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };
        assert!(enforce_custom_permission_ceiling(&auth, &req).is_ok());
    }

    /// An unparsable permission string is left to the service layer's
    /// existing validation (400), not rejected here as a ceiling violation.
    #[test]
    fn unparsable_permission_string_is_not_a_ceiling_violation() {
        let auth = user_auth(Role::User);
        let req = custom_request(vec!["not-a-real-permission"]);
        assert!(enforce_custom_permission_ceiling(&auth, &req).is_ok());
    }
}
