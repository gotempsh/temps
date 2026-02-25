use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use utoipa::{IntoParams, ToSchema};

use crate::service::ip_access_control_service::{
    CreateIpAccessControlRequest, IpAccessControlError, IpAccessControlResponse,
    IpAccessControlService, UpdateIpAccessControlRequest,
};

impl From<IpAccessControlError> for Problem {
    fn from(error: IpAccessControlError) -> Self {
        match error {
            IpAccessControlError::NotFound(_) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("IP Access Control Rule Not Found")
                .with_detail(error.to_string()),

            IpAccessControlError::InvalidIpAddress(_) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid IP Address")
                    .with_detail(error.to_string())
            }

            IpAccessControlError::DuplicateIp(_) => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Duplicate IP Address")
                .with_detail(error.to_string()),

            IpAccessControlError::Database(_) | IpAccessControlError::Internal(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn list_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<IpAccessControlService>>,
    Query(query): Query<IpAccessControlQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let rules = service.list(query.action).await.map_err(Problem::from)?;

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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "IP access control rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn get_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<IpAccessControlService>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let rule = service.get_by_id(id).await.map_err(Problem::from)?;

    Ok(Json(IpAccessControlResponse::from(rule)))
}

/// Create a new IP access control rule
#[utoipa::path(
    post,
    path = "/ip-access-control",
    request_body = CreateIpAccessControlRequest,
    responses(
        (status = 201, description = "IP access control rule created", body = IpAccessControlResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "Duplicate IP address", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn create_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<IpAccessControlService>>,
    Json(request): Json<CreateIpAccessControlRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    let created_by = Some(auth.user_id());

    let rule = service
        .create(request, created_by)
        .await
        .map_err(Problem::from)?;

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
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "IP access control rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn update_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<IpAccessControlService>>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateIpAccessControlRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    let rule = service.update(id, request).await.map_err(Problem::from)?;

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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "IP access control rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn delete_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<IpAccessControlService>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    service.delete(id).await.map_err(Problem::from)?;

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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn check_ip_blocked(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<IpAccessControlService>>,
    Path(ip): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let is_blocked = service.is_blocked(&ip).await.map_err(Problem::from)?;

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
