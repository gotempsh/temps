// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Endpoints of autofix incidents (ADR-054): whether autofix can run for a
//! project, its policy, the incidents with their timelines, and the user's
//! decisions on them.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard,
    AuthContext, RequireAuth,
};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, AuditLogger, AuditOperation, ProjectAccessChecker, RequestMetadata,
};
use utoipa::{IntoParams, OpenApi};

use crate::audit::{
    AutofixIncidentClosedAudit, AutofixIncidentOpenedAudit, AutofixIncidentStoppedAudit,
    AutofixPausedAudit, AutofixSettingsUpdatedAudit,
};
use crate::autofix::evidence::{Assessment, CommandOutcome, FileChange, Signal};
use crate::autofix::{
    AutofixCapabilityResponse, AutofixError, AutofixEventListResponse, AutofixEventResponse,
    AutofixIncidentListResponse, AutofixIncidentResponse, AutofixService, AutofixSettingsResponse,
    CloseIncidentRequest, EventDetails, OpenIncidentRequest, OpenIncidentResponse,
    PauseAutofixRequest, UpdateAutofixSettingsRequest,
};

pub struct AutofixState {
    pub autofix: Arc<AutofixService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

impl AutofixState {
    async fn audit(&self, entry: &dyn AuditOperation) {
        if let Err(error) = self.audit_service.create_audit_log(entry).await {
            tracing::error!("Failed to create autofix audit log: {error}");
        }
    }
}

impl From<AutofixError> for Problem {
    fn from(error: AutofixError) -> Self {
        match error {
            AutofixError::ProjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(error.to_string()),
            AutofixError::ErrorGroupNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Error Group Not Found")
                .with_detail(error.to_string()),
            AutofixError::IncidentNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Autofix Incident Not Found")
                .with_detail(error.to_string()),
            AutofixError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Autofix Request")
                .with_detail(error.to_string()),
            AutofixError::InvalidState { .. } | AutofixError::Stopped { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Autofix Incident State Conflict")
                    .with_detail(error.to_string())
            }
            AutofixError::Unavailable { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Autofix Unavailable")
                .with_detail(error.to_string()),
            AutofixError::QueueFull { .. } => problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                .with_title("Autofix Queue Full")
                .with_detail(error.to_string()),
            AutofixError::ProjectQueueFull { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Autofix Project Queue Full")
                    .with_detail(error.to_string())
            }
            AutofixError::OwnerCannotAct { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Autofix Owner Cannot Act")
                .with_detail(error.to_string()),
            AutofixError::Database { .. } | AutofixError::Step { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListIncidentsQuery {
    /// Page number, from 1. Default 1.
    pub page: Option<u64>,
    /// Items per page. Default 20, max 100.
    pub page_size: Option<u64>,
    /// `open`, `closed`, or one exact status.
    pub status: Option<String>,
    /// Only incidents of this error group.
    pub error_group_id: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListEventsQuery {
    /// Only events after this id; pass the last id seen to poll.
    pub after_id: Option<i64>,
    /// Default 100, max 500.
    pub limit: Option<u64>,
}

fn audit_context(auth: &AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Whether autofix can run for the project, what a fix would become, and
/// the project's autofix policy.
#[utoipa::path(
    get, tag = "Autofix",
    operation_id = "get_autofix_capability",
    path = "/projects/{project_id}/autofix",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, body = AutofixCapabilityResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_capability(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<AutofixCapabilityResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(state.autofix.capability(project_id).await?))
}

/// Change the project's autofix policy. The caller becomes the user that
/// automatic incidents act as.
#[utoipa::path(
    patch, tag = "Autofix",
    operation_id = "update_autofix_settings",
    path = "/projects/{project_id}/autofix/settings",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = UpdateAutofixSettingsRequest,
    responses(
        (status = 200, body = AutofixSettingsResponse),
        (status = 400, body = ProblemDetails),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<UpdateAutofixSettingsRequest>,
) -> Result<Json<AutofixSettingsResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let settings = state
        .autofix
        .update_settings(project_id, auth.user_id(), request)
        .await?;
    state
        .audit(&AutofixSettingsUpdatedAudit {
            context: audit_context(&auth, &metadata),
            project_id,
            mode: settings.mode.clone(),
            min_confidence: settings.min_confidence,
            protected_paths: settings.protected_paths.clone(),
            has_test_command: settings.test_command.is_some(),
        })
        .await;
    Ok(Json(settings))
}

/// Pause or resume autofix for the project. Queued incidents wait while it
/// is paused.
#[utoipa::path(
    post, tag = "Autofix",
    operation_id = "set_autofix_paused",
    path = "/projects/{project_id}/autofix/pause",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = PauseAutofixRequest,
    responses(
        (status = 200, body = AutofixSettingsResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_paused(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<PauseAutofixRequest>,
) -> Result<Json<AutofixSettingsResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let settings = state
        .autofix
        .set_paused(project_id, auth.user_id(), request.paused)
        .await?;
    state
        .audit(&AutofixPausedAudit {
            context: audit_context(&auth, &metadata),
            project_id,
            paused: request.paused,
        })
        .await;
    Ok(Json(settings))
}

/// The project's autofix incidents, newest first.
#[utoipa::path(
    get, tag = "Autofix",
    operation_id = "list_autofix_incidents",
    path = "/projects/{project_id}/autofix/incidents",
    params(("project_id" = i32, Path, description = "Project ID"), ListIncidentsQuery),
    responses(
        (status = 200, body = AutofixIncidentListResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_incidents(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListIncidentsQuery>,
) -> Result<Json<AutofixIncidentListResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (incidents, total, page, page_size) = state
        .autofix
        .list_incidents(
            project_id,
            query.page,
            query.page_size,
            query.status.as_deref(),
            query.error_group_id,
        )
        .await?;
    Ok(Json(AutofixIncidentListResponse {
        items: state.autofix.responses(incidents).await?,
        total,
        page,
        page_size,
    }))
}

/// Ask autofix to reproduce and fix an error group. Returns the open
/// incident of the group when there already is one.
#[utoipa::path(
    post, tag = "Autofix",
    operation_id = "open_autofix_incident",
    path = "/projects/{project_id}/autofix/incidents",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = OpenIncidentRequest,
    responses(
        (status = 201, description = "Incident queued", body = OpenIncidentResponse),
        (status = 200, description = "The group already had an open incident", body = OpenIncidentResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 409, body = ProblemDetails),
        (status = 429, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn open_incident(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<OpenIncidentRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Starting an incident spends AI and sandbox time and acts on the
    // repository as the caller.
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (incident, created) = state
        .autofix
        .open_incident(project_id, request.error_group_id, auth.user_id(), "manual")
        .await?;
    if created {
        state
            .audit(&AutofixIncidentOpenedAudit {
                context: audit_context(&auth, &metadata),
                project_id,
                error_group_id: request.error_group_id,
                incident_id: incident.public_id.clone(),
                trigger: "manual".into(),
            })
            .await;
    }
    let incident = state.autofix.response(incident).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(OpenIncidentResponse { incident, created })))
}

/// One autofix incident.
#[utoipa::path(
    get, tag = "Autofix",
    operation_id = "get_autofix_incident",
    path = "/projects/{project_id}/autofix/incidents/{incident_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("incident_id" = String, Path, description = "Incident public id")
    ),
    responses(
        (status = 200, body = AutofixIncidentResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_incident(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Path((project_id, incident_id)): Path<(i32, String)>,
) -> Result<Json<AutofixIncidentResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let incident = state.autofix.get_incident(project_id, &incident_id).await?;
    Ok(Json(state.autofix.response(incident).await?))
}

/// The incident's timeline: every action the server, the agent and people
/// took, oldest first.
#[utoipa::path(
    get, tag = "Autofix",
    operation_id = "list_autofix_incident_events",
    path = "/projects/{project_id}/autofix/incidents/{incident_id}/events",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("incident_id" = String, Path, description = "Incident public id"),
        ListEventsQuery
    ),
    responses(
        (status = 200, body = AutofixEventListResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Path((project_id, incident_id)): Path<(i32, String)>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<AutofixEventListResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (events, incident_status) = state
        .autofix
        .events(project_id, &incident_id, query.after_id, query.limit)
        .await?;
    Ok(Json(AutofixEventListResponse {
        items: events.into_iter().map(AutofixEventResponse::from).collect(),
        incident_status,
    }))
}

/// The fix as a unified diff, for `git apply`.
#[utoipa::path(
    get, tag = "Autofix",
    operation_id = "download_autofix_patch",
    path = "/projects/{project_id}/autofix/incidents/{incident_id}/patch",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("incident_id" = String, Path, description = "Incident public id")
    ),
    responses(
        (status = 200, description = "Unified diff", content_type = "text/x-diff", body = String),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 409, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn download_patch(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Path((project_id, incident_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let patch = state.autofix.patch(project_id, &incident_id).await?;
    let file_name: String = incident_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "text/x-diff; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"autofix-{file_name}.patch\""),
            ),
        ],
        patch,
    ))
}

/// Stop an incident: cancels the agent's turn, deletes the sandbox and
/// keeps the transcript and timeline.
#[utoipa::path(
    post, tag = "Autofix",
    operation_id = "stop_autofix_incident",
    path = "/projects/{project_id}/autofix/incidents/{incident_id}/stop",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("incident_id" = String, Path, description = "Incident public id")
    ),
    responses(
        (status = 200, body = AutofixIncidentResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 409, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop_incident(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, incident_id)): Path<(i32, String)>,
) -> Result<Json<AutofixIncidentResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (incident, previous_status) = state
        .autofix
        .stop(project_id, &incident_id, auth.user_id())
        .await?;
    state
        .audit(&AutofixIncidentStoppedAudit {
            context: audit_context(&auth, &metadata),
            project_id,
            incident_id: incident.public_id.clone(),
            previous_status,
        })
        .await;
    Ok(Json(state.autofix.response(incident).await?))
}

/// Record the decision on a fix waiting for review. Closing deletes the
/// sandbox and keeps the transcript, timeline and decision.
#[utoipa::path(
    post, tag = "Autofix",
    operation_id = "close_autofix_incident",
    path = "/projects/{project_id}/autofix/incidents/{incident_id}/close",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("incident_id" = String, Path, description = "Incident public id")
    ),
    request_body = CloseIncidentRequest,
    responses(
        (status = 200, body = AutofixIncidentResponse),
        (status = 400, body = ProblemDetails),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 409, body = ProblemDetails),
        (status = 500, body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn close_incident(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AutofixState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, incident_id)): Path<(i32, String)>,
    Json(request): Json<CloseIncidentRequest>,
) -> Result<Json<AutofixIncidentResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let incident = state
        .autofix
        .close(project_id, &incident_id, auth.user_id(), &request.decision)
        .await?;
    state
        .audit(&AutofixIncidentClosedAudit {
            context: audit_context(&auth, &metadata),
            project_id,
            incident_id: incident.public_id.clone(),
            decision: request.decision.clone(),
            pr_url: incident.pr_url.clone(),
        })
        .await;
    Ok(Json(state.autofix.response(incident).await?))
}

pub fn configure_routes() -> Router<Arc<AutofixState>> {
    Router::new()
        .route("/projects/{project_id}/autofix", get(get_capability))
        .route(
            "/projects/{project_id}/autofix/settings",
            patch(update_settings),
        )
        .route("/projects/{project_id}/autofix/pause", post(set_paused))
        .route(
            "/projects/{project_id}/autofix/incidents",
            get(list_incidents).post(open_incident),
        )
        .route(
            "/projects/{project_id}/autofix/incidents/{incident_id}",
            get(get_incident),
        )
        .route(
            "/projects/{project_id}/autofix/incidents/{incident_id}/events",
            get(list_events),
        )
        .route(
            "/projects/{project_id}/autofix/incidents/{incident_id}/patch",
            get(download_patch),
        )
        .route(
            "/projects/{project_id}/autofix/incidents/{incident_id}/stop",
            post(stop_incident),
        )
        .route(
            "/projects/{project_id}/autofix/incidents/{incident_id}/close",
            post(close_incident),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_capability,
        update_settings,
        set_paused,
        list_incidents,
        open_incident,
        get_incident,
        list_events,
        download_patch,
        stop_incident,
        close_incident
    ),
    components(schemas(
        AutofixCapabilityResponse,
        AutofixSettingsResponse,
        UpdateAutofixSettingsRequest,
        PauseAutofixRequest,
        OpenIncidentRequest,
        OpenIncidentResponse,
        AutofixIncidentResponse,
        AutofixIncidentListResponse,
        AutofixEventResponse,
        AutofixEventListResponse,
        CloseIncidentRequest,
        EventDetails,
        Assessment,
        Signal,
        CommandOutcome,
        FileChange
    )),
    tags((name = "Autofix", description = "Crash autofix incidents run as workspace chats"))
)]
pub struct AutofixApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    fn status_of(error: AutofixError) -> u16 {
        Problem::from(error).status_code.as_u16()
    }

    #[test]
    fn errors_map_to_their_http_status() {
        assert_eq!(
            status_of(AutofixError::IncidentNotFound {
                incident_id: "afx_1".into(),
                project_id: 1
            }),
            404
        );
        assert_eq!(
            status_of(AutofixError::ErrorGroupNotFound {
                group_id: 2,
                project_id: 1
            }),
            404
        );
        assert_eq!(
            status_of(AutofixError::Validation {
                project_id: 1,
                message: "bad".into()
            }),
            400
        );
        assert_eq!(
            status_of(AutofixError::InvalidState {
                incident_id: "afx_1".into(),
                action: "closed",
                status: "working".into()
            }),
            409
        );
        assert_eq!(
            status_of(AutofixError::QueueFull {
                project_id: 1,
                group_id: 2,
                queued: 20
            }),
            429
        );
        assert_eq!(
            status_of(AutofixError::ProjectQueueFull {
                project_id: 1,
                group_id: 2,
                queued: 10,
                limit: 10
            }),
            429
        );
        assert_eq!(
            status_of(AutofixError::OwnerCannotAct {
                project_id: 1,
                owner_user_id: 7,
                reason: "user 7 no longer has access to project 1".into()
            }),
            403
        );
        assert_eq!(
            status_of(AutofixError::Step {
                incident_id: "afx_1".into(),
                step: "push the fix",
                reason: "boom".into()
            }),
            500
        );
    }

    #[test]
    fn problem_details_name_the_incident() {
        let problem = Problem::from(AutofixError::IncidentNotFound {
            incident_id: "afx_42".into(),
            project_id: 7,
        });
        let body = serde_json::to_string(&problem.body).unwrap_or_default();
        assert!(body.contains("afx_42"), "{body}");
        assert!(body.contains("project 7"), "{body}");
    }
}
