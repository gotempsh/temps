// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::services::runtime_health::{
    RuntimeHealthError, RuntimeHealthPolicy, RuntimeHealthService, RuntimeHealthSummary,
    RuntimeWorkloadMetadata,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use std::sync::Arc;
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, AuditLogger, ProjectAccessChecker, RequestMetadata,
};
use utoipa::OpenApi;

pub struct RuntimeHealthAppState {
    pub service: Arc<RuntimeHealthService>,
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
    pub audit: Arc<dyn AuditLogger>,
}

impl From<RuntimeHealthError> for Problem {
    fn from(error: RuntimeHealthError) -> Self {
        let status = match &error {
            RuntimeHealthError::NotFound { .. } => StatusCode::NOT_FOUND,
            RuntimeHealthError::Configuration { .. } => StatusCode::BAD_REQUEST,
            RuntimeHealthError::StaleReport => StatusCode::CONFLICT,
            RuntimeHealthError::Transport { .. } => StatusCode::SERVICE_UNAVAILABLE,
            RuntimeHealthError::Database { .. } | RuntimeHealthError::Alarm { .. } => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        tracing::warn!(%error, "Runtime health API operation failed");
        problemdetails::new(status)
            .with_title("Runtime health operation failed")
            .with_detail(match error {
                RuntimeHealthError::NotFound { .. } => "Environment not found in this project",
                RuntimeHealthError::Configuration { .. } => {
                    "Runtime monitoring policy or deployment configuration is invalid"
                }
                RuntimeHealthError::StaleReport => "The runtime observation is stale",
                RuntimeHealthError::Transport { .. } => {
                    "The assigned runtime health host is unavailable"
                }
                RuntimeHealthError::Database { .. } => "Runtime health storage is unavailable",
                RuntimeHealthError::Alarm { .. } => "Runtime health alarm delivery is unavailable",
            })
    }
}

#[utoipa::path(get, path="/projects/{project_id}/environments/{environment_id}/runtime-health",tag="Runtime health", params(("project_id"=i32,Path,description="Project ID"),("environment_id"=i32,Path,description="Environment ID")),responses((status=200,body=RuntimeHealthSummary),(status=401,description="Authentication required",body=ProblemDetails),(status=403,description="Environment read permission required",body=ProblemDetails),(status=404,description="Environment not found",body=ProblemDetails),(status=500,description="Storage unavailable",body=ProblemDetails)),security(("bearer_auth"=[])))]
pub async fn get_runtime_health(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<RuntimeHealthAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<Json<RuntimeHealthSummary>, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    Ok(Json(
        state.service.summary(project_id, environment_id).await?,
    ))
}

#[utoipa::path(put, path="/projects/{project_id}/environments/{environment_id}/runtime-health",tag="Runtime health",params(("project_id"=i32,Path,description="Project ID"),("environment_id"=i32,Path,description="Environment ID")),request_body=RuntimeHealthPolicy,responses((status=204,description="Monitoring policy saved"),(status=400,description="Invalid monitoring policy",body=ProblemDetails),(status=401,description="Authentication required",body=ProblemDetails),(status=403,description="Environment write permission required",body=ProblemDetails),(status=404,description="Environment not found",body=ProblemDetails),(status=500,description="Storage unavailable",body=ProblemDetails)),security(("bearer_auth"=[])))]
pub async fn update_runtime_health(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<RuntimeHealthAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(policy): Json<RuntimeHealthPolicy>,
) -> Result<StatusCode, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    state
        .service
        .update_policy(project_id, environment_id, policy.clone())
        .await?;
    let audit = super::audit::RuntimeHealthPolicyUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        environment_id,
        enabled: policy.enabled,
        paused: policy.paused,
        required_healthy: policy.required_healthy,
    };
    if let Err(error) = state.audit.create_audit_log(&audit).await {
        tracing::error!(project_id,environment_id,%error,"Failed to audit runtime monitoring policy update");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get,path="/projects/{project_id}/runtime-health",tag="Runtime health",params(("project_id"=i32,Path,description="Project ID")),responses((status=200,body=Vec<RuntimeWorkloadMetadata>),(status=401,description="Authentication required",body=ProblemDetails),(status=403,description="Environment read permission required",body=ProblemDetails),(status=404,description="Project not found",body=ProblemDetails),(status=500,description="Storage unavailable",body=ProblemDetails)),security(("bearer_auth"=[])))]
pub async fn get_project_runtime_health(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<RuntimeHealthAppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<RuntimeWorkloadMetadata>>, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    Ok(Json(state.service.workloads(project_id).await?))
}

pub fn routes() -> Router<Arc<RuntimeHealthAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/runtime-health",
            get(get_project_runtime_health),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/runtime-health",
            get(get_runtime_health).put(update_runtime_health),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(get_runtime_health, update_runtime_health, get_project_runtime_health),
    components(schemas(RuntimeHealthSummary, RuntimeHealthPolicy, RuntimeWorkloadMetadata))
)]
pub struct RuntimeHealthApiDoc;
