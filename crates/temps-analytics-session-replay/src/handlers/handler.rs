use super::types::AppState;
use crate::services::service::{
    Screen, SessionMetadata, SessionReplayError, SessionReplayInfo, SessionReplayWithEvents,
    SessionReplayWithVisitor, Viewport,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Extension, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use tracing::info;
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
    pub visitor_custom_data: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    pub details: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AddEventsRequest {
    pub events: String, // Base64 encoded, compressed events
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
        }
    }
}

impl From<SessionReplayError> for Problem {
    fn from(error: SessionReplayError) -> Self {
        let (status, message) = match &error {
            SessionReplayError::VisitorNotFound(_) => (StatusCode::NOT_FOUND, "Visitor not found"),
            SessionReplayError::SessionNotFound(_) => (StatusCode::NOT_FOUND, "Session not found"),
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
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn get_project_session_replays(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GetProjectSessionReplaysQuery>,
) -> Result<Json<GetProjectSessionReplaysResponse>, Problem> {
    info!("Getting session replays for project: {}", query.project_id);

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
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn get_visitor_sessions(
    State(state): State<Arc<AppState>>,
    Path(visitor_id): Path<i32>,
    Query(query): Query<GetVisitorSessionsQuery>,
) -> Result<Json<GetVisitorSessionsResponse>, Problem> {
    info!("Getting session replays for visitor: {}", visitor_id);

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
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn get_session_replay(
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, i32)>,
) -> Result<Json<GetSessionReplayResponse>, Problem> {
    info!(
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
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn get_session_replay_events(
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, i32)>,
) -> Result<Json<SessionReplayWithEventsDto>, Problem> {
    info!(
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
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn update_session_duration(
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, String)>,
    Json(request): Json<UpdateSessionDurationRequest>,
) -> Result<Json<UpdateSessionDurationResponse>, Problem> {
    info!(
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
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn delete_session_replay(
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, String)>,
) -> Result<StatusCode, Problem> {
    info!(
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
        (status = 404, description = "Session not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "Analytics"
)]
pub async fn add_events(
    State(state): State<Arc<AppState>>,
    Path((visitor_id, session_id)): Path<(i32, String)>,
    Json(request): Json<AddEventsRequest>,
) -> Result<Json<AddEventsResponse>, Problem> {
    info!(
        "Adding events to session: {} for visitor: {}",
        session_id, visitor_id
    );

    match state
        .session_replay_service
        .add_session_events(&session_id, &request.events)
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
    Json(request): Json<SessionReplayInitRequest>,
) -> Result<(StatusCode, Json<SessionReplayInitResponse>), Problem> {
    info!(
        "Initializing session replay for session: {}",
        request.session_id
    );

    let visitor_id = metadata.visitor_id_cookie.ok_or_else(|| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Visitor ID is required")
            .build()
    })?;

    // Resolve project, environment, and deployment from route table
    let (project_id, environment_id, deployment_id) =
        match state.route_table.get_route(&metadata.host) {
            Some(route_info) => {
                let project_id = route_info.project.as_ref().map(|p| p.id).unwrap_or(1);
                let environment_id = route_info.environment.as_ref().map(|e| e.id);
                let deployment_id = route_info.deployment.as_ref().map(|d| d.id);

                info!(
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
            info!("Successfully initialized session replay: {}", session_id);
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
    Json(request): Json<SessionReplayEventsRequest>,
) -> Result<Json<AddEventsResponse>, Problem> {
    info!(
        "Adding events to session replay for session: {}",
        request.session_id
    );

    match state
        .session_replay_service
        .add_session_events(&request.session_id, &request.events)
        .await
    {
        Ok(event_count) => {
            info!(
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
            post(update_session_duration),
        )
        .route("/_temps/session-replay/init", post(init_session_replay))
        .route(
            "/_temps/session-replay/events",
            post(add_session_replay_events),
        )
}

// #[cfg(test)]
// mod session_replay_tests;

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
}
