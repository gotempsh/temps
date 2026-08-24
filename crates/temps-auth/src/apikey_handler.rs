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
    /// Central policy evaluator for sensitive mutations.
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}

/// Privilege-escalation ceiling: an API key must never grant more than the
/// creating user's own effective permissions — whether requested as an
/// explicit `role_type: "custom"` permission list, or as a predefined role
/// name (`role_type: "admin"`, `"platform_admin"`, etc.).
///
/// Without this check, any role holding `ApiKeysCreate` (e.g. the standard
/// `Role::User`) could mint a key carrying `Permission::SystemAdmin` or any
/// other permission outside its own role — a full privilege escalation,
/// since `ApiKeyService::create_api_key` only validates that requested
/// custom permission strings parse, and for predefined roles only that the
/// role name is one of the known roles — neither path checks that the
/// requester actually holds the permissions being granted. Unparsable
/// custom permission strings and unrecognized role names are left to that
/// existing validation, which returns 400.
pub(crate) fn enforce_permission_ceiling_for_role(
    auth: &crate::context::AuthContext,
    role_type: &str,
    permissions: Option<&[String]>,
) -> Result<(), Problem> {
    if role_type == "custom" {
        let Some(permissions) = permissions else {
            return Ok(());
        };
        for perm_str in permissions {
            if let Some(perm) = crate::permissions::Permission::from_str(perm_str) {
                if !auth.has_permission(&perm) {
                    return Err(permission_ceiling_exceeded(perm_str));
                }
            }
        }
        return Ok(());
    }

    // Predefined role type: the minted key inherits that role's ENTIRE
    // permission set, so every permission the role carries must also be
    // held by the requester — not just some of them.
    if let Some(role) = crate::permissions::Role::from_str(role_type) {
        for perm in role.permissions() {
            if !auth.has_permission(perm) {
                return Err(permission_ceiling_exceeded(&perm.to_string()));
            }
        }
    }
    Ok(())
}

fn enforce_permission_ceiling(
    auth: &crate::context::AuthContext,
    request: &CreateApiKeyRequest,
) -> Result<(), Problem> {
    enforce_permission_ceiling_for_role(auth, &request.role_type, request.permissions.as_deref())
}

fn permission_ceiling_exceeded(perm_str: &str) -> Problem {
    temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
        .type_("https://temps.sh/probs/permission-ceiling-exceeded")
        .title("Forbidden")
        .detail(format!(
            "Cannot grant permission '{perm_str}': it exceeds your own permissions"
        ))
        .value("error_code", "PERMISSION_CEILING_EXCEEDED")
        .build()
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
        (status = 428, description = "Recent MFA verification required"),
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

    enforce_permission_ceiling(&auth, &request)?;

    crate::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::CreateApiKey,
    )
    .await?;

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
        (status = 428, description = "Recent MFA verification required"),
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
    permission_guard!(auth, ApiKeysCreate);

    crate::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::RotateApiKey { api_key_id },
    )
    .await?;

    let target = state
        .api_key_service
        .get_api_key(auth.user_id(), api_key_id)
        .await
        .map_err(|e| e.to_problem())?;
    enforce_permission_ceiling_for_role(&auth, &target.role_type, target.permissions.as_deref())?;

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
    use async_trait::async_trait;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::{SensitiveActionAuthorizationError, SensitiveActionDecision};
    use temps_entities::users;

    use crate::context::AuthContext;
    use crate::permissions::Role;

    struct RequireVerificationAuthorizer;

    #[async_trait]
    impl temps_core::SensitiveActionAuthorizer for RequireVerificationAuthorizer {
        async fn authorize(
            &self,
            _action: &temps_core::SensitiveAction,
            _principal: &temps_core::SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false,
            })
        }
    }

    struct NoopAuditLogger;

    #[async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::anyhow::Result<()> {
            Ok(())
        }
    }

    fn guarded_state() -> Arc<ApiKeyState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        Arc::new(ApiKeyState {
            api_key_service: Arc::new(ApiKeyService::new(db)),
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            audit_service: Arc::new(NoopAuditLogger),
            sensitive_action_authorizer: Arc::new(RequireVerificationAuthorizer),
        })
    }

    fn request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "sensitive-action-handler-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

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

    fn user_auth(role: Role) -> AuthContext {
        AuthContext::new_session(test_user(42), role)
    }

    fn api_key_auth(role: Role) -> AuthContext {
        AuthContext::new_api_key(test_user(42), Some(role), None, "caller".to_string(), 7)
    }

    fn persisted_admin_auth() -> AuthContext {
        AuthContext::new_persisted_session(test_user(42), Role::Admin, 99)
    }

    #[tokio::test]
    async fn create_handler_stops_before_service_when_step_up_is_required() {
        let result = create_api_key(
            RequireAuth(persisted_admin_auth()),
            State(guarded_state()),
            Extension(request_metadata()),
            Json(predefined_request("admin")),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("API key creation must stop before querying the service"),
        };

        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(
            error.body.get("action"),
            Some(&serde_json::json!("create_api_key"))
        );
    }

    #[tokio::test]
    async fn rotation_handler_stops_before_service_when_step_up_is_required() {
        let result = rotate_api_key(
            RequireAuth(persisted_admin_auth()),
            State(guarded_state()),
            Extension(request_metadata()),
            Path(123),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("API key rotation must stop before querying the service"),
        };

        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(
            error.body.get("action"),
            Some(&serde_json::json!("rotate_api_key"))
        );
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
        let err = enforce_permission_ceiling(&auth, &req).unwrap_err();
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    /// `Role::User` does hold `ProjectsRead` — a custom key scoped to a
    /// subset of the actor's own permissions must succeed.
    #[test]
    fn custom_role_can_grant_permission_actor_holds() {
        let auth = user_auth(Role::User);
        let req = custom_request(vec!["projects:read"]);
        assert!(enforce_permission_ceiling(&auth, &req).is_ok());
    }

    /// `Role::Admin` holds every permission, including `SystemAdmin` —
    /// confirms the check is a real ceiling, not a blanket denial.
    #[test]
    fn admin_can_grant_any_permission() {
        let auth = user_auth(Role::Admin);
        let req = custom_request(vec!["system:admin", "users:delete"]);
        assert!(enforce_permission_ceiling(&auth, &req).is_ok());
    }

    fn predefined_request(role_type: &str) -> CreateApiKeyRequest {
        CreateApiKeyRequest {
            name: "test-key".to_string(),
            role_type: role_type.to_string(),
            permissions: None,
            expires_at: None,
        }
    }

    /// `Role::User` does not hold every permission `Role::Admin` carries —
    /// self-minting a key with `role_type: "admin"` is the predefined-role
    /// counterpart of the custom-permission escalation above, and must be
    /// denied the same way.
    #[test]
    fn predefined_role_type_cannot_grant_role_actor_does_not_hold() {
        let auth = user_auth(Role::User);
        let req = predefined_request("admin");
        let err = enforce_permission_ceiling(&auth, &req).unwrap_err();
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn lower_privileged_api_key_cannot_rotate_admin_key() {
        let auth = api_key_auth(Role::User);
        let err = enforce_permission_ceiling_for_role(&auth, "admin", None)
            .expect_err("user API key must not acquire an admin key secret");

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            err.body.get("error_code"),
            Some(&serde_json::json!("PERMISSION_CEILING_EXCEEDED"))
        );
    }

    #[test]
    fn api_key_can_rotate_key_within_its_permission_ceiling() {
        let auth = api_key_auth(Role::User);

        assert!(enforce_permission_ceiling_for_role(&auth, "user", None).is_ok());
        assert!(enforce_permission_ceiling_for_role(
            &auth,
            "custom",
            Some(&["projects:read".to_string()])
        )
        .is_ok());
    }

    /// A user requesting a predefined role that is a subset of (or equal
    /// to) their own permissions — the common case of a `Role::User`
    /// minting a `role_type: "user"` key — must succeed.
    #[test]
    fn predefined_role_type_can_grant_role_actor_holds() {
        let auth = user_auth(Role::User);
        let req = predefined_request("user");
        assert!(enforce_permission_ceiling(&auth, &req).is_ok());
    }

    /// `Role::Admin` holds every permission, including everything
    /// `Role::Admin` itself carries — confirms the predefined-role check is
    /// a real ceiling, not a blanket denial.
    #[test]
    fn admin_can_grant_predefined_admin_role() {
        let auth = user_auth(Role::Admin);
        let req = predefined_request("admin");
        assert!(enforce_permission_ceiling(&auth, &req).is_ok());
    }

    /// An unrecognized role name is left to the service layer's existing
    /// validation (400), not rejected here as a ceiling violation.
    #[test]
    fn unrecognized_role_type_is_not_a_ceiling_violation() {
        let auth = user_auth(Role::User);
        let req = predefined_request("not-a-real-role");
        assert!(enforce_permission_ceiling(&auth, &req).is_ok());
    }

    /// An unparsable permission string is left to the service layer's
    /// existing validation (400), not rejected here as a ceiling violation.
    #[test]
    fn unparsable_permission_string_is_not_a_ceiling_violation() {
        let auth = user_auth(Role::User);
        let req = custom_request(vec!["not-a-real-permission"]);
        assert!(enforce_permission_ceiling(&auth, &req).is_ok());
    }
}
