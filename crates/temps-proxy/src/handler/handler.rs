use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Extension, Json, Router,
};
use serde::Serialize;
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use tracing::{error, info};
use utoipa::OpenApi;

use super::types::AppState;
use super::types::{parse_route_type, CreateRouteRequest, RouteResponse, UpdateRouteRequest};
use crate::service::lb_service::LbServiceError;
use temps_core::{
    error_builder::ErrorBuilder, problemdetails::Problem, AuditContext, AuditOperation,
    RequestMetadata,
};

#[derive(Debug, Clone, Serialize)]
struct RouteCreatedAudit {
    context: AuditContext,
    domain: String,
    host: String,
    port: i32,
    route_type: String,
    force_override: bool,
    allow_private_upstream: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RouteUpdatedAudit {
    context: AuditContext,
    domain: String,
    previous_host: String,
    previous_port: i32,
    previous_route_type: String,
    previous_enabled: bool,
    new_host: String,
    new_port: i32,
    new_route_type: String,
    new_enabled: bool,
    force_override: bool,
    allow_private_upstream: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RouteDeletedAudit {
    context: AuditContext,
    domain: String,
    host: String,
    port: i32,
    route_type: String,
}

macro_rules! impl_route_audit {
    ($audit:ty, $operation:literal) => {
        impl AuditOperation for $audit {
            fn operation_type(&self) -> String {
                $operation.to_string()
            }
            fn user_id(&self) -> i32 {
                self.context.user_id
            }
            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }
            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }
            fn serialize(&self) -> temps_core::anyhow::Result<String> {
                serde_json::to_string(self).map_err(Into::into)
            }
        }
    };
}

impl_route_audit!(RouteCreatedAudit, "CUSTOM_ROUTE_CREATE_REQUESTED");
impl_route_audit!(RouteUpdatedAudit, "CUSTOM_ROUTE_UPDATE_REQUESTED");
impl_route_audit!(RouteDeletedAudit, "CUSTOM_ROUTE_DELETE_REQUESTED");

fn audit_context(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

fn route_problem(error: LbServiceError) -> Problem {
    let (status, title, code) = match &error {
        LbServiceError::InvalidDomain { .. }
        | LbServiceError::InvalidUpstream { .. }
        | LbServiceError::UpstreamResolution { .. }
        | LbServiceError::PrivateUpstreamRequiresAcknowledgement { .. }
        | LbServiceError::BlockedUpstream { .. } => {
            (StatusCode::BAD_REQUEST, "Invalid route", "INVALID_ROUTE")
        }
        LbServiceError::RouteAlreadyExists { .. }
        | LbServiceError::RouteOverlap { .. }
        | LbServiceError::ManagedDomainConflict { .. } => {
            (StatusCode::CONFLICT, "Route conflict", "ROUTE_CONFLICT")
        }
        LbServiceError::RouteNotFound { .. } | LbServiceError::NotFound(_) => {
            (StatusCode::NOT_FOUND, "Route not found", "ROUTE_NOT_FOUND")
        }
        LbServiceError::DatabaseConnectionError(_)
        | LbServiceError::DatabaseError(_)
        | LbServiceError::ConnectionError { .. }
        | LbServiceError::PublicIpError(_)
        | LbServiceError::DnsResolutionError { .. }
        | LbServiceError::DomainNotPointingToServer { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Route operation failed",
            "ROUTE_INTERNAL_ERROR",
        ),
    };

    let detail = if status == StatusCode::INTERNAL_SERVER_ERROR {
        "The route operation failed due to an internal service error".to_string()
    } else {
        error.to_string()
    };
    ErrorBuilder::new(status)
        .type_(format!(
            "https://temps.sh/probs/{}",
            code.to_ascii_lowercase()
        ))
        .title(title)
        .detail(detail)
        .value("error_code", code)
        .build()
}

fn audit_problem(operation: &str) -> Problem {
    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
        .type_("https://temps.sh/probs/audit-write-failed")
        .title("Audit write failed")
        .detail(format!(
            "The route {operation} was not applied because its audit record could not be stored"
        ))
        .value("error_code", "AUDIT_WRITE_FAILED")
        .build()
}

fn invalid_json_problem(error: JsonRejection) -> Problem {
    ErrorBuilder::new(error.status())
        .type_("https://temps.sh/probs/invalid-json")
        .title("Invalid JSON request")
        .detail(error.body_text())
        .value("error_code", "INVALID_JSON")
        .build()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_route,
        list_routes,
        get_route,
        update_route,
        delete_route,
    ),
    components(
        schemas(
            CreateRouteRequest,
            UpdateRouteRequest,
            RouteResponse,
        )
    ),
    info(
        title = "Load Balancer API",
        description = "API endpoints for load balancer configuration and management. \
        Handles routing rules, health checks, and traffic distribution settings.",
        version = "1.0.0"
    ),
    tags(
        (name = "Load Balancer", description = "Load balancer management endpoints")
    )
)]
pub struct LbApiDoc;

#[utoipa::path(
    tag = "Load Balancer",
    post,
    path = "/lb/routes",
    request_body = CreateRouteRequest,
    responses(
        (status = 201, description = "Route created successfully", body = RouteResponse),
        (status = 400, description = "Invalid request")
    )
)]
pub async fn create_route(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    payload: Result<Json<CreateRouteRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);
    let Json(req) = payload.map_err(invalid_json_problem)?;

    info!(
        "Creating route for domain: {} (type: {:?})",
        req.domain, req.route_type
    );
    let route_type = parse_route_type(req.route_type.as_ref()).map_err(|detail| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid route type")
            .detail(detail)
            .value("error_code", "INVALID_ROUTE_TYPE")
            .build()
    })?;
    let audit = RouteCreatedAudit {
        context: audit_context(&auth, &metadata),
        domain: req.domain.clone(),
        host: req.host.clone(),
        port: req.port,
        route_type: route_type.clone().unwrap_or_default().to_string(),
        force_override: req.force_override,
        allow_private_upstream: req.allow_private_upstream,
    };
    app_state
        .audit_service
        .create_audit_log(&audit)
        .await
        .map_err(|error| {
            error!(%error, "Refusing custom route creation because audit storage failed");
            audit_problem("creation")
        })?;
    let route = app_state
        .lb_service
        .create_route_with_options(
            req.domain,
            req.host,
            req.port,
            route_type,
            req.force_override,
            req.allow_private_upstream,
        )
        .await
        .map_err(|error| {
            error!(%error, "Failed to create custom route");
            route_problem(error)
        })?;

    Ok((StatusCode::CREATED, Json(RouteResponse::from(route))).into_response())
}

#[utoipa::path(
    tag = "Load Balancer",
    get,
    path = "/lb/routes",
    responses(
        (status = 200, description = "List of routes", body = Vec<RouteResponse>),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn list_routes(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    app_state
        .lb_service
        .list_routes()
        .await
        .map(|routes| {
            (
                StatusCode::OK,
                Json(
                    routes
                        .into_iter()
                        .map(RouteResponse::from)
                        .collect::<Vec<_>>(),
                ),
            )
                .into_response()
        })
        .map_err(|error| {
            error!(%error, "Failed to list custom routes");
            route_problem(error)
        })
}

#[utoipa::path(
    tag = "Load Balancer",
    get,
    path = "/lb/routes/{domain}",
    responses(
        (status = 200, description = "Route found", body = RouteResponse),
        (status = 404, description = "Route not found")
    )
)]
pub async fn get_route(
    State(app_state): State<Arc<AppState>>,
    Path(domain): Path<String>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    app_state
        .lb_service
        .get_route_exact(&domain)
        .await
        .map(|route| (StatusCode::OK, Json(RouteResponse::from(route))).into_response())
        .map_err(route_problem)
}

#[utoipa::path(
    tag = "Load Balancer",
    put,
    path = "/lb/routes/{domain}",
    request_body = UpdateRouteRequest,
    responses(
        (status = 200, description = "Route updated successfully", body = RouteResponse),
        (status = 404, description = "Route not found")
    )
)]
pub async fn update_route(
    State(app_state): State<Arc<AppState>>,
    Path(domain): Path<String>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    payload: Result<Json<UpdateRouteRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);
    let Json(req) = payload.map_err(invalid_json_problem)?;

    let route_type = parse_route_type(req.route_type.as_ref()).map_err(|detail| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid route type")
            .detail(detail)
            .value("error_code", "INVALID_ROUTE_TYPE")
            .build()
    })?;
    let previous = app_state
        .lb_service
        .get_route_exact(&domain)
        .await
        .map_err(route_problem)?;
    let audit = RouteUpdatedAudit {
        context: audit_context(&auth, &metadata),
        domain: previous.domain.clone(),
        previous_host: previous.host.clone(),
        previous_port: previous.port,
        previous_route_type: previous.route_type.to_string(),
        previous_enabled: previous.enabled,
        new_host: req.host.clone(),
        new_port: req.port,
        new_route_type: route_type
            .clone()
            .unwrap_or_else(|| previous.route_type.clone())
            .to_string(),
        new_enabled: req.enabled,
        force_override: previous.force_override,
        allow_private_upstream: req.allow_private_upstream,
    };
    app_state
        .audit_service
        .create_audit_log(&audit)
        .await
        .map_err(|error| {
            error!(%error, domain = %previous.domain, "Refusing custom route update because audit storage failed");
            audit_problem("update")
        })?;
    let route = app_state
        .lb_service
        .update_route_with_options(
            &domain,
            req.host,
            req.port,
            req.enabled,
            route_type,
            req.allow_private_upstream,
        )
        .await
        .map_err(route_problem)?;

    Ok((StatusCode::OK, Json(RouteResponse::from(route))).into_response())
}

#[utoipa::path(
    tag = "Load Balancer",
    delete,
    path = "/lb/routes/{domain}",
    responses(
        (status = 204, description = "Route deleted successfully"),
        (status = 404, description = "Route not found")
    )
)]
pub async fn delete_route(
    State(app_state): State<Arc<AppState>>,
    Path(domain): Path<String>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    let route = app_state
        .lb_service
        .get_route_exact(&domain)
        .await
        .map_err(route_problem)?;
    let audit = RouteDeletedAudit {
        context: audit_context(&auth, &metadata),
        domain: route.domain.clone(),
        host: route.host,
        port: route.port,
        route_type: route.route_type.to_string(),
    };
    app_state
        .audit_service
        .create_audit_log(&audit)
        .await
        .map_err(|error| {
            error!(%error, domain = %route.domain, "Refusing custom route deletion because audit storage failed");
            audit_problem("deletion")
        })?;
    app_state
        .lb_service
        .delete_route(&domain)
        .await
        .map_err(route_problem)?;
    Ok((StatusCode::NO_CONTENT, "").into_response())
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/lb/routes", post(create_route))
        .route("/lb/routes", get(list_routes))
        .route("/lb/routes/{domain}", get(get_route))
        .route("/lb/routes/{domain}", put(update_route))
        .route("/lb/routes/{domain}", delete(delete_route))
}
