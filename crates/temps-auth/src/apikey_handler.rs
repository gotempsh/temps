// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
use crate::ai_harness::{
    AiHarnessCredential, AiHarnessCredentialKind, AiHarnessState, AiHarnessStatusResponse,
};
use crate::apikey_handler_types::{
    ApiKeyListResponse, ApiKeyResponse, CreateApiKeyRequest, CreateApiKeyResponse,
    UpdateApiKeyRequest,
};
use crate::audit::{
    ApiKeyCreatedAudit, ApiKeyDeletedAudit, ApiKeyRotatedAudit, ApiKeyUpdatedAudit,
};
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
    // Explicitly requested permissions are checked for every role type, not
    // only `custom`: a predefined-role request that also lists permissions
    // above the requester's ceiling is rejected (403) rather than having the
    // list silently dropped by the service.
    enforce_explicit_permission_ceiling(auth, permissions)?;

    if role_type == "custom" {
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

/// Reject any explicitly requested permission the requester does not hold.
///
/// Shared by key creation/rotation (via
/// [`enforce_permission_ceiling_for_role`]) and key update, so a permission
/// list can never widen a key beyond its owner's own permissions on any
/// write path — including update, whose service layer does not currently
/// persist `permissions` but must not become an escalation path if it ever
/// does. Unparsable strings are left to the service layer's validation.
fn enforce_explicit_permission_ceiling(
    auth: &crate::context::AuthContext,
    permissions: Option<&[String]>,
) -> Result<(), Problem> {
    for perm_str in permissions.unwrap_or_default() {
        if let Some(perm) = crate::permissions::Permission::from_str(perm_str) {
            if !auth.has_permission(&perm) {
                return Err(permission_ceiling_exceeded(perm_str));
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
        (status = 400, description = "Bad request: a blank name, an expiry in the past, permissions with a role other than custom, or an invalid scope"),
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
        project_id: api_key.project_id,
        environment_id: api_key.environment_id,
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
    patch,
    path = "/api-keys/{id}",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    request_body = UpdateApiKeyRequest,
    responses(
        (status = 200, description = "API key updated successfully", body = ApiKeyResponse),
        (status = 400, description = "Bad request: a blank name, an expiry in the past or removed, or permissions that differ from the key's (they cannot change after creation)"),
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
    Extension(metadata): Extension<RequestMetadata>,
    Path(api_key_id): Path<i32>,
    Json(request): Json<UpdateApiKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    enforce_explicit_permission_ceiling(&auth, request.permissions.as_deref())?;

    let changed_fields = [
        ("name", request.name.is_some()),
        ("is_active", request.is_active.is_some()),
        ("expires_at", matches!(request.expires_at, Some(Some(_)))),
    ]
    .into_iter()
    .filter_map(|(field, set)| set.then_some(field))
    .collect::<Vec<_>>();

    let api_key = state
        .api_key_service
        .update_api_key(auth.user_id(), api_key_id, request.into())
        .await
        .map_err(|e| e.to_problem())?;

    let audit = ApiKeyUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        api_key_id: api_key.id,
        api_key_name: api_key.name.clone(),
        changed_fields,
        is_active: api_key.is_active,
        expires_at: api_key.expires_at,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for API key {} update: {}",
            api_key.id, e
        );
    }

    Ok(Json(api_key))
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
    Extension(metadata): Extension<RequestMetadata>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysDelete);

    let deleted = state
        .api_key_service
        .delete_api_key(auth.user_id(), api_key_id)
        .await
        .map_err(|e| e.to_problem())?;

    let audit = ApiKeyDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        api_key_id: deleted.id,
        api_key_name: deleted.name,
        key_prefix: deleted.key_prefix,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for API key {} deletion: {}",
            deleted.id, e
        );
    }

    Ok(StatusCode::NO_CONTENT)
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

/// Whether the caller's external AI harness has connected.
///
/// Backs the console's "Let your AI operate Temps" onboarding and its
/// app-wide "finish connecting" banner. `connected` when the dedicated
/// `Temps AI harness` key has authenticated a request, or when a key minted
/// by a browser-approved `temps login` after it has: the CLI falls back to
/// that flow when it is not handed a usable key, so the harness works
/// without ever sending the dedicated key.
#[utoipa::path(
    get,
    path = "/api-keys/ai-harness/status",
    responses(
        (status = 200, description = "AI harness connection status for the caller", body = AiHarnessStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_ai_harness_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysRead);

    let status = state
        .api_key_service
        .ai_harness_status(auth.user_id())
        .await
        .map_err(|e| e.to_problem())?;
    Ok(Json(status))
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
        get_ai_harness_status,
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
            AiHarnessStatusResponse,
            AiHarnessState,
            AiHarnessCredential,
            AiHarnessCredentialKind,
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
        ) -> temps_core::AuditResult<()> {
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
            project_id: None,
            environment_id: None,
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
            project_id: None,
            environment_id: None,
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

    fn predefined_request_with_permissions(
        role_type: &str,
        permissions: Vec<&str>,
    ) -> CreateApiKeyRequest {
        CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            permissions: Some(permissions.into_iter().map(String::from).collect()),
            ..predefined_request(role_type)
        }
    }

    /// A predefined role whose own permission set fits the ceiling must
    /// still be rejected when the request also lists explicit permissions
    /// the requester does not hold — previously the list was silently
    /// dropped and the key created (201).
    #[test]
    fn predefined_role_with_explicit_permissions_above_ceiling_is_rejected() {
        let auth = user_auth(Role::User);
        let req = predefined_request_with_permissions("user", vec!["users:manage", "secrets:read"]);
        let err = enforce_permission_ceiling(&auth, &req).unwrap_err();
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            err.body.get("error_code"),
            Some(&serde_json::json!("PERMISSION_CEILING_EXCEEDED"))
        );
    }

    #[test]
    fn predefined_role_with_explicit_permissions_within_ceiling_is_allowed() {
        let auth = user_auth(Role::User);
        let req = predefined_request_with_permissions("user", vec!["projects:read"]);
        assert!(enforce_permission_ceiling(&auth, &req).is_ok());
    }

    #[tokio::test]
    async fn create_handler_rejects_explicit_permissions_above_ceiling() {
        let result = create_api_key(
            RequireAuth(user_auth(Role::User)),
            State(guarded_state()),
            Extension(request_metadata()),
            Json(predefined_request_with_permissions(
                "user",
                vec!["users:manage", "secrets:read"],
            )),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("over-ceiling API key creation must be rejected"),
        };

        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            error.body.get("error_code"),
            Some(&serde_json::json!("PERMISSION_CEILING_EXCEEDED"))
        );
    }

    fn update_request(permissions: Option<Vec<&str>>) -> UpdateApiKeyRequest {
        UpdateApiKeyRequest {
            name: None,
            is_active: None,
            permissions: permissions.map(|p| p.into_iter().map(String::from).collect()),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn update_handler_rejects_permissions_above_ceiling() {
        let result = update_api_key(
            RequireAuth(user_auth(Role::User)),
            State(guarded_state()),
            Extension(request_metadata()),
            Path(123),
            Json(update_request(Some(vec!["system:admin"]))),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("over-ceiling API key update must be rejected"),
        };

        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            error.body.get("error_code"),
            Some(&serde_json::json!("PERMISSION_CEILING_EXCEEDED"))
        );
    }

    #[test]
    fn update_permissions_within_ceiling_or_absent_pass_the_check() {
        let auth = user_auth(Role::User);
        let within = update_request(Some(vec!["projects:read"]));
        assert!(enforce_explicit_permission_ceiling(&auth, within.permissions.as_deref()).is_ok());
        let absent = update_request(None);
        assert!(enforce_explicit_permission_ceiling(&auth, absent.permissions.as_deref()).is_ok());
    }

    fn harness_key_model(last_used: bool) -> temps_entities::api_keys::Model {
        let now = Utc::now();
        temps_entities::api_keys::Model {
            id: 12,
            name: crate::ai_harness::AI_HARNESS_KEY_NAME.to_string(),
            key_hash: "stored-hash-never-returned".to_string(),
            key_prefix: "tk_abc12".to_string(),
            user_id: 42,
            role_type: "admin".to_string(),
            permissions: None,
            is_active: true,
            expires_at: None,
            last_used_at: last_used.then_some(now),
            created_at: now,
            updated_at: now,
            service_id: None,
            project_id: None,
            environment_id: None,
        }
    }

    fn state_over(
        db: sea_orm::DatabaseConnection,
    ) -> (Arc<ApiKeyState>, Arc<RecordingAuditLogger>) {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = Arc::new(ApiKeyState {
            api_key_service: Arc::new(ApiKeyService::new(Arc::new(db))),
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(RequireVerificationAuthorizer),
        });
        (state, audit)
    }

    #[derive(Default)]
    struct RecordingAuditLogger {
        entries: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            let payload = operation.serialize()?;
            self.entries
                .lock()
                .unwrap()
                .push((operation.operation_type(), payload));
            Ok(())
        }
    }

    #[tokio::test]
    async fn ai_harness_status_route_requires_authentication() {
        use axum::{body::Body, http::Request, routing::get, Router};
        use tower::ServiceExt;

        let router = Router::new()
            .route("/api-keys/ai-harness/status", get(get_ai_harness_status))
            .with_state(guarded_state());
        let response = router
            .oneshot(
                Request::get("/api-keys/ai-harness/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ai_harness_status_requires_api_key_read_permission() {
        let result =
            get_ai_harness_status(RequireAuth(user_auth(Role::Reader)), State(guarded_state()))
                .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("a reader without api_keys:read must be refused"),
        };
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn ai_harness_status_returns_the_typed_status_without_secrets() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![harness_key_model(false)]])
            .append_query_results([Vec::<temps_entities::api_keys::Model>::new()])
            .into_connection();
        let (state, _) = state_over(db);

        let response = get_ai_harness_status(RequireAuth(user_auth(Role::Admin)), State(state))
            .await
            .unwrap_or_else(|_| panic!("status lookup succeeds"))
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "waiting");
        assert_eq!(json["key_id"], 12);
        assert_eq!(json["setup_path"], "/setup/ai");
        assert!(!String::from_utf8_lossy(&body).contains("stored-hash"));
    }

    #[tokio::test]
    async fn ai_harness_status_database_failure_is_a_problem_document() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("api_keys unavailable".into())])
            .into_connection();
        let (state, _) = state_over(db);

        let error =
            match get_ai_harness_status(RequireAuth(user_auth(Role::Admin)), State(state)).await {
                Err(error) => error,
                Ok(_) => panic!("a database failure must surface"),
            };
        assert_eq!(error.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(error.body.contains_key("title"));
    }

    #[tokio::test]
    async fn updating_a_key_is_audit_logged_without_its_secret() {
        let stored = harness_key_model(false);
        let mut updated = stored.clone();
        updated.is_active = false;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stored]])
            .append_query_results([vec![updated]])
            .into_connection();
        let (state, audit) = state_over(db);

        let response = update_api_key(
            RequireAuth(user_auth(Role::Admin)),
            State(state),
            Extension(request_metadata()),
            Path(12),
            Json(UpdateApiKeyRequest {
                is_active: Some(false),
                ..Default::default()
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("update succeeds"))
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let entries = audit.entries.lock().unwrap();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].0, "API_KEY_UPDATED");
        let payload: serde_json::Value = serde_json::from_str(&entries[0].1).unwrap();
        assert_eq!(payload["api_key_id"], 12);
        assert_eq!(payload["changed_fields"], serde_json::json!(["is_active"]));
        assert_eq!(payload["is_active"], false);
        assert!(!entries[0].1.contains("stored-hash"));
    }

    #[tokio::test]
    async fn a_refused_permission_change_is_a_400_and_is_not_audited() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![harness_key_model(false)]])
            .into_connection();
        let (state, audit) = state_over(db);

        let error = match update_api_key(
            RequireAuth(user_auth(Role::Admin)),
            State(state),
            Extension(request_metadata()),
            Path(12),
            Json(update_request(Some(vec!["projects:read"]))),
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("a permission change must not report success"),
        };
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.body.get("error_code"),
            Some(&serde_json::json!("API_KEY_PERMISSIONS_IMMUTABLE"))
        );
        assert!(audit.entries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_key_is_audit_logged_without_its_secret() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![harness_key_model(false)]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let (state, audit) = state_over(db);

        let response = delete_api_key(
            RequireAuth(user_auth(Role::Admin)),
            State(state),
            Extension(request_metadata()),
            Path(12),
        )
        .await
        .unwrap_or_else(|_| panic!("delete succeeds"))
        .into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let entries = audit.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        let (operation, payload) = &entries[0];
        assert_eq!(operation, "API_KEY_DELETED");
        assert!(payload.contains("\"api_key_id\":12"), "{payload}");
        assert!(payload.contains(crate::ai_harness::AI_HARNESS_KEY_NAME));
        assert!(!payload.contains("stored-hash"));
    }

    #[tokio::test]
    async fn deleting_a_missing_key_is_404_and_not_audited() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::api_keys::Model>::new()])
            .into_connection();
        let (state, audit) = state_over(db);

        let error = match delete_api_key(
            RequireAuth(user_auth(Role::Admin)),
            State(state),
            Extension(request_metadata()),
            Path(77),
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("deleting a missing key must fail"),
        };
        assert_eq!(error.status_code, StatusCode::NOT_FOUND);
        assert!(audit.entries.lock().unwrap().is_empty());
    }
}
