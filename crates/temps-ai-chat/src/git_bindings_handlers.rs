// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use anyhow::Result as AnyResult;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{deny_deployment_token, permission_guard, permissions::Permission, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem},
    AuditContext, AuditOperation, RequestMetadata,
};
use utoipa::{OpenApi, ToSchema};

use crate::{
    git_bindings::{EligibleGitRepository, GitBindingError},
    handlers::{ensure_application_project_permission, AppState},
    GitBindingService,
};

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitBindingResponse {
    pub id: i64,
    pub project_id: i32,
    pub connection_id: i32,
    pub repository_id: i32,
    pub repository_url: String,
    pub remote_name: String,
    /// `configured` means the durable authorization exists; it does not claim a live remote.
    pub status: &'static str,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::ai_application_git_bindings::Model> for GitBindingResponse {
    fn from(value: temps_entities::ai_application_git_bindings::Model) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            connection_id: value.connection_id,
            repository_id: value.repository_id,
            repository_url: value.repository_url,
            remote_name: value.remote_name,
            status: "configured",
            created_at: value.created_at.to_rfc3339(),
            updated_at: value.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EligibleGitRepositoryResponse {
    pub connection_id: i32,
    pub repository_id: i32,
    pub account_name: String,
    pub full_name: String,
    pub repository_url: String,
    pub private: bool,
}

impl From<EligibleGitRepository> for EligibleGitRepositoryResponse {
    fn from(value: EligibleGitRepository) -> Self {
        Self {
            connection_id: value.connection.id,
            repository_id: value.repository.id,
            account_name: value.connection.account_name,
            full_name: value.repository.full_name,
            repository_url: value.repository_url,
            private: value.repository.private,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationGitConnectionsResponse {
    pub bindings: Vec<GitBindingResponse>,
    pub eligible_repositories: Vec<EligibleGitRepositoryResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindApplicationGitConnectionRequest {
    pub project_id: i32,
    pub connection_id: i32,
    pub repository_id: i32,
    pub remote_name: String,
}

#[derive(Debug, Clone, Serialize)]
struct GitBindingAudit {
    context: AuditContext,
    application_id: String,
    action: &'static str,
    binding_id: i64,
    project_id: i32,
    connection_id: i32,
    repository_id: i32,
    remote_name: String,
}

impl AuditOperation for GitBindingAudit {
    fn operation_type(&self) -> String {
        format!("ai.application.git_binding.{}", self.action)
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
    fn serialize(&self) -> AnyResult<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
}

fn audit_context(user_id: i32, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id,
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn audit(state: &AppState, operation: &dyn AuditOperation) {
    if let Err(error) = state.audit_service.create_audit_log(operation).await {
        tracing::error!(error = %error, "failed to write application Git-binding audit log");
    }
}

#[utoipa::path(get, operation_id = "listApplicationGitConnections", tag = "AI Applications", path = "/ai/applications/{application_public_id}/git-connections", params(("application_public_id" = String, Path)), responses((status = 200, body = ApplicationGitConnectionsResponse), (status = 401), (status = 403), (status = 404)), security(("bearer_auth" = [])))]
pub async fn list_application_git_connections(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(application_public_id): Path<String>,
) -> Result<Json<ApplicationGitConnectionsResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    permission_guard!(auth, GitRepositoriesRead);
    deny_deployment_token!(auth);
    ensure_linked_project_permission(
        &state,
        &auth,
        &application_public_id,
        &Permission::ProjectsRead,
    )
    .await?;
    let service = GitBindingService::new(state.db.clone());
    let bindings = service.list(auth.user_id(), &application_public_id).await?;
    let eligible_repositories = service
        .eligible_repositories(auth.user_id(), &application_public_id)
        .await?;
    Ok(Json(ApplicationGitConnectionsResponse {
        bindings: bindings.into_iter().map(Into::into).collect(),
        eligible_repositories: eligible_repositories.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(post, operation_id = "bindApplicationGitConnection", tag = "AI Applications", path = "/ai/applications/{application_public_id}/git-connections", params(("application_public_id" = String, Path)), request_body = BindApplicationGitConnectionRequest, responses((status = 201, body = GitBindingResponse), (status = 400), (status = 401), (status = 403), (status = 404), (status = 409)), security(("bearer_auth" = [])))]
pub async fn bind_application_git_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(application_public_id): Path<String>,
    Json(request): Json<BindApplicationGitConnectionRequest>,
) -> Result<(StatusCode, Json<GitBindingResponse>), Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, GitRepositoriesSync);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    ensure_linked_project_permission(
        &state,
        &auth,
        &application_public_id,
        &Permission::ProjectsWrite,
    )
    .await?;
    ensure_application_project_permission(
        &auth,
        &state.project_access_checker,
        &[request.project_id],
        &Permission::ProjectsWrite,
    )
    .await?;
    let binding = GitBindingService::new(state.db.clone())
        .bind(
            auth.user_id(),
            &application_public_id,
            request.project_id,
            request.connection_id,
            request.repository_id,
            &request.remote_name,
        )
        .await?;
    audit(
        &state,
        &GitBindingAudit {
            context: audit_context(auth.user_id(), &metadata),
            application_id: application_public_id,
            action: "bound",
            binding_id: binding.id,
            project_id: binding.project_id,
            connection_id: binding.connection_id,
            repository_id: binding.repository_id,
            remote_name: binding.remote_name.clone(),
        },
    )
    .await;
    Ok((StatusCode::CREATED, Json(binding.into())))
}

#[utoipa::path(delete, operation_id = "disconnectApplicationGitConnection", tag = "AI Applications", path = "/ai/applications/{application_public_id}/git-connections/{binding_id}", params(("application_public_id" = String, Path), ("binding_id" = i64, Path)), responses((status = 204), (status = 401), (status = 403), (status = 404)), security(("bearer_auth" = [])))]
pub async fn disconnect_application_git_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((application_public_id, binding_id)): Path<(String, i64)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, GitRepositoriesSync);
    permission_guard!(auth, SandboxesWrite);
    deny_deployment_token!(auth);
    ensure_linked_project_permission(
        &state,
        &auth,
        &application_public_id,
        &Permission::ProjectsWrite,
    )
    .await?;
    let binding = GitBindingService::new(state.db.clone())
        .disconnect(auth.user_id(), &application_public_id, binding_id)
        .await?;
    audit(
        &state,
        &GitBindingAudit {
            context: audit_context(auth.user_id(), &metadata),
            application_id: application_public_id,
            action: "disconnected",
            binding_id: binding.id,
            project_id: binding.project_id,
            connection_id: binding.connection_id,
            repository_id: binding.repository_id,
            remote_name: binding.remote_name,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_linked_project_permission(
    state: &AppState,
    auth: &temps_auth::context::AuthContext,
    application_id: &str,
    permission: &Permission,
) -> Result<(), Problem> {
    let application = state
        .applications
        .get(auth.user_id(), application_id)
        .await?;
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    ensure_application_project_permission(
        auth,
        &state.project_access_checker,
        &project_ids,
        permission,
    )
    .await
}

impl From<GitBindingError> for Problem {
    fn from(error: GitBindingError) -> Self {
        match error {
            GitBindingError::ApplicationNotFound { .. }
            | GitBindingError::ProjectNotLinked { .. }
            | GitBindingError::BindingNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Application Git Binding Not Found")
                .with_detail(error.to_string()),
            GitBindingError::ConnectionUnavailable { .. }
            | GitBindingError::RepositoryUnavailable { .. } => {
                problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("Git Repository Access Denied")
                    .with_detail(error.to_string())
            }
            GitBindingError::RepositoryUrlUnavailable { .. }
            | GitBindingError::InvalidRemoteName { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Git Binding")
                    .with_detail(error.to_string())
            }
            GitBindingError::RemoteAlreadyBound { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Git Remote Already Bound")
                .with_detail(error.to_string()),
            GitBindingError::Database { .. } => {
                tracing::error!(error = %error, "application Git binding operation failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("The Git binding could not be persisted.")
            }
        }
    }
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ai/applications/{application_public_id}/git-connections",
            get(list_application_git_connections).post(bind_application_git_connection),
        )
        .route(
            "/ai/applications/{application_public_id}/git-connections/{binding_id}",
            delete(disconnect_application_git_connection),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_application_git_connections,
        bind_application_git_connection,
        disconnect_application_git_connection
    ),
    components(schemas(
        GitBindingResponse,
        EligibleGitRepositoryResponse,
        ApplicationGitConnectionsResponse,
        BindApplicationGitConnectionRequest
    ))
)]
pub struct GitBindingsApiDoc;
