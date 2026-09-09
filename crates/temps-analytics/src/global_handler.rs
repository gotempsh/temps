// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::global::*;
use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::{
    error_builder::{bad_request, internal_server_error},
    problemdetails::Problem,
};
use utoipa::OpenApi;

pub struct GlobalAnalyticsState {
    pub service: Arc<GlobalAnalyticsService>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}
#[derive(OpenApi)]
#[openapi(
    paths(get_global_analytics, get_analytics_projects),
    components(schemas(
        GlobalAnalyticsResponse,
        GlobalAnalyticsRow,
        AnalyticsProjectOption,
        AnalyticsFacet
    ))
)]
pub struct GlobalAnalyticsApiDoc;
pub fn routes() -> Router<Arc<GlobalAnalyticsState>> {
    Router::new()
        .route("/analytics/global", get(get_global_analytics))
        .route("/analytics/global/projects", get(get_analytics_projects))
}
fn failure(error: impl std::fmt::Display) -> Problem {
    tracing::error!(%error,"Global analytics query failed");
    internal_server_error()
        .title("Could not load aggregate analytics")
        .build()
}
async fn hidden(
    auth: &temps_auth::AuthContext,
    state: &GlobalAnalyticsState,
) -> Result<Vec<i32>, Problem> {
    if !auth.is_deployment_token() && !auth.is_instance_admin() {
        if let Some(checker) = &state.project_access_checker {
            let id = auth.user_id_opt().ok_or_else(|| {
                temps_core::error_builder::forbidden()
                    .title("Project access denied")
                    .build()
            })?;
            return Ok(checker
                .hidden_project_ids(id)
                .await
                .map_err(failure)?
                .unwrap_or_default());
        }
    }
    Ok(Vec::new())
}
#[utoipa::path(get,path="/analytics/global",tag="Analytics",params(GlobalAnalyticsQuery),responses((status=200,body=GlobalAnalyticsResponse),(status=400,description="Invalid analytics filters"),(status=403,description="Access denied")),security(("bearer_auth"=[])))]
pub async fn get_global_analytics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GlobalAnalyticsState>>,
    Query(mut q): Query<GlobalAnalyticsQuery>,
) -> Result<Json<GlobalAnalyticsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    if let Some(id) = q.project_id {
        project_scope_guard!(auth, id);
        project_access_guard!(auth, id, state.project_access_checker);
    }
    q.project_id = auth.project_id().or(q.project_id);
    validate(&q).map_err(|e| bad_request().title(e.to_string()).build())?;
    let hidden = hidden(&auth, &state).await?;
    state
        .service
        .query(&q, &hidden)
        .await
        .map(Json)
        .map_err(|e| match e {
            GlobalAnalyticsError::InvalidQuery { .. } => bad_request().title(e.to_string()).build(),
            _ => failure(e),
        })
}
#[utoipa::path(get,path="/analytics/global/projects",tag="Analytics",responses((status=200,body=Vec<AnalyticsProjectOption>),(status=403,description="Access denied")),security(("bearer_auth"=[])))]
pub async fn get_analytics_projects(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GlobalAnalyticsState>>,
) -> Result<Json<Vec<AnalyticsProjectOption>>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    let hidden = hidden(&auth, &state).await?;
    state
        .service
        .projects(auth.project_id(), &hidden)
        .await
        .map(Json)
        .map_err(failure)
}
