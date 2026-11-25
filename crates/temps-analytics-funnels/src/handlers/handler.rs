// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use std::sync::Arc;
use temps_authz::RequireAuth;
use temps_authz::{permission_guard, project_access_guard, project_scope_guard};
use temps_core::PageQuery;

use super::types::{
    AppState, CreateFunnelRequest, CreateFunnelStep, EventType, EventTypesResponse, FunnelList,
    FunnelMessageResponse, FunnelMetricsResponse, FunnelResponse, FunnelStepResponse,
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

/// Map an HTTP funnel definition to the service request.
fn service_request(request: CreateFunnelRequest) -> ServiceCreateFunnelRequest {
    ServiceCreateFunnelRequest {
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
    }
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
        (status = 201, description = "Funnel created; the body is the funnel with its steps", body = FunnelResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let funnel_id = state
        .funnel_service
        .create_funnel(project_id, service_request(request))
        .await
        .map_err(|e| funnel_problem(e, "Failed to create funnel", "Error creating funnel"))?;
    let funnel = state
        .funnel_service
        .get_funnel(project_id, funnel_id)
        .await
        .map_err(|e| {
            funnel_problem(
                e,
                "Failed to read the created funnel",
                "Error reading funnel",
            )
        })?;
    Ok((StatusCode::CREATED, Json(FunnelResponse::from(funnel))))
}

/// Get one funnel with its steps
#[utoipa::path(
    get,
    path = "/projects/{project_id}/funnels/{funnel_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("funnel_id" = i32, Path, description = "Funnel ID")
    ),
    responses(
        (status = 200, description = "The funnel with its steps", body = FunnelResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No active funnel with this ID in the project"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Funnels",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_funnel(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, funnel_id)): Path<(i32, i32)>,
) -> Result<Json<FunnelResponse>, Problem> {
    permission_guard!(auth, FunnelRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let funnel = state
        .funnel_service
        .get_funnel(project_id, funnel_id)
        .await
        .map_err(|e| funnel_problem(e, "Failed to get funnel", "Error retrieving funnel"))?;
    Ok(Json(FunnelResponse::from(funnel)))
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

/// List a project's active funnels, newest first, with their steps
#[utoipa::path(
    get,
    path = "/projects/{project_id}/funnels",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        PageQuery
    ),
    responses(
        (status = 200, description = "One page of funnels, newest first", body = FunnelList),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
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
    Query(page): Query<PageQuery>,
) -> Result<Json<FunnelList>, Problem> {
    permission_guard!(auth, FunnelRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (funnels, total) = state
        .funnel_service
        .list_funnel_page(project_id, page)
        .await
        .map_err(|e| funnel_problem(e, "Failed to retrieve funnels", "Error retrieving funnels"))?;
    Ok(Json(FunnelList::new(
        funnels.into_iter().map(FunnelResponse::from).collect(),
        total,
        page,
    )))
}

/// Update a funnel
///
/// Replaces the funnel's name, description and steps.
#[utoipa::path(
    put,
    path = "/projects/{project_id}/funnels/{funnel_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("funnel_id" = i32, Path, description = "Funnel ID")
    ),
    request_body = CreateFunnelRequest,
    responses(
        (status = 200, description = "Funnel updated; the body is the funnel with its steps", body = FunnelResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
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
) -> Result<Json<FunnelResponse>, Problem> {
    permission_guard!(auth, FunnelWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state
        .funnel_service
        .update_funnel(project_id, funnel_id, service_request(request))
        .await
        .map_err(|e| funnel_problem(e, "Failed to update funnel", "Error updating funnel"))?;
    let funnel = state
        .funnel_service
        .get_funnel(project_id, funnel_id)
        .await
        .map_err(|e| {
            funnel_problem(
                e,
                "Failed to read the updated funnel",
                "Error reading funnel",
            )
        })?;
    Ok(Json(FunnelResponse::from(funnel)))
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
        (status = 403, description = "Insufficient permissions"),
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
    project_scope_guard!(auth, project_id);
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
        get_funnel,
        update_funnel,
        delete_funnel
    ),
    components(
        schemas(
            FunnelResponse,
            FunnelStepResponse,
            FunnelList,
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
    use axum::routing::{get, post};

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
            get(get_funnel).put(update_funnel).delete(delete_funnel),
        )
        .route(
            "/projects/{project_id}/funnels/{funnel_id}/metrics",
            get(get_funnel_metrics),
        )
}

#[cfg(test)]
mod funnel_handler_tests {
    use super::*;
    use crate::services::FunnelService;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_authz::context::AuthContext;
    use temps_authz::permissions::{Permission, Role};
    use temps_entities::{funnel_steps, funnels, users};
    use tower::ServiceExt;
    use utoipa::OpenApi;

    fn user() -> users::Model {
        let now = chrono::Utc::now();
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
        }
    }

    fn admin() -> AuthContext {
        AuthContext::new_session(user(), Role::Admin)
    }

    /// An API key that can do everything with funnels except `granted`'s
    /// complement: it holds only the listed permissions.
    fn api_key(granted: Vec<Permission>) -> AuthContext {
        AuthContext::new_api_key(user(), None, Some(granted), "ci".to_string(), 3)
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

    fn step(order: i32, filter: Option<&str>) -> funnel_steps::Model {
        funnel_steps::Model {
            id: 10 + order,
            funnel_id: 2,
            step_order: order,
            event_name: format!("step_{order}"),
            event_filter: filter.map(str::to_string),
            created_at: chrono::Utc::now(),
        }
    }

    fn state(db: MockDatabase) -> Arc<AppState> {
        Arc::new(AppState {
            funnel_service: Arc::new(FunnelService::new(Arc::new(db.into_connection()))),
            project_access_checker: None,
        })
    }

    fn empty_state() -> Arc<AppState> {
        state(MockDatabase::new(DatabaseBackend::Postgres))
    }

    /// A database that finds the funnel and its two steps.
    fn db_with_funnel() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[funnel()]])
            .append_query_results([[step(1, None), step(2, Some(r#"{"pathname":"/thanks"}"#))]])
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("the response body is buffered");
        serde_json::from_slice(&bytes).expect("the response body is JSON")
    }

    fn request(
        method: &str,
        uri: &str,
        auth: Option<AuthContext>,
    ) -> axum::http::Request<axum::body::Body> {
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                r#"{"name":"Checkout","steps":[{"event_name":"step_1"}]}"#,
            ))
            .expect("a valid request");
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        request
    }

    #[test]
    fn a_missing_funnel_is_a_404_and_anything_else_a_500() {
        let missing = funnel_problem(
            sea_orm::DbErr::RecordNotFound("funnel 9 not found in project 1".to_string()),
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

    #[tokio::test]
    async fn every_funnel_endpoint_requires_authentication() {
        let router = configure_routes().with_state(empty_state());
        for (method, uri) in [
            ("GET", "/projects/1/funnels"),
            ("POST", "/projects/1/funnels"),
            ("GET", "/projects/1/funnels/2"),
            ("PUT", "/projects/1/funnels/2"),
            ("DELETE", "/projects/1/funnels/2"),
        ] {
            let response = router
                .clone()
                .oneshot(request(method, uri, None))
                .await
                .expect("the router answers");
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn get_funnel_returns_the_funnel_with_its_steps() {
        let router = configure_routes().with_state(state(db_with_funnel()));
        let response = router
            .oneshot(request("GET", "/projects/1/funnels/2", Some(admin())))
            .await
            .expect("the router answers");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["id"], 2);
        assert_eq!(body["project_id"], 1);
        assert!(
            body["created_at"]
                .as_str()
                .is_some_and(|t| t.ends_with('Z')),
            "timestamps are ISO 8601 in UTC: {body}"
        );
        assert_eq!(
            body["steps"],
            serde_json::json!([
                {"id": 11, "order": 1, "event_name": "step_1", "event_filter": []},
                {"id": 12, "order": 2, "event_name": "step_2",
                 "event_filter": [{"type": "page_path", "value": "/thanks"}]}
            ])
        );
    }

    #[tokio::test]
    async fn get_funnel_is_a_404_problem_naming_the_funnel() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<funnels::Model>::new()]);
        let router = configure_routes().with_state(state(db));
        let response = router
            .oneshot(request("GET", "/projects/1/funnels/99", Some(admin())))
            .await
            .expect("the router answers");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert_eq!(body["title"], "Funnel not found");
        assert!(
            body["detail"]
                .as_str()
                .is_some_and(|d| d.contains("funnel 99 not found in project 1")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn reading_a_funnel_takes_funnel_read() {
        let denied = get_funnel(
            RequireAuth(api_key(vec![Permission::FunnelWrite])),
            State(empty_state()),
            Path((1, 2)),
        )
        .await
        .expect_err("the key lacks funnel:read");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        let allowed = get_funnel(
            RequireAuth(api_key(vec![Permission::FunnelRead])),
            State(state(db_with_funnel())),
            Path((1, 2)),
        )
        .await
        .expect("funnel:read is enough");
        assert_eq!(allowed.0.steps.len(), 2);
    }

    #[tokio::test]
    async fn a_deployment_token_cannot_read_another_projects_funnel() {
        let token = AuthContext::new_deployment_token(
            5,
            None,
            None,
            1,
            "deploy".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::FullAccess],
        );
        let denied = get_funnel(RequireAuth(token), State(empty_state()), Path((1, 2)))
            .await
            .expect_err("the token belongs to project 5");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_funnel_returns_201_with_the_created_funnel() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[funnel()]]) // INSERT funnel RETURNING
            .append_query_results([[step(1, None)]]) // INSERT step RETURNING
            .append_query_results([[funnel()]])
            .append_query_results([[step(1, None)]]);
        let router = configure_routes().with_state(state(db));
        let response = router
            .oneshot(request("POST", "/projects/1/funnels", Some(admin())))
            .await
            .expect("the router answers");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        assert_eq!(body["id"], 2);
        assert_eq!(body["steps"][0]["event_name"], "step_1");
    }

    #[tokio::test]
    async fn update_funnel_returns_the_updated_funnel() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[funnel()], [funnel()]]) // find, UPDATE RETURNING
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }]) // delete the old steps
            .append_query_results([[step(1, None)]]) // INSERT step RETURNING
            .append_query_results([[funnel()]])
            .append_query_results([[step(1, None)]]);
        let response = update_funnel(
            RequireAuth(admin()),
            State(state(db)),
            Path((1, 2)),
            Json(CreateFunnelRequest {
                name: "Checkout".to_string(),
                description: None,
                steps: vec![CreateFunnelStep {
                    event_name: "step_1".to_string(),
                    event_filter: Vec::new(),
                }],
            }),
        )
        .await
        .expect("the funnel exists");
        assert_eq!(response.0.id, 2);
        assert_eq!(response.0.steps.len(), 1);
    }

    #[tokio::test]
    async fn writes_take_funnel_write() {
        let reader = api_key(vec![Permission::FunnelRead]);
        let denied = update_funnel(
            RequireAuth(reader.clone()),
            State(empty_state()),
            Path((1, 2)),
            Json(CreateFunnelRequest {
                name: "Checkout".to_string(),
                description: None,
                steps: Vec::new(),
            }),
        )
        .await
        .expect_err("the key lacks funnel:write");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
        let denied = create_funnel(
            RequireAuth(reader),
            State(empty_state()),
            Path(1),
            Json(CreateFunnelRequest {
                name: "Checkout".to_string(),
                description: None,
                steps: Vec::new(),
            }),
        )
        .await
        .err()
        .expect("the key lacks funnel:write");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_list_is_a_page_envelope() {
        let count = std::collections::BTreeMap::from([(
            "num_items".to_string(),
            sea_orm::Value::BigInt(Some(41)),
        )]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count]])
            .append_query_results([[funnel()]])
            .append_query_results([[step(1, None)]]);
        let router = configure_routes().with_state(state(db));
        let response = router
            .oneshot(request(
                "GET",
                "/projects/1/funnels?page=3&page_size=500",
                Some(admin()),
            ))
            .await
            .expect("the router answers");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["total"], 41);
        assert_eq!(body["page"], 3);
        assert_eq!(body["page_size"], 100, "page_size is clamped to 100");
        assert_eq!(body["items"][0]["steps"][0]["order"], 1);
    }

    #[tokio::test]
    async fn delete_funnel_keeps_its_json_on_the_wire() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[funnel()], [funnel()]]);
        let response = delete_funnel(RequireAuth(admin()), State(state(db)), Path((1, 2)))
            .await
            .expect("the funnel exists");
        assert_eq!(
            body_json(response.into_response()).await,
            serde_json::json!({"message": "Funnel deleted successfully"})
        );
    }

    #[test]
    fn the_spec_documents_the_funnel_bodies() {
        let doc = serde_json::to_value(FunnelApiDoc::openapi()).expect("the spec serializes");
        let funnel_ref = "#/components/schemas/FunnelResponse";
        let item = &doc["paths"]["/projects/{project_id}/funnels/{funnel_id}"];
        for (method, status) in [("get", "200"), ("put", "200")] {
            assert_eq!(
                item[method]["responses"][status]["content"]["application/json"]["schema"]["$ref"],
                funnel_ref,
                "{method} returns the funnel"
            );
            assert!(
                item[method]["responses"]["404"].is_object(),
                "{method} documents 404"
            );
        }
        let collection = &doc["paths"]["/projects/{project_id}/funnels"];
        assert_eq!(
            collection["post"]["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
            funnel_ref
        );
        assert_eq!(
            collection["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/FunnelList"
        );
        let params: Vec<&str> = collection["get"]["parameters"]
            .as_array()
            .expect("the list takes parameters")
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect();
        assert!(
            params.contains(&"page") && params.contains(&"page_size"),
            "{params:?}"
        );
        let created_at =
            &doc["components"]["schemas"]["FunnelResponse"]["properties"]["created_at"];
        assert_eq!(created_at["format"], "date-time");
    }
}
