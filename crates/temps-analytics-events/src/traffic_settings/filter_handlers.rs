// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for per-project analytics traffic filters (referrer, IP/CIDR,
//! network and country bans) and the filtered-traffic summary that lets the
//! user see what was kept out and why.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::filters::{
    FilteredTrafficReason, FilteredTrafficSummary, NewTrafficFilter, TrafficFilter,
    TrafficFilterError, TrafficFilterKind, MAX_FILTERS_PER_KIND,
};
use super::handlers::TrafficSettingsAppState;

/// Default window of the filtered-traffic summary.
const DEFAULT_FILTERED_TRAFFIC_DAYS: u32 = 7;

fn iso8601(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A traffic filter.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct TrafficFilterResponse {
    pub id: i32,
    pub project_id: i32,
    pub kind: TrafficFilterKind,
    /// Normalised value: a domain (matches its subdomains too), an IP address
    /// or CIDR range, a network (ASN) number, or an ISO country code.
    pub value: String,
    pub note: Option<String>,
    pub created_by: Option<i32>,
    /// ISO 8601.
    pub created_at: String,
}

impl From<TrafficFilter> for TrafficFilterResponse {
    fn from(filter: TrafficFilter) -> Self {
        Self {
            id: filter.id,
            project_id: filter.project_id,
            kind: filter.kind,
            value: filter.value,
            note: filter.note,
            created_by: filter.created_by,
            created_at: iso8601(filter.created_at),
        }
    }
}

/// A page of a project's traffic filters, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct TrafficFilterListResponse {
    pub filters: Vec<TrafficFilterResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    /// Most filters a project may have of each kind.
    pub max_filters_per_kind: u64,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListTrafficFiltersQuery {
    /// Only filters of this kind.
    pub kind: Option<TrafficFilterKind>,
    /// Page number, starting at 1 (default 1).
    pub page: Option<u64>,
    /// Page size (default 20, max 100).
    pub page_size: Option<u64>,
}

/// A traffic filter to add.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTrafficFilterRequest {
    pub kind: TrafficFilterKind,
    /// A domain or URL (`referrer`), an IP address or CIDR range (`ip`), a
    /// network number with or without the `AS` prefix (`asn`), or a two-letter
    /// country code (`country`). Normalised before it is stored.
    pub value: String,
    /// Why the filter exists, for whoever reviews it later.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FilteredTrafficQuery {
    /// Window in days, ending now (default 7, max 30).
    pub days: Option<u32>,
}

/// Events kept out of the project's analytics for one reason.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct FilteredTrafficReasonCount {
    pub reason: FilteredTrafficReason,
    pub events: i64,
}

/// What was counted and what was filtered over a window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct FilteredTrafficResponse {
    pub project_id: i32,
    pub days: u32,
    /// Start of the window (ISO 8601).
    pub since: String,
    /// Events that count towards the project's analytics.
    pub counted_events: i64,
    /// Events kept out of the project's analytics, all reasons.
    pub filtered_events: i64,
    /// Filtered events per reason, largest first.
    pub reasons: Vec<FilteredTrafficReasonCount>,
}

impl From<FilteredTrafficSummary> for FilteredTrafficResponse {
    fn from(summary: FilteredTrafficSummary) -> Self {
        Self {
            project_id: summary.project_id,
            days: summary.days,
            since: iso8601(summary.since),
            counted_events: summary.counted_events,
            filtered_events: summary.filtered.iter().map(|count| count.events).sum(),
            reasons: summary
                .filtered
                .into_iter()
                .map(|count| FilteredTrafficReasonCount {
                    reason: count.reason,
                    events: count.events,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficFilterAudit {
    #[serde(skip)]
    operation: &'static str,
    pub context: AuditContext,
    pub project_id: i32,
    pub filter_id: i32,
    pub kind: TrafficFilterKind,
    pub value: String,
    pub note: Option<String>,
}

impl TrafficFilterAudit {
    fn new(operation: &'static str, context: AuditContext, filter: &TrafficFilter) -> Self {
        Self {
            operation,
            context,
            project_id: filter.project_id,
            filter_id: filter.id,
            kind: filter.kind,
            value: filter.value.clone(),
            note: filter.note.clone(),
        }
    }
}

impl AuditOperation for TrafficFilterAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize {} audit for traffic filter {} in project {}: {}",
                self.operation,
                self.filter_id,
                self.project_id,
                e
            )
        })
    }
}

impl From<TrafficFilterError> for Problem {
    fn from(error: TrafficFilterError) -> Self {
        match error {
            TrafficFilterError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            TrafficFilterError::FilterNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Traffic Filter Not Found")
                .with_detail(error.to_string()),
            TrafficFilterError::InvalidValue { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Traffic Filter")
                .with_detail(error.to_string()),
            TrafficFilterError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            TrafficFilterError::LimitReached { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Traffic Filter Limit Reached")
                .with_detail(error.to_string()),
            TrafficFilterError::Duplicate { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Traffic Filter Already Exists")
                .with_detail(error.to_string()),
            // The underlying error stays in the server log: a database error
            // can name tables and constraints, and a corrupt row's reason
            // quotes stored data.
            TrafficFilterError::CorruptRow {
                filter_id,
                project_id,
                ..
            } => {
                error!("{error}");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(format!(
                        "Traffic filter {filter_id} in project {project_id} could not be read; \
                         see the server log for details"
                    ))
            }
            TrafficFilterError::Database {
                project_id,
                operation,
                ..
            } => {
                error!("{error}");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(format!(
                        "A database error occurred while {operation} traffic filters for \
                         project {project_id}; see the server log for details"
                    ))
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_traffic_filters,
        create_traffic_filter,
        delete_traffic_filter,
        get_filtered_traffic
    ),
    components(schemas(
        TrafficFilterKind,
        TrafficFilterResponse,
        TrafficFilterListResponse,
        CreateTrafficFilterRequest,
        FilteredTrafficReason,
        FilteredTrafficReasonCount,
        FilteredTrafficResponse,
    )),
    tags(
        (name = "Analytics Settings", description = "Per-project analytics traffic settings")
    )
)]
pub struct TrafficFiltersApiDoc;

pub fn configure_traffic_filter_routes() -> Router<Arc<TrafficSettingsAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/analytics/traffic-filters",
            get(list_traffic_filters).post(create_traffic_filter),
        )
        .route(
            "/projects/{project_id}/analytics/traffic-filters/{filter_id}",
            delete(delete_traffic_filter),
        )
        .route(
            "/projects/{project_id}/analytics/filtered-traffic",
            get(get_filtered_traffic),
        )
}

/// List a project's traffic filters, newest first.
#[utoipa::path(
    tag = "Analytics Settings",
    get,
    path = "/projects/{project_id}/analytics/traffic-filters",
    operation_id = "list_traffic_filters",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ListTrafficFiltersQuery
    ),
    responses(
        (status = 200, description = "Traffic filters", body = TrafficFilterListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_traffic_filters(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrafficSettingsAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListTrafficFiltersQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let (filters, total) = state
        .filter_service
        .list(project_id, query.kind, Some(page), Some(page_size))
        .await?;

    Ok(Json(TrafficFilterListResponse {
        filters: filters.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
        max_filters_per_kind: MAX_FILTERS_PER_KIND,
    }))
}

/// Add a traffic filter. Matching traffic is kept out of the project's
/// analytics from now on; already-recorded events are not changed.
#[utoipa::path(
    tag = "Analytics Settings",
    post,
    path = "/projects/{project_id}/analytics/traffic-filters",
    operation_id = "create_traffic_filter",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateTrafficFilterRequest,
    responses(
        (status = 201, description = "Traffic filter created", body = TrafficFilterResponse),
        (status = 400, description = "Invalid value, or the per-kind limit is reached"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 409, description = "The same filter already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn create_traffic_filter(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrafficSettingsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateTrafficFilterRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let filter = state
        .filter_service
        .create(
            project_id,
            NewTrafficFilter {
                kind: request.kind,
                value: request.value,
                note: request.note,
            },
            Some(auth.user_id()),
        )
        .await?;

    let audit = TrafficFilterAudit::new(
        "ANALYTICS_TRAFFIC_FILTER_CREATED",
        AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        &filter,
    );
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for traffic filter {} in project {}: {}",
            filter.id, project_id, e
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(TrafficFilterResponse::from(filter)),
    ))
}

/// Remove a traffic filter. Matching traffic counts again from now on; events
/// it already excluded stay excluded.
#[utoipa::path(
    tag = "Analytics Settings",
    delete,
    path = "/projects/{project_id}/analytics/traffic-filters/{filter_id}",
    operation_id = "delete_traffic_filter",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("filter_id" = i32, Path, description = "Traffic filter ID")
    ),
    responses(
        (status = 204, description = "Traffic filter removed"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project or traffic filter not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn delete_traffic_filter(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrafficSettingsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, filter_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let filter = state.filter_service.delete(project_id, filter_id).await?;

    let audit = TrafficFilterAudit::new(
        "ANALYTICS_TRAFFIC_FILTER_DELETED",
        AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        &filter,
    );
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for deleting traffic filter {} in project {}: {}",
            filter_id, project_id, e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}

/// How many events were counted and how many were kept out of the project's
/// analytics, by reason, over the last `days` days.
#[utoipa::path(
    tag = "Analytics Settings",
    get,
    path = "/projects/{project_id}/analytics/filtered-traffic",
    operation_id = "get_filtered_traffic",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        FilteredTrafficQuery
    ),
    responses(
        (status = 200, description = "Filtered traffic summary", body = FilteredTrafficResponse),
        (status = 400, description = "Window out of range"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_filtered_traffic(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrafficSettingsAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<FilteredTrafficQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let days = query.days.unwrap_or(DEFAULT_FILTERED_TRAFFIC_DAYS);
    let summary = state
        .filter_service
        .filtered_traffic(project_id, days)
        .await?;
    Ok(Json(FilteredTrafficResponse::from(summary)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traffic_settings::filters::tests::lock_acquired;
    use crate::traffic_settings::handlers::tests::{auth, metadata, state_with};
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;
    use temps_auth::permissions::Role;
    use temps_entities::analytics_traffic_filters;

    fn detail(problem: &Problem) -> String {
        problem
            .body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn internal_errors_keep_context_but_not_the_database_error() {
        let problem = Problem::from(TrafficFilterError::Database {
            project_id: 7,
            operation: "loading",
            source: sea_orm::DbErr::Custom("relation \"secret_table\" does not exist".into()),
        });
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let text = detail(&problem);
        assert!(
            text.contains("project 7") && text.contains("loading"),
            "{text}"
        );
        assert!(!text.contains("secret_table"), "{text}");

        let problem = Problem::from(TrafficFilterError::CorruptRow {
            filter_id: 3,
            project_id: 7,
            reason: "stored value 'private-data'".into(),
        });
        let text = detail(&problem);
        assert!(
            text.contains("filter 3") && text.contains("project 7"),
            "{text}"
        );
        assert!(!text.contains("private-data"), "{text}");
    }

    fn project_id_row(id: i32) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([("id", Value::Int(Some(id)))])
    }

    fn count_row(n: i64) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([("num_items", Value::BigInt(Some(n)))])
    }

    fn rule_row(id: i32, kind: &str, value: &str) -> analytics_traffic_filters::Model {
        analytics_traffic_filters::Model {
            id,
            project_id: 1,
            kind: kind.to_string(),
            value: value.to_string(),
            note: None,
            created_by: Some(1),
            created_at: Utc::now(),
        }
    }

    fn create_request(kind: TrafficFilterKind, value: &str) -> Json<CreateTrafficFilterRequest> {
        Json(CreateTrafficFilterRequest {
            kind,
            value: value.to_string(),
            note: None,
        })
    }

    async fn json<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("readable body");
        serde_json::from_slice(&bytes).expect("JSON body")
    }

    #[tokio::test]
    async fn create_returns_201_with_the_normalised_rule_and_audits_it() {
        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                // The advisory lock serializing creates for the project.
                .append_exec_results([lock_acquired()])
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([Vec::<analytics_traffic_filters::Model>::new()])
                .append_query_results([vec![count_row(0)]])
                .append_query_results([vec![rule_row(5, "referrer", "spam-site.example")]]),
        );
        let response = create_traffic_filter(
            auth(Role::Admin),
            State(state),
            metadata(),
            Path(1),
            create_request(TrafficFilterKind::Referrer, "https://spam-site.example/x"),
        )
        .await
        .expect("admin can add a filter")
        .into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let filter: TrafficFilterResponse = json(response).await;
        assert_eq!(filter.value, "spam-site.example");
        assert_eq!(filter.kind, TrafficFilterKind::Referrer);
        assert_eq!(audit.recorded(), vec!["ANALYTICS_TRAFFIC_FILTER_CREATED"]);
    }

    #[tokio::test]
    async fn create_with_an_invalid_value_is_a_400_and_writes_no_audit() {
        let (state, audit) = state_with(MockDatabase::new(DatabaseBackend::Postgres));
        let err = create_traffic_filter(
            auth(Role::Admin),
            State(state),
            metadata(),
            Path(1),
            create_request(TrafficFilterKind::Ip, "0.0.0.0/0"),
        )
        .await
        .err()
        .expect("too broad");
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn create_of_a_duplicate_is_a_409() {
        let (state, _) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                // The advisory lock serializing creates for the project.
                .append_exec_results([lock_acquired()])
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([vec![rule_row(4, "asn", "132203")]]),
        );
        let err = create_traffic_filter(
            auth(Role::Admin),
            State(state),
            metadata(),
            Path(1),
            create_request(TrafficFilterKind::Asn, "AS132203"),
        )
        .await
        .err()
        .expect("duplicate");
        assert_eq!(err.status_code, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn writes_require_analytics_write() {
        let (state, audit) = state_with(MockDatabase::new(DatabaseBackend::Postgres));
        let err = create_traffic_filter(
            auth(Role::ApiReader),
            State(state.clone()),
            metadata(),
            Path(1),
            create_request(TrafficFilterKind::Country, "SG"),
        )
        .await
        .err()
        .expect("a reader cannot add filters");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);

        let err = delete_traffic_filter(
            auth(Role::ApiReader),
            State(state),
            metadata(),
            Path((1, 5)),
        )
        .await
        .err()
        .expect("a reader cannot remove filters");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn reads_require_analytics_read() {
        let (state, _) = state_with(MockDatabase::new(DatabaseBackend::Postgres));
        let err = list_traffic_filters(
            auth(Role::MetricsIngest),
            State(state.clone()),
            Path(1),
            Query(ListTrafficFiltersQuery::default()),
        )
        .await
        .err()
        .expect("no AnalyticsRead");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);

        let err = get_filtered_traffic(
            auth(Role::MetricsIngest),
            State(state),
            Path(1),
            Query(FilteredTrafficQuery::default()),
        )
        .await
        .err()
        .expect("no AnalyticsRead");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_clamps_the_page_size_and_reports_the_limit() {
        let (state, _) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([vec![count_row(1)]])
                .append_query_results([vec![rule_row(5, "country", "SG")]]),
        );
        let response = list_traffic_filters(
            auth(Role::Admin),
            State(state),
            Path(1),
            Query(ListTrafficFiltersQuery {
                kind: None,
                page: Some(0),
                page_size: Some(1_000),
            }),
        )
        .await
        .expect("list")
        .into_response();
        let list: TrafficFilterListResponse = json(response).await;
        assert_eq!(list.page, 1);
        assert_eq!(list.page_size, 100);
        assert_eq!(list.total, 1);
        assert_eq!(list.max_filters_per_kind, MAX_FILTERS_PER_KIND);
        assert_eq!(list.filters[0].value, "SG");
    }

    #[tokio::test]
    async fn delete_returns_204_and_audits_and_a_missing_rule_is_a_404() {
        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![rule_row(5, "ip", "203.0.113.0/24")]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results([Vec::<analytics_traffic_filters::Model>::new()]),
        );
        let response = delete_traffic_filter(
            auth(Role::Admin),
            State(state.clone()),
            metadata(),
            Path((1, 5)),
        )
        .await
        .expect("delete")
        .into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(audit.recorded(), vec!["ANALYTICS_TRAFFIC_FILTER_DELETED"]);

        let err = delete_traffic_filter(auth(Role::Admin), State(state), metadata(), Path((1, 5)))
            .await
            .err()
            .expect("already gone");
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn filtered_traffic_totals_the_reasons() {
        let row = |is_crawler: bool, name: Option<&str>, events: i64| {
            BTreeMap::from([
                ("is_crawler", Value::Bool(Some(is_crawler))),
                (
                    "crawler_name",
                    Value::String(name.map(|n| Box::new(n.to_string()))),
                ),
                ("events", Value::BigInt(Some(events))),
            ])
        };
        let (state, _) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([vec![
                    row(false, None, 500),
                    row(true, Some("datacenter"), 40),
                    row(true, Some("spam-referrer"), 2),
                ]]),
        );
        let response = get_filtered_traffic(
            auth(Role::Admin),
            State(state),
            Path(1),
            Query(FilteredTrafficQuery { days: None }),
        )
        .await
        .expect("summary")
        .into_response();
        let summary: FilteredTrafficResponse = json(response).await;
        assert_eq!(summary.days, DEFAULT_FILTERED_TRAFFIC_DAYS);
        assert_eq!(summary.counted_events, 500);
        assert_eq!(summary.filtered_events, 42);
        assert_eq!(summary.reasons[0].reason, FilteredTrafficReason::Datacenter);
    }

    #[tokio::test]
    async fn filtered_traffic_rejects_an_out_of_range_window_with_400() {
        let (state, _) = state_with(MockDatabase::new(DatabaseBackend::Postgres));
        let err = get_filtered_traffic(
            auth(Role::Admin),
            State(state),
            Path(1),
            Query(FilteredTrafficQuery { days: Some(365) }),
        )
        .await
        .err()
        .expect("window too long");
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
    }
}
