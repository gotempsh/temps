//! A minimal, stable read contract over the distributed-trace store.
//!
//! Consumers that only need to *read* traces (e.g. the AI debugging chat in
//! `temps-ai-chat`) depend on this trait, NOT on the heavy `temps-otel` storage
//! crate (ClickHouse / TimescaleDB clients, ingest pipeline, migrations). The
//! concrete `OtelService` implements [`TraceReader`] and is injected as
//! `Arc<dyn TraceReader>` through the plugin DI — the same decoupling pattern as
//! `temps_ai::AiService` (trait) vs `temps-ai-gateway` (concrete impl).
//!
//! The DTOs here are deliberately flat and dependency-light (enum values are
//! rendered as lowercase strings, attributes as `BTreeMap<String, String>`) so
//! `temps-core` never has to know the storage backend's rich span model. The
//! implementor maps its own types onto these.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// Filter for listing trace summaries. `project_id` is always supplied by the
/// caller (server-side, from the authenticated context) — never by an untrusted
/// source — so a reader can never be steered to another project's traces.
#[derive(Debug, Clone, Default)]
pub struct TraceQueryFilter {
    /// The project whose traces to read. Forced server-side; this is the tenancy
    /// boundary for trace reads.
    pub project_id: i32,
    /// When true, restrict to traces that contain at least one error span.
    pub only_errors: bool,
    /// Restrict to traces containing at least one span emitted by this service
    /// (exact match). Matching is at span granularity, not trace-level.
    pub service_name: Option<String>,
    /// Restrict to traces containing a span whose name matches this pattern
    /// (substring / ILIKE, backend-dependent).
    pub name_pattern: Option<String>,
    /// Restrict to traces containing at least one span this many milliseconds
    /// long (find slow operations anywhere in the request). Span-level, not the
    /// trace's overall/root duration.
    pub min_duration_ms: Option<f64>,
    /// Inclusive lower bound on trace start time.
    pub start_time: Option<DateTime<Utc>>,
    /// Inclusive upper bound on trace start time.
    pub end_time: Option<DateTime<Utc>>,
    /// Max rows to return. The implementor clamps this to a sane ceiling.
    pub limit: Option<u64>,
}

/// One trace, summarised for a list view.
#[derive(Debug, Clone)]
pub struct TraceSummaryDto {
    pub trace_id: String,
    pub root_span_name: String,
    pub service_name: String,
    pub deployment_environment: Option<String>,
    /// Lowercase trace-level status, derived from whether the trace contains any
    /// error span: `"error"` if `error_count > 0`, else `"ok"`. (Never
    /// `"unset"` — that only applies to an individual span's status.)
    pub status: String,
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub span_count: i64,
    pub error_count: i64,
}

/// One span within a trace, for drill-down.
#[derive(Debug, Clone)]
pub struct TraceSpanDto {
    pub span_id: String,
    /// `None` for the root span; otherwise the id of the parent span, so callers
    /// can reconstruct the parent/child tree.
    pub parent_span_id: Option<String>,
    pub name: String,
    /// Lowercase span kind: `"server"`, `"client"`, `"internal"`, `"producer"`,
    /// `"consumer"`, or `"unspecified"`.
    pub kind: String,
    pub service_name: String,
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    /// Lowercase status: `"ok"`, `"error"`, or `"unset"`.
    pub status: String,
    /// Human-readable status detail (often the error message). May be empty.
    pub status_message: String,
    pub attributes: BTreeMap<String, String>,
    pub events: Vec<TraceSpanEventDto>,
}

/// A timestamped event recorded on a span (e.g. an exception).
#[derive(Debug, Clone)]
pub struct TraceSpanEventDto {
    pub timestamp: DateTime<Utc>,
    pub name: String,
    pub attributes: BTreeMap<String, String>,
}

/// Failure reading from the trace store. Kept opaque on purpose: the consumer
/// surfaces it as recoverable text to the model, never as a hard error.
#[derive(Debug, Error)]
pub enum TraceReaderError {
    #[error("Trace store error: {0}")]
    Backend(String),
}

/// Read-only access to the distributed-trace store, scoped per project.
///
/// Implemented by `temps_otel::OtelService`. Methods return recoverable errors
/// (never panic) and always enforce the `project_id` tenancy boundary.
#[async_trait]
pub trait TraceReader: Send + Sync {
    /// List recent trace summaries matching `filter`, newest first. The
    /// implementor clamps `filter.limit` to a sane ceiling.
    async fn list_traces(
        &self,
        filter: TraceQueryFilter,
    ) -> Result<Vec<TraceSummaryDto>, TraceReaderError>;

    /// All spans of a single trace, scoped to `project_id` (a trace from another
    /// project returns empty, never another tenant's spans). Order is
    /// backend-defined; callers reconstruct the tree from `parent_span_id`.
    async fn get_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<TraceSpanDto>, TraceReaderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_default_is_unscoped_no_error_filter() {
        let f = TraceQueryFilter::default();
        assert_eq!(f.project_id, 0);
        assert!(!f.only_errors);
        assert!(f.service_name.is_none());
        assert!(f.limit.is_none());
    }
}
