// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET /projects/{project_id}/ingest-setup`: where an app sends each kind of
//! telemetry and how it authenticates, for the console's framework-agnostic
//! setup and its "Copy prompt".
//!
//! Read-only and credential-free (see [`crate::services::ingest_setup`]), so
//! there is no audit entry.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::error_builder;
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use tracing::error;
use utoipa::IntoParams;

use crate::services::ingest_setup::{IngestSetupError, IngestSetupService, ProjectIngestSetup};

/// State for the ingest-setup route. Kept separate from the projects
/// `AppState` so it depends only on what it reads.
pub struct IngestSetupAppState {
    pub ingest_setup_service: Arc<IngestSetupService>,
    /// Source of the public ingress URL (ADR-051) every endpoint is built on.
    pub config_service: Arc<temps_config::ConfigService>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

pub fn configure_ingest_setup_routes() -> Router<Arc<IngestSetupAppState>> {
    Router::new().route(
        "/projects/{project_id}/ingest-setup",
        get(get_project_ingest_setup),
    )
}

/// Query parameters for `GET /projects/{project_id}/ingest-setup`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct IngestSetupQuery {
    /// Attribute telemetry to this environment of the project.
    pub environment_id: Option<i32>,
}

impl From<IngestSetupError> for Problem {
    fn from(error: IngestSetupError) -> Self {
        match error {
            IngestSetupError::ProjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(error.to_string()),
            IngestSetupError::EnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            IngestSetupError::InvalidPublicUrl { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Public URL Misconfigured")
                    .with_detail(error.to_string())
            }
            IngestSetupError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Ingest Setup Unavailable")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Get where this project's apps send telemetry
///
/// The public URL, method and content type of every ingest route (analytics,
/// session replay, errors, OTLP logs and traces), the credential each expects
/// and where to create it, and whether the project already has it. Header
/// values are templates over an environment variable; no credential is
/// returned.
#[utoipa::path(
    tag = "Projects",
    get,
    path = "/projects/{project_id}/ingest-setup",
    operation_id = "get_project_ingest_setup",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        IngestSetupQuery
    ),
    responses(
        (status = 200, description = "Ingest setup of the project", body = ProjectIngestSetup),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_ingest_setup(
    State(state): State<Arc<IngestSetupAppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<IngestSetupQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let public_base_url = state
        .config_service
        .get_public_ingress_url()
        .await
        .map_err(|e| {
            error!(project_id, error = %e, "Failed to read the public ingress URL");
            error_builder::internal_server_error()
                .title("Ingest Setup Unavailable")
                .detail(format!(
                    "Could not read the public ingress URL for project {project_id}: {e}"
                ))
                .build()
        })?;

    let setup = state
        .ingest_setup_service
        .get_ingest_setup(project_id, query.environment_id, &public_base_url)
        .await
        .map_err(|e| {
            error!(project_id, environment_id = ?query.environment_id, error = %e, "Failed to resolve ingest setup");
            Problem::from(e)
        })?;

    Ok(Json(setup))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::{AuthContext, Permission, Role};
    use temps_entities::deployment_tokens::DeploymentTokenPermission;
    use temps_entities::users;
    use tower::ServiceExt;

    fn test_user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 7,
            name: "Operator".to_string(),
            email: "operator@example.com".to_string(),
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

    fn state_with(db: MockDatabase) -> Arc<IngestSetupAppState> {
        let db = Arc::new(db.into_connection());
        let config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .expect("test server config"),
        );
        Arc::new(IngestSetupAppState {
            ingest_setup_service: Arc::new(IngestSetupService::new(db.clone())),
            config_service: Arc::new(temps_config::ConfigService::new(config, db)),
            project_access_checker: None,
        })
    }

    fn app_state() -> Arc<IngestSetupAppState> {
        state_with(MockDatabase::new(DatabaseBackend::Postgres))
    }

    async fn rejection_status(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        get_project_ingest_setup(
            State(app_state()),
            RequireAuth(auth),
            Path(project_id),
            Query(IngestSetupQuery::default()),
        )
        .await
        .err()
        .map(|problem| problem.status_code)
    }

    #[tokio::test]
    async fn unauthenticated_request_is_rejected_with_401() {
        let router = configure_ingest_setup_routes().with_state(app_state());
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/projects/1/ingest-setup")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn key_without_projects_read_is_rejected_with_403() {
        let auth = AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![Permission::AnalyticsRead]),
            "analytics-only".to_string(),
            1,
        );
        assert_eq!(rejection_status(auth, 1).await, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn deployment_token_of_another_project_is_rejected_with_403() {
        let auth = AuthContext::new_deployment_token(
            3,
            None,
            None,
            1,
            "ci".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        );
        assert_eq!(rejection_status(auth, 4).await, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn reader_gets_the_setup_built_on_the_public_url() {
        use sea_orm::Value;
        use std::collections::BTreeMap;
        // No settings row: the defaults apply, so the public URL is the
        // local default. Then the one setup query.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .append_query_results([vec![BTreeMap::from([
                ("slug", Value::from("my-app")),
                ("environment_found", Value::from(true)),
                ("has_analytics_key", Value::from(false)),
                ("has_dsn", Value::from(true)),
            ])]]);
        let auth = AuthContext::new_session(test_user(), Role::Reader);
        let response = get_project_ingest_setup(
            State(state_with(db)),
            RequireAuth(auth),
            Path(1),
            Query(IngestSetupQuery::default()),
        )
        .await
        .map(IntoResponse::into_response)
        .expect("reader may read the setup");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let setup: ProjectIngestSetup = serde_json::from_slice(&body).expect("typed body");
        assert_eq!(setup.project_slug, "my-app");
        assert!(setup.signals[0].endpoints[0]
            .url
            .starts_with(&setup.public_base_url));
        assert!(!setup.signals[0].configured);
    }

    #[tokio::test]
    async fn unknown_project_is_a_404_problem() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()]);
        let auth = AuthContext::new_session(test_user(), Role::Reader);
        let problem = get_project_ingest_setup(
            State(state_with(db)),
            RequireAuth(auth),
            Path(404),
            Query(IngestSetupQuery::default()),
        )
        .await
        .err()
        .expect("no such project");
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        assert!(problem
            .body
            .get("detail")
            .and_then(|d| d.as_str())
            .is_some_and(|d| d.contains("404")));
    }

    #[test]
    fn errors_map_to_problem_statuses() {
        let cases = [
            (
                IngestSetupError::ProjectNotFound { project_id: 9 },
                StatusCode::NOT_FOUND,
            ),
            (
                IngestSetupError::EnvironmentNotFound {
                    project_id: 9,
                    environment_id: 3,
                },
                StatusCode::NOT_FOUND,
            ),
            (
                IngestSetupError::InvalidPublicUrl {
                    project_id: 9,
                    url: "temps.example.com".to_string(),
                    reason: "must start with http:// or https://",
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                IngestSetupError::Database {
                    project_id: 9,
                    source: sea_orm::DbErr::Custom("connection reset".to_string()),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            let detail = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status);
            assert!(detail.contains('9'));
        }
    }
}
