// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{storage::global_traces::*, types::*, OtelAppState};
use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::{problemdetails::Problem, ProblemDetails};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Deserialize, IntoParams)]
pub struct GlobalTraceParams {
    pub project_id: Option<i32>,
    pub trace_id: Option<String>,
    pub service_name: Option<String>,
    pub status: Option<String>,
    pub min_duration_ms: Option<f64>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    /// Filter by span attributes as comma-separated key=value pairs.
    /// e.g. "gen_ai.system=openai,gen_ai.request.model=gpt-4"
    pub attributes: Option<String>,
    /// Filter by span name pattern (ILIKE).
    pub name_pattern: Option<String>,
    /// Sort field for the trace-summaries list: "start_time" (default) or
    /// "duration". Anything else falls back to start_time.
    pub sort_by: Option<String>,
    /// Sort direction: "asc" or "desc" (default).
    pub sort_order: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalTraceSummariesResponse {
    pub data: Vec<GlobalTraceSummary>,
    pub total: u64,
    pub projects: Vec<TraceProject>,
    /// Effective per-project windows; totals and rows describe these windows.
    pub windows: Vec<GlobalTraceWindow>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct TraceProject {
    pub id: i32,
    pub name: String,
    pub slug: String,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalTracesResponse {
    pub data: Vec<SpanRecord>,
    pub total: u64,
    /// Effective per-project windows; totals and rows describe these windows.
    pub windows: Vec<GlobalTraceWindow>,
}

/// A global query can use different stores and effective windows per project.
/// A non-null clamp explicitly tells clients the pre-cutover range is excluded;
/// request an earlier window to read the older source (ADR-040/041).
#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalTraceWindow {
    pub project_id: i32,
    pub source: temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode,
    #[schema(value_type = String, format = DateTime)]
    pub effective_start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub effective_end_time: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub window_clamped_at: Option<DateTime<Utc>>,
}

impl From<&TraceReadScope> for GlobalTraceWindow {
    fn from(scope: &TraceReadScope) -> Self {
        use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;
        Self {
            project_id: scope.project_id,
            source: if scope.cloud {
                CloudTelemetryWriteMode::Cloud
            } else {
                CloudTelemetryWriteMode::Local
            },
            effective_start_time: scope.from,
            effective_end_time: scope.to,
            window_clamped_at: scope.window_clamped_at,
        }
    }
}

fn window(p: &GlobalTraceParams) -> Result<(DateTime<Utc>, DateTime<Utc>), Problem> {
    let parse = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|_| invalid("Use RFC3339 timestamps"))
    };
    let to = p
        .end_time
        .as_deref()
        .map(parse)
        .transpose()?
        .unwrap_or_else(Utc::now);
    let from = p
        .start_time
        .as_deref()
        .map(parse)
        .transpose()?
        .unwrap_or_else(|| {
            if p.trace_id.is_some() {
                DateTime::from_timestamp_millis(0).unwrap()
            } else {
                to - chrono::Duration::days(1)
            }
        });
    if from >= to
        || (p.trace_id.is_none() && to - from > chrono::Duration::days(31))
        || p.limit == Some(0)
        || p.limit.is_some_and(|v| v > 100)
        || p.offset.is_some_and(|v| v > i64::MAX as u64 - 100)
    {
        return Err(
            invalid("Use a time window up to 31 days and a page size between 1 and 100").into(),
        );
    }
    if p.trace_id
        .as_ref()
        .is_some_and(|s| !crate::services::cross_project::is_valid_trace_id(s))
        || p.name_pattern.as_ref().is_some_and(|s| s.len() > 500)
        || p.min_duration_ms.is_some_and(|n| !n.is_finite() || n < 0.0)
    {
        return Err(invalid("Invalid trace filters").into());
    }
    Ok((from, to))
}

// Access is resolved before either storage backend is queried. A missing scope
// means all accessible projects, never an authorization bypass.
async fn read(
    RequireAuth(auth): RequireAuth,
    state: OtelAppState,
    p: GlobalTraceParams,
    summaries: bool,
) -> Result<
    (
        GlobalTracePage,
        std::collections::BTreeMap<i32, (String, String)>,
        Vec<GlobalTraceWindow>,
    ),
    Problem,
> {
    permission_guard!(auth, OtelRead);
    if let Some(id) = p.project_id {
        project_scope_guard!(auth, id);
        project_access_guard!(auth, id, state.project_access_checker);
    }
    let project_id = auth.project_id().or(p.project_id);
    let mut hidden = Vec::new();
    if !auth.is_deployment_token() && !auth.is_instance_admin() {
        if let Some(checker) = &state.project_access_checker {
            let user = auth.user_id_opt().ok_or_else(|| {
                temps_core::error_builder::forbidden()
                    .title("Project access denied")
                    .build()
            })?;
            hidden = checker
                .hidden_project_ids(user)
                .await
                .map_err(|_| {
                    temps_core::error_builder::internal_server_error()
                        .title("Project access check failed")
                        .build()
                })?
                .unwrap_or_default();
        }
    }
    let (from, to) = window(&p)?;
    let (mut scopes, names) = state
        .telemetry_write_modes
        .global_trace_scopes(auth.project_id(), &hidden, from, to)
        .await
        .map_err(|e| {
            tracing::error!(%e,"Could not resolve global trace scopes");
            temps_core::error_builder::internal_server_error()
                .title("Could not resolve trace storage")
                .build()
        })?;
    if let Some(id) = project_id {
        scopes.retain(|s| s.project_id == id);
    }
    let status = p
        .status
        .as_deref()
        .map(|s| match s.to_ascii_uppercase().as_str() {
            "ERROR" => Ok(SpanStatusCode::Error),
            "OK" => Ok(SpanStatusCode::Ok),
            "UNSET" => Ok(SpanStatusCode::Unset),
            _ => Err(invalid("Invalid trace status")),
        })
        .transpose()?;
    let windows = scopes.iter().map(GlobalTraceWindow::from).collect();
    let query = GlobalTraceQuery {
        source_offset: 0,
        summaries,
        scopes,
        filter: TraceQuery {
            trace_id: p.trace_id,
            service_name: p.service_name,
            status,
            min_duration_ms: p.min_duration_ms,
            start_time: Some(from),
            end_time: Some(to),
            environment_id: p.environment_id,
            deployment_id: p.deployment_id,
            attributes: p
                .attributes
                .as_deref()
                .map(super::query_handler::parse_attributes)
                .filter(|v| !v.is_empty()),
            name_pattern: p.name_pattern,
            sort_by: p
                .sort_by
                .as_deref()
                .map(TraceSortField::parse)
                .unwrap_or_default(),
            sort_order: p
                .sort_order
                .as_deref()
                .map(SortOrder::parse)
                .unwrap_or_default(),
            limit: Some(p.limit.unwrap_or(20)),
            offset: p.offset,
            ..Default::default()
        },
    };
    Ok((
        state.otel_service.global_trace_page(query).await?,
        names,
        windows,
    ))
}

#[utoipa::path(get, path="/otel/global/trace-summaries", tag="Traces", params(GlobalTraceParams), responses((status=200,body=GlobalTraceSummariesResponse),(status=403,body=ProblemDetails)), security(("bearer_auth"=[])))]
pub async fn query_global_trace_summaries(
    auth: RequireAuth,
    State(state): State<OtelAppState>,
    Query(p): Query<GlobalTraceParams>,
) -> Result<Json<GlobalTraceSummariesResponse>, Problem> {
    let (page, names, windows) = read(auth, state, p, true).await?;
    let data = page
        .data
        .into_iter()
        .map(|r| {
            let (name, slug) = names
                .get(&r.project_id)
                .cloned()
                .ok_or_else(|| invalid("Trace returned outside authorized scope"))?;
            r.summary(name, slug)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(GlobalTraceSummariesResponse {
        data,
        total: page.total,
        windows,
        projects: names
            .into_iter()
            .map(|(id, (name, slug))| TraceProject { id, name, slug })
            .collect(),
    }))
}
#[utoipa::path(get, path="/otel/global/spans", tag="Traces", params(GlobalTraceParams), responses((status=200,body=GlobalTracesResponse),(status=403,body=ProblemDetails)), security(("bearer_auth"=[])))]
pub async fn query_global_traces(
    auth: RequireAuth,
    State(state): State<OtelAppState>,
    Query(p): Query<GlobalTraceParams>,
) -> Result<Json<GlobalTracesResponse>, Problem> {
    let (page, _, windows) = read(auth, state, p, false).await?;
    Ok(Json(GlobalTracesResponse {
        windows,
        data: page
            .data
            .into_iter()
            .map(GlobalTraceRow::span)
            .collect::<Result<Vec<_>, _>>()?,
        total: page.total,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_trace_responses_disclose_effective_windows_even_on_empty_pages() {
        let to = DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::hours(2);
        let cutover = to - chrono::Duration::minutes(30);
        for cloud in [false, true] {
            for clamped in [None, Some(cutover)] {
                let scope = TraceReadScope {
                    project_id: 7,
                    from: clamped.unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
                    to,
                    cloud,
                    window_clamped_at: clamped,
                };
                let summary = GlobalTraceSummariesResponse {
                    data: vec![],
                    total: 0,
                    projects: vec![],
                    windows: vec![GlobalTraceWindow::from(&scope)],
                };
                let spans = GlobalTracesResponse {
                    data: vec![],
                    total: 0,
                    windows: vec![GlobalTraceWindow::from(&scope)],
                };
                for response in [
                    serde_json::to_value(summary).unwrap(),
                    serde_json::to_value(spans).unwrap(),
                ] {
                    let window = &response["windows"][0];
                    assert_eq!(window["project_id"], 7);
                    assert_eq!(window["source"], if cloud { "cloud" } else { "local" });
                    assert_eq!(
                        window["effective_start_time"],
                        serde_json::to_value(scope.from).unwrap()
                    );
                    assert_eq!(
                        window["effective_end_time"],
                        serde_json::to_value(to).unwrap()
                    );
                    assert_eq!(
                        window["window_clamped_at"],
                        serde_json::to_value(clamped).unwrap()
                    );
                }
            }
        }
    }
}
