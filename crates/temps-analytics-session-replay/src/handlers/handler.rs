// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::AppState;
use crate::services::service::{
    Screen, SessionMetadata, SessionReplayError, SessionReplayInfo, SessionReplayWithEvents,
    SessionReplayWithVisitor, Viewport,
};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, RawQuery, State},
    http::{
        header::{self, HeaderMap, HeaderName},
        Method, StatusCode,
    },
    response::Json,
    routing::{get, post, put},
    Extension, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use temps_analytics::ingest_keys::{
    extract_analytics_key, resolve_client_identity, resolve_keyed_ingest_scope,
    ANALYTICS_INGEST_KEY_HEADER,
};
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use tower_http::cors::{Any, CorsLayer};
use tracing::debug;
use utoipa::{OpenApi, ToSchema};

/// OpenAPI documentation for session replay endpoints
#[derive(OpenApi)]
#[openapi(
    paths(
        get_visitor_sessions,
        get_session_replay,
        get_session_replay_events,
        update_session_duration,
        delete_session_replay,
        add_events,
        get_project_session_replays,
        init_session_replay,
        add_session_replay_events
    ),
    components(
        schemas(
            GetVisitorSessionsQuery,
            GetVisitorSessionsResponse,
            GetSessionReplayResponse,
            UpdateSessionDurationRequest,
            UpdateSessionDurationResponse,
            SessionReplayInfoDto,
            SessionEventDto,
            SessionReplayWithEventsDto,
            SessionReplayWithVisitorDto,
            ErrorResponse,
            AddEventsRequest,
            AddEventsResponse,
            GetProjectSessionReplaysQuery,
            GetProjectSessionReplaysResponse,
            SessionReplayInitRequest,
            SessionReplayInitResponse,
            SessionReplayEventsRequest
        )
    ),
    tags(
        (name = "Analytics", description = "Analytics and session replay management")
    )
)]
pub struct SessionReplayApiDoc;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GetProjectSessionReplaysQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub page: Option<u64>,
    pub per_page: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GetProjectSessionReplaysResponse {
    pub sessions: Vec<SessionReplayWithVisitorDto>,
    pub page: u64,
    pub per_page: u64,
    pub total_count: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GetVisitorSessionsQuery {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GetVisitorSessionsResponse {
    pub sessions: Vec<SessionReplayWithVisitorDto>,
    pub page: u64,
    pub per_page: u64,
    pub total_count: usize,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GetSessionReplayResponse {
    pub session: SessionReplayWithVisitorDto,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateSessionDurationRequest {
    pub duration: i32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateSessionDurationResponse {
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionReplayInfoDto {
    pub id: String,
    pub visitor_id: i32,
    pub created_at: Option<String>,
    pub user_agent: Option<String>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    pub screen_width: Option<i32>,
    pub screen_height: Option<i32>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub url: Option<String>,
    pub duration: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionEventDto {
    pub id: i32,
    pub session_id: i32,
    pub data: serde_json::Value,
    pub timestamp: i64,
    pub event_type: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionReplayWithEventsDto {
    pub session: SessionReplayWithVisitorDto,
    pub events: Vec<SessionEventDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionReplayWithVisitorDto {
    pub id: i32,
    pub session_replay_id: String,
    pub visitor_id: i32,
    pub created_at: Option<String>,
    pub user_agent: Option<String>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    pub screen_width: Option<i32>,
    pub screen_height: Option<i32>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub url: Option<String>,
    pub duration: Option<i32>,
    // Parsed user agent fields
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    // Visitor info merged
    pub visitor_uuid: String,
    pub visitor_project_id: i32,
    pub visitor_environment_id: i32,
    pub visitor_first_seen: String,
    pub visitor_last_seen: String,
    pub visitor_is_crawler: bool,
    pub visitor_crawler_name: Option<String>,
    pub visitor_custom_data: Option<serde_json::Value>,
    // Geolocation fields
    pub visitor_city: Option<String>,
    pub visitor_country: Option<String>,
    pub visitor_country_code: Option<String>,
    pub visitor_region: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    pub details: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddEventsRequest {
    pub events: String, // Base64 encoded, compressed events
    /// Client-generated id, stable across retries of the same batch. When
    /// present the append is idempotent; omitted, delivery is at-least-once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AddEventsResponse {
    pub event_count: usize,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReplayInitRequest {
    pub session_id: String,
    /// Client-generated visitor id, used only when the request carries no
    /// Temps-issued `_temps_visitor_id` cookie — i.e. Temps is used purely as
    /// an analytics backend for an app it doesn't deploy/proxy (gotempsh/temps#848).
    /// The SDK already sends this today via `getSessionMetadata()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visitor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionReplayInitResponse {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReplayEventsRequest {
    pub session_id: String,
    pub events: String, // Base64 encoded, compressed events
    /// Client-generated id, stable across retries of the same batch. When
    /// present the append is idempotent; omitted, delivery is at-least-once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

impl From<SessionReplayInfo> for SessionReplayInfoDto {
    fn from(info: SessionReplayInfo) -> Self {
        Self {
            id: info.session_replay_id,
            visitor_id: info.visitor_id,
            created_at: info.created_at.map(|dt| dt.to_rfc3339()),
            user_agent: info.user_agent,
            viewport_width: info.viewport_width,
            viewport_height: info.viewport_height,
            screen_width: info.screen_width,
            screen_height: info.screen_height,
            language: info.language,
            timezone: info.timezone,
            url: info.url,
            duration: info.duration,
        }
    }
}

impl From<SessionReplayWithEvents> for SessionReplayWithEventsDto {
    fn from(replay: SessionReplayWithEvents) -> Self {
        // Convert SessionReplayInfo to SessionReplayWithVisitor for the DTO
        let session_with_visitor = SessionReplayWithVisitor {
            id: replay.session.id,
            visitor_internal_id: replay.session.visitor.id,
            visitor_user_agent: replay.session.visitor.user_agent,
            session_replay_id: replay.session.session_replay_id,
            visitor_id: replay.session.visitor_id,
            created_at: replay.session.created_at,
            user_agent: replay.session.user_agent,
            viewport_width: replay.session.viewport_width,
            viewport_height: replay.session.viewport_height,
            screen_width: replay.session.screen_width,
            screen_height: replay.session.screen_height,
            language: replay.session.language,
            timezone: replay.session.timezone,
            url: replay.session.url,
            duration: replay.session.duration,
            // These fields would need to be fetched from DB or set to defaults
            browser: None,
            browser_version: None,
            operating_system: None,
            operating_system_version: None,
            device_type: None,
            // Visitor info from the nested visitor
            visitor_uuid: replay.session.visitor.visitor_id,
            visitor_project_id: replay.session.visitor.project_id,
            visitor_environment_id: replay.session.visitor.environment_id,
            visitor_first_seen: replay.session.visitor.first_seen,
            visitor_last_seen: replay.session.visitor.last_seen,
            visitor_is_crawler: replay.session.visitor.is_crawler,
            visitor_crawler_name: replay.session.visitor.crawler_name,
            visitor_custom_data: replay.session.visitor.custom_data,
            // Geolocation not available through this path
            visitor_city: None,
            visitor_country: None,
            visitor_country_code: None,
            visitor_region: None,
        };

        Self {
            session: session_with_visitor.into(),
            events: replay
                .events
                .into_iter()
                .map(|event| SessionEventDto {
                    id: event.id,
                    session_id: event.session_id,
                    data: event.data,
                    timestamp: event.timestamp,
                    event_type: event.event_type,
                })
                .collect(),
        }
    }
}

impl From<SessionReplayWithVisitor> for SessionReplayWithVisitorDto {
    fn from(replay: SessionReplayWithVisitor) -> Self {
        Self {
            id: replay.id,
            session_replay_id: replay.session_replay_id,
            visitor_id: replay.visitor_id,
            created_at: replay.created_at.map(|dt| dt.to_rfc3339()),
            user_agent: replay.user_agent,
            viewport_width: replay.viewport_width,
            viewport_height: replay.viewport_height,
            screen_width: replay.screen_width,
            screen_height: replay.screen_height,
            language: replay.language,
            timezone: replay.timezone,
            url: replay.url,
            duration: replay.duration,
            // Parsed user agent fields
            browser: replay.browser,
            browser_version: replay.browser_version,
            operating_system: replay.operating_system,
            operating_system_version: replay.operating_system_version,
            device_type: replay.device_type,
            // Visitor info merged
            visitor_uuid: replay.visitor_uuid,
            visitor_project_id: replay.visitor_project_id,
            visitor_environment_id: replay.visitor_environment_id,
            visitor_first_seen: replay.visitor_first_seen.to_string(),
            visitor_last_seen: replay.visitor_last_seen.to_string(),
            visitor_is_crawler: replay.visitor_is_crawler,
            visitor_crawler_name: replay.visitor_crawler_name,
            visitor_custom_data: replay.visitor_custom_data,
            // Geolocation fields
            visitor_city: replay.visitor_city,
            visitor_country: replay.visitor_country,
            visitor_country_code: replay.visitor_country_code,
            visitor_region: replay.visitor_region,
        }
    }
}

impl From<SessionReplayError> for Problem {
    fn from(error: SessionReplayError) -> Self {
        let (status, message) = match &error {
            SessionReplayError::VisitorNotFound(_) => (StatusCode::NOT_FOUND, "Visitor not found"),
            SessionReplayError::SessionNotFound(_) => (StatusCode::NOT_FOUND, "Session not found"),
            // Cross-project access attempts are surfaced as 404 to avoid
            // disclosing the existence of sessions belonging to other tenants.
            SessionReplayError::CrossProjectAccess { .. } => {
                (StatusCode::NOT_FOUND, "Session not found")
            }
            SessionReplayError::InvalidPackedData(_) => {
                (StatusCode::BAD_REQUEST, "Invalid packed data")
            }
            SessionReplayError::DecompressionError(_) => {
                (StatusCode::BAD_REQUEST, "Decompression failed")
            }
            SessionReplayError::JsonError(_) => (StatusCode::BAD_REQUEST, "Invalid JSON data"),
            SessionReplayError::Base64Error(_) => {
                (StatusCode::BAD_REQUEST, "Invalid base64 encoding")
            }
            SessionReplayError::Database(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error")
            }
            SessionReplayError::IoError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "IO error"),
            SessionReplayError::InvalidBatchId { .. } => {
                (StatusCode::BAD_REQUEST, "Invalid batch id")
            }
            SessionReplayError::BatchTooLarge { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "Session replay batch too large",
            ),
        };

        ErrorBuilder::new(status)
            .title(message)
            .detail(error.to_string())
            .build()
    }
}

/// Get session replays for a project
#[utoipa::path(
    get,
    path = "/session-replays",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("page" = Option<u64>, Query, description = "Page number (1-based)"),
        ("per_page" = Option<u64>, Query, description = "Items per page")
    ),
    responses(
        (status = 200, description = "Session replays retrieved successfully", body = GetProjectSessionReplaysResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics",
    security(("bearer_auth" = []))
)]
pub async fn get_project_session_replays(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<GetProjectSessionReplaysQuery>,
) -> Result<Json<GetProjectSessionReplaysResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, state.project_access_checker);

    debug!("Getting session replays for project: {}", query.project_id);

    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(50).min(100); // Cap at 100 items per page

    match state
        .session_replay_service
        .get_sessions_for_project(query.project_id, query.environment_id, page, per_page)
        .await
    {
        Ok((sessions, total_count)) => {
            let sessions_dto: Vec<SessionReplayWithVisitorDto> =
                sessions.into_iter().map(Into::into).collect();

            Ok(Json(GetProjectSessionReplaysResponse {
                sessions: sessions_dto,
                page,
                per_page,
                total_count,
            }))
        }
        Err(e) => Err(e.into()),
    }
}

/// Get session replays for a visitor
#[utoipa::path(
    get,
    path = "/visitors/{visitor_id}/session-replays",
    params(
        ("visitor_id" = i32, Path, description = "Visitor ID"),
        ("page" = Option<u64>, Query, description = "Page number (1-based)"),
        ("per_page" = Option<u64>, Query, description = "Items per page")
    ),
    responses(
        (status = 200, description = "Session replays retrieved successfully", body = GetVisitorSessionsResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics",
    security(("bearer_auth" = []))
)]
pub async fn get_visitor_sessions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(visitor_id): Path<i32>,
    Query(query): Query<GetVisitorSessionsQuery>,
) -> Result<Json<GetVisitorSessionsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    // No project_id is available on this route (only visitor_id) — deny
    // project-scoped deployment tokens outright rather than skip scoping.
    deny_deployment_token!(auth);
    // Resolve the project this visitor belongs to so team scoping applies:
    // without it, any holder of instance-wide AnalyticsRead could read
    // another team's visitors by walking visitor ids.
    let project_id = state
        .session_replay_service
        .project_id_for_visitor(visitor_id)
        .await?;
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!("Getting session replays for visitor: {}", visitor_id);

    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(50).min(100); // Cap at 100 items per page

    match state
        .session_replay_service
        .get_sessions_for_visitor(visitor_id, page, per_page)
        .await
    {
        Ok(sessions) => {
            let total_count = sessions.len();
            let sessions_dto: Vec<SessionReplayWithVisitorDto> =
                sessions.into_iter().map(Into::into).collect();

            Ok(Json(GetVisitorSessionsResponse {
                sessions: sessions_dto,
                page,
                per_page,
                total_count,
            }))
        }
        Err(e) => Err(e.into()),
    }
}

/// Get session replay data with visitor info (without events)
#[utoipa::path(
    get,
    path = "/visitors/{visitor_id}/session-replays/{session_id}",
    params(
        ("visitor_id" = i32, Path, description = "Visitor ID"),
        ("session_id" = i32, Path, description = "Session ID")
    ),
    responses(
        (status = 200, description = "Session replay retrieved successfully", body = GetSessionReplayResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics",
    security(("bearer_auth" = []))
)]
pub async fn get_session_replay(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, i32)>,
) -> Result<Json<GetSessionReplayResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);
    let project_id = state
        .session_replay_service
        .project_id_for_session_pk(session_id)
        .await?;
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "Getting session replay: {} for visitor: {}",
        session_id, visitor_id
    );

    match state
        .session_replay_service
        .get_session_replay_without_events(session_id)
        .await
    {
        Ok(session_replay) => Ok(Json(GetSessionReplayResponse {
            session: session_replay.into(),
        })),
        Err(e) => Err(e.into()),
    }
}

/// Get session replay events (with session and visitor metadata)
#[utoipa::path(
    get,
    path = "/visitors/{visitor_id}/session-replays/{session_id}/events",
    params(
        ("visitor_id" = i32, Path, description = "Visitor ID"),
        ("session_id" = i32, Path, description = "Session ID")
    ),
    responses(
        (status = 200, description = "Session replay with events retrieved successfully", body = SessionReplayWithEventsDto),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics",
    security(("bearer_auth" = []))
)]
pub async fn get_session_replay_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, i32)>,
) -> Result<Json<SessionReplayWithEventsDto>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);
    let project_id = state
        .session_replay_service
        .project_id_for_session_pk(session_id)
        .await?;
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "Getting session replay events: {} for visitor: {}",
        session_id, visitor_id
    );

    match state
        .session_replay_service
        .get_session_replay(session_id)
        .await
    {
        Ok(session_replay_with_events) => Ok(Json(session_replay_with_events.into())),
        Err(e) => Err(e.into()),
    }
}

/// Update session duration
#[utoipa::path(
    put,
    path = "/visitors/{visitor_id}/session-replays/{session_id}/duration",
    params(
        ("visitor_id" = i32, Path, description = "Visitor ID"),
        ("session_id" = String, Path, description = "Session ID")
    ),
    request_body = UpdateSessionDurationRequest,
    responses(
        (status = 200, description = "Session duration updated successfully", body = UpdateSessionDurationResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics",
    security(("bearer_auth" = []))
)]
pub async fn update_session_duration(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, String)>,
    Json(request): Json<UpdateSessionDurationRequest>,
) -> Result<Json<UpdateSessionDurationResponse>, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    let project_id = state
        .session_replay_service
        .project_id_for_session_replay_id(&session_id)
        .await?;
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "Updating duration for session: {} for visitor: {}",
        session_id, visitor_id
    );

    match state
        .session_replay_service
        .update_session_duration(&session_id, request.duration)
        .await
    {
        Ok(()) => Ok(Json(UpdateSessionDurationResponse {
            message: "Session duration updated successfully".to_string(),
        })),
        Err(e) => Err(e.into()),
    }
}

/// Delete a session replay
#[utoipa::path(
    delete,
    path = "/visitors/{visitor_id}/session-replays/{session_id}",
    params(
        ("visitor_id" = i32, Path, description = "Visitor ID"),
        ("session_id" = String, Path, description = "Session ID")
    ),
    responses(
        (status = 200, description = "Session replay deleted successfully"),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics",
    security(("bearer_auth" = []))
)]
pub async fn delete_session_replay(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, String)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    let project_id = state
        .session_replay_service
        .project_id_for_session_replay_id(&session_id)
        .await?;
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "Deleting session replay: {} for visitor: {}",
        session_id, visitor_id
    );

    match state
        .session_replay_service
        .delete_session_replay(&session_id)
        .await
    {
        Ok(()) => Ok(StatusCode::OK),
        Err(e) => Err(e.into()),
    }
}

/// Add events to an existing session
#[utoipa::path(
    post,
    path = "/visitors/{visitor_id}/session-replays/{session_id}/events",
    params(
        ("visitor_id" = i32, Path, description = "Visitor ID"),
        ("session_id" = String, Path, description = "Session ID")
    ),
    request_body = AddEventsRequest,
    responses(
        (status = 200, description = "Events added successfully", body = AddEventsResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics",
    security(("bearer_auth" = []))
)]
pub async fn add_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, String)>,
    Json(request): Json<AddEventsRequest>,
) -> Result<Json<AddEventsResponse>, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);

    debug!(
        "Adding events to session: {} for visitor: {}",
        session_id, visitor_id
    );

    // Resolve the project that owns this session so that add_session_events
    // can enforce the project-ownership check consistently across both the
    // public ingest path and the admin path.
    let project_id = state
        .session_replay_service
        .get_project_id_for_session(&session_id)
        .await
        .map_err(Problem::from)?;
    // Sixth handler on this route group keyed by session id rather than
    // project id — same scoping gap as the five reads, and this one writes.
    project_access_guard!(auth, project_id, state.project_access_checker);

    match state
        .session_replay_service
        .add_session_events(
            project_id,
            &session_id,
            &request.events,
            request.batch_id.as_deref(),
        )
        .await
    {
        Ok(event_count) => Ok(Json(AddEventsResponse {
            event_count,
            message: format!("Successfully added {} events", event_count),
        })),
        Err(e) => Err(e.into()),
    }
}

/// Initialize session replay with metadata
#[utoipa::path(
    post,
    path = "/_temps/session-replay/init",
    request_body = SessionReplayInitRequest,
    params(
        ("x-temps-analytics-key" = Option<String>, Header, description = "Analytics ingest key (ADR-040), `pa_` followed by 64 hex characters. An alternative to Host-based project resolution, for apps Temps does not deploy and which therefore have no route-table entry. When present it takes precedence and the Host header is not consulted for resolution; a key that does not resolve to an active row is a 401, never a fallback to Host. The value is public by design — it ships in client JS — and is write-only: it grants analytics ingest for one project (optionally one environment) and nothing else."),
        ("temps_key" = Option<String>, Query, description = "Query-string fallback for the analytics ingest key, for clients that cannot set custom headers (`navigator.sendBeacon`). Consulted only when the `x-temps-analytics-key` header is absent; identical precedence and error semantics.")
    ),
    responses(
        (status = 201, description = "Session initialized successfully", body = SessionReplayInitResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn init_session_replay(
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    Json(request): Json<SessionReplayInitRequest>,
) -> Result<(StatusCode, Json<SessionReplayInitResponse>), Problem> {
    debug!(
        "Initializing session replay for session: {}",
        request.session_id
    );

    // ADR-040 §3. Resolved before the payload checks below so a bad credential
    // is answered as such, and so an invalid key can never fall through to the
    // `Host` path. The no-key branch is untouched: when no key is presented
    // this resolves to `None` and the original ordering (visitor check, then
    // route table) stands exactly as before.
    let keyed_scope = match extract_analytics_key(&headers, raw_query.as_deref()) {
        Some(key) => {
            let origin = headers
                .get(header::ORIGIN)
                .and_then(|value| value.to_str().ok());
            Some(
                resolve_keyed_ingest_scope(
                    state.ingest_key_service.as_ref(),
                    state.ingest_rate_limiter.as_ref(),
                    &key,
                    origin,
                )
                .await?,
            )
        }
        None => None,
    };
    let is_keyed = keyed_scope.is_some();

    // Only reject the request if the client sent neither a cookie nor a
    // fallback id, which means it's an SDK version too old to generate one
    // (or, on the Host-resolved branch, that no cookie is present at all —
    // the fallback id is never trusted there; see `resolve_client_identity`).
    let visitor_id = resolve_client_identity(
        metadata.visitor_id_cookie,
        request.visitor_id.clone(),
        is_keyed,
    )
    .ok_or_else(|| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Visitor ID is required")
            .detail(
                "No _temps_visitor_id cookie and no visitorId in the request body. \
                     Update the Temps analytics SDK to a version that sends a \
                     client-generated visitorId fallback.",
            )
            .build()
    })?;

    // A resolved key replaces Host-based resolution outright (ADR-040 §3); the
    // route table is not consulted at all in that case.
    let (project_id, environment_id, deployment_id) = match keyed_scope {
        Some(scope) => {
            debug!(
                "Resolved analytics ingest key {} to project={}, env={:?}, deploy={:?}",
                scope.key_id, scope.project_id, scope.environment_id, scope.deployment_id
            );
            (scope.project_id, scope.environment_id, scope.deployment_id)
        }
        // Resolve project, environment, and deployment from route table
        None => match state.route_table.get_route(&metadata.host) {
            Some(route_info) => {
                let Some(project) = route_info.project.as_ref() else {
                    return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                        .title("No project associated with host")
                        .detail(format!(
                            "Host {} resolved but has no project (sandbox/orphan route)",
                            metadata.host
                        ))
                        .build());
                };
                let project_id = project.id;
                let environment_id = route_info.environment.as_ref().map(|e| e.id);
                let deployment_id = route_info.deployment.as_ref().map(|d| d.id);

                debug!(
                    "Resolved host {} to project={}, env={:?}, deploy={:?}",
                    metadata.host, project_id, environment_id, deployment_id
                );

                (project_id, environment_id, deployment_id)
            }
            None => {
                return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                    .title("Host not found in route table")
                    .detail(format!("Host {} not found", metadata.host))
                    .build());
            }
        },
    };

    let session_metadata = SessionMetadata {
        visitor_id,
        user_agent: request.user_agent.unwrap_or_else(|| "Unknown".to_string()),
        language: request.language.unwrap_or_else(|| "en".to_string()),
        timezone: request.timezone.unwrap_or_else(|| "UTC".to_string()),
        screen: Screen {
            width: request.screen_width.unwrap_or(1920),
            height: request.screen_height.unwrap_or(1080),
            color_depth: request.color_depth.unwrap_or(24),
        },
        viewport: Viewport {
            width: request.viewport_width.unwrap_or(1200),
            height: request.viewport_height.unwrap_or(800),
        },
        timestamp: request
            .timestamp
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        url: request.url.unwrap_or_else(|| "/".to_string()),
    };

    match state
        .session_replay_service
        .initialize_session(
            &request.session_id,
            session_metadata,
            project_id,
            environment_id,
            deployment_id,
        )
        .await
    {
        Ok(session_id) => {
            debug!("Successfully initialized session replay: {}", session_id);
            // Once-per-instance: "session replay is in use here", not "a session
            // started". Guard so it fires once, not on every replay session.
            state.telemetry.report_once(
                "session_replay_first_session",
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::SessionReplayFirstSession,
                ),
            );
            Ok((
                StatusCode::CREATED,
                Json(SessionReplayInitResponse {
                    session_id,
                    message: "Session initialized successfully".to_string(),
                }),
            ))
        }
        Err(e) => Err(e.into()),
    }
}

/// Add events to existing session replay
#[utoipa::path(
    post,
    path = "/_temps/session-replay/events",
    request_body = SessionReplayEventsRequest,
    params(
        ("x-temps-analytics-key" = Option<String>, Header, description = "Analytics ingest key (ADR-040), `pa_` followed by 64 hex characters. An alternative to Host-based project resolution, for apps Temps does not deploy and which therefore have no route-table entry. When present it takes precedence and the Host header is not consulted for resolution; a key that does not resolve to an active row is a 401, never a fallback to Host. The value is public by design — it ships in client JS — and is write-only: it grants analytics ingest for one project (optionally one environment) and nothing else."),
        ("temps_key" = Option<String>, Query, description = "Query-string fallback for the analytics ingest key, for clients that cannot set custom headers (`navigator.sendBeacon`, used to flush the final replay batch on page unload). Consulted only when the `x-temps-analytics-key` header is absent; identical precedence and error semantics.")
    ),
    responses(
        (status = 200, description = "Events added successfully", body = AddEventsResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn add_session_replay_events(
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    Json(request): Json<SessionReplayEventsRequest>,
) -> Result<Json<AddEventsResponse>, Problem> {
    debug!(
        "Adding events to session replay for session: {}",
        request.session_id
    );

    // ADR-040 §3: an ingest key resolves the owning project outright and the
    // Host header is not consulted. The binding is just as tight as the Host
    // path below — `add_session_events` still enforces that the session belongs
    // to the resolved project — so a key for project A cannot append events to
    // project B's session.
    let keyed_scope = match extract_analytics_key(&headers, raw_query.as_deref()) {
        Some(key) => {
            let origin = headers
                .get(header::ORIGIN)
                .and_then(|value| value.to_str().ok());
            Some(
                resolve_keyed_ingest_scope(
                    state.ingest_key_service.as_ref(),
                    state.ingest_rate_limiter.as_ref(),
                    &key,
                    origin,
                )
                .await?,
            )
        }
        None => None,
    };

    // Resolve the project from the Host header — the same path used by
    // init_session_replay.  This binds the event-append operation to the
    // project that owns the originating host, preventing a cross-tenant
    // attacker from injecting rrweb events into another project's session.
    let project_id = match keyed_scope {
        Some(scope) => scope.project_id,
        None => match state.route_table.get_route(&metadata.host) {
            Some(route_info) => {
                let Some(project) = route_info.project.as_ref() else {
                    return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                        .title("No project associated with host")
                        .detail(format!(
                            "Host {} resolved but has no project (sandbox/orphan route)",
                            metadata.host
                        ))
                        .build());
                };
                project.id
            }
            None => {
                return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                    .title("Host not found in route table")
                    .detail(format!("Host {} not found", metadata.host))
                    .build());
            }
        },
    };

    match state
        .session_replay_service
        .add_session_events(
            project_id,
            &request.session_id,
            &request.events,
            request.batch_id.as_deref(),
        )
        .await
    {
        Ok(event_count) => {
            debug!(
                "Successfully added {} events to session: {}",
                event_count, request.session_id
            );
            Ok(Json(AddEventsResponse {
                event_count,
                message: format!("Successfully added {} events", event_count),
            }))
        }
        Err(e) => Err(e.into()),
    }
}

/// Admin routes for session replay (dashboard queries / management).
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/session-replays", get(get_project_session_replays))
        .route(
            "/visitors/{visitor_id}/session-replays",
            get(get_visitor_sessions),
        )
        .route(
            "/visitors/{visitor_id}/session-replays/{session_id}",
            get(get_session_replay).delete(delete_session_replay),
        )
        .route(
            "/visitors/{visitor_id}/session-replays/{session_id}/events",
            get(get_session_replay_events).post(add_events),
        )
        .route(
            "/visitors/{visitor_id}/session-replays/{session_id}/duration",
            put(update_session_duration),
        )
}

/// Largest request body accepted on the unauthenticated replay ingest routes.
///
/// Caps the compressed payload before axum buffers it, so a client cannot hold
/// a worker thread by drip-feeding a huge body. The service applies a second,
/// independent cap to the *decompressed* size — this limit alone would not
/// bound that, since zlib expands roughly 1000:1 on repetitive input.
const SESSION_REPLAY_INGEST_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Public ingest routes for session replay — called directly by browser SDKs.
pub fn configure_public_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/_temps/session-replay/init", post(init_session_replay))
        .route(
            "/_temps/session-replay/events",
            post(add_session_replay_events),
        )
        .layer(DefaultBodyLimit::max(SESSION_REPLAY_INGEST_BODY_LIMIT))
        .layer(public_ingest_cors())
}

/// CORS for the public session-replay ingest routes.
///
/// Required by ADR-040: with an ingest key the request is cross-origin by
/// definition, and without this layer the browser blocks it before it ever
/// leaves the page.
///
/// `allow_credentials` stays at its default `false`, and must never be set
/// true. Key-based ingest needs no cookies by design; credentialed CORS on a
/// wildcard origin would be a real vulnerability, and browsers reject the
/// combination outright.
pub(crate) fn public_ingest_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static(ANALYTICS_INGEST_KEY_HEADER),
        ])
        .max_age(Duration::from_secs(600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::service::VisitorInfo;

    #[tokio::test]
    async fn test_session_replay_dto_conversion() {
        let session_info = SessionReplayInfo {
            id: 1,
            session_replay_id: "test-session".to_string(),
            visitor_id: 123,
            created_at: Some(chrono::Utc::now()),
            user_agent: Some("Test Agent".to_string()),
            viewport_width: Some(1200),
            viewport_height: Some(800),
            screen_width: Some(1920),
            screen_height: Some(1080),
            language: Some("en".to_string()),
            timezone: Some("UTC".to_string()),
            url: Some("https://example.com".to_string()),
            duration: Some(30000),
            // event_count: 42, // Field doesn't exist on SessionReplayInfo
            visitor: VisitorInfo {
                id: 123,
                visitor_id: "visitor123".to_string(),
                project_id: 1,
                environment_id: 1,
                first_seen: chrono::Utc::now(),
                last_seen: chrono::Utc::now(),
                user_agent: Some("Mozilla/5.0".to_string()),
                is_crawler: false,
                crawler_name: None,
                custom_data: None,
            },
        };

        let dto: SessionReplayInfoDto = session_info.into();
        assert_eq!(dto.id, "test-session");
        assert_eq!(dto.visitor_id, 123);
    }

    #[test]
    fn test_error_conversion() {
        let error = SessionReplayError::VisitorNotFound("123".to_string());
        let problem: Problem = error.into();

        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        assert_eq!(
            problem.body.get("title").and_then(|v| v.as_str()),
            Some("Visitor not found")
        );
    }

    /// CrossProjectAccess must produce HTTP 404, not 403, so that callers
    /// cannot distinguish "session does not exist" from "session belongs to
    /// another project" — preventing cross-tenant existence probing.
    #[test]
    fn cross_project_access_maps_to_404_not_403() {
        let error = SessionReplayError::CrossProjectAccess {
            session_replay_id: "some-session-id".to_string(),
            project_id: 99,
        };
        let problem: Problem = error.into();

        assert_eq!(
            problem.status_code,
            StatusCode::NOT_FOUND,
            "CrossProjectAccess must return 404 to avoid existence disclosure"
        );
        assert_eq!(
            problem.body.get("title").and_then(|v| v.as_str()),
            Some("Session not found"),
            "title must be identical to SessionNotFound title"
        );
    }

    // ── ADR-040: keyed ingest on POST /_temps/session-replay/{init,events} ──
    //
    // The no-key regression cases matter most: breaking session replay for a
    // Temps-deployed app would be worse than not shipping keyed ingest at all.

    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware;
    use base64::Engine;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use temps_analytics::ingest_keys::{AnalyticsIngestKeyService, AnalyticsIngestRateLimiter};
    use temps_database::test_utils::TestDatabase;
    use tower::ServiceExt;

    /// A syntactically valid key that is guaranteed not to exist.
    const UNKNOWN_KEY: &str = "pa_0000000000000000000000000000000000000000000000000000000000000000";

    async fn insert_db_project(
        db: &sea_orm::DatabaseConnection,
    ) -> temps_entities::projects::Model {
        temps_entities::projects::ActiveModel {
            name: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            slug: Set("test-project".to_string()),
            source_type: Set(temps_entities::source_type::SourceType::Git),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to insert test project")
    }

    /// `initialize_session` resolves the visitor row by its GUID and 404s when
    /// it is absent — pre-existing behaviour, independent of ADR-040: the
    /// browser SDK sends the `page_view` event (which upserts the visitor)
    /// before it starts a replay. Seed one so these tests exercise the
    /// resolution branch under test rather than that lookup.
    async fn insert_db_visitor(
        db: &sea_orm::DatabaseConnection,
        project_id: i32,
        visitor_id: &str,
    ) -> temps_entities::visitor::Model {
        temps_entities::visitor::ActiveModel {
            visitor_id: Set(visitor_id.to_string()),
            project_id: Set(project_id),
            // `visitor.environment_id` is still `NOT NULL` with no FK; making
            // it nullable is tracked separately (ADR-040 §4 follow-up).
            environment_id: Set(0),
            first_seen: Set(chrono::Utc::now()),
            last_seen: Set(chrono::Utc::now()),
            has_activity: Set(false),
            is_crawler: Set(false),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to insert test visitor")
    }

    /// A no-op audit logger — the public ingest routes never write audit logs,
    /// but `AppState` is shared with the admin router that does.
    struct NoopAuditLogger;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn build_state(db: Arc<sea_orm::DatabaseConnection>) -> Arc<AppState> {
        Arc::new(AppState {
            session_replay_service: Arc::new(crate::services::SessionReplayService::new(
                db.clone(),
            )),
            audit_service: Arc::new(NoopAuditLogger),
            route_table: Arc::new(temps_routes::CachedPeerTable::new(db.clone())),
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            project_access_checker: None,
            ingest_key_service: Arc::new(AnalyticsIngestKeyService::new(db.clone())),
            ingest_rate_limiter: Arc::new(AnalyticsIngestRateLimiter::new()),
        })
    }

    /// Test-only stand-in for the `_temps_visitor_id` cookie header, since this
    /// harness fabricates `RequestMetadata` from plain request headers rather
    /// than decrypting a real cookie. Never a real header name — it exists so
    /// Host-resolved-path tests can simulate "the Temps proxy already issued
    /// this visitor a cookie" without pulling in `CookieCrypto`.
    const TEST_VISITOR_ID_COOKIE_HEADER: &str = "x-test-visitor-id-cookie";

    fn test_request_metadata(headers: &HeaderMap) -> RequestMetadata {
        let host = headers
            .get(axum::http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let visitor_id_cookie = headers
            .get(TEST_VISITOR_ID_COOKIE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(|s| s.to_string());
        RequestMetadata {
            ip_address: String::new(),
            user_agent: String::new(),
            headers: headers.clone(),
            visitor_id_cookie,
            session_id_cookie: None,
            base_url: format!("http://{host}"),
            scheme: "http".to_string(),
            host,
            is_secure: false,
        }
    }

    /// The public ingest router with a middleware that fabricates the
    /// `RequestMetadata` the real server injects, deriving `host` from the
    /// request's own `Host` header so tests can exercise both branches.
    fn setup_public_app(state: Arc<AppState>) -> axum::Router {
        let metadata_middleware = middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                let metadata = test_request_metadata(req.headers());
                req.extensions_mut().insert(metadata);
                next.run(req).await
            },
        );

        configure_public_routes()
            .layer(metadata_middleware)
            .with_state(state)
    }

    fn insert_test_route(
        route_table: &temps_routes::CachedPeerTable,
        host: &str,
        project: Option<temps_entities::projects::Model>,
    ) {
        route_table.insert_route_for_test(
            host,
            temps_routes::RouteInfo {
                backend: temps_routes::BackendType::StaticDir {
                    path: "/tmp".to_string(),
                },
                redirect_to: None,
                status_code: None,
                project: project.map(Arc::new),
                environment: None,
                deployment: None,
                cert_eligible: false,
            },
        );
    }

    fn init_payload(session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "sessionId": session_id,
            "visitorId": "client-generated-visitor",
            "url": "/pricing"
        })
    }

    /// Base64(zlib(json)) — the wire format `add_session_events` expects.
    fn encoded_events() -> String {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let json =
            serde_json::json!([{ "type": 2, "timestamp": 1_700_000_000_000i64, "data": {} }])
                .to_string();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(json.as_bytes())
            .expect("zlib write must succeed");
        let compressed = encoder.finish().expect("zlib finish must succeed");
        base64::engine::general_purpose::STANDARD.encode(compressed)
    }

    async fn stored_sessions(
        db: &sea_orm::DatabaseConnection,
    ) -> Vec<temps_entities::session_replay_sessions::Model> {
        temps_entities::session_replay_sessions::Entity::find()
            .all(db)
            .await
            .expect("Failed to query session replay sessions")
    }

    #[tokio::test]
    async fn keyed_session_replay_init_resolves_the_keys_project_without_the_route_table() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        insert_db_visitor(db.as_ref(), project.id, "client-generated-visitor").await;
        let state = build_state(db.clone());

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/init")
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", &key.public_key)
                    .body(Body::from(init_payload("session-a").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let sessions = stored_sessions(db.as_ref()).await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_id, project.id);
        // The `unwrap_or(0)` sentinel is gone: no environment means NULL, not a
        // pointer at a nonexistent `environments.id = 0`.
        assert_eq!(sessions[0].environment_id, None);
        assert_eq!(sessions[0].deployment_id, None);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn keyed_session_replay_events_append_accepts_the_query_param_fallback() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        insert_db_visitor(db.as_ref(), project.id, "client-generated-visitor").await;
        let state = build_state(db.clone());

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/init")
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", &key.public_key)
                    .body(Body::from(init_payload("session-b").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/_temps/session-replay/events?temps_key={}",
                        key.public_key
                    ))
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-b",
                            "events": encoded_events(),
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["event_count"], 1);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn invalid_session_replay_key_returns_401_and_never_falls_back_to_host() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        insert_db_visitor(db.as_ref(), project.id, "client-generated-visitor").await;
        let state = build_state(db.clone());
        // The Host *would* resolve — a typo'd key must not silently use it.
        insert_test_route(&state.route_table, "app.example.test", Some(project));

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/init")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", UNKNOWN_KEY)
                    .body(Body::from(init_payload("session-c").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/events")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    // A `tk_` admin secret pasted here must never authenticate,
                    // and must never be looked up against `api_keys` either.
                    .header("x-temps-analytics-key", "tk_not_an_analytics_key")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-c",
                            "events": encoded_events(),
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        assert!(
            stored_sessions(db.as_ref()).await.is_empty(),
            "a rejected key must not fall through to Host-based resolution"
        );

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn scoped_session_replay_key_enforces_the_origin_allowlist() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        insert_db_visitor(db.as_ref(), project.id, "client-generated-visitor").await;
        let state = build_state(db.clone());

        let key = state
            .ingest_key_service
            .create(
                project.id,
                None,
                None,
                Some(vec!["https://app.example.com".to_string()]),
                None,
                None,
            )
            .await
            .expect("minting an ingest key must succeed");

        let post = |origin: Option<&'static str>, session: &'static str| {
            let app = setup_public_app(state.clone());
            let public_key = key.public_key.clone();
            async move {
                let mut builder = Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/init")
                    .header("host", "app.example.com")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", public_key);
                if let Some(origin) = origin {
                    builder = builder.header("origin", origin);
                }
                app.oneshot(
                    builder
                        .body(Body::from(init_payload(session).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };

        assert_eq!(post(None, "session-d").await, StatusCode::FORBIDDEN);
        assert_eq!(
            post(Some("https://evil.example.com"), "session-e").await,
            StatusCode::FORBIDDEN
        );
        assert!(stored_sessions(db.as_ref()).await.is_empty());

        assert_eq!(
            post(Some("https://app.example.com"), "session-f").await,
            StatusCode::CREATED
        );
        assert_eq!(stored_sessions(db.as_ref()).await.len(), 1);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn session_replay_key_over_its_rate_limit_returns_429() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        insert_db_visitor(db.as_ref(), project.id, "client-generated-visitor").await;
        let state = build_state(db.clone());

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, Some(1), None)
            .await
            .expect("minting an ingest key must succeed");

        let post = |session: &'static str| {
            let app = setup_public_app(state.clone());
            let public_key = key.public_key.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/_temps/session-replay/init")
                        .header("host", "app.example.com")
                        .header("content-type", "application/json")
                        .header("x-temps-analytics-key", public_key)
                        .body(Body::from(init_payload(session).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };

        assert_eq!(post("session-g").await, StatusCode::CREATED);
        assert_eq!(post("session-h").await, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(stored_sessions(db.as_ref()).await.len(), 1);

        test_db.cleanup().await;
    }

    /// Regression: no key at all resolves from Host exactly as before — via
    /// the Temps-issued cookie, exactly as it always did. The client-supplied
    /// `visitorId` fallback (ADR-040 §3) is keyed-path-only: a Host-resolved
    /// request without a cookie is now a 400, never a silent trust of
    /// whatever `visitorId` the request body claims (see
    /// `no_key_session_replay_init_without_a_cookie_requires_one`).
    #[tokio::test]
    async fn no_key_still_resolves_the_session_replay_scope_from_host() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        insert_db_visitor(db.as_ref(), project.id, "client-generated-visitor").await;
        let state = build_state(db.clone());
        insert_test_route(
            &state.route_table,
            "app.example.test",
            Some(project.clone()),
        );

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/init")
                    .header("host", "app.example.test")
                    .header(TEST_VISITOR_ID_COOKIE_HEADER, "client-generated-visitor")
                    .header("content-type", "application/json")
                    .body(Body::from(init_payload("session-i").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let sessions = stored_sessions(db.as_ref()).await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_id, project.id);

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/events")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-i",
                            "events": encoded_events(),
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        test_db.cleanup().await;
    }

    /// The client-supplied `visitorId` fallback (ADR-040 §3) exists only for
    /// the keyed cross-origin path. A Host-resolved request — the case every
    /// Temps-hosted app takes — must still 400 when it carries no
    /// `_temps_visitor_id` cookie, even though the payload includes a
    /// plausible-looking `visitorId`: trusting that value here would let any
    /// unauthenticated caller forge a visitor's identity on an app Temps
    /// *does* deploy just by omitting the cookie, which is exactly what the
    /// cookie's tamper-evidence is supposed to prevent.
    #[tokio::test]
    async fn no_key_session_replay_init_without_a_cookie_requires_one() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());
        insert_test_route(
            &state.route_table,
            "app.example.test",
            Some(project.clone()),
        );

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/session-replay/init")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(init_payload("session-j").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(stored_sessions(db.as_ref()).await.len(), 0);

        test_db.cleanup().await;
    }

    /// Regression: the no-key rejection shapes are unchanged — an unknown host
    /// and a sandbox/orphan route both stay 404 Problems on both routes.
    #[tokio::test]
    async fn no_key_session_replay_rejection_shapes_are_unchanged() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let state = build_state(db.clone());
        insert_test_route(&state.route_table, "orphan.example.test", None);

        for (uri, body) in [
            (
                "/_temps/session-replay/init",
                init_payload("session-j").to_string(),
            ),
            (
                "/_temps/session-replay/events",
                serde_json::json!({
                    "sessionId": "session-j",
                    "events": encoded_events(),
                })
                .to_string(),
            ),
        ] {
            for (host, expected_title) in [
                ("unknown.example.test", "Host not found in route table"),
                ("orphan.example.test", "No project associated with host"),
            ] {
                let response = setup_public_app(state.clone())
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(uri)
                            .header("host", host)
                            // Irrelevant to `/events`; lets `/init` reach the
                            // host-resolution logic this test actually
                            // targets instead of tripping the (correct,
                            // unrelated) no-cookie 400 first.
                            .header(TEST_VISITOR_ID_COOKIE_HEADER, "client-generated-visitor")
                            .header("content-type", "application/json")
                            .body(Body::from(body.clone()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{uri} with host {host}"
                );
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let problem: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    problem["title"].as_str(),
                    Some(expected_title),
                    "{uri} with host {host}"
                );
            }
        }

        assert!(stored_sessions(db.as_ref()).await.is_empty());

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn public_session_replay_routes_answer_cors_preflight_without_credentials() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let state = build_state(db.clone());

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/_temps/session-replay/init")
                    .header("host", "app.example.com")
                    .header("origin", "https://app.example.com")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "x-temps-analytics-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_success(), "{}", response.status());
        let headers = response.headers();
        assert_eq!(
            headers
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );
        assert!(
            headers.get("access-control-allow-credentials").is_none(),
            "wildcard-origin ingest must never be credentialed"
        );

        test_db.cleanup().await;
    }
}
