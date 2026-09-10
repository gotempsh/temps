// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{handlers::types::LogAggregatorAppState, services::global_search::*};
use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};

// Control-plane work only: never let archive scans compete without bounds
// with ingestion/proxy traffic. Saturation rejects immediately, with no queue.
static SEARCH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[utoipa::path(post, path="/logs/global/search", tag="Logs", request_body=GlobalLogSearchRequest,
    responses((status=200,body=GlobalLogSearchResponse),(status=400,body=ProblemDetails),(status=403,body=ProblemDetails),(status=429,body=ProblemDetails)), security(("bearer_auth"=[])))]
pub async fn search_global_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Json(request): Json<GlobalLogSearchRequest>,
) -> Result<Json<GlobalLogSearchResponse>, Problem> {
    permission_guard!(auth, LogsRead);
    let _permit = SEARCH_SLOTS.try_acquire().map_err(|_| {
        problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
            .with_title("Log search is busy")
            .with_detail("Two archive searches are already running. Retry shortly.")
    })?;
    let mut hidden_projects = vec![];
    let unrestricted_services = auth.is_instance_admin() || state.project_access_checker.is_none();
    if !auth.is_deployment_token() && !auth.is_instance_admin() {
        if let Some(checker) = &state.project_access_checker {
            let user = auth.user_id_opt().ok_or_else(|| {
                problemdetails::new(StatusCode::FORBIDDEN).with_title("Log access denied")
            })?;
            hidden_projects = checker
                .hidden_project_ids(user)
                .await
                .map_err(|error| {
                    tracing::error!(%error,"Could not resolve global log access");
                    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Could not resolve log access")
                })?
                .unwrap_or_default();
        }
    }
    let access = GlobalLogAccess {
        hidden_projects,
        bound_project: auth.project_id(),
        unrestricted_services,
        user_id: auth.user_id_opt(),
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        state.search_service.search_global(&request, &access),
    )
    .await
    .map_err(|_| {
        problemdetails::new(StatusCode::REQUEST_TIMEOUT)
            .with_title("Log search exceeded its time budget")
            .with_detail("Shorten the time window or narrow the selected sources.")
    })??;
    Ok(Json(result))
}
