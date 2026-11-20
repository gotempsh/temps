// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The steps of a sandbox create, while it runs (see
//! [`crate::services::creation_progress`]).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Json,
};

use temps_auth::RequireAuth;
use temps_core::problemdetails::Problem;

use crate::error::SandboxError;
use crate::handlers::sandboxes::require_sandbox_read;
use crate::handlers::SandboxAppState;

/// Read a create's steps by the `creation_id` its `POST /v1/sandboxes` sent.
/// Only the user who started the create can read it.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/creations/{creation_id}",
    params(("creation_id" = String, Path, description = "The `creation_id` sent with the create")),
    responses(
        (status = 200, description = "The create and its ordered steps", body = crate::services::creation_progress::SandboxCreation),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No create with this id is tracked for the caller (unknown, or finished more than 15 minutes ago)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_sandbox_creation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(creation_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_read(&auth)?;
    let creation = state
        .sandbox_service
        .creation(auth.user_id(), &creation_id)?;
    Ok(Json(creation))
}

/// Read the steps of the create that made a sandbox, while this server still
/// tracks it (it is running, or finished less than 15 minutes ago).
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/creation",
    params(("id" = String, Path, description = "Sandbox id")),
    responses(
        (status = 200, description = "The create and its ordered steps", body = crate::services::creation_progress::SandboxCreation),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such sandbox for the caller, or its create is no longer tracked")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_sandbox_creation_by_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_read(&auth)?;
    let user_id = auth.user_id();
    // Authorizes the caller for the sandbox (404 for anyone else's).
    let row = state
        .sandbox_service
        .find_by_public_id(&id, user_id)
        .await?;
    let creation = state
        .sandbox_service
        .creation_for_sandbox(&row.public_id)
        .ok_or(SandboxError::CreationNotFound {
            creation_id: format!("(create of sandbox {})", row.public_id),
            user_id,
        })?;
    Ok(Json(creation))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum::Extension;
    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_core::{
        RequestMetadata, SensitiveAction, SensitiveActionAuthorizationError,
        SensitiveActionAuthorizer, SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use temps_entities::users;
    use tower::ServiceExt;

    use crate::handlers::SandboxAppState;
    use crate::services::creation_progress::CreationStepKind;
    use crate::services::sandbox_service::tests::{
        image_preparation_for, preview_test_service_with_db, track_creation,
    };
    use crate::services::sandbox_service::SandboxService;

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

    fn user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
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

    fn service() -> Arc<SandboxService> {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        Arc::new(preview_test_service_with_db(db))
    }

    /// The sandbox routes as `user_id` with `role`, or unauthenticated.
    fn app(service: Arc<SandboxService>, caller: Option<(i32, Role)>) -> axum::Router {
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
        match caller {
            Some((user_id, role)) => router.layer(Extension(AuthContext::new_persisted_session(
                user(user_id),
                role,
                9,
            ))),
            None => router,
        }
    }

    async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let request = axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn reading_a_create_needs_a_signed_in_caller() {
        let (status, _) = get(app(service(), None), "/v1/sandboxes/creations/create-0001").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_caller_reads_the_ordered_steps_of_their_create() {
        let service = service();
        let progress = track_creation(&service, 7, "create-0001");
        progress.start(CreationStepKind::CheckRequest);
        progress.done(CreationStepKind::CheckRequest, None);
        progress.start(CreationStepKind::SelectNode);

        let (status, body) = get(
            app(service, Some((7, Role::User))),
            "/v1/sandboxes/creations/create-0001",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["creation_id"], "create-0001");
        assert_eq!(body["status"], "running");
        let steps = body["steps"].as_array().expect("steps");
        assert_eq!(steps[0]["kind"], "check_request");
        assert_eq!(steps[0]["status"], "done");
        let finished = steps[0]["finished_at"].as_str().expect("finished_at");
        assert!(finished.ends_with('Z'), "ISO 8601 with Z: {finished}");
        assert_eq!(steps[1]["kind"], "select_node");
        assert_eq!(steps[1]["status"], "running");
        assert_eq!(steps.last().expect("a step")["kind"], "ready");
        assert_eq!(steps.last().expect("a step")["status"], "pending");
    }

    #[tokio::test]
    async fn another_users_create_is_not_found_as_a_problem() {
        let service = service();
        let _progress = track_creation(&service, 7, "create-0001");
        let (status, body) = get(
            app(service, Some((8, Role::User))),
            "/v1/sandboxes/creations/create-0001",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["title"].is_string(), "{body}");
        let detail = body["detail"].as_str().expect("detail");
        assert!(detail.contains("create-0001"), "{detail}");
    }

    #[tokio::test]
    async fn a_failed_create_names_the_step_and_why() {
        let service = service();
        let progress = track_creation(&service, 7, "create-0002");
        progress.start(CreationStepKind::CheckRequest);
        progress.done(CreationStepKind::CheckRequest, None);
        progress.start(CreationStepKind::PullImage);
        progress.fail("pulling image 'img:1' failed: no space left on device");

        let (status, body) = get(
            app(service, Some((7, Role::User))),
            "/v1/sandboxes/creations/create-0002",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "failed");
        let failed: Vec<_> = body["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .filter(|s| s["status"] == "failed")
            .collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["kind"], "pull_image");
        assert!(failed[0]["error"]
            .as_str()
            .expect("error")
            .contains("no space left on device"));
    }
}
