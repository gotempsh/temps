//! `temps_core::TraceReader` implementation for [`OtelService`].
//!
//! Adapts the rich storage-layer trace types to the flat, dependency-light DTOs
//! in `temps-core` so read-only consumers (the AI debugging chat) depend on the
//! trait, not on this crate. The `project_id` tenancy boundary is preserved:
//! `list_traces` injects it into the `TraceQuery`, and `get_trace_spans` passes
//! it straight to `get_trace`, which filters by project.

use async_trait::async_trait;

use temps_core::{
    TraceQueryFilter, TraceReader, TraceReaderError, TraceSpanDto, TraceSpanEventDto,
    TraceSummaryDto,
};

use crate::services::otel_service::OtelService;
use crate::types::{SpanEvent, SpanRecord, SpanStatusCode, TraceQuery, TraceSummary};

/// Implementor-side ceiling on how many trace summaries one `list_traces` call
/// can return, regardless of what the caller asks for. Bounds work + payload.
const MAX_LIST_LIMIT: u64 = 50;

/// Default page size when the caller doesn't specify one.
const DEFAULT_LIST_LIMIT: u64 = 20;

#[async_trait]
impl TraceReader for OtelService {
    async fn list_traces(
        &self,
        filter: TraceQueryFilter,
    ) -> Result<Vec<TraceSummaryDto>, TraceReaderError> {
        let summaries = self
            .query_trace_summaries(build_trace_query(filter))
            .await
            .map_err(|e| TraceReaderError::Backend(e.to_string()))?;
        Ok(summaries.into_iter().map(map_summary).collect())
    }

    async fn get_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<TraceSpanDto>, TraceReaderError> {
        let spans = self
            .get_trace(project_id, trace_id)
            .await
            .map_err(|e| TraceReaderError::Backend(e.to_string()))?;
        Ok(spans.into_iter().map(map_span).collect())
    }
}

/// Translate the storage-agnostic filter into the backend `TraceQuery`,
/// clamping the limit and mapping `only_errors` to a status filter. `only_errors`
/// → `status = Error`, which the storage layer applies as "trace contains at
/// least one error span" (a `HAVING countIf(status = ERROR) > 0`).
fn build_trace_query(filter: TraceQueryFilter) -> TraceQuery {
    let limit = filter
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    TraceQuery {
        project_id: filter.project_id,
        status: if filter.only_errors {
            Some(SpanStatusCode::Error)
        } else {
            None
        },
        service_name: filter.service_name,
        name_pattern: filter.name_pattern,
        min_duration_ms: filter.min_duration_ms,
        start_time: filter.start_time,
        end_time: filter.end_time,
        limit: Some(limit),
        ..Default::default()
    }
}

fn map_summary(s: TraceSummary) -> TraceSummaryDto {
    TraceSummaryDto {
        trace_id: s.trace_id,
        root_span_name: s.root_span_name,
        service_name: s.service_name,
        deployment_environment: s.deployment_environment,
        status: s.status_code.to_string().to_lowercase(),
        start_time: s.start_time,
        duration_ms: s.duration_ms,
        span_count: s.span_count,
        error_count: s.error_count,
    }
}

fn map_span(s: SpanRecord) -> TraceSpanDto {
    TraceSpanDto {
        span_id: s.span_id,
        // Normalise an empty parent id (some backends store "" for the root) to
        // `None` so callers can reliably detect the trace root.
        parent_span_id: s.parent_span_id.filter(|p| !p.is_empty()),
        name: s.name,
        kind: s.kind.to_string().to_lowercase(),
        service_name: s.resource.service_name,
        start_time: s.start_time,
        duration_ms: s.duration_ms,
        status: s.status_code.to_string().to_lowercase(),
        status_message: s.status_message,
        attributes: s.attributes,
        events: s.events.into_iter().map(map_event).collect(),
    }
}

fn map_event(e: SpanEvent) -> TraceSpanEventDto {
    TraceSpanEventDto {
        timestamp: e.timestamp,
        name: e.name,
        attributes: e.attributes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResourceInfo, SpanKind};
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn span(parent: Option<&str>, status: SpanStatusCode, kind: SpanKind) -> SpanRecord {
        let now = Utc::now();
        SpanRecord {
            project_id: 7,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: "checkout".to_string(),
                service_version: None,
                deployment_environment: None,
                attributes: BTreeMap::new(),
            },
            trace_id: "t1".to_string(),
            span_id: "s1".to_string(),
            parent_span_id: parent.map(str::to_string),
            name: "GET /pay".to_string(),
            kind,
            start_time: now,
            end_time: now,
            duration_ms: 12.5,
            status_code: status,
            status_message: "boom".to_string(),
            attributes: BTreeMap::new(),
            events: vec![],
        }
    }

    #[test]
    fn test_map_span_lowercases_status_and_kind() {
        let dto = map_span(span(Some("p1"), SpanStatusCode::Error, SpanKind::Server));
        assert_eq!(dto.status, "error");
        assert_eq!(dto.kind, "server");
        assert_eq!(dto.parent_span_id.as_deref(), Some("p1"));
        assert_eq!(dto.service_name, "checkout");
    }

    #[test]
    fn test_map_span_normalises_empty_parent_to_none() {
        // A root span stored with an empty parent id must become `None`.
        let dto = map_span(span(Some(""), SpanStatusCode::Ok, SpanKind::Internal));
        assert!(dto.parent_span_id.is_none());
        assert_eq!(dto.status, "ok");
        assert_eq!(dto.kind, "internal");
    }

    #[test]
    fn test_build_trace_query_clamps_limit_and_maps_only_errors() {
        let q = build_trace_query(TraceQueryFilter {
            project_id: 7,
            only_errors: true,
            limit: Some(999), // over the ceiling
            ..Default::default()
        });
        assert_eq!(q.project_id, 7);
        assert_eq!(q.limit, Some(MAX_LIST_LIMIT));
        assert!(matches!(q.status, Some(SpanStatusCode::Error)));

        let q2 = build_trace_query(TraceQueryFilter {
            project_id: 1,
            only_errors: false,
            limit: None, // falls back to default
            ..Default::default()
        });
        assert_eq!(q2.limit, Some(DEFAULT_LIST_LIMIT));
        assert!(q2.status.is_none());
    }

    #[test]
    fn test_map_summary_lowercases_status_and_carries_fields() {
        let now = chrono::Utc::now();
        let dto = map_summary(TraceSummary {
            trace_id: "t".to_string(),
            root_span_name: "GET /".to_string(),
            service_name: "api".to_string(),
            deployment_environment: Some("prod".to_string()),
            kind: SpanKind::Server,
            status_code: SpanStatusCode::Error,
            start_time: now,
            duration_ms: 9.0,
            span_count: 4,
            error_count: 2,
        });
        assert_eq!(dto.status, "error");
        assert_eq!(dto.error_count, 2);
        assert_eq!(dto.span_count, 4);
        assert_eq!(dto.service_name, "api");
        assert_eq!(dto.deployment_environment.as_deref(), Some("prod"));
    }
}
