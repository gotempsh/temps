// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compose configuration diagnostics: the host's Compose CLI status, the
//! "Run compose check" action on a deployment, and a project's stored check.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use temps_auth::{
    permission_guard, project_access_guard, project_permission_guard, project_scope_guard,
    RequireAuth,
};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_presets::compose_check::{
    ComposeCheckCommand, ComposeCheckErrorClass, ComposeCheckReport,
};
use utoipa::OpenApi;

use crate::services::compose_check_service::{
    ComposeCheckService, ComposeCheckServiceError, ComposeHostStatusResponse,
    ComposeValidationReportResponse,
};

pub struct ComposeCheckAppState {
    pub compose_check_service: Arc<ComposeCheckService>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<ComposeCheckServiceError> for Problem {
    fn from(error: ComposeCheckServiceError) -> Self {
        match error {
            ComposeCheckServiceError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            ComposeCheckServiceError::DeploymentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Deployment Not Found")
                    .with_detail(error.to_string())
            }
            ComposeCheckServiceError::EnvironmentNotFound { .. }
            | ComposeCheckServiceError::NoEnvironment { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            ComposeCheckServiceError::NotComposeProject { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Not a Docker Compose Project")
                    .with_detail(error.to_string())
            }
            ComposeCheckServiceError::SourceUnavailable { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Compose File Unavailable")
                    .with_detail(error.to_string())
            }
            ComposeCheckServiceError::Decrypt { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Decryption Error")
                    .with_detail(error.to_string())
            }
            ComposeCheckServiceError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// The host's Docker Compose CLI, as probed at startup. Compose deployments
/// that need `docker compose config` fail on an unsupported CLI; the console
/// shows a platform warning from this.
#[utoipa::path(
    tag = "Compose",
    get,
    path = "/platform/compose-status",
    responses(
        (status = 200, description = "Docker Compose CLI status", body = ComposeHostStatusResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_compose_status(
    State(state): State<Arc<ComposeCheckAppState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    Ok(Json(state.compose_check_service.host_status()))
}

/// Run the deploy job's Compose configuration check for a deployment, without
/// deploying: the deployment's commit, the project's current override,
/// exclusions and security policy, and the environment's variables. Returns
/// the error the deployment would fail with, its class, and the exact
/// `docker compose config` command with sanitized output. Read-only.
#[utoipa::path(
    tag = "Compose",
    post,
    path = "/projects/{project_id}/deployments/{deployment_id}/compose-check",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    ),
    responses(
        (status = 200, description = "Check result (passed or the failure)", body = ComposeCheckReport),
        (status = 400, description = "Not a Docker Compose project", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project, deployment or environment not found", body = ProblemDetails),
        (status = 502, description = "The Compose file could not be read from the Git provider", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn run_deployment_compose_check(
    State(state): State<Arc<ComposeCheckAppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let report = state
        .compose_check_service
        .check_deployment(project_id, deployment_id)
        .await?;
    Ok(Json(report))
}

/// The project's most recent stored Compose check (from its last deployment
/// or the revalidation after an upgrade); `null` when it was never checked.
#[utoipa::path(
    tag = "Compose",
    get,
    path = "/projects/{project_id}/compose-validation",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Last stored check, or null", body = Option<ComposeValidationReportResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_compose_validation(
    State(state): State<Arc<ComposeCheckAppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let report = state
        .compose_check_service
        .latest_report(project_id)
        .await?;
    Ok(Json(report))
}

pub fn configure_routes() -> Router<Arc<ComposeCheckAppState>> {
    Router::new()
        .route("/platform/compose-status", get(get_compose_status))
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/compose-check",
            post(run_deployment_compose_check),
        )
        .route(
            "/projects/{project_id}/compose-validation",
            get(get_project_compose_validation),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_compose_status,
        run_deployment_compose_check,
        get_project_compose_validation
    ),
    components(schemas(
        ComposeHostStatusResponse,
        ComposeValidationReportResponse,
        ComposeCheckReport,
        ComposeCheckCommand,
        ComposeCheckErrorClass
    )),
    tags((name = "Compose", description = "Docker Compose configuration diagnostics"))
)]
pub struct ComposeCheckApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_errors_map_to_specific_statuses() {
        let cases = [
            (
                ComposeCheckServiceError::ProjectNotFound { project_id: 4 },
                StatusCode::NOT_FOUND,
                "Project 4",
            ),
            (
                ComposeCheckServiceError::DeploymentNotFound {
                    project_id: 4,
                    deployment_id: 9,
                },
                StatusCode::NOT_FOUND,
                "Deployment 9",
            ),
            (
                ComposeCheckServiceError::NotComposeProject { project_id: 4 },
                StatusCode::BAD_REQUEST,
                "not a Docker Compose project",
            ),
            (
                ComposeCheckServiceError::SourceUnavailable {
                    project_id: 4,
                    reason: "no Compose file".to_string(),
                },
                StatusCode::BAD_GATEWAY,
                "no Compose file",
            ),
        ];
        for (error, status, detail) in cases {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status);
            let body = serde_json::to_value(&problem.body).expect("problem serializes");
            assert!(
                body["detail"]
                    .as_str()
                    .is_some_and(|text| text.contains(detail)),
                "{body}"
            );
        }
    }

    #[test]
    fn openapi_documents_every_route() {
        let paths = ComposeCheckApiDoc::openapi().paths.paths;
        for path in [
            "/platform/compose-status",
            "/projects/{project_id}/deployments/{deployment_id}/compose-check",
            "/projects/{project_id}/compose-validation",
        ] {
            assert!(paths.contains_key(path), "{path}");
        }
    }
}
