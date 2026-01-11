use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;

use super::types::{
    AppState, CreateFunnelRequest, CreateFunnelResponse, CreateFunnelStep, EventType,
    EventTypesResponse, FunnelMetricsResponse, FunnelResponse, GetFunnelMetricsQuery,
    StepConversionResponse,
};
use crate::services::{
    CreateFunnelRequest as ServiceCreateFunnelRequest, CreateFunnelStep as ServiceCreateFunnelStep,
    FunnelFilter,
};
use temps_core::problemdetails::Problem;

/// Create a new funnel
#[utoipa::path(
    post,
    path = "/projects/{project_id}/funnels",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = CreateFunnelRequest,
    responses(
        (status = 201, description = "Funnel created successfully", body = CreateFunnelResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_funnel(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateFunnelRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FunnelWrite);
    // Map HTTP request to service request
    let service_request = ServiceCreateFunnelRequest {
        name: request.name,
        description: request.description,
        steps: request
            .steps
            .into_iter()
            .map(|step| ServiceCreateFunnelStep {
                event_name: step.event_name,
                event_filter: step.event_filter,
            })
            .collect(),
    };

    match state
        .funnel_service
        .create_funnel(project_id, service_request)
        .await
    {
        Ok(funnel_id) => Ok((
            StatusCode::CREATED,
            Json(CreateFunnelResponse {
                funnel_id,
                message: "Funnel created successfully".to_string(),
            }),
        )),
        Err(e) => Err(temps_core::error_builder::ErrorBuilder::new(
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .title("Failed to create funnel")
        .detail(format!("Error creating funnel: {}", e))
        .build()),
    }
}

/// Get funnel metrics
#[utoipa::path(
    get,
    path = "/projects/{project_id}/funnels/{funnel_id}/metrics",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("funnel_id" = i32, Path, description = "Funnel ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID filter"),
        ("country_code" = Option<String>, Query, description = "Country code filter"),
        ("start_date" = Option<String>, Query, description = "Start date filter (ISO 8601)"),
        ("end_date" = Option<String>, Query, description = "End date filter (ISO 8601)")
    ),
    responses(
        (status = 200, description = "Funnel metrics retrieved successfully", body = FunnelMetricsResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Funnel not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_funnel_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, funnel_id)): Path<(i32, i32)>,
    Query(query): Query<GetFunnelMetricsQuery>,
) -> Result<Json<FunnelMetricsResponse>, Problem> {
    permission_guard!(auth, FunnelRead);
    // Parse dates if provided

    let filter = FunnelFilter {
        project_id: Some(project_id),
        environment_id: query.environment_id,
        country_code: query.country_code,
        start_date: query.start_date.map(|d| d.into()),
        end_date: query.end_date.map(|d| d.into()),
    };

    match state
        .funnel_service
        .get_funnel_metrics(funnel_id, filter)
        .await
    {
        Ok(metrics) => {
            // Map service response to HTTP response
            let response = FunnelMetricsResponse {
                funnel_id: metrics.funnel_id,
                funnel_name: metrics.funnel_name,
                total_entries: metrics.total_entries,
                step_conversions: metrics
                    .step_conversions
                    .into_iter()
                    .map(|step| StepConversionResponse {
                        step_id: step.step_id,
                        step_name: step.step_name,
                        step_order: step.step_order,
                        completions: step.completions,
                        conversion_rate: step.conversion_rate,
                        drop_off_rate: step.drop_off_rate,
                        average_time_to_complete_seconds: step.average_time_to_complete_seconds,
                    })
                    .collect(),
                overall_conversion_rate: metrics.overall_conversion_rate,
                average_completion_time_seconds: metrics.average_completion_time_seconds,
            };
            Ok(Json(response))
        }
        Err(e) => {
            let (_status, title) = match e {
                sea_orm::DbErr::RecordNotFound(_) => (StatusCode::NOT_FOUND, "Funnel not found"),
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to get funnel metrics",
                ),
            };
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title(title)
                    .detail(format!("Error retrieving funnel metrics: {}", e))
                    .build(),
            )
        }
    }
}

/// List all funnels for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/funnels",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 200, description = "Funnels retrieved successfully", body = Vec<FunnelResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_funnels(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<FunnelResponse>>, Problem> {
    permission_guard!(auth, FunnelRead);
    match state.funnel_service.list_funnels(project_id).await {
        Ok(funnels) => {
            let funnel_responses: Vec<FunnelResponse> = funnels
                .into_iter()
                .map(|funnel| FunnelResponse {
                    id: funnel.id,
                    name: funnel.name,
                    description: funnel.description,
                    is_active: funnel.is_active,
                    created_at: funnel.created_at.to_string(),
                    updated_at: funnel.updated_at.to_string(),
                })
                .collect();

            Ok(Json(funnel_responses))
        }
        Err(e) => Err(temps_core::error_builder::ErrorBuilder::new(
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .title("Failed to retrieve funnels")
        .detail(format!("Error retrieving funnels: {}", e))
        .build()),
    }
}

/// Update a funnel
#[utoipa::path(
    put,
    path = "/projects/{project_id}/funnels/{funnel_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("funnel_id" = i32, Path, description = "Funnel ID")
    ),
    request_body = CreateFunnelRequest,
    responses(
        (status = 200, description = "Funnel updated successfully"),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Funnel not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_funnel(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, funnel_id)): Path<(i32, i32)>,
    Json(request): Json<CreateFunnelRequest>,
) -> Result<Json<serde_json::Value>, Problem> {
    permission_guard!(auth, FunnelWrite);
    // Map HTTP request to service request
    let service_request = ServiceCreateFunnelRequest {
        name: request.name,
        description: request.description,
        steps: request
            .steps
            .into_iter()
            .map(|step| ServiceCreateFunnelStep {
                event_name: step.event_name,
                event_filter: step.event_filter,
            })
            .collect(),
    };

    match state
        .funnel_service
        .update_funnel(project_id, funnel_id, service_request)
        .await
    {
        Ok(_) => Ok(Json(serde_json::json!({
            "message": "Funnel updated successfully"
        }))),
        Err(e) => {
            let (_status, title) = match e {
                sea_orm::DbErr::RecordNotFound(_) => (StatusCode::NOT_FOUND, "Funnel not found"),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "Failed to update funnel"),
            };
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title(title)
                    .detail(format!("Error updating funnel: {}", e))
                    .build(),
            )
        }
    }
}

/// Delete a funnel
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/funnels/{funnel_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("funnel_id" = i32, Path, description = "Funnel ID")
    ),
    responses(
        (status = 200, description = "Funnel deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Funnel not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_funnel(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, funnel_id)): Path<(i32, i32)>,
) -> Result<Json<serde_json::Value>, Problem> {
    permission_guard!(auth, FunnelWrite);
    match state
        .funnel_service
        .delete_funnel(project_id, funnel_id)
        .await
    {
        Ok(_) => Ok(Json(serde_json::json!({
            "message": "Funnel deleted successfully"
        }))),
        Err(e) => {
            let (_status, title) = match e {
                sea_orm::DbErr::RecordNotFound(_) => (StatusCode::NOT_FOUND, "Funnel not found"),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete funnel"),
            };
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title(title)
                    .detail(format!("Error deleting funnel: {}", e))
                    .build(),
            )
        }
    }
}

/// Get all unique/distinct event types for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/events/unique",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 200, description = "Unique event types retrieved successfully", body = EventTypesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_unique_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<EventTypesResponse>, Problem> {
    permission_guard!(auth, FunnelRead);

    match state.funnel_service.get_unique_events(project_id).await {
        Ok(events) => {
            let response = EventTypesResponse {
                events: events
                    .into_iter()
                    .map(|(name, count)| EventType { name, count })
                    .collect(),
            };
            Ok(Json(response))
        }
        Err(e) => Err(temps_core::error_builder::ErrorBuilder::new(
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .title("Failed to retrieve event types")
        .detail(format!("Error: {}", e))
        .build()),
    }
}

/// Preview funnel metrics without creating the funnel
#[utoipa::path(
    post,
    path = "/projects/{project_id}/funnels/preview",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = CreateFunnelRequest,
    responses(
        (status = 200, description = "Funnel metrics preview", body = FunnelMetricsResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn preview_funnel_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<GetFunnelMetricsQuery>,
    Json(request): Json<CreateFunnelRequest>,
) -> Result<Json<FunnelMetricsResponse>, Problem> {
    permission_guard!(auth, FunnelRead);

    let filter = FunnelFilter {
        project_id: Some(project_id),
        environment_id: query.environment_id,
        country_code: query.country_code,
        start_date: query.start_date.map(|d| d.into()),
        end_date: query.end_date.map(|d| d.into()),
    };

    // Convert HTTP request to service request
    let service_request = ServiceCreateFunnelRequest {
        name: request.name.clone(),
        description: request.description.clone(),
        steps: request
            .steps
            .into_iter()
            .map(|step| ServiceCreateFunnelStep {
                event_name: step.event_name,
                event_filter: step.event_filter,
            })
            .collect(),
    };

    // Call the service to preview metrics (without saving the funnel)
    match state
        .funnel_service
        .preview_funnel_metrics(project_id, service_request, filter)
        .await
    {
        Ok(metrics) => {
            let response = FunnelMetricsResponse {
                funnel_id: 0, // Preview mode, no ID
                funnel_name: request.name,
                total_entries: metrics.total_entries,
                step_conversions: metrics
                    .step_conversions
                    .into_iter()
                    .map(|step| StepConversionResponse {
                        step_id: 0, // Preview mode, no ID
                        step_name: step.step_name,
                        step_order: step.step_order,
                        completions: step.completions,
                        conversion_rate: step.conversion_rate,
                        drop_off_rate: step.drop_off_rate,
                        average_time_to_complete_seconds: step.average_time_to_complete_seconds,
                    })
                    .collect(),
                overall_conversion_rate: metrics.overall_conversion_rate,
                average_completion_time_seconds: metrics.average_completion_time_seconds,
            };

            Ok(Json(response))
        }
        Err(e) => Err(temps_core::error_builder::ErrorBuilder::new(
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .title("Failed to preview funnel metrics")
        .detail(format!("Error: {}", e))
        .build()),
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        create_funnel,
        get_funnel_metrics,
        preview_funnel_metrics,
        get_unique_events,
        list_funnels,
        update_funnel,
        delete_funnel
    ),
    components(
        schemas(
            FunnelResponse,
            CreateFunnelResponse,
            CreateFunnelRequest,
            CreateFunnelStep,
            FunnelMetricsResponse,
            StepConversionResponse,
            GetFunnelMetricsQuery,
            EventTypesResponse,
            EventType
        )
    ),
    tags(
        (name = "Funnels", description = "Funnel management endpoints")
    )
)]
pub struct FunnelApiDoc;

pub fn configure_routes() -> axum::Router<Arc<super::types::AppState>> {
    use axum::routing::{delete, get, post, put};

    axum::Router::new()
        .route("/projects/{project_id}/funnels", post(create_funnel))
        .route("/projects/{project_id}/funnels", get(list_funnels))
        .route(
            "/projects/{project_id}/funnels/preview",
            post(preview_funnel_metrics),
        )
        .route(
            "/projects/{project_id}/events/unique",
            get(get_unique_events),
        )
        .route(
            "/projects/{project_id}/funnels/{funnel_id}",
            put(update_funnel),
        )
        .route(
            "/projects/{project_id}/funnels/{funnel_id}",
            delete(delete_funnel),
        )
        .route(
            "/projects/{project_id}/funnels/{funnel_id}/metrics",
            get(get_funnel_metrics),
        )
}
