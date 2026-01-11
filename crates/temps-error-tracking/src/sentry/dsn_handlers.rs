use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{OpenApi, ToSchema};

use crate::sentry::{DSNService, ProjectDSN, SentryIngesterError};

#[derive(OpenApi)]
#[openapi(
    paths(
        create_dsn,
        get_or_create_dsn,
        list_dsns,
        regenerate_dsn,
        revoke_dsn,
    ),
    components(schemas(
        CreateDSNRequest,
        GetOrCreateDSNRequest,
        ProjectDSNResponse,
        RegenerateDSNRequest,
    )),
    tags(
        (name = "dsn", description = "DSN management endpoints")
    )
)]
pub struct DSNApiDoc;

#[derive(Clone)]
pub struct DSNAppState {
    pub dsn_service: Arc<DSNService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub config_service: Arc<temps_config::ConfigService>,
}

pub fn configure_dsn_routes() -> Router<Arc<DSNAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/dsns",
            post(create_dsn).get(list_dsns),
        )
        .route(
            "/projects/{project_id}/dsns/get-or-create",
            post(get_or_create_dsn),
        )
        .route(
            "/projects/{project_id}/dsns/{dsn_id}/regenerate",
            post(regenerate_dsn),
        )
        .route(
            "/projects/{project_id}/dsns/{dsn_id}/revoke",
            post(revoke_dsn),
        )
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateDSNRequest {
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GetOrCreateDSNRequest {
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegenerateDSNRequest {
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectDSNResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: String,
    pub public_key: String,
    pub dsn: String,
    pub created_at: String,
    pub is_active: bool,
    pub event_count: i64,
}

impl From<ProjectDSN> for ProjectDSNResponse {
    fn from(dsn: ProjectDSN) -> Self {
        Self {
            id: dsn.id,
            project_id: dsn.project_id,
            environment_id: dsn.environment_id,
            deployment_id: dsn.deployment_id,
            name: dsn.name,
            public_key: dsn.public_key,
            dsn: dsn.dsn,
            created_at: dsn.created_at.to_string(),
            is_active: dsn.is_active,
            event_count: dsn.event_count,
        }
    }
}

impl IntoResponse for SentryIngesterError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            SentryIngesterError::ProjectNotFound => (StatusCode::NOT_FOUND, "Project not found"),
            SentryIngesterError::InvalidDSN => (StatusCode::NOT_FOUND, "DSN not found"),
            SentryIngesterError::Validation(msg) => (StatusCode::BAD_REQUEST, msg.leak() as &str),
            SentryIngesterError::Database(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error")
            }
        };

        (status, message).into_response()
    }
}

/// Create a new DSN for a project
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = CreateDSNRequest,
    responses(
        (status = 201, description = "DSN created", body = ProjectDSNResponse),
        (status = 404, description = "Project not found"),
    ),
    tag = "dsn"
)]
async fn create_dsn(
    State(state): State<Arc<DSNAppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateDSNRequest>,
) -> Result<(StatusCode, Json<ProjectDSNResponse>), SentryIngesterError> {
    // Get base URL from config service if not provided (defaults to http://localho.st)
    let base_url = match request.base_url {
        Some(url) => url,
        None => state
            .config_service
            .get_external_url_or_default()
            .await
            .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?,
    };

    let dsn = state
        .dsn_service
        .create_project_dsn(
            project_id,
            request.environment_id,
            request.deployment_id,
            request.name,
            &base_url,
        )
        .await?;

    Ok((StatusCode::CREATED, Json(dsn.into())))
}

/// Get or create DSN for a project/environment/deployment combination
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns/get-or-create",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = GetOrCreateDSNRequest,
    responses(
        (status = 200, description = "DSN retrieved or created", body = ProjectDSNResponse),
        (status = 404, description = "Project not found"),
    ),
    tag = "dsn"
)]
async fn get_or_create_dsn(
    State(state): State<Arc<DSNAppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<GetOrCreateDSNRequest>,
) -> Result<Json<ProjectDSNResponse>, SentryIngesterError> {
    // Get base URL from config service if not provided (defaults to http://localho.st)
    let base_url = match request.base_url {
        Some(url) => url,
        None => state
            .config_service
            .get_external_url_or_default()
            .await
            .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?,
    };

    let dsn = state
        .dsn_service
        .get_or_create_project_dsn(
            project_id,
            request.environment_id,
            request.deployment_id,
            &base_url,
        )
        .await?;

    Ok(Json(dsn.into()))
}

/// List all DSNs for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/dsns",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 200, description = "List of DSNs", body = Vec<ProjectDSNResponse>),
    ),
    tag = "dsn"
)]
async fn list_dsns(
    State(state): State<Arc<DSNAppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<ProjectDSNResponse>>, SentryIngesterError> {
    // Get base URL from config service (with default fallback to http://localho.st)
    let base_url = state
        .config_service
        .get_external_url_or_default()
        .await
        .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?;

    let dsns = state
        .dsn_service
        .list_project_dsns(project_id, &base_url)
        .await?;

    Ok(Json(dsns.into_iter().map(|d| d.into()).collect()))
}

/// Regenerate DSN keys (rotate keys)
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns/{dsn_id}/regenerate",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("dsn_id" = i32, Path, description = "DSN ID")
    ),
    request_body = RegenerateDSNRequest,
    responses(
        (status = 200, description = "DSN keys regenerated", body = ProjectDSNResponse),
        (status = 404, description = "DSN not found"),
    ),
    tag = "dsn"
)]
async fn regenerate_dsn(
    State(state): State<Arc<DSNAppState>>,
    Path((project_id, dsn_id)): Path<(i32, i32)>,
    Json(request): Json<RegenerateDSNRequest>,
) -> Result<Json<ProjectDSNResponse>, SentryIngesterError> {
    // Get base URL from config service if not provided (defaults to http://localho.st)
    let base_url = match request.base_url {
        Some(url) => url,
        None => state
            .config_service
            .get_external_url_or_default()
            .await
            .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?,
    };

    let dsn = state
        .dsn_service
        .regenerate_project_dsn(dsn_id, project_id, &base_url)
        .await?;

    Ok(Json(dsn.into()))
}

/// Revoke (deactivate) a DSN
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns/{dsn_id}/revoke",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("dsn_id" = i32, Path, description = "DSN ID")
    ),
    responses(
        (status = 204, description = "DSN revoked"),
        (status = 404, description = "DSN not found"),
    ),
    tag = "dsn"
)]
async fn revoke_dsn(
    State(state): State<Arc<DSNAppState>>,
    Path((project_id, dsn_id)): Path<(i32, i32)>,
) -> Result<StatusCode, SentryIngesterError> {
    state.dsn_service.revoke_dsn(dsn_id, project_id).await?;

    Ok(StatusCode::NO_CONTENT)
}
