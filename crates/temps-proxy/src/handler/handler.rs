use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use tracing::{error, info};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use utoipa::OpenApi;

use super::types::AppState;
use super::types::{CreateRouteRequest, RouteResponse, UpdateRouteRequest};
use temps_core::{error_builder::ErrorBuilder, problemdetails::Problem};

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
    Json(req): Json<CreateRouteRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    info!("Creating route for domain: {}", req.domain);
    match app_state
        .lb_service
        .create_route(req.domain, req.host, req.port)
        .await
    {
        Ok(route) => Ok((StatusCode::CREATED, Json(RouteResponse::from(route))).into_response()),
        Err(e) => {
            error!("Error creating route: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Failed to create route")
                .detail(&format!("Error creating route: {}", e))
                .build())
        }
    }
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

    match app_state.lb_service.list_routes().await {
        Ok(routes) => Ok((
            StatusCode::OK,
            Json(
                routes
                    .into_iter()
                    .map(RouteResponse::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response()),
        Err(e) => {
            error!("Error listing routes: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to list routes")
                .detail(&format!("Error listing routes: {}", e))
                .build())
        }
    }
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

    match app_state.lb_service.get_route(&domain).await {
        Ok(route) => Ok((StatusCode::OK, Json(RouteResponse::from(route))).into_response()),
        Err(e) => {
            error!("Error getting route: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get route")
                .detail(&format!("Error getting route: {}", e))
                .build())
        }
    }
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
    Json(req): Json<UpdateRouteRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    match app_state
        .lb_service
        .update_route(&domain, req.host.clone(), req.port, req.enabled)
        .await
    {
        Ok(route) => Ok((StatusCode::OK, Json(RouteResponse::from(route))).into_response()),
        Err(e) => {
            error!("Error updating route: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Failed to update route")
                .detail(&format!("Error updating route: {}", e))
                .build())
        }
    }
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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    match app_state.lb_service.delete_route(&domain).await {
        Ok(_) => Ok((StatusCode::NO_CONTENT, "").into_response()),
        Err(e) => {
            error!("Error deleting route: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Failed to delete route")
                .detail(&format!("Error deleting route: {}", e))
                .build())
        }
    }
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/lb/routes", post(create_route))
        .route("/lb/routes", get(list_routes))
        .route("/lb/routes/{domain}", get(get_route))
        .route("/lb/routes/{domain}", put(update_route))
        .route("/lb/routes/{domain}", delete(delete_route))
}
