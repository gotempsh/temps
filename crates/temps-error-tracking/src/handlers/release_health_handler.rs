// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Release health (ADR 111, D3): crash-free sessions and users, adoption and
//! the deployment of each release, and one release's hourly series.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use utoipa::{IntoParams, OpenApi};

use super::types::AppState;
use crate::services::release_health::{
    ReleaseDeployment, ReleaseHealth, ReleaseHealthPage, ReleaseHealthPoint, ReleaseHealthSeries,
    ReleaseHealthTracking, DEFAULT_HEALTH_DAYS,
};

#[derive(OpenApi)]
#[openapi(
    paths(list_release_health, get_release_health_series),
    components(schemas(
        ReleaseHealthPage,
        ReleaseHealth,
        ReleaseDeployment,
        ReleaseHealthTracking,
        ReleaseHealthSeries,
        ReleaseHealthPoint
    )),
    tags((name = "releases", description = "Release health from SDK sessions"))
)]
pub struct ReleaseHealthApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/release-health",
            get(list_release_health),
        )
        .route(
            "/projects/{project_id}/release-health/series",
            get(get_release_health_series),
        )
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ReleaseHealthQuery {
    /// SDK environment (as the SDK sent it); absent reads all of them.
    pub environment: Option<String>,
    /// Window in days, 1 to 14; default 7.
    pub days: Option<u32>,
    pub page: Option<u64>,
    /// 1 to 100; default 20.
    pub per_page: Option<u64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ReleaseHealthSeriesQuery {
    /// The release, exactly as the SDK sent it.
    pub release: String,
    pub environment: Option<String>,
    /// Window in days, 1 to 14; default 7.
    pub days: Option<u32>,
}

/// Releases with sessions in the window, the most recently active first.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/release-health",
    params(("project_id" = i32, Path, description = "Project ID"), ReleaseHealthQuery),
    responses(
        (status = 200, description = "Release health of the project", body = ReleaseHealthPage),
        (status = 400, description = "Invalid window, page or environment"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Release health could not be read")
    ),
    tag = "releases",
    security(("bearer_auth" = []))
)]
pub async fn list_release_health(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<ReleaseHealthQuery>,
) -> Result<Json<ReleaseHealthPage>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let environment = query.environment.filter(|e| !e.is_empty());
    Ok(Json(
        state
            .error_tracking_service
            .release_health
            .list(
                project_id,
                environment,
                query.days.unwrap_or(DEFAULT_HEALTH_DAYS),
                query.page.unwrap_or(1),
                query.per_page.unwrap_or(20),
            )
            .await?,
    ))
}

/// One release's sessions and crash-free rate per hour.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/release-health/series",
    params(("project_id" = i32, Path, description = "Project ID"), ReleaseHealthSeriesQuery),
    responses(
        (status = 200, description = "Hourly health of the release", body = ReleaseHealthSeries),
        (status = 400, description = "Invalid release, window or environment"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Release health could not be read")
    ),
    tag = "releases",
    security(("bearer_auth" = []))
)]
pub async fn get_release_health_series(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<ReleaseHealthSeriesQuery>,
) -> Result<Json<ReleaseHealthSeries>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let environment = query.environment.filter(|e| !e.is_empty());
    Ok(Json(
        state
            .error_tracking_service
            .release_health
            .series(
                project_id,
                query.release,
                environment,
                query.days.unwrap_or(DEFAULT_HEALTH_DAYS),
            )
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::group_workflow_handler::tests::{
        app_state, test_user, RecordingAuditLogger,
    };
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use temps_authz::Role;

    fn state_without_queries() -> Arc<AppState> {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        app_state(&db, Arc::new(RecordingAuditLogger::default()))
    }

    #[tokio::test]
    async fn without_a_token_both_routes_answer_401() {
        let server = TestServer::new(routes().with_state(state_without_queries()));
        for uri in [
            "/projects/1/release-health",
            "/projects/1/release-health/series?release=1.0.0",
        ] {
            let response = server.get(uri).await;
            assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_role_without_error_tracking_read_is_refused() {
        let problem = list_release_health(
            State(state_without_queries()),
            RequireAuth(test_user(3, Role::MetricsIngest)),
            Path(1),
            Query(ReleaseHealthQuery {
                environment: None,
                days: None,
                page: None,
                per_page: None,
            }),
        )
        .await
        .expect_err("refused");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_out_of_range_window_is_a_400_naming_the_bounds() {
        let problem = list_release_health(
            State(state_without_queries()),
            RequireAuth(test_user(3, Role::Reader)),
            Path(1),
            Query(ReleaseHealthQuery {
                environment: None,
                days: Some(15),
                page: None,
                per_page: None,
            }),
        )
        .await
        .expect_err("refused");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert!(problem
            .body
            .get("detail")
            .and_then(|d| d.as_str())
            .is_some_and(|d| d.contains("1 to 14 days")));

        let problem = get_release_health_series(
            State(state_without_queries()),
            RequireAuth(test_user(3, Role::Reader)),
            Path(1),
            Query(ReleaseHealthSeriesQuery {
                release: "   ".into(),
                environment: None,
                days: None,
            }),
        )
        .await
        .expect_err("refused");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }
}
