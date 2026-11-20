// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::audit::{AuditContext, ErrorGroupUpdatedAudit};
use super::types::AppState;
use crate::services::{ErrorEventDomain, ErrorGroupDomain, ErrorTrackingError};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Extension, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{DateTime, RequestMetadata};
use utoipa::{IntoParams, OpenApi, ToSchema};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_global_error_groups,
        list_error_groups,
        get_error_group,
        update_error_group,
        list_error_events,
        get_error_event,
        get_error_stats,
        get_error_dashboard_stats,
        get_error_time_series,
        has_error_groups,
    ),
    components(schemas(
        GlobalErrorGroupResponse,
        GlobalErrorGroupsResponse,
        GlobalErrorGroupsQuery,
        ErrorGroupResponse,
        ErrorGroupDeploymentResponse,
        ErrorEventResponse,
        ErrorGroupStatsResponse,
        ErrorDashboardStatsResponse,
        ErrorTimeSeriesDataResponse,
        ListErrorGroupsQuery,
        ListErrorEventsQuery,
        UpdateErrorGroupRequest,
        ErrorDashboardStatsQuery,
        ErrorTimeSeriesQuery,
        HasErrorGroupsResponse,
        PaginatedErrorGroupsResponse,
        PaginatedErrorEventsResponse,
        PaginationMeta,
    )),
    tags(
        (name = "error-tracking", description = "Error tracking data fetching endpoints")
    )
)]
pub struct ErrorTrackingApiDoc;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(super::feedback_handler::routes())
        .merge(super::group_workflow_handler::routes())
        .merge(super::assignee_handler::routes())
        .merge(super::activity_handler::routes())
        .merge(super::saved_search_handler::routes())
        .merge(super::performance_handler::routes())
        .merge(super::inbound_filter_handler::routes())
        .merge(super::release_handler::routes())
        .merge(super::client_outcome_handler::routes())
        .route("/error-groups", get(list_global_error_groups))
        .route(
            "/projects/{project_id}/error-groups",
            get(list_error_groups),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}",
            get(get_error_group).put(update_error_group),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/events",
            get(list_error_events),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/events/{event_id}",
            get(get_error_event),
        )
        .route("/projects/{project_id}/error-stats", get(get_error_stats))
        .route(
            "/projects/{project_id}/error-dashboard-stats",
            get(get_error_dashboard_stats),
        )
        .route(
            "/projects/{project_id}/error-time-series",
            get(get_error_time_series),
        )
        .route(
            "/projects/{project_id}/has-error-groups",
            get(has_error_groups),
        )
}

// ===== Request/Response Types =====

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ListErrorGroupsQuery {
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
    pub status: Option<String>,
    pub environment_id: Option<i32>,
    pub start_date: Option<DateTime>,
    pub end_date: Option<DateTime>,
    pub sort_by: Option<String>,
    #[serde(default = "default_sort_order")]
    pub sort_order: String,
    /// Only groups with this assignee: `me` (you, or a team you are in),
    /// `none`, `user:<id>` or `team:<id>`.
    pub assignee: Option<String>,
    /// Case-insensitive text in the title, error type or message.
    pub search: Option<String>,
    /// `error` or `performance`; omitted for both.
    pub issue_type: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ListErrorEventsQuery {
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
    /// Exact native user ID match.
    pub user_id: Option<String>,
    /// Exact tag match; supply tag_value together with this field.
    pub tag_key: Option<String>,
    pub tag_value: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateErrorGroupRequest {
    pub status: String,
    /// Deprecated: assign by email. Prefer
    /// `PUT /projects/{project_id}/error-groups/{group_id}/assignee`.
    /// - `Some("user@example.com")`: assigns the user with that email; an
    ///   email that is no user's is refused.
    /// - `Some("")` (empty string): unassigns the group.
    /// - `null` / field omitted: leaves the assignee unchanged.
    ///
    /// The status `assigned` is accepted and stored as `unresolved`.
    pub assigned_to: Option<String>,
    /// With `status: ignored`: when the group comes back. Omitted, the next
    /// event reopens it.
    pub ignore: Option<super::group_workflow_handler::IgnoreConditionsRequest>,
    /// With `status: resolved` (or `action: resolve`): resolved in the next
    /// release. Events from the project's newest release at this moment, or
    /// an older one, or with no release, keep it resolved; an event from a
    /// newer release reopens it. Fails when the project has no release yet.
    #[serde(default)]
    pub in_next_release: bool,
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ErrorTimeSeriesQuery {
    pub start_time: DateTime,
    pub end_time: DateTime,
    /// Time bucket size (e.g., "1h", "15m", "1d", "1 hour", "30 minutes")
    #[serde(default = "default_interval")]
    #[schema(example = "1h")]
    pub bucket: String,
    /// Filter chart data to a specific environment.
    /// Always AND-combined with project_id — an environment from a different project
    /// returns zero-filled buckets rather than cross-project data.
    pub environment_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ErrorDashboardStatsQuery {
    pub start_time: DateTime,
    pub end_time: DateTime,
    pub environment_id: Option<i32>,
    pub compare_to_previous: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupStatsResponse {
    pub total_groups: i64,
    pub unresolved_groups: i64,
    pub resolved_groups: i64,
    pub ignored_groups: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorTimeSeriesDataResponse {
    pub timestamp: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDashboardStatsResponse {
    pub total_errors: i64,
    pub total_errors_previous_period: i64,
    pub total_errors_change_percent: f64,
    pub error_groups: i64,
    pub error_groups_previous_period: i64,
    pub start_time: DateTime,
    pub end_time: DateTime,
    pub comparison_start_time: Option<DateTime>,
    pub comparison_end_time: Option<DateTime>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupResponse {
    pub id: i32,
    pub title: String,
    pub error_type: String,
    pub message_template: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub total_count: i32,
    pub status: String,
    /// Deprecated: a free-text assignee from before assignees were users or
    /// teams, kept read-only when it matched no user. Use `assignee`.
    pub assigned_to: Option<String>,
    /// The user or team the group is assigned to.
    pub assignee: Option<crate::services::group_workflow::GroupAssignee>,
    /// Set when this group was merged into another one. Its events and count
    /// stay with it so the merge can be undone, it is left out of lists, and
    /// its future events land in the group named here.
    pub merged_into_group_id: Option<i32>,
    /// While the group is ignored with conditions: when it comes back.
    pub ignore: Option<crate::services::types::GroupIgnoreState>,
    /// While the group is resolved in the next release: the release it was
    /// resolved after.
    pub resolution: Option<crate::services::types::GroupResolutionState>,
    /// `error`, or `performance` for an issue a trace detector filed: an
    /// N+1 query, a slow database query, consecutive HTTP calls or an
    /// oversized payload.
    pub issue_type: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub created_at: String,
    pub updated_at: String,
    /// Count of error events within the requested time window.
    /// Present only when `start_date` and `end_date` were supplied on the list request.
    pub events_in_range: Option<i64>,
    /// Count of distinct affected visitors/users within the requested time window.
    /// Present only when `start_date` and `end_date` were supplied on the list request.
    pub affected_users: Option<i64>,
    /// The deployment this group's `deployment_id` points to, resolved to its commit info.
    /// `None` when the group has no linked deployment.
    pub deployment: Option<ErrorGroupDeploymentResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupDeploymentResponse {
    pub id: i32,
    pub commit_hash: Option<String>,
    pub commit_message: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEventResponse {
    pub id: i64,
    pub error_group_id: i32,
    pub timestamp: String,
    pub created_at: String,
    /// Source of the error event (e.g., "sentry", "custom", "bugsnag")
    pub source: Option<String>,
    /// Full error event data (contains raw Sentry event or custom error data)
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HasErrorGroupsResponse {
    pub has_error_groups: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedErrorGroupsResponse {
    pub data: Vec<ErrorGroupResponse>,
    pub pagination: PaginationMeta,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginationMeta {
    pub page: u64,
    pub page_size: u64,
    pub total_count: u64,
    pub total_pages: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedErrorEventsResponse {
    pub data: Vec<ErrorEventResponse>,
    pub pagination: PaginationMeta,
}

fn default_page() -> u64 {
    1
}
fn default_page_size() -> u64 {
    20
}
fn default_sort_order() -> String {
    "desc".to_string()
}
fn default_interval() -> String {
    "1h".to_string()
}

// ===== Conversions =====

impl From<ErrorGroupDomain> for ErrorGroupResponse {
    fn from(group: ErrorGroupDomain) -> Self {
        Self {
            id: group.id,
            title: group.title,
            error_type: group.error_type,
            message_template: group.message_template,
            first_seen: group.first_seen.to_rfc3339(),
            last_seen: group.last_seen.to_rfc3339(),
            total_count: group.total_count,
            status: group.status,
            assigned_to: group.assigned_to,
            assignee: group.assignee,
            merged_into_group_id: group.merged_into_group_id,
            ignore: group.ignore,
            resolution: group.resolution,
            issue_type: group.issue_type,
            project_id: group.project_id,
            environment_id: group.environment_id,
            deployment_id: group.deployment_id,
            visitor_id: group.visitor_id,
            created_at: group.created_at.to_rfc3339(),
            updated_at: group.updated_at.to_rfc3339(),
            events_in_range: group.events_in_range,
            affected_users: group.affected_users,
            deployment: group.deployment.map(|d| ErrorGroupDeploymentResponse {
                id: d.id,
                commit_hash: d.commit_hash,
                commit_message: d.commit_message,
                branch: d.branch,
            }),
        }
    }
}

impl From<ErrorEventDomain> for ErrorEventResponse {
    fn from(event: ErrorEventDomain) -> Self {
        Self {
            id: event.id,
            error_group_id: event.error_group_id,
            timestamp: event.timestamp.to_rfc3339(),
            created_at: event.created_at.to_rfc3339(),
            source: event.source,
            data: event.data,
        }
    }
}

// ===== Error Handling =====

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    pub details: Option<String>,
}

impl From<ErrorTrackingError> for temps_core::problemdetails::Problem {
    fn from(err: ErrorTrackingError) -> Self {
        use temps_core::problemdetails;
        match err {
            ErrorTrackingError::GroupNotFound
            | ErrorTrackingError::EventNotFound
            | ErrorTrackingError::ProjectNotFound => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Not Found")
                .with_detail(err.to_string()),
            ErrorTrackingError::GroupsNotFound {
                project_id,
                ref group_ids,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/error-groups-not-found")
                .title("Error Groups Not Found")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("group_ids", group_ids.clone())
                .build(),
            ErrorTrackingError::GroupNotInProject {
                project_id,
                group_id,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/error-group-not-found")
                .title("Error Group Not Found")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("group_id", group_id)
                .build(),
            ErrorTrackingError::CommentNotFound {
                group_id,
                comment_id,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/error-group-comment-not-found")
                .title("Comment Not Found")
                .detail(err.to_string())
                .value("group_id", group_id)
                .value("comment_id", comment_id)
                .build(),
            ErrorTrackingError::NotCommentAuthor {
                group_id,
                comment_id,
                ..
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/not-comment-author")
                .title("Not The Comment's Author")
                .detail(err.to_string())
                .value("group_id", group_id)
                .value("comment_id", comment_id)
                .build(),
            ErrorTrackingError::SavedSearchNotFound {
                project_id,
                saved_search_id,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/error-saved-search-not-found")
                .title("Saved Search Not Found")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("saved_search_id", saved_search_id)
                .build(),
            ErrorTrackingError::FingerprintsNotInGroup {
                group_id,
                ref fingerprints,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/fingerprints-not-in-error-group")
                .title("Fingerprints Not In Error Group")
                .detail(err.to_string())
                .value("group_id", group_id)
                .value("fingerprints", fingerprints.clone())
                .build(),
            ErrorTrackingError::GroupAlreadyMerged {
                group_id,
                merged_into_group_id,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/error-group-already-merged")
                .title("Error Group Already Merged")
                .detail(err.to_string())
                .value("group_id", group_id)
                .value("merged_into_group_id", merged_into_group_id)
                .build(),
            ErrorTrackingError::InboundFilterNotFound {
                project_id,
                filter_id,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/error-inbound-filter-not-found")
                .title("Inbound Filter Not Found")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("filter_id", filter_id)
                .build(),
            ErrorTrackingError::NotSavedSearchOwner {
                project_id,
                saved_search_id,
                ..
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/not-saved-search-owner")
                .title("Not The Saved Search's Owner")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("saved_search_id", saved_search_id)
                .build(),
            ErrorTrackingError::SavedSearchExists {
                project_id,
                ref name,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/error-saved-search-exists")
                .title("Saved Search Already Exists")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("name", name.clone())
                .build(),
            ErrorTrackingError::SavedSearchLimit {
                project_id,
                user_id,
                limit,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .type_("https://temps.sh/probs/error-saved-search-limit")
                .title("Saved Search Limit Reached")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("user_id", user_id)
                .value("limit", limit)
                .build(),
            ErrorTrackingError::InboundFilterExists {
                project_id,
                ref kind,
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/error-inbound-filter-exists")
                .title("Inbound Filter Already Exists")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("kind", kind.clone())
                .build(),
            ErrorTrackingError::InboundFilterLimit { project_id, limit } => {
                temps_core::error_builder::ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/error-inbound-filter-limit")
                    .title("Inbound Filter Limit Reached")
                    .detail(err.to_string())
                    .value("project_id", project_id)
                    .value("limit", limit)
                    .build()
            }
            ErrorTrackingError::GroupIsMergeTarget { group_id } => {
                temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                    .type_("https://temps.sh/probs/error-group-is-merge-target")
                    .title("Error Group Is A Merge Target")
                    .detail(err.to_string())
                    .value("group_id", group_id)
                    .build()
            }
            ErrorTrackingError::GroupNotMerged { group_id } => {
                temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                    .type_("https://temps.sh/probs/error-group-not-merged")
                    .title("Error Group Is Not Merged")
                    .detail(err.to_string())
                    .value("group_id", group_id)
                    .build()
            }
            ErrorTrackingError::InvalidFingerprint | ErrorTrackingError::Validation(_) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(err.to_string())
            }
            ErrorTrackingError::AlertRuleLimitReached {
                project_id, limit, ..
            } => temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/alert-rule-limit-reached")
                .title("Alert Rule Limit Reached")
                .detail(err.to_string())
                .value("project_id", project_id)
                .value("limit", limit)
                .build(),
            ErrorTrackingError::PayloadTooLarge { .. } => {
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Payload Too Large")
                    .with_detail(err.to_string())
            }
            ErrorTrackingError::Database(_) | ErrorTrackingError::EmbeddingService(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(err.to_string())
            }
        }
    }
}

impl axum::response::IntoResponse for ErrorTrackingError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            ErrorTrackingError::Database(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            ),
            ErrorTrackingError::GroupNotFound => {
                (StatusCode::NOT_FOUND, "Error group not found".to_string())
            }
            ErrorTrackingError::NotCommentAuthor { .. }
            | ErrorTrackingError::NotSavedSearchOwner { .. } => {
                (StatusCode::FORBIDDEN, self.to_string())
            }
            ErrorTrackingError::GroupsNotFound { .. }
            | ErrorTrackingError::GroupNotInProject { .. }
            | ErrorTrackingError::CommentNotFound { .. }
            | ErrorTrackingError::SavedSearchNotFound { .. }
            | ErrorTrackingError::InboundFilterNotFound { .. }
            | ErrorTrackingError::FingerprintsNotInGroup { .. } => {
                (StatusCode::NOT_FOUND, self.to_string())
            }
            ErrorTrackingError::GroupAlreadyMerged { .. }
            | ErrorTrackingError::GroupIsMergeTarget { .. }
            | ErrorTrackingError::GroupNotMerged { .. }
            | ErrorTrackingError::InboundFilterExists { .. }
            | ErrorTrackingError::SavedSearchExists { .. } => {
                (StatusCode::CONFLICT, self.to_string())
            }
            ErrorTrackingError::InboundFilterLimit { .. }
            | ErrorTrackingError::SavedSearchLimit { .. } => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            ErrorTrackingError::EventNotFound => {
                (StatusCode::NOT_FOUND, "Error event not found".to_string())
            }
            ErrorTrackingError::InvalidFingerprint => {
                (StatusCode::BAD_REQUEST, "Invalid fingerprint".to_string())
            }
            ErrorTrackingError::EmbeddingService(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Embedding service error: {}", msg),
            ),
            ErrorTrackingError::Validation(msg) => (
                StatusCode::BAD_REQUEST,
                format!("Validation error: {}", msg),
            ),
            ErrorTrackingError::ProjectNotFound => {
                (StatusCode::NOT_FOUND, "Project not found".to_string())
            }
            ErrorTrackingError::AlertRuleLimitReached { .. } => {
                (StatusCode::CONFLICT, self.to_string())
            }
            ErrorTrackingError::PayloadTooLarge {
                field,
                limit_bytes,
                actual_bytes,
            } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Payload too large: '{}' exceeds {} bytes (got {})",
                    field, limit_bytes, actual_bytes
                ),
            ),
        };

        (
            status,
            Json(ErrorResponse {
                error: message,
                details: None,
            }),
        )
            .into_response()
    }
}

// ===== Handlers =====

/// List error groups for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups",
    responses(
        (status = 200, description = "Paginated list of error groups", body = PaginatedErrorGroupsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ListErrorGroupsQuery
    ),
    tag = "error-tracking"
)]
pub async fn list_error_groups(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<ListErrorGroupsQuery>,
) -> Result<Json<PaginatedErrorGroupsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let page = query.page;
    let page_size = query.page_size.clamp(1, 100);
    let refine = crate::services::types::GroupListRefinement {
        assignee: query
            .assignee
            .as_deref()
            .filter(|a| !a.trim().is_empty())
            .map(|a| crate::services::group_workflow::AssigneeFilter::parse(a, auth.user_id()))
            .transpose()?,
        search: query.search,
        issue_type: query.issue_type.filter(|t| !t.trim().is_empty()),
    };

    let (groups, total_count) = state
        .error_tracking_service
        .list_error_groups(
            project_id,
            Some(page),
            Some(page_size),
            query.status,
            query.environment_id,
            query.sort_by,
            Some(query.sort_order),
            query.start_date.map(|d| d.into()),
            query.end_date.map(|d| d.into()),
            &refine,
        )
        .await?;

    let total_pages = if total_count > 0 {
        (total_count as f64 / page_size as f64).ceil() as u64
    } else {
        0
    };

    Ok(Json(PaginatedErrorGroupsResponse {
        data: groups.into_iter().map(ErrorGroupResponse::from).collect(),
        pagination: PaginationMeta {
            page,
            page_size,
            total_count,
            total_pages,
        },
    }))
}

/// Get a specific error group
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}",
    responses(
        (status = 200, description = "Error group details", body = ErrorGroupResponse),
        (status = 404, description = "Error group not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking"
)]
pub async fn get_error_group(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
) -> Result<Json<ErrorGroupResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let group = state
        .error_tracking_service
        .get_error_group(group_id, project_id)
        .await?;

    Ok(Json(ErrorGroupResponse::from(group)))
}

/// Update error group status
#[utoipa::path(
    put,
    path = "/projects/{project_id}/error-groups/{group_id}",
    request_body = UpdateErrorGroupRequest,
    responses(
        (status = 200, description = "Error group updated successfully"),
        (status = 404, description = "Error group not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking"
)]
pub async fn update_error_group(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateErrorGroupRequest>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    // Legacy assignee by email (ADR 111, D1.6): resolved to a user and set
    // through the assignee route, so it is checked and audited the same way.
    if let Some(assigned_to) = request.assigned_to.as_deref() {
        let assignee = if assigned_to.trim().is_empty() {
            None
        } else {
            Some(
                state
                    .error_tracking_service
                    .workflow
                    .user_by_email(assigned_to)
                    .await?,
            )
        };
        super::assignee_handler::assign_and_audit(
            &state, &auth, &metadata, project_id, group_id, assignee,
        )
        .await?;
    }
    state
        .error_tracking_service
        .update_error_group_status(
            group_id,
            project_id,
            request.status.clone(),
            request.ignore.clone().map(Into::into),
            request.in_next_release,
        )
        .await?;
    super::activity_handler::record_activity(
        &state,
        project_id,
        &[group_id],
        crate::services::group_workflow::ActivityKind::StatusChanged,
        auth.user_id_opt(),
        crate::services::group_workflow::ActivityDetails {
            status: crate::services::error_crud_service::canonical_group_status(&request.status)
                .ok()
                .map(str::to_string),
            in_next_release: request.in_next_release,
            ..Default::default()
        },
    )
    .await;

    let audit = ErrorGroupUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        group_id,
        status: request.status,
        assigned_to: request.assigned_to,
        ignore: request.ignore,
        in_next_release: request.in_next_release,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            group_id,
            "Failed to create error-group update audit log: {}",
            e
        );
    }

    Ok(StatusCode::OK)
}

/// List error events for a specific group
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/events",
    responses(
        (status = 200, description = "Paginated list of error events", body = PaginatedErrorEventsResponse),
        (status = 400, description = "Invalid metadata filters"),
        (status = 404, description = "Error group not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ListErrorEventsQuery
    ),
    tag = "error-tracking"
)]
pub async fn list_error_events(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Query(query): Query<ListErrorEventsQuery>,
) -> Result<Json<PaginatedErrorEventsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 100);

    let (events, total_count) = state
        .error_tracking_service
        .list_error_events_filtered(
            group_id,
            project_id,
            Some(page),
            Some(page_size),
            &crate::services::ErrorEventFilter {
                user_id: query.user_id,
                tag_key: query.tag_key,
                tag_value: query.tag_value,
            },
        )
        .await?;

    let total_pages = if total_count > 0 {
        (total_count as f64 / page_size as f64).ceil() as u64
    } else {
        0
    };

    Ok(Json(PaginatedErrorEventsResponse {
        data: events.into_iter().map(ErrorEventResponse::from).collect(),
        pagination: PaginationMeta {
            page,
            page_size,
            total_count,
            total_pages,
        },
    }))
}

/// Get a specific error event
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/events/{event_id}",
    responses(
        (status = 200, description = "Error event details", body = ErrorEventResponse),
        (status = 404, description = "Event not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ("event_id" = i64, Path, description = "Error event ID")
    ),
    tag = "error-tracking"
)]
pub async fn get_error_event(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id, event_id)): Path<(i32, i32, i64)>,
) -> Result<Json<ErrorEventResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let event = state
        .error_tracking_service
        .get_error_event(event_id, group_id, project_id)
        .await?;

    Ok(Json(ErrorEventResponse::from(event)))
}

/// Get error statistics for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-stats",
    responses(
        (status = 200, description = "Error statistics", body = ErrorGroupStatsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "error-tracking"
)]
pub async fn get_error_stats(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<ErrorGroupStatsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let stats = state
        .error_tracking_service
        .get_error_stats(project_id, None)
        .await?;

    Ok(Json(ErrorGroupStatsResponse {
        total_groups: stats.total_groups,
        unresolved_groups: stats.unresolved_groups,
        resolved_groups: stats.resolved_groups,
        ignored_groups: stats.ignored_groups,
    }))
}

/// Get error dashboard statistics
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-dashboard-stats",
    responses(
        (status = 200, description = "Error dashboard statistics", body = ErrorDashboardStatsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ErrorDashboardStatsQuery
    ),
    tag = "error-tracking"
)]
pub async fn get_error_dashboard_stats(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<ErrorDashboardStatsQuery>,
) -> Result<Json<ErrorDashboardStatsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let stats = state
        .error_tracking_service
        .get_dashboard_stats(
            project_id,
            query.start_time.into(),
            query.end_time.into(),
            query.environment_id,
            query.compare_to_previous.unwrap_or(true),
        )
        .await?;

    Ok(Json(ErrorDashboardStatsResponse {
        total_errors: stats.total_errors,
        total_errors_previous_period: stats.total_errors_previous_period,
        total_errors_change_percent: stats.total_errors_change_percent,
        error_groups: stats.error_groups,
        error_groups_previous_period: stats.error_groups_previous_period,
        start_time: query.start_time,
        end_time: query.end_time,
        comparison_start_time: stats.comparison_start_time.map(|dt| dt.into()),
        comparison_end_time: stats.comparison_end_time.map(|dt| dt.into()),
    }))
}

/// Get error time series data for charts
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-time-series",
    responses(
        (status = 200, description = "Error time series data", body = Vec<ErrorTimeSeriesDataResponse>),
        (status = 400, description = "Range wider than 366 days, too many points, or a bucket that can't be read or is too wide"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ErrorTimeSeriesQuery
    ),
    tag = "error-tracking"
)]
pub async fn get_error_time_series(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<ErrorTimeSeriesQuery>,
) -> Result<Json<Vec<ErrorTimeSeriesDataResponse>>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let data = state
        .error_tracking_service
        .get_error_time_series(
            project_id,
            query.start_time.into(),
            query.end_time.into(),
            &query.bucket,
            query.environment_id,
        )
        .await?;

    let response: Vec<ErrorTimeSeriesDataResponse> = data
        .into_iter()
        .map(|item| ErrorTimeSeriesDataResponse {
            timestamp: item.timestamp.to_rfc3339(),
            count: item.count,
        })
        .collect();

    Ok(Json(response))
}

/// Check if project has any error groups
#[utoipa::path(
    get,
    path = "/projects/{project_id}/has-error-groups",
    responses(
        (status = 200, description = "Error groups existence check", body = HasErrorGroupsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "error-tracking"
)]
pub async fn has_error_groups(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<HasErrorGroupsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let has_error_groups = state
        .error_tracking_service
        .has_error_groups(project_id)
        .await?;

    Ok(Json(HasErrorGroupsResponse { has_error_groups }))
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct GlobalErrorGroupsQuery {
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
    pub project_id: Option<i32>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub start_date: Option<DateTime>,
    pub end_date: Option<DateTime>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalErrorGroupResponse {
    pub id: i32,
    pub title: String,
    pub error_type: String,
    pub status: String,
    pub assigned_to: Option<String>,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_name: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub total_count: i64,
    pub events_in_range: i64,
    pub affected_users: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalErrorGroupsResponse {
    pub data: Vec<GlobalErrorGroupResponse>,
    pub pagination: PaginationMeta,
}

/// List issues across every project visible to the caller.
#[utoipa::path(get, path = "/error-groups", params(GlobalErrorGroupsQuery),
    responses((status = 200, body = GlobalErrorGroupsResponse, description = "Visible errors across projects"),
              (status = 400, description = "Invalid filters"), (status = 403, description = "Access denied")),
    tag = "error-tracking")]
pub async fn list_global_error_groups(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Query(query): Query<GlobalErrorGroupsQuery>,
) -> Result<Json<GlobalErrorGroupsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    if let Some(project_id) = query.project_id {
        project_scope_guard!(auth, project_id);
        project_access_guard!(auth, project_id, state.project_access_checker);
    }
    let project_id = auth.project_id().or(query.project_id);
    let mut hidden = Vec::new();
    if !auth.is_deployment_token() && !auth.is_instance_admin() {
        if let Some(checker) = state.project_access_checker.as_ref() {
            let user_id = auth.user_id_opt().ok_or_else(|| {
                temps_core::error_builder::forbidden()
                    .title("Project access denied")
                    .detail("Could not resolve the caller for the global errors list")
                    .build()
            })?;
            hidden = checker.hidden_project_ids(user_id).await.map_err(|error| {
                tracing::error!(user_id, %error, "Could not check project access for global errors");
                temps_core::error_builder::internal_server_error().title("Project access check failed").detail("Could not verify access to the global errors list").build()
            })?.unwrap_or_default();
        }
    }
    let end: chrono::DateTime<chrono::Utc> = query
        .end_date
        .map(Into::into)
        .unwrap_or_else(chrono::Utc::now);
    let start: chrono::DateTime<chrono::Utc> = query
        .start_date
        .map(Into::into)
        .unwrap_or(end - chrono::Duration::days(1));
    if query.page == 0
        || query.page_size == 0
        || query.page > i64::MAX as u64 / 100
        || start >= end
        || end - start > chrono::Duration::days(90)
        || query
            .status
            .as_deref()
            .is_some_and(|s| !["unresolved", "resolved", "ignored"].contains(&s))
        || query.search.as_ref().is_some_and(|s| s.len() > 500)
    {
        return Err(temps_core::error_builder::bad_request().title("Invalid error filters").detail("Use positive pagination, a supported status, a search up to 500 bytes and a time window up to 90 days").build());
    }
    let page_size = query.page_size.min(100);
    let (groups, total_count) = state
        .error_tracking_service
        .list_global_error_groups(
            project_id,
            &hidden,
            query.page,
            page_size,
            query.status.as_deref(),
            query.search.as_deref(),
            start,
            end,
        )
        .await?;
    Ok(Json(GlobalErrorGroupsResponse {
        data: groups
            .into_iter()
            .map(|g| GlobalErrorGroupResponse {
                id: g.id,
                title: g.title,
                error_type: g.error_type,
                status: g.status,
                assigned_to: g.assigned_to,
                project_id: g.project_id,
                project_name: g.project_name,
                project_slug: g.project_slug,
                environment_name: g.environment_name,
                first_seen: g
                    .first_seen
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
                last_seen: g
                    .last_seen
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
                total_count: g.total_count,
                events_in_range: g.events_in_range,
                affected_users: g.affected_users,
            })
            .collect(),
        pagination: PaginationMeta {
            page: query.page,
            page_size,
            total_count,
            total_pages: total_count.div_ceil(page_size),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::error_alert_service::ErrorAlertService;
    use crate::services::error_tracking_service::ErrorTrackingService;
    use async_trait::async_trait;
    use axum::http::HeaderMap;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use std::sync::Mutex;
    use temps_authz::{AuthContext, Role};
    use temps_entities::{error_groups, projects, users};
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    /// Audit logger that records every operation it receives so tests can
    /// assert exactly which audit events a handler emitted.
    #[derive(Default)]
    struct RecordingAuditLogger {
        entries: Mutex<Vec<(String, Option<i32>, serde_json::Value)>>,
    }

    #[async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            let payload: serde_json::Value = serde_json::from_str(&operation.serialize()?)
                .expect("audit operation serializes to valid JSON");
            self.entries
                .lock()
                .map_err(|_| {
                    temps_core::AuditError::write(
                        operation.operation_type(),
                        "recording audit logger lock poisoned",
                    )
                })?
                .push((operation.operation_type(), operation.user_id(), payload));
            Ok(())
        }
    }

    fn test_user(id: i32) -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id,
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

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "audit-test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.test".to_string(),
            is_secure: true,
        }
    }

    async fn seed_group(db: &sea_orm::DatabaseConnection) -> (i32, i32) {
        let now = chrono::Utc::now();
        let project = projects::ActiveModel {
            name: Set("Audit Project".to_string()),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("audit-project-{}", uuid::Uuid::new_v4())),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert project");
        let group = error_groups::ActiveModel {
            title: Set("TypeError: x is undefined".to_string()),
            error_type: Set("TypeError".to_string()),
            message_template: Set(None),
            embedding: Set(None),
            first_seen: Set(now),
            last_seen: Set(now),
            total_count: Set(1),
            status: Set("unresolved".to_string()),
            assigned_to: Set(None),
            project_id: Set(project.id),
            environment_id: Set(None),
            deployment_id: Set(None),
            visitor_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert error group");
        (project.id, group.id)
    }

    #[tokio::test]
    async fn update_error_group_emits_audit_event_only_on_success() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping error-group audit test: {error}");
                return;
            }
            Err(error) => panic!("error-group audit test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let (project_id, group_id) = seed_group(db.as_ref()).await;

        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = Arc::new(AppState {
            error_tracking_service: Arc::new(ErrorTrackingService::new(db.clone())),
            alert_service: Arc::new(ErrorAlertService::new(db.clone())),
            audit_service: recorder.clone(),
            project_access_checker: None,
        });
        let auth = AuthContext::new_session(test_user(5), Role::Admin);

        let status = update_error_group(
            State(state.clone()),
            RequireAuth(auth.clone()),
            Path((project_id, group_id)),
            Extension(metadata()),
            Json(UpdateErrorGroupRequest {
                ignore: None,
                in_next_release: false,
                status: "resolved".to_string(),
                assigned_to: Some(String::new()),
            }),
        )
        .await
        .expect("update succeeds");
        assert_eq!(status, StatusCode::OK);

        let group = error_groups::Entity::find_by_id(group_id)
            .one(db.as_ref())
            .await
            .expect("load group")
            .expect("group exists");
        assert_eq!(group.status, "resolved");

        {
            let entries = recorder.entries.lock().expect("lock");
            // The legacy assignee field is applied through the assignee
            // route, which audits it on its own.
            assert_eq!(entries.len(), 2, "the assignment, then the update");
            assert_eq!(entries[0].0, "ERROR_GROUP_ASSIGNED");
            let (op, user_id, payload) = &entries[1];
            assert_eq!(op, "ERROR_GROUP_UPDATED");
            assert_eq!(*user_id, Some(5));
            assert_eq!(payload["project_id"], project_id);
            assert_eq!(payload["group_id"], group_id);
            assert_eq!(payload["status"], "resolved");
            assert_eq!(payload["assigned_to"], "");
            assert_eq!(payload["context"]["ip_address"], "203.0.113.9");
        }

        // An email that is no user's is refused before anything is written.
        let unknown = update_error_group(
            State(state.clone()),
            RequireAuth(auth.clone()),
            Path((project_id, group_id)),
            Extension(metadata()),
            Json(UpdateErrorGroupRequest {
                ignore: None,
                in_next_release: false,
                status: "unresolved".to_string(),
                assigned_to: Some("nobody@example.com".to_string()),
            }),
        )
        .await
        .expect_err("an unknown email is refused");
        assert_eq!(
            axum::response::IntoResponse::into_response(unknown).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(recorder.entries.lock().expect("lock").len(), 2);

        // A group from another project is not found: no write, no audit event.
        let missing = update_error_group(
            State(state),
            RequireAuth(auth),
            Path((project_id + 10_000, group_id)),
            Extension(metadata()),
            Json(UpdateErrorGroupRequest {
                ignore: None,
                in_next_release: false,
                status: "ignored".to_string(),
                assigned_to: None,
            }),
        )
        .await;
        assert!(missing.is_err(), "cross-project update must fail");
        assert_eq!(
            recorder.entries.lock().expect("lock").len(),
            2,
            "a failed update must not emit an audit event"
        );
    }
}
