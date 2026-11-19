// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preparing sandbox hosts: pulling the default sandbox image before the
//! first create (see [`crate::services::image_preparation`]). Reading is open
//! to anyone who can read sandboxes; starting a multi-gigabyte pull or
//! changing a host's policy is an administrator's call and is audit-logged.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_auth::RequireAuth;
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditOperation, RequestMetadata};

use crate::handlers::sandboxes::{require_sandbox_admin, require_sandbox_read};
use crate::handlers::SandboxAppState;
use crate::services::image_preparation::{ImagePreparationError, SandboxImagePreparationResponse};

impl From<ImagePreparationError> for Problem {
    fn from(error: ImagePreparationError) -> Self {
        match error {
            ImagePreparationError::Database { .. } => {
                tracing::error!(%error, "sandbox image preparation: database error");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Sandbox Image Preparation Failed")
                    .with_detail(error.to_string())
            }
            ImagePreparationError::Placement { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Sandbox Hosts Unavailable")
                    .with_detail(error.to_string())
            }
            ImagePreparationError::HostNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Sandbox Host Not Found")
                    .with_detail(error.to_string())
            }
            ImagePreparationError::HostNotEligible { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Host Cannot Run Sandboxes")
                    .with_detail(error.to_string())
            }
            ImagePreparationError::NoEligibleHost => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("No Sandbox Host")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Start preparing sandbox hosts.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareSandboxImagesBody {
    /// The host to prepare: a node id, or `0` for the control plane.
    /// Omitted or `null`: every host eligible for sandbox placement.
    #[serde(default)]
    pub node_id: Option<i32>,
}

/// The hosts a "prepare" was accepted for.
#[derive(Debug, Serialize, ToSchema)]
pub struct PrepareSandboxImagesResponse {
    /// The hosts now kept prepared and asked to pull (node id, `0` for the
    /// control plane). Their progress is read from
    /// `GET /v1/sandboxes/image-preparation`.
    pub node_ids: Vec<i32>,
}

/// Turn keeping one host prepared on or off.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateHostImagePreparationBody {
    /// Keep this host's default sandbox image pulled, re-pulling it when an
    /// upgrade changes the default. `false` stops that; it removes nothing.
    pub keep_prepared: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxImagesPrepareRequestedAudit {
    context: AuditContext,
    requested_node_id: Option<i32>,
    prepared_node_ids: Vec<i32>,
}

impl AuditOperation for SandboxImagesPrepareRequestedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_IMAGES_PREPARE_REQUESTED".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[derive(Debug, Clone, Serialize)]
struct SandboxImagePreparationUpdatedAudit {
    context: AuditContext,
    node_id: i32,
    keep_prepared: bool,
}

impl AuditOperation for SandboxImagePreparationUpdatedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_IMAGE_PREPARATION_UPDATED".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

async fn write_audit<A: AuditOperation>(state: &SandboxAppState, event: &A) {
    if let Some(audit) = &state.audit_service {
        if let Err(error) = audit.create_audit_log(event).await {
            tracing::error!(
                operation = %event.operation_type(),
                "sandbox image preparation: failed to write audit log: {}",
                error
            );
        }
    }
}

/// Every host that can run sandboxes, with whether its default sandbox
/// image is pulled (with download progress while it pulls), and whether
/// Temps keeps it prepared.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/image-preparation",
    responses(
        (status = 200, description = "Image preparation of every sandbox host", body = SandboxImagePreparationResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "The hosts could not be listed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_sandbox_image_preparation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_read(&auth)?;
    Ok(Json(state.image_preparation.status().await?))
}

/// Pull the default sandbox image on every eligible host (or one), so the
/// first sandbox there starts without a multi-gigabyte download, and keep
/// those hosts prepared across upgrades. Saves the policy of every host at
/// once and returns; the hosts are asked to pull in the background and
/// their progress is read from the GET. Admin only.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/image-preparation",
    request_body(content = Option<PrepareSandboxImagesBody>, description = "Optional; an empty body prepares every eligible host"),
    responses(
        (status = 202, description = "Preparation started on these hosts", body = PrepareSandboxImagesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Administrator role required"),
        (status = 404, description = "No such host"),
        (status = 422, description = "The host (or every host) cannot take sandboxes"),
        (status = 500, description = "The preparation could not be recorded")
    ),
    security(("bearer_auth" = []))
)]
pub async fn prepare_sandbox_images(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    body: Option<Json<PrepareSandboxImagesBody>>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_admin(&auth)?;
    let requested = body.and_then(|Json(b)| b.node_id);
    let prepared = state.image_preparation.prepare(requested).await?;
    let user_id = auth.user_id();
    tracing::info!(user_id, requested_node_id = ?requested, prepared_node_ids = ?prepared, "sandbox image preparation requested");
    write_audit(
        &state,
        &SandboxImagesPrepareRequestedAudit {
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            requested_node_id: requested,
            prepared_node_ids: prepared.clone(),
        },
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(PrepareSandboxImagesResponse { node_ids: prepared }),
    ))
}

/// Turn keeping one host's sandbox image prepared on or off. Admin only.
#[utoipa::path(
    tag = "Sandboxes",
    put,
    path = "/v1/sandboxes/image-preparation/nodes/{node_id}",
    params(("node_id" = i32, Path, description = "Node id, or `0` for the control plane")),
    request_body = UpdateHostImagePreparationBody,
    responses(
        (status = 200, description = "Updated image preparation of every host", body = SandboxImagePreparationResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Administrator role required"),
        (status = 404, description = "No such host"),
        (status = 500, description = "The change could not be saved")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_host_image_preparation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node_id): Path<i32>,
    Json(body): Json<UpdateHostImagePreparationBody>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_admin(&auth)?;
    state
        .image_preparation
        .set_keep_prepared(node_id, body.keep_prepared)
        .await?;
    let user_id = auth.user_id();
    tracing::info!(
        user_id,
        node_id,
        keep_prepared = body.keep_prepared,
        "sandbox image preparation policy updated"
    );
    write_audit(
        &state,
        &SandboxImagePreparationUpdatedAudit {
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            node_id,
            keep_prepared: body.keep_prepared,
        },
    )
    .await;
    Ok(Json(state.image_preparation.status().await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_core::{
        SensitiveAction, SensitiveActionAuthorizationError, SensitiveActionAuthorizer,
        SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use tower::ServiceExt;

    use crate::services::sandbox_service::tests::{
        image_preparation_for, preview_test_service_with_db,
    };

    struct AllowAll;

    #[async_trait::async_trait]
    impl SensitiveActionAuthorizer for AllowAll {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(SensitiveActionDecision::Allow)
        }
    }

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 3,
            name: "Developer".to_string(),
            email: "developer@example.com".to_string(),
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

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "192.0.2.10".into(),
            user_agent: "test".into(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".into(),
            scheme: "http".into(),
            host: "localhost".into(),
            is_secure: false,
        }
    }

    /// The sandbox routes over a database that answers nothing: every
    /// request here must be decided before the service touches it.
    fn app(role: Option<Role>) -> axum::Router {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let service = Arc::new(preview_test_service_with_db(db));
        let image_preparation = image_preparation_for(&service);
        let state = Arc::new(SandboxAppState {
            sandbox_service: service,
            snapshot_service: None,
            project_access_checker: None,
            audit_service: None,
            sensitive_action_authorizer: Arc::new(AllowAll),
            image_preparation,
        });
        let router = crate::handlers::configure_routes()
            .with_state(state)
            .layer(Extension(metadata()));
        match role {
            Some(role) => router.layer(Extension(AuthContext::new_persisted_session(
                user(),
                role,
                9,
            ))),
            None => router,
        }
    }

    async fn send(app: axum::Router, method: &str, uri: &str, body: &str) -> StatusCode {
        let request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request");
        app.oneshot(request).await.expect("response").status()
    }

    #[test]
    fn the_prepare_body_is_documented_as_optional() {
        use utoipa::OpenApi;
        let api = crate::handlers::SandboxApiDoc::openapi();
        let post = api.paths.paths["/v1/sandboxes/image-preparation"]
            .post
            .as_ref()
            .expect("POST is documented");
        let Some(utoipa::openapi::RefOr::T(body)) = post.request_body.as_ref() else {
            panic!("the request body is documented inline");
        };
        // OpenAPI's default for `required` is false: absent means optional.
        assert_ne!(body.required, Some(utoipa::openapi::Required::True));
    }

    #[tokio::test]
    async fn every_route_needs_a_signed_in_caller() {
        for (method, uri, body) in [
            ("GET", "/v1/sandboxes/image-preparation", ""),
            ("POST", "/v1/sandboxes/image-preparation", "{}"),
            (
                "PUT",
                "/v1/sandboxes/image-preparation/nodes/0",
                r#"{"keep_prepared": true}"#,
            ),
        ] {
            let status = send(app(None), method, uri, body).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn only_administrators_start_pulls_or_change_the_policy() {
        for (method, uri, body) in [
            ("POST", "/v1/sandboxes/image-preparation", "{}"),
            (
                "POST",
                "/v1/sandboxes/image-preparation",
                r#"{"node_id": 0}"#,
            ),
            (
                "PUT",
                "/v1/sandboxes/image-preparation/nodes/0",
                r#"{"keep_prepared": false}"#,
            ),
        ] {
            let status = send(app(Some(Role::User)), method, uri, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn a_policy_update_without_its_field_is_rejected() {
        let status = send(
            app(Some(Role::Admin)),
            "PUT",
            "/v1/sandboxes/image-preparation/nodes/0",
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn errors_map_to_problems_that_name_the_host() {
        let cases = [
            (
                ImagePreparationError::HostNotFound {
                    host: "42".to_string(),
                },
                StatusCode::NOT_FOUND,
                "42",
            ),
            (
                ImagePreparationError::HostNotEligible {
                    host: "worker-a".to_string(),
                    reason: "draining".to_string(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "draining",
            ),
            (
                ImagePreparationError::NoEligibleHost,
                StatusCode::UNPROCESSABLE_ENTITY,
                "placement",
            ),
            (
                ImagePreparationError::Database {
                    operation: "saving",
                    host: "control plane".to_string(),
                    source: sea_orm::DbErr::Custom("connection reset".to_string()),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
                "connection reset",
            ),
        ];
        for (error, status, needle) in cases {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status);
            let body = serde_json::to_value(&problem.body).expect("problem body");
            let detail = body["detail"].as_str().expect("detail").to_string();
            assert!(detail.contains(needle), "{detail}");
        }
    }
}
