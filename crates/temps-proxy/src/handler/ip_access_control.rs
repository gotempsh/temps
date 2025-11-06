use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

use crate::service::ip_access_control_service::{
    CreateIpAccessControlRequest, IpAccessControlResponse, IpAccessControlService,
    UpdateIpAccessControlRequest,
};

/// Query parameters for listing IP access control rules
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct IpAccessControlQuery {
    /// Filter by action ("block" or "allow")
    pub action: Option<String>,
}

/// List all IP access control rules
#[utoipa::path(
    get,
    path = "/ip-access-control",
    params(IpAccessControlQuery),
    responses(
        (status = 200, description = "List of IP access control rules", body = Vec<IpAccessControlResponse>),
        (status = 500, description = "Internal server error")
    ),
    tag = "IP Access Control"
)]
pub async fn list_ip_access_control(
    State(service): State<Arc<IpAccessControlService>>,
    Query(query): Query<IpAccessControlQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let rules = service
        .list(query.action)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let responses: Vec<IpAccessControlResponse> = rules
        .into_iter()
        .map(IpAccessControlResponse::from)
        .collect();

    Ok(Json(responses))
}

/// Get a single IP access control rule by ID
#[utoipa::path(
    get,
    path = "/ip-access-control/{id}",
    params(
        ("id" = i32, Path, description = "IP access control rule ID")
    ),
    responses(
        (status = 200, description = "IP access control rule details", body = IpAccessControlResponse),
        (status = 404, description = "IP access control rule not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "IP Access Control"
)]
pub async fn get_ip_access_control(
    State(service): State<Arc<IpAccessControlService>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let rule = service.get_by_id(id).await.map_err(|e| match e {
        crate::service::ip_access_control_service::IpAccessControlError::NotFound(_) => (
            StatusCode::NOT_FOUND,
            "IP access control rule not found".to_string(),
        ),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    })?;

    Ok(Json(IpAccessControlResponse::from(rule)))
}

/// Create a new IP access control rule
#[utoipa::path(
    post,
    path = "/ip-access-control",
    request_body = CreateIpAccessControlRequest,
    responses(
        (status = 201, description = "IP access control rule created", body = IpAccessControlResponse),
        (status = 400, description = "Invalid request"),
        (status = 409, description = "Duplicate IP address"),
        (status = 500, description = "Internal server error")
    ),
    tag = "IP Access Control"
)]
pub async fn create_ip_access_control(
    State(service): State<Arc<IpAccessControlService>>,
    Json(request): Json<CreateIpAccessControlRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // TODO: Get user_id from authentication context
    let created_by = None;

    let rule = service
        .create(request, created_by)
        .await
        .map_err(|e| match e {
            crate::service::ip_access_control_service::IpAccessControlError::InvalidIpAddress(
                _,
            ) => (StatusCode::BAD_REQUEST, e.to_string()),
            crate::service::ip_access_control_service::IpAccessControlError::DuplicateIp(_) => {
                (StatusCode::CONFLICT, e.to_string())
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        })?;

    Ok((
        StatusCode::CREATED,
        Json(IpAccessControlResponse::from(rule)),
    ))
}

/// Update an IP access control rule
#[utoipa::path(
    patch,
    path = "/ip-access-control/{id}",
    params(
        ("id" = i32, Path, description = "IP access control rule ID")
    ),
    request_body = UpdateIpAccessControlRequest,
    responses(
        (status = 200, description = "IP access control rule updated", body = IpAccessControlResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "IP access control rule not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "IP Access Control"
)]
pub async fn update_ip_access_control(
    State(service): State<Arc<IpAccessControlService>>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateIpAccessControlRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let rule = service.update(id, request).await.map_err(|e| match e {
        crate::service::ip_access_control_service::IpAccessControlError::NotFound(_) => (
            StatusCode::NOT_FOUND,
            "IP access control rule not found".to_string(),
        ),
        crate::service::ip_access_control_service::IpAccessControlError::InvalidIpAddress(_) => {
            (StatusCode::BAD_REQUEST, e.to_string())
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    })?;

    Ok(Json(IpAccessControlResponse::from(rule)))
}

/// Delete an IP access control rule
#[utoipa::path(
    delete,
    path = "/ip-access-control/{id}",
    params(
        ("id" = i32, Path, description = "IP access control rule ID")
    ),
    responses(
        (status = 204, description = "IP access control rule deleted"),
        (status = 404, description = "IP access control rule not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "IP Access Control"
)]
pub async fn delete_ip_access_control(
    State(service): State<Arc<IpAccessControlService>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    service.delete(id).await.map_err(|e| match e {
        crate::service::ip_access_control_service::IpAccessControlError::NotFound(_) => (
            StatusCode::NOT_FOUND,
            "IP access control rule not found".to_string(),
        ),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Check if an IP address is blocked
#[utoipa::path(
    get,
    path = "/ip-access-control/check/{ip}",
    params(
        ("ip" = String, Path, description = "IP address to check")
    ),
    responses(
        (status = 200, description = "IP block status"),
        (status = 500, description = "Internal server error")
    ),
    tag = "IP Access Control"
)]
pub async fn check_ip_blocked(
    State(service): State<Arc<IpAccessControlService>>,
    Path(ip): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let is_blocked = service
        .is_blocked(&ip)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "ip": ip,
        "is_blocked": is_blocked
    })))
}

/// Create routes for IP access control handlers
pub fn create_routes() -> axum::Router<Arc<IpAccessControlService>> {
    use axum::routing::{delete, get, patch, post};

    axum::Router::new()
        .route("/ip-access-control", get(list_ip_access_control))
        .route("/ip-access-control", post(create_ip_access_control))
        .route("/ip-access-control/{id}", get(get_ip_access_control))
        .route("/ip-access-control/{id}", patch(update_ip_access_control))
        .route("/ip-access-control/{id}", delete(delete_ip_access_control))
        .route("/ip-access-control/check/{ip}", get(check_ip_blocked))
}

/// Get OpenAPI documentation for IP access control handlers
pub fn openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;

    #[derive(OpenApi)]
    #[openapi(
        paths(
            list_ip_access_control,
            get_ip_access_control,
            create_ip_access_control,
            update_ip_access_control,
            delete_ip_access_control,
            check_ip_blocked,
        ),
        components(schemas(
            CreateIpAccessControlRequest,
            UpdateIpAccessControlRequest,
            IpAccessControlResponse,
            IpAccessControlQuery,
        )),
        tags(
            (name = "IP Access Control", description = "IP access control management endpoints")
        )
    )]
    struct IpAccessControlApiDoc;

    IpAccessControlApiDoc::openapi()
}
