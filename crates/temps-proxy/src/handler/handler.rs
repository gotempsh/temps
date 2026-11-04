// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use std::sync::Arc;
use temps_authz::permission_guard;
use temps_authz::RequireAuth;
use tracing::{error, info};
use utoipa::OpenApi;

use super::types::AppState;
use super::types::{parse_route_type, CreateRouteRequest, RouteResponse, UpdateRouteRequest};
use crate::service::lb_service::LbServiceError;
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

impl From<LbServiceError> for Problem {
    fn from(error: LbServiceError) -> Self {
        let (status, title) = match &error {
            LbServiceError::NotFound(_) | LbServiceError::RouteNotFound { .. } => {
                (StatusCode::NOT_FOUND, "Route Not Found")
            }
            LbServiceError::RouteAlreadyExists { .. }
            | LbServiceError::AttachedToStatusPage { .. } => {
                (StatusCode::CONFLICT, "Route Conflict")
            }
            LbServiceError::DomainNotPointingToServer { .. }
            | LbServiceError::DnsResolutionError { .. } => {
                (StatusCode::BAD_REQUEST, "Domain Not Resolvable")
            }
            LbServiceError::DatabaseConnectionError(_)
            | LbServiceError::DatabaseError(_)
            | LbServiceError::ConnectionError { .. }
            | LbServiceError::PublicIpError(_) => {
                error!("Load balancer route operation failed: {:?}", error);
                (StatusCode::INTERNAL_SERVER_ERROR, "Route Operation Failed")
            }
        };
        ErrorBuilder::new(status)
            .title(title)
            .detail(error.to_string())
            .build()
    }
}

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

    info!(
        "Creating route for domain: {} (type: {:?})",
        req.domain, req.route_type
    );
    let route_type = parse_route_type(req.route_type.as_ref());
    match app_state
        .lb_service
        .create_route(req.domain, req.host, req.port, route_type)
        .await
    {
        Ok(route) => Ok((StatusCode::CREATED, Json(RouteResponse::from(route))).into_response()),
        Err(e) => {
            error!("Error creating route: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Failed to create route")
                .detail(format!("Error creating route: {}", e))
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
                .detail(format!("Error listing routes: {}", e))
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
        (status = 404, description = "Route not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_route(
    State(app_state): State<Arc<AppState>>,
    Path(domain): Path<String>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let route = app_state.lb_service.get_route(&domain).await?;
    Ok((StatusCode::OK, Json(RouteResponse::from(route))).into_response())
}

#[utoipa::path(
    tag = "Load Balancer",
    put,
    path = "/lb/routes/{domain}",
    request_body = UpdateRouteRequest,
    responses(
        (status = 200, description = "Route updated successfully", body = RouteResponse),
        (status = 404, description = "Route not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn update_route(
    State(app_state): State<Arc<AppState>>,
    Path(domain): Path<String>,
    RequireAuth(auth): RequireAuth,
    Json(req): Json<UpdateRouteRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    let route_type = parse_route_type(req.route_type.as_ref());
    let route = app_state
        .lb_service
        .update_route(&domain, req.host.clone(), req.port, req.enabled, route_type)
        .await?;
    Ok((StatusCode::OK, Json(RouteResponse::from(route))).into_response())
}

#[utoipa::path(
    tag = "Load Balancer",
    delete,
    path = "/lb/routes/{domain}",
    responses(
        (status = 204, description = "Route deleted successfully"),
        (status = 404, description = "Route not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn delete_route(
    State(app_state): State<Arc<AppState>>,
    Path(domain): Path<String>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    app_state.lb_service.delete_route(&domain).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_route_is_a_404_naming_the_domain() {
        for error in [
            LbServiceError::NotFound("nothing.invalid".to_string()),
            LbServiceError::RouteNotFound {
                domain: "nothing.invalid".to_string(),
            },
        ] {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
            let body = serde_json::to_string(&problem.body).unwrap();
            assert!(body.contains("nothing.invalid"), "{body}");
        }
    }

    #[test]
    fn a_database_failure_is_a_500_and_not_reported_as_missing() {
        let problem = Problem::from(LbServiceError::DatabaseError(sea_orm::DbErr::Custom(
            "connection lost".to_string(),
        )));
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
