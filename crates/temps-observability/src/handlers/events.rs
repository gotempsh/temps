//! Unified `GET /projects/{project_id}/observe/events` endpoint.
//!
//! Returns the merged `ObservabilityEvent[]` page with each row
//! self-contained (truncated heavy fields + `*_truncated` flags). The
//! frontend renders both the list row AND its detail panel from this
//! payload alone.
//!
//! OpenAPI operation IDs are prefixed `observability_*` per the project's
//! collision convention.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::error::ObservabilityError;
use crate::filters::{clamp_limit, parse_kinds, EventFilters};
use crate::service::{FullError, FullEvent, FullRequest, ObservabilityService};
use crate::types::{ErrorRow, EventKind, ObservabilityEvent, RequestRow, RevenueRow, SpanRow};

pub struct ObservabilityState {
    pub service: Arc<ObservabilityService>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(observability_list_events, observability_full_event),
    components(schemas(
        ObservabilityEvent,
        RequestRow,
        SpanRow,
        ErrorRow,
        RevenueRow,
        EventKind,
        EventsResponse,
        FullEvent,
        FullRequest,
        FullError,
    )),
    tags(
        (name = "Observability", description = "Unified observability event stream — runtime logs, requests, spans, errors, revenue")
    )
)]
pub struct ObservabilityApiDoc;

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Comma-separated kinds: `log,request,span,error,revenue`. Empty or
    /// missing returns every kind.
    pub kinds: Option<String>,
    /// Inclusive lower bound on event timestamp (ISO 8601, `Z` suffix).
    pub from: Option<DateTime<Utc>>,
    /// Inclusive upper bound on event timestamp.
    pub to: Option<DateTime<Utc>>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    /// Free-text substring matched against per-kind summary fields
    /// (request path / error class / revenue event_type).
    pub search: Option<String>,
    /// Page size (default 50, max 200).
    pub limit: Option<u64>,
    /// When `true`, exclude bot/crawler request rows. When `false`, only
    /// include bot rows. Omitted means "include everything" (default).
    /// Only affects the `Request` kind.
    pub hide_bots: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventsResponse {
    pub events: Vec<ObservabilityEvent>,
    /// Echo of the kinds filter actually applied (server-resolved). Useful
    /// for clients that pass `kinds=` empty and want to know what they got.
    pub applied_kinds: Vec<EventKind>,
}

/// List a merged page of observability events for a project.
///
/// Each row carries everything the side panel needs to render — no
/// follow-up fetch is required for the common case. Heavy fields
/// (stacktraces, headers, span attributes) are truncated server-side and
/// expose a `*_truncated` flag; clients fetch the full row from the
/// `/full` endpoint only when the user explicitly clicks "Show full".
#[utoipa::path(
    get,
    operation_id = "observability_list_events",
    tag = "Observability",
    path = "/projects/{project_id}/observe/events",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        EventsQuery,
    ),
    responses(
        (status = 200, description = "Merged event page", body = EventsResponse),
        (status = 400, description = "Invalid filter (kinds, time range, …)", body = String),
        (status = 401, description = "Unauthorized", body = String),
        (status = 403, description = "Insufficient permissions", body = String),
        (status = 500, description = "Internal server error", body = String),
    ),
    security(("bearer_auth" = []))
)]
pub async fn observability_list_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ObservabilityState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let kinds = parse_kinds(query.kinds.as_deref())?;
    let limit = clamp_limit(query.limit);
    let filters = EventFilters {
        project_id,
        kinds: kinds.clone(),
        from: query.from,
        to: query.to,
        deployment_id: query.deployment_id,
        environment_id: query.environment_id,
        search: query.search,
        limit,
        hide_bots: query.hide_bots,
    };

    let events = state.service.query(filters).await?;

    let mut applied: Vec<EventKind> = kinds.into_iter().collect();
    applied.sort_by_key(|k| *k as u8);

    Ok((
        StatusCode::OK,
        Json(EventsResponse {
            events,
            applied_kinds: applied,
        }),
    ))
}

impl From<ObservabilityError> for Problem {
    fn from(error: ObservabilityError) -> Self {
        match error {
            ObservabilityError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            ObservabilityError::EventNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Event Not Found")
                .with_detail(error.to_string()),
            ObservabilityError::InvalidKindsFilter { .. }
            | ObservabilityError::InvalidCursor { .. }
            | ObservabilityError::InvalidTimeRange { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Request")
                    .with_detail(error.to_string())
            }
            ObservabilityError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Fetch the un-truncated form of one event by `(kind, id)`. Side panel
/// "Show full" action calls this — the list response carries truncated
/// previews + a `*_truncated` flag to let the UI decide whether to fetch.
#[utoipa::path(
    get,
    operation_id = "observability_full_event",
    tag = "Observability",
    path = "/projects/{project_id}/observe/events/{kind}/{event_id}/full",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("kind" = EventKind, Path, description = "Event kind discriminator"),
        ("event_id" = String, Path, description = "Per-kind primary key"),
    ),
    responses(
        (status = 200, description = "Full row", body = FullEvent),
        (status = 401, description = "Unauthorized", body = String),
        (status = 403, description = "Insufficient permissions", body = String),
        (status = 404, description = "Event not found in project", body = String),
        (status = 500, description = "Internal server error", body = String),
    ),
    security(("bearer_auth" = []))
)]
pub async fn observability_full_event(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ObservabilityState>>,
    Path((project_id, kind, event_id)): Path<(i32, EventKind, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let event = state
        .service
        .fetch_full(project_id, kind, &event_id)
        .await?;
    Ok((StatusCode::OK, Json(event)))
}

pub fn configure_observability_routes() -> Router<Arc<ObservabilityState>> {
    Router::new()
        .route(
            "/projects/{project_id}/observe/events",
            get(observability_list_events),
        )
        .route(
            "/projects/{project_id}/observe/events/{kind}/{event_id}/full",
            get(observability_full_event),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_mapping_is_exhaustive() {
        // Compile-time check: if a new ObservabilityError variant is added,
        // the From impl must handle it (no _ catch-all).
        let cases: Vec<ObservabilityError> = vec![
            ObservabilityError::ProjectNotFound { project_id: 1 },
            ObservabilityError::EventNotFound {
                project_id: 1,
                kind: "log".into(),
                event_id: "x".into(),
            },
            ObservabilityError::InvalidKindsFilter {
                value: "bad".into(),
            },
            ObservabilityError::InvalidCursor { reason: "x".into() },
            ObservabilityError::InvalidTimeRange {
                from: "a".into(),
                to: "b".into(),
            },
            ObservabilityError::Database(sea_orm::DbErr::Custom("x".into())),
        ];
        for err in cases {
            // Ensure no panic — Problem construction succeeds for every variant.
            let _: Problem = err.into();
        }
    }
}
