// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::AppState;
use crate::services::release_service::{TelemetryRelease, TelemetryReleasePage};
use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use utoipa::{IntoParams, OpenApi};

#[derive(OpenApi)]
#[openapi(paths(list_telemetry_releases),components(schemas(TelemetryRelease,TelemetryReleasePage)),tags((name="releases",description="Release correlation across native telemetry")))]
pub struct ReleaseApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/projects/{project_id}/telemetry-releases",
        get(list_telemetry_releases),
    )
}

#[derive(Deserialize, IntoParams)]
pub struct ReleaseQuery {
    /// Exact SDK release identity; absent lists observed releases.
    pub release: Option<String>,
    /// Retained observation window, 1–31 days; default 7.
    pub days: Option<u32>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[utoipa::path(get,path="/projects/{project_id}/telemetry-releases",params(("project_id"=i32,Path),ReleaseQuery),responses((status=200,body=TelemetryReleasePage),(status=400,description="Invalid release query"),(status=401,description="Authentication required"),(status=403,description="Project and telemetry permissions required")),tag="releases")]
pub async fn list_telemetry_releases(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<ReleaseQuery>,
) -> Result<Json<TelemetryReleasePage>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    permission_guard!(auth, OtelRead);
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .error_tracking_service
            .releases
            .list(
                project_id,
                query.release,
                query.days.unwrap_or(7),
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(20),
            )
            .await?,
    ))
}
