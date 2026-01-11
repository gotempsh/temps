//! Deployment Token Handlers
//!
//! REST API endpoints for managing deployment tokens that provide
//! TEMPS_API_URL and TEMPS_API_TOKEN environment variables to deployed applications.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use tracing::error;
use utoipa::OpenApi;

use crate::services::deployment_token_service::{
    CreateDeploymentTokenRequest, CreateDeploymentTokenResponse, DeploymentTokenListResponse,
    DeploymentTokenResponse, DeploymentTokenService, UpdateDeploymentTokenRequest,
};
use temps_core::problemdetails::Problem;

use serde::Deserialize;
use utoipa::ToSchema;

/// App state for deployment token handlers
pub struct DeploymentTokenAppState {
    pub deployment_token_service: Arc<DeploymentTokenService>,
}

#[derive(Deserialize, ToSchema)]
pub struct ListDeploymentTokensQuery {
    #[schema(example = 1)]
    pub page: Option<u64>,
    #[schema(example = 20)]
    pub page_size: Option<u64>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_deployment_tokens,
        create_deployment_token,
        get_deployment_token,
        update_deployment_token,
        delete_deployment_token,
    ),
    components(schemas(
        DeploymentTokenResponse,
        CreateDeploymentTokenResponse,
        DeploymentTokenListResponse,
        CreateDeploymentTokenRequest,
        UpdateDeploymentTokenRequest,
        ListDeploymentTokensQuery,
    )),
    info(
        title = "Deployment Tokens API",
        description = "API endpoints for managing deployment tokens. \
        Deployment tokens provide API access credentials (TEMPS_API_URL and TEMPS_API_TOKEN) \
        that are automatically injected into deployed applications, allowing them to access \
        Temps platform APIs for visitor enrichment, email sending, and more.",
        version = "1.0.0"
    )
)]
pub struct DeploymentTokensApiDoc;

/// Configure deployment token routes
pub fn configure_routes() -> Router<Arc<DeploymentTokenAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/deployment-tokens",
            get(list_deployment_tokens).post(create_deployment_token),
        )
        .route(
            "/projects/{project_id}/deployment-tokens/{token_id}",
            get(get_deployment_token)
                .patch(update_deployment_token)
                .delete(delete_deployment_token),
        )
}

/// List all deployment tokens for a project
#[utoipa::path(
    tag = "Deployment Tokens",
    get,
    path = "/projects/{project_id}/deployment-tokens",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "List of deployment tokens", body = DeploymentTokenListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_deployment_tokens(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListDeploymentTokensQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensRead);

    let page = query.page.unwrap_or(1);
    let page_size = std::cmp::min(query.page_size.unwrap_or(20), 100);

    let response = app_state
        .deployment_token_service
        .list_tokens(project_id, page, page_size)
        .await
        .map_err(|e| {
            error!("Failed to list deployment tokens: {}", e);
            e.to_problem()
        })?;

    Ok(Json(response))
}

/// Create a new deployment token for a project
#[utoipa::path(
    tag = "Deployment Tokens",
    post,
    path = "/projects/{project_id}/deployment-tokens",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = CreateDeploymentTokenRequest,
    responses(
        (status = 201, description = "Deployment token created successfully", body = CreateDeploymentTokenResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Token with this name already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn create_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateDeploymentTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensCreate);

    let response = app_state
        .deployment_token_service
        .create_token(project_id, Some(auth.user_id()), request)
        .await
        .map_err(|e| {
            error!("Failed to create deployment token: {}", e);
            e.to_problem()
        })?;

    Ok((StatusCode::CREATED, Json(response)))
}

/// Get a specific deployment token
#[utoipa::path(
    tag = "Deployment Tokens",
    get,
    path = "/projects/{project_id}/deployment-tokens/{token_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("token_id" = i32, Path, description = "Deployment token ID")
    ),
    responses(
        (status = 200, description = "Deployment token details", body = DeploymentTokenResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment token not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path((project_id, token_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensRead);

    let response = app_state
        .deployment_token_service
        .get_token(project_id, token_id)
        .await
        .map_err(|e| {
            error!("Failed to get deployment token: {}", e);
            e.to_problem()
        })?;

    Ok(Json(response))
}

/// Update a deployment token
#[utoipa::path(
    tag = "Deployment Tokens",
    patch,
    path = "/projects/{project_id}/deployment-tokens/{token_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("token_id" = i32, Path, description = "Deployment token ID")
    ),
    request_body = UpdateDeploymentTokenRequest,
    responses(
        (status = 200, description = "Deployment token updated successfully", body = DeploymentTokenResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment token not found"),
        (status = 409, description = "Token with this name already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn update_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path((project_id, token_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateDeploymentTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensWrite);

    let response = app_state
        .deployment_token_service
        .update_token(project_id, token_id, request)
        .await
        .map_err(|e| {
            error!("Failed to update deployment token: {}", e);
            e.to_problem()
        })?;

    Ok(Json(response))
}

/// Delete a deployment token
#[utoipa::path(
    tag = "Deployment Tokens",
    delete,
    path = "/projects/{project_id}/deployment-tokens/{token_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("token_id" = i32, Path, description = "Deployment token ID")
    ),
    responses(
        (status = 204, description = "Deployment token deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment token not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn delete_deployment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DeploymentTokenAppState>>,
    Path((project_id, token_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentTokensDelete);

    app_state
        .deployment_token_service
        .delete_token(project_id, token_id)
        .await
        .map_err(|e| {
            error!("Failed to delete deployment token: {}", e);
            e.to_problem()
        })?;

    Ok(StatusCode::NO_CONTENT)
}
