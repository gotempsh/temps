// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use std::sync::Arc;
use temps_authz::RequireAuth;
use temps_authz::{permission_guard, project_access_guard};

use super::types::{
    AppState, CreateFunnelRequest, CreateFunnelResponse, CreateFunnelStep, EventType,
    EventTypesResponse, FunnelMessageResponse, FunnelMetricsResponse, FunnelResponse,
    GetFunnelMetricsQuery, GetUniqueEventsQuery, StepConversionResponse,
};
use crate::services::{
    CreateFunnelRequest as ServiceCreateFunnelRequest, CreateFunnelStep as ServiceCreateFunnelStep,
    FunnelFilter,
};
use temps_core::problemdetails::Problem;

/// The Problem for a failed funnel operation: 404 when the funnel isn't there,
/// so a client can tell "doesn't exist" from "Temps is broken".
fn funnel_problem(error: sea_orm::DbErr, title: &str, detail: &str) -> Problem {
    let (status, title) = match error {
        sea_orm::DbErr::RecordNotFound(_) => (StatusCode::NOT_FOUND, "Funnel not found"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, title),
    };
    temps_core::error_builder::ErrorBuilder::new(status)
        .title(title)
        .detail(format!("{detail}: {error}"))
        .build()
}

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
    project_access_guard!(auth, project_id, state.project_access_checker);
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
    project_access_guard!(auth, project_id, state.project_access_checker);
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
        Err(e) => Err(funnel_problem(
            e,
            "Failed to get funnel metrics",
            "Error retrieving funnel metrics",
        )),
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
    project_access_guard!(auth, project_id, state.project_access_checker);
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
        (status = 200, description = "Funnel updated successfully", body = FunnelMessageResponse),
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
) -> Result<Json<FunnelMessageResponse>, Problem> {
    permission_guard!(auth, FunnelWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
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
        Ok(_) => Ok(Json(FunnelMessageResponse {
            message: "Funnel updated successfully".to_string(),
        })),
        Err(e) => Err(funnel_problem(
            e,
            "Failed to update funnel",
            "Error updating funnel",
        )),
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
        (status = 200, description = "Funnel deleted successfully", body = FunnelMessageResponse),
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
) -> Result<Json<FunnelMessageResponse>, Problem> {
    permission_guard!(auth, FunnelWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);
    match state
        .funnel_service
        .delete_funnel(project_id, funnel_id)
        .await
    {
        Ok(_) => Ok(Json(FunnelMessageResponse {
            message: "Funnel deleted successfully".to_string(),
        })),
        Err(e) => Err(funnel_problem(
            e,
            "Failed to delete funnel",
            "Error deleting funnel",
        )),
    }
}

/// Get all unique/distinct event types for a project (paginated)
#[utoipa::path(
    get,
    path = "/projects/{project_id}/events/unique",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Items per page (default: 50, max: 100)")
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
    Query(query): Query<GetUniqueEventsQuery>,
) -> Result<Json<EventTypesResponse>, Problem> {
    permission_guard!(auth, FunnelRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    match state
        .funnel_service
        .get_unique_events(project_id, query.page, query.page_size)
        .await
    {
        Ok((events, total)) => {
            let page = query.page.unwrap_or(1);
            let page_size = query.page_size.unwrap_or(50).min(100);
            let response = EventTypesResponse {
                events: events
                    .into_iter()
                    .map(|(name, count)| EventType { name, count })
                    .collect(),
                total,
                page,
                page_size,
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
///
/// Requires `FunnelWrite`, the same as creating a funnel: although nothing is
/// persisted, the request carries an arbitrary caller-defined step list that
/// is run as an ad-hoc multi-step query over the project's analytics events.
/// The console only calls it from the create/edit funnel form, whose save
/// already needs `FunnelWrite`; read-only callers use the saved-funnel
/// `GET .../funnels/{funnel_id}/metrics` endpoint instead.
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
        (status = 403, description = "Insufficient permissions (requires funnel:write)"),
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
    permission_guard!(auth, FunnelWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
            FunnelMessageResponse,
            CreateFunnelRequest,
            CreateFunnelStep,
            FunnelMetricsResponse,
            StepConversionResponse,
            GetFunnelMetricsQuery,
            GetUniqueEventsQuery,
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

#[cfg(test)]
mod funnel_problem_tests {
    use super::*;

    #[test]
    fn a_missing_funnel_is_a_404_and_anything_else_a_500() {
        let missing = funnel_problem(
            sea_orm::DbErr::RecordNotFound("Funnel not found".to_string()),
            "Failed to delete funnel",
            "Error deleting funnel",
        );
        assert_eq!(missing.status_code, StatusCode::NOT_FOUND);

        let broken = funnel_problem(
            sea_orm::DbErr::Custom("connection lost".to_string()),
            "Failed to delete funnel",
            "Error deleting funnel",
        );
        assert_eq!(broken.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[cfg(test)]
mod funnel_message_tests {
    use super::*;
    use crate::services::FunnelService;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_authz::context::AuthContext;
    use temps_authz::permissions::Role;
    use temps_entities::{funnels, users};
    use utoipa::OpenApi;

    fn admin() -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 1,
                name: "Test User".to_string(),
                email: "test@example.com".to_string(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: now,
                updated_at: now,
            },
            Role::Admin,
        )
    }

    fn funnel() -> funnels::Model {
        let now = chrono::Utc::now();
        funnels::Model {
            id: 2,
            project_id: 1,
            name: "Checkout".to_string(),
            description: None,
            is_active: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// A state whose database finds the funnel and accepts the write.
    fn state_with_funnel() -> Arc<AppState> {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[funnel()], [funnel()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        Arc::new(AppState {
            funnel_service: Arc::new(FunnelService::new(Arc::new(db))),
            project_access_checker: None,
        })
    }

    async fn wire_body(response: Json<FunnelMessageResponse>) -> String {
        let bytes = axum::body::to_bytes(response.into_response().into_body(), 1024)
            .await
            .expect("the response body is buffered");
        String::from_utf8(bytes.to_vec()).expect("the response body is UTF-8")
    }

    #[tokio::test]
    async fn update_funnel_keeps_its_json_on_the_wire() {
        let response = update_funnel(
            RequireAuth(admin()),
            State(state_with_funnel()),
            Path((1, 2)),
            Json(CreateFunnelRequest {
                name: "Checkout".to_string(),
                description: None,
                steps: Vec::new(),
            }),
        )
        .await
        .expect("the funnel exists");
        assert_eq!(
            wire_body(response).await,
            r#"{"message":"Funnel updated successfully"}"#
        );
    }

    #[tokio::test]
    async fn delete_funnel_keeps_its_json_on_the_wire() {
        let response = delete_funnel(
            RequireAuth(admin()),
            State(state_with_funnel()),
            Path((1, 2)),
        )
        .await
        .expect("the funnel exists");
        assert_eq!(
            wire_body(response).await,
            r#"{"message":"Funnel deleted successfully"}"#
        );
    }

    #[test]
    fn update_and_delete_document_the_message_response() {
        let doc = serde_json::to_value(FunnelApiDoc::openapi()).expect("the spec serializes");
        assert!(doc["components"]["schemas"]["FunnelMessageResponse"].is_object());
        let path = &doc["paths"]["/projects/{project_id}/funnels/{funnel_id}"];
        for method in ["put", "delete"] {
            assert_eq!(
                path[method]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/FunnelMessageResponse",
                "{method} must document its 200 body"
            );
        }
    }
}
