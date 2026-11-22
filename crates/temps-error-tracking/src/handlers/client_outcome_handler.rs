// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::AppState;
use crate::services::client_outcome_service::{ClientOutcome, ClientOutcomePage};
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
#[openapi(paths(list_client_outcomes),components(schemas(ClientOutcome,ClientOutcomePage)),tags((name="ingestion-health",description="Approximate SDK-reported telemetry discards")))]
pub struct ClientOutcomeApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/projects/{project_id}/client-outcomes",
        get(list_client_outcomes),
    )
}

#[derive(Deserialize, IntoParams)]
pub struct ClientOutcomeQuery {
    /// Retained observation window, 1–31 days; default 7.
    pub days: Option<u32>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[utoipa::path(get,path="/projects/{project_id}/client-outcomes",params(("project_id"=i32,Path),ClientOutcomeQuery),responses((status=200,body=ClientOutcomePage),(status=400,description="Invalid outcome query"),(status=401,description="Authentication required"),(status=403,description="Project and telemetry permissions required")),tag="ingestion-health")]
pub async fn list_client_outcomes(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<ClientOutcomeQuery>,
) -> Result<Json<ClientOutcomePage>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .error_tracking_service
            .client_outcomes
            .list(
                project_id,
                query.days.unwrap_or(7),
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(20),
            )
            .await?,
    ))
}
