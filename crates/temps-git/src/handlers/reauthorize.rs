// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated start and completion of an existing connection's OAuth grant.
use super::{
    audit::{AuditContext, GitConnectionReauthorizationAudit},
    base::ConnectionResponse,
    types::GitAppState,
};
use axum::{
    extract::{Path, State},
    routing::post,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::{problemdetails::Problem, RequestMetadata, SensitiveAction};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct ReauthorizationResponse {
    pub authorization_url: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CompleteReauthorizationRequest {
    pub state: String,
    /// Empty when the user cancelled at the provider; the state is still consumed.
    pub code: String,
}

async fn audit(
    state: &GitAppState,
    metadata: RequestMetadata,
    user_id: i32,
    connection_id: i32,
    provider_id: i32,
    completed: bool,
) {
    let event = GitConnectionReauthorizationAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        connection_id,
        provider_id,
        completed,
    };
    if let Err(error) = state.audit_service.create_audit_log(&event).await {
        tracing::error!(connection_id, provider_id, user_id, %error, "Failed to audit git connection reauthorization");
    }
}

#[utoipa::path(post, path = "/git-connections/{connection_id}/reauthorize",
    params(("connection_id" = i32, Path, description = "Existing OAuth connection ID")),
    responses((status = 200, body = ReauthorizationResponse, description = "OAuth authorization URL"),
        (status = 400, description = "Not an OAuth connection or provider misconfigured"),
        (status = 401, description = "Authentication required"), (status = 403, description = "Permission denied"),
        (status = 428, description = "Recent MFA verification required"),
        (status = 404, description = "Connection missing or inaccessible"), (status = 500, description = "Internal error")),
    tag = "Git Providers", security(("bearer_auth" = [])))]
pub async fn reauthorize_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GitAppState>>,
    Path(connection_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Json<ReauthorizationResponse>, Problem> {
    permission_guard!(auth, GitConnectionsWrite);
    let user = auth.require_user().map_err(|message| {
        temps_core::error_builder::forbidden()
            .detail(message)
            .build()
    })?;
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::ReauthorizeGitConnection,
    )
    .await?;
    let (authorization_url, provider_id) = state
        .git_provider_manager
        .start_connection_reauthorization(connection_id, user.id)
        .await?;
    audit(&state, metadata, user.id, connection_id, provider_id, false).await;
    Ok(Json(ReauthorizationResponse { authorization_url }))
}

#[utoipa::path(post, path = "/git-connections/reauthorize/complete",
    request_body = CompleteReauthorizationRequest,
    responses((status = 200, body = ConnectionResponse, description = "Original connection reauthorized"),
        (status = 400, description = "Invalid state, cancellation or provider configuration"),
        (status = 401, description = "Authentication required"), (status = 403, description = "Permission denied"),
        (status = 428, description = "Recent MFA verification required"),
        (status = 404, description = "Connection missing or inaccessible"), (status = 409, description = "Wrong provider account"),
        (status = 500, description = "Internal error")),
    tag = "Git Providers", security(("bearer_auth" = [])))]
pub async fn complete_connection_reauthorization(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GitAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CompleteReauthorizationRequest>,
) -> Result<Json<ConnectionResponse>, Problem> {
    permission_guard!(auth, GitConnectionsWrite);
    let user = auth.require_user().map_err(|message| {
        temps_core::error_builder::forbidden()
            .detail(message)
            .build()
    })?;
    // Check step-up before consuming state, so the console can verify and retry.
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::ReauthorizeGitConnection,
    )
    .await?;
    let connection = state
        .git_provider_manager
        .complete_connection_reauthorization(&request.state, request.code, user.id)
        .await?;
    audit(
        &state,
        metadata,
        user.id,
        connection.id,
        connection.provider_id,
        true,
    )
    .await;
    Ok(Json(ConnectionResponse::from(connection)))
}

pub fn configure_routes() -> Router<Arc<GitAppState>> {
    Router::new()
        .route(
            "/git-connections/{connection_id}/reauthorize",
            post(reauthorize_connection),
        )
        .route(
            "/git-connections/reauthorize/complete",
            post(complete_connection_reauthorization),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{HeaderMap, Request, StatusCode},
        response::IntoResponse,
    };
    use temps_auth::{AuthContext, Role};
    use temps_core::AuditOperation;
    use temps_entities::users;
    use tower::ServiceExt;
    #[test]
    fn reauthorization_openapi_paths_match_the_router() {
        use utoipa::OpenApi;
        let document = super::super::base::GitProvidersApiDoc::openapi();
        for path in [
            "/git-connections/{connection_id}/reauthorize",
            "/git-connections/reauthorize/complete",
        ] {
            assert!(
                document.paths.paths.contains_key(path),
                "missing route {path}"
            );
            assert!(!document.paths.paths.contains_key(&format!("/{path}")));
        }
    }

    fn test_user() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
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

    fn test_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    #[tokio::test]
    async fn reauthorization_endpoints_require_authentication() {
        for path in [
            "/git-connections/11/reauthorize",
            "/git-connections/reauthorize/complete",
        ] {
            let response = configure_routes()
                .with_state(super::super::public_route_tests::state())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"state":"test","code":"test"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn reauthorization_endpoints_refuse_readers_before_accessing_state() {
        let state = super::super::public_route_tests::state();
        let error = reauthorize_connection(
            RequireAuth(AuthContext::new_session(test_user(), Role::Reader)),
            State(state.clone()),
            Path(11),
            Extension(test_metadata()),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        let error = complete_connection_reauthorization(
            RequireAuth(AuthContext::new_session(test_user(), Role::Reader)),
            State(state),
            Extension(test_metadata()),
            Json(CompleteReauthorizationRequest {
                state: "test".into(),
                code: "test".into(),
            }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    }

    struct RequireVerification;

    #[async_trait::async_trait]
    impl temps_core::SensitiveActionAuthorizer for RequireVerification {
        async fn authorize(
            &self,
            action: &SensitiveAction,
            _principal: &temps_core::SensitiveActionPrincipal,
        ) -> Result<
            temps_core::SensitiveActionDecision,
            temps_core::SensitiveActionAuthorizationError,
        > {
            assert_eq!(action.as_str(), "reauthorize_git_connection");
            Ok(temps_core::SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false,
            })
        }
    }

    #[tokio::test]
    async fn reauthorization_requires_step_up_before_consuming_state() {
        // The empty mock database fails any query, proving the state survives
        // the MFA challenge and can be retried after verification.
        let mut state = (*super::super::public_route_tests::state()).clone();
        state.sensitive_action_authorizer = Arc::new(RequireVerification);
        let state = Arc::new(state);
        let error = reauthorize_connection(
            RequireAuth(AuthContext::new_persisted_session(
                test_user(),
                Role::Admin,
                1,
            )),
            State(state.clone()),
            Path(11),
            Extension(test_metadata()),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        let error = complete_connection_reauthorization(
            RequireAuth(AuthContext::new_persisted_session(
                test_user(),
                Role::Admin,
                1,
            )),
            State(state),
            Extension(test_metadata()),
            Json(CompleteReauthorizationRequest {
                state: "reauthorize.test".into(),
                code: "test".into(),
            }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
    }

    #[tokio::test]
    async fn reauthorization_errors_are_typed_problem_details() {
        use crate::services::git_provider_manager::GitProviderManagerError;
        for (error, status, code) in [
            (
                GitProviderManagerError::ReauthorizationUnsupported { connection_id: 11 },
                StatusCode::BAD_REQUEST,
                "reauthorization_unsupported",
            ),
            (
                GitProviderManagerError::ReauthorizationAccountMismatch {
                    connection_id: 11,
                    expected: "example-account".into(),
                    actual: "other-account".into(),
                },
                StatusCode::CONFLICT,
                "reauthorization_account_mismatch",
            ),
        ] {
            let response = Problem::from(error).into_response();
            assert_eq!(response.status(), status);
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let body = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["code"], code);
            assert!(value["detail"].as_str().unwrap().contains("11"));
        }
    }

    #[test]
    fn reauthorization_audit_has_identity_and_no_credentials() {
        let event = GitConnectionReauthorizationAudit {
            context: AuditContext {
                user_id: 4,
                ip_address: None,
                user_agent: "test".into(),
            },
            connection_id: 11,
            provider_id: 7,
            completed: true,
        };
        let value: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&event).unwrap()).unwrap();
        assert_eq!(value["connection_id"], 11);
        assert_eq!(value["provider_id"], 7);
        assert_eq!(value["context"]["user_id"], 4);
        assert_eq!(event.operation_type(), "GIT_CONNECTION_REAUTHORIZED");
        assert!(!value.to_string().contains("token"));
    }
}
