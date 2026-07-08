//! Test support: MockOtelStorage and helper builders.
//!
//! Provides an in-memory storage backend for unit and integration tests,
//! plus helpers to construct protobuf trace trees and OtelService instances.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::OtelError;
use crate::storage::{BaselinePoint, DeployEvent, MinuteAggregate, OtelStorage, StorageResult};
use crate::types::*;

/// In-memory storage backend for tests.
///
/// Stores all data in `Arc<Mutex<...>>` collections so tests can
/// inspect what was stored after calling service methods.
#[derive(Clone, Default)]
pub struct MockOtelStorage {
    pub metrics: Arc<Mutex<Vec<MetricPoint>>>,
    pub spans: Arc<Mutex<Vec<SpanRecord>>>,
    pub logs: Arc<Mutex<Vec<LogRecord>>>,
    pub archived_logs: Arc<Mutex<Vec<LogRecord>>>,
    pub insights: Arc<Mutex<Vec<Insight>>>,
    pub health_summaries: Arc<Mutex<Vec<HealthSummary>>>,
    pub next_insight_id: Arc<Mutex<i64>>,
    /// If set, store_spans will return this error instead.
    pub fail_store_spans: Arc<Mutex<Option<String>>>,
    /// If set, archive_logs will return this error.
    pub fail_archive_logs: Arc<Mutex<Option<String>>>,
    /// Counts calls to `get_storage_quota`, so tests can assert on
    /// caching behavior in callers like `OtelService::check_quota`.
    pub get_storage_quota_calls: Arc<Mutex<u32>>,
    /// Overrides `get_storage_quota`'s `usage_pct`/`total_bytes`/`limit_bytes`
    /// for tests that need to simulate a project over its quota.
    pub quota_override: Arc<Mutex<Option<StorageQuota>>>,
}

impl MockOtelStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return all stored spans.
    pub fn stored_spans(&self) -> Vec<SpanRecord> {
        self.spans.lock().unwrap().clone()
    }

    /// Return all stored metrics.
    pub fn stored_metrics(&self) -> Vec<MetricPoint> {
        self.metrics.lock().unwrap().clone()
    }

    /// Return all stored logs (DB path).
    pub fn stored_logs(&self) -> Vec<LogRecord> {
        self.logs.lock().unwrap().clone()
    }

    /// Return all archived logs (S3 path).
    pub fn stored_archived_logs(&self) -> Vec<LogRecord> {
        self.archived_logs.lock().unwrap().clone()
    }

    /// Number of times `get_storage_quota` has been called.
    pub fn get_storage_quota_call_count(&self) -> u32 {
        *self.get_storage_quota_calls.lock().unwrap()
    }
}

#[async_trait]
impl OtelStorage for MockOtelStorage {
    async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64> {
        let count = points.len() as u64;
        self.metrics.lock().unwrap().extend(points);
        Ok(count)
    }

    async fn store_spans(&self, spans: Vec<SpanRecord>) -> StorageResult<u64> {
        if let Some(msg) = self.fail_store_spans.lock().unwrap().as_ref() {
            return Err(OtelError::Storage {
                message: msg.clone(),
            });
        }
        let count = spans.len() as u64;
        self.spans.lock().unwrap().extend(spans);
        Ok(count)
    }

    async fn store_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        let count = records.len() as u64;
        self.logs.lock().unwrap().extend(records);
        Ok(count)
    }

    async fn archive_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        if let Some(msg) = self.fail_archive_logs.lock().unwrap().as_ref() {
            return Err(OtelError::S3 {
                project_id: records.first().map(|r| r.project_id).unwrap_or(0),
                reason: msg.clone(),
            });
        }
        let count = records.len() as u64;
        self.archived_logs.lock().unwrap().extend(records);
        Ok(count)
    }

    async fn query_metrics(&self, _query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        // Return empty for now — not the focus of trace tree tests
        Ok(vec![])
    }

    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>> {
        let names: Vec<String> = self
            .metrics
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.project_id == project_id)
            .map(|m| m.metric_name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        Ok(names)
    }

    async fn list_metric_label_keys(
        &self,
        project_id: i32,
        metric_name: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        let mut keys: Vec<String> = self
            .metrics
            .lock()
            .unwrap()
            .iter()
            .filter(|m| {
                m.project_id == project_id
                    && m.metric_name == metric_name
                    && m.timestamp >= start_time
                    && m.timestamp <= end_time
            })
            .flat_map(|m| m.attributes.keys().cloned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        keys.sort();
        Ok(keys)
    }

    async fn list_metric_label_values(
        &self,
        project_id: i32,
        metric_name: &str,
        label_key: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        let mut values: Vec<String> = self
            .metrics
            .lock()
            .unwrap()
            .iter()
            .filter(|m| {
                m.project_id == project_id
                    && m.metric_name == metric_name
                    && m.timestamp >= start_time
                    && m.timestamp <= end_time
            })
            .filter_map(|m| m.attributes.get(label_key).cloned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        values.sort();
        Ok(values)
    }

    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        let spans = self.spans.lock().unwrap();
        let filtered: Vec<SpanRecord> = spans
            .iter()
            .filter(|s| s.project_id == query.project_id)
            .filter(|s| {
                query
                    .trace_id
                    .as_ref()
                    .map(|tid| s.trace_id == *tid)
                    .unwrap_or(true)
            })
            .filter(|s| {
                query
                    .service_name
                    .as_ref()
                    .map(|sn| s.resource.service_name == *sn)
                    .unwrap_or(true)
            })
            .filter(|s| {
                query
                    .status
                    .as_ref()
                    .map(|st| s.status_code == *st)
                    .unwrap_or(true)
            })
            .filter(|s| {
                query
                    .min_duration_ms
                    .map(|min| s.duration_ms >= min)
                    .unwrap_or(true)
            })
            .take(query.limit.unwrap_or(100) as usize)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>> {
        let spans = self.spans.lock().unwrap();
        let filtered: Vec<SpanRecord> = spans
            .iter()
            .filter(|s| s.project_id == project_id && s.trace_id == trace_id)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn query_logs(&self, query: LogQuery) -> StorageResult<Vec<LogRecord>> {
        let logs = self.logs.lock().unwrap();
        let filtered: Vec<LogRecord> = logs
            .iter()
            .filter(|l| l.project_id == query.project_id)
            .filter(|l| {
                query
                    .severity
                    .as_ref()
                    .map(|sev| l.severity == *sev)
                    .unwrap_or(true)
            })
            .filter(|l| {
                query
                    .search
                    .as_ref()
                    .map(|s| l.body.contains(s.as_str()))
                    .unwrap_or(true)
            })
            .take(query.limit.unwrap_or(100) as usize)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn upsert_insight(&self, insight: &Insight) -> StorageResult<i64> {
        let mut id_counter = self.next_insight_id.lock().unwrap();
        *id_counter += 1;
        let id = *id_counter;
        let mut stored = insight.clone();
        stored.id = id;
        self.insights.lock().unwrap().push(stored);
        Ok(id)
    }

    async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> StorageResult<Vec<Insight>> {
        let insights = self.insights.lock().unwrap();
        let filtered: Vec<Insight> = insights
            .iter()
            .filter(|i| i.project_id == project_id)
            .filter(|i| status.map(|s| i.status == s).unwrap_or(true))
            .skip(offset as usize)
            .take(limit as usize)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn resolve_insight(&self, insight_id: i64) -> StorageResult<()> {
        let mut insights = self.insights.lock().unwrap();
        if let Some(i) = insights.iter_mut().find(|i| i.id == insight_id) {
            i.status = InsightStatus::Resolved;
            i.resolved_at = Some(chrono::Utc::now());
        }
        Ok(())
    }

    async fn store_health_summary(&self, summary: &HealthSummary) -> StorageResult<()> {
        self.health_summaries.lock().unwrap().push(summary.clone());
        Ok(())
    }

    async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> StorageResult<Vec<HealthSummary>> {
        let summaries = self.health_summaries.lock().unwrap();
        let filtered: Vec<HealthSummary> = summaries
            .iter()
            .filter(|s| s.project_id == project_id)
            .filter(|s| {
                environment_id
                    .map(|eid| s.environment_id == Some(eid))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn get_storage_quota(&self, project_id: i32) -> StorageResult<StorageQuota> {
        *self.get_storage_quota_calls.lock().unwrap() += 1;
        if let Some(quota) = self.quota_override.lock().unwrap().clone() {
            return Ok(quota);
        }
        Ok(StorageQuota {
            project_id,
            metrics_bytes: 0,
            traces_bytes: 0,
            logs_bytes: 0,
            total_bytes: 0,
            limit_bytes: 10 * 1024 * 1024 * 1024,
            usage_pct: 0.0,
        })
    }

    async fn check_quota(&self, _project_id: i32) -> StorageResult<bool> {
        Ok(false)
    }

    async fn get_metric_baseline(
        &self,
        _project_id: i32,
        _service_name: &str,
        _metric_name: &str,
        _environment: Option<&str>,
        _lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>> {
        Ok(vec![])
    }

    async fn get_recent_minute_aggregates(
        &self,
        _project_id: i32,
        _service_name: &str,
        _metric_name: &str,
        _environment: Option<&str>,
        _minutes: i32,
    ) -> StorageResult<Vec<MinuteAggregate>> {
        Ok(vec![])
    }

    async fn get_recent_deploys(
        &self,
        _project_id: i32,
        _minutes: i32,
    ) -> StorageResult<Vec<DeployEvent>> {
        Ok(vec![])
    }

    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        let spans = self.spans.lock().unwrap();

        // Group spans by trace_id, applying the same filters as query_spans
        let mut trace_map: std::collections::HashMap<String, (Vec<&SpanRecord>, i64)> =
            std::collections::HashMap::new();

        for span in spans.iter() {
            if span.project_id != query.project_id {
                continue;
            }
            if let Some(ref tid) = query.trace_id {
                if span.trace_id != *tid {
                    continue;
                }
            }
            if let Some(ref svc) = query.service_name {
                if span.resource.service_name != *svc {
                    continue;
                }
            }
            if let Some(min_dur) = query.min_duration_ms {
                if span.duration_ms < min_dur {
                    continue;
                }
            }
            trace_map
                .entry(span.trace_id.clone())
                .or_insert_with(|| (vec![], 0))
                .0
                .push(span);
        }

        // Apply status filter at trace level
        let mut summaries: Vec<TraceSummary> = trace_map
            .into_iter()
            .filter_map(|(trace_id, (spans_in_trace, _))| {
                let error_count = spans_in_trace
                    .iter()
                    .filter(|s| s.status_code == SpanStatusCode::Error)
                    .count() as i64;

                // Status filter: ERROR = traces with errors, OK = traces without
                match query.status {
                    Some(SpanStatusCode::Error) if error_count == 0 => return None,
                    Some(SpanStatusCode::Ok) if error_count > 0 => return None,
                    _ => {}
                }

                // Pick root span (no parent) or longest span
                let root = spans_in_trace
                    .iter()
                    .filter(|s| s.parent_span_id.is_none())
                    .max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap())
                    .or_else(|| {
                        spans_in_trace
                            .iter()
                            .max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap())
                    })?;

                let status_code = if error_count > 0 {
                    SpanStatusCode::Error
                } else {
                    SpanStatusCode::Ok
                };

                Some(TraceSummary {
                    trace_id,
                    root_span_name: root.name.clone(),
                    service_name: root.resource.service_name.clone(),
                    deployment_environment: root.resource.deployment_environment.clone(),
                    kind: root.kind,
                    status_code,
                    start_time: root.start_time,
                    duration_ms: root.duration_ms,
                    span_count: spans_in_trace.len() as i64,
                    error_count,
                })
            })
            .collect();

        // Sort by start_time descending
        summaries.sort_by_key(|s| std::cmp::Reverse(s.start_time));

        // Apply pagination
        let offset = query.offset.unwrap_or(0) as usize;
        let limit = query.limit.unwrap_or(50).min(100) as usize;
        let paged = summaries.into_iter().skip(offset).take(limit).collect();

        Ok(paged)
    }

    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        let spans = self.spans.lock().unwrap();

        let mut trace_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut trace_errors: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();

        for span in spans.iter() {
            if span.project_id != query.project_id {
                continue;
            }
            if let Some(ref tid) = query.trace_id {
                if span.trace_id != *tid {
                    continue;
                }
            }
            if let Some(ref svc) = query.service_name {
                if span.resource.service_name != *svc {
                    continue;
                }
            }
            trace_ids.insert(span.trace_id.clone());
            if span.status_code == SpanStatusCode::Error {
                trace_errors.insert(span.trace_id.clone(), true);
            }
        }

        // Apply status filter at trace level
        let count = match query.status {
            Some(SpanStatusCode::Error) => trace_ids
                .iter()
                .filter(|tid| trace_errors.contains_key(*tid))
                .count(),
            Some(SpanStatusCode::Ok) => trace_ids
                .iter()
                .filter(|tid| !trace_errors.contains_key(*tid))
                .count(),
            _ => trace_ids.len(),
        };

        Ok(count as u64)
    }

    async fn apply_retention(&self, _project_id: i32) -> StorageResult<u64> {
        Ok(0)
    }

    async fn get_p95_latency(
        &self,
        _project_id: i32,
        _service_name: &str,
        _window_minutes: i32,
    ) -> StorageResult<f64> {
        Ok(50.0) // 50ms default
    }

    async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>> {
        let spans = self.spans.lock().unwrap();
        let mut trace_map: std::collections::HashMap<String, Vec<&SpanRecord>> =
            std::collections::HashMap::new();

        for span in spans.iter() {
            if span.project_id != query.project_id {
                continue;
            }
            if !span.attributes.contains_key("gen_ai.system")
                && !span.attributes.contains_key("gen_ai.provider.name")
            {
                continue;
            }
            trace_map
                .entry(span.trace_id.clone())
                .or_default()
                .push(span);
        }

        let mut summaries: Vec<GenAiTraceSummary> = trace_map
            .into_iter()
            .map(|(trace_id, spans_in_trace)| {
                let root = spans_in_trace
                    .iter()
                    .filter(|s| s.parent_span_id.is_none())
                    .max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap())
                    .or_else(|| spans_in_trace.first())
                    .unwrap();

                let sum_i64_attr = |primary: &str, fallback: Option<&str>| -> Option<i64> {
                    let total: i64 = spans_in_trace
                        .iter()
                        .filter_map(|s| {
                            s.attributes
                                .get(primary)
                                .or_else(|| fallback.and_then(|fb| s.attributes.get(fb)))
                                .and_then(|v| v.parse::<i64>().ok())
                        })
                        .sum();
                    if total > 0 {
                        Some(total)
                    } else {
                        None
                    }
                };

                GenAiTraceSummary {
                    trace_id,
                    root_span_name: root.name.clone(),
                    service_name: root.resource.service_name.clone(),
                    gen_ai_system: root
                        .attributes
                        .get("gen_ai.provider.name")
                        .or_else(|| root.attributes.get("gen_ai.system"))
                        .cloned(),
                    gen_ai_model: root.attributes.get("gen_ai.request.model").cloned(),
                    gen_ai_operation: root.attributes.get("gen_ai.operation.name").cloned(),
                    start_time: root.start_time,
                    duration_ms: root.duration_ms,
                    span_count: spans_in_trace.len() as i64,
                    error_count: spans_in_trace
                        .iter()
                        .filter(|s| s.status_code == SpanStatusCode::Error)
                        .count() as i64,
                    total_input_tokens: sum_i64_attr(
                        "gen_ai.usage.input_tokens",
                        Some("gen_ai.usage.prompt_tokens"),
                    ),
                    total_output_tokens: sum_i64_attr(
                        "gen_ai.usage.output_tokens",
                        Some("gen_ai.usage.completion_tokens"),
                    ),
                    total_cache_creation_input_tokens: sum_i64_attr(
                        "gen_ai.usage.cache_creation.input_tokens",
                        None,
                    ),
                    total_cache_read_input_tokens: sum_i64_attr(
                        "gen_ai.usage.cache_read.input_tokens",
                        None,
                    ),
                }
            })
            .collect();

        summaries.sort_by_key(|s| std::cmp::Reverse(s.start_time));
        Ok(summaries)
    }

    async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiSpanDetail>> {
        let spans = self.spans.lock().unwrap();
        // Return ALL spans in the trace (not just GenAI-attributed ones)
        // to show the full trace tree including HTTP, DB, and tool spans.
        let details: Vec<GenAiSpanDetail> = spans
            .iter()
            .filter(|s| s.project_id == project_id && s.trace_id == trace_id)
            .map(|s| {
                GenAiSpanDetail::from_span_attrs(
                    s.span_id.clone(),
                    s.parent_span_id.clone(),
                    s.name.clone(),
                    s.kind,
                    s.start_time,
                    s.duration_ms,
                    s.status_code,
                    s.attributes.clone(),
                )
            })
            .collect();
        Ok(details)
    }

    async fn count_genai_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        let spans = self.spans.lock().unwrap();
        let trace_ids: std::collections::HashSet<String> = spans
            .iter()
            .filter(|s| {
                s.project_id == query.project_id
                    && (s.attributes.contains_key("gen_ai.system")
                        || s.attributes.contains_key("gen_ai.provider.name"))
            })
            .map(|s| s.trace_id.clone())
            .collect();
        Ok(trace_ids.len() as u64)
    }

    async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiEvent>> {
        let spans = self.spans.lock().unwrap();
        let mut events = Vec::new();
        for span in spans.iter() {
            if span.project_id != project_id || span.trace_id != trace_id {
                continue;
            }
            for event in &span.events {
                if event.name.starts_with("gen_ai.") {
                    events.push(GenAiEvent {
                        span_id: span.span_id.clone(),
                        trace_id: trace_id.to_string(),
                        event_name: event.name.clone(),
                        timestamp: event.timestamp,
                        attributes: event.attributes.clone(),
                    });
                }
            }
        }
        Ok(events)
    }
}

// ── Protobuf trace tree builders ────────────────────────────────────

use crate::proto;
use prost::Message;

/// Helper to build a KeyValue attribute.
pub fn kv(key: &str, string_value: &str) -> proto::common::v1::KeyValue {
    proto::common::v1::KeyValue {
        key: key.to_string(),
        value: Some(proto::common::v1::AnyValue {
            value: Some(proto::common::v1::any_value::Value::StringValue(
                string_value.to_string(),
            )),
        }),
    }
}

/// Build a protobuf Resource with service.name.
pub fn resource(service_name: &str) -> proto::resource::v1::Resource {
    proto::resource::v1::Resource {
        attributes: vec![kv("service.name", service_name)],
        dropped_attributes_count: 0,
    }
}

/// Build a minimal gauge `MetricPoint` for storage-level tests, with the given
/// data-point attributes. All histogram/exponential/summary fields are left
/// empty — callers that need those should construct the point directly.
pub fn metric_point(
    project_id: i32,
    metric_name: &str,
    timestamp: chrono::DateTime<chrono::Utc>,
    attributes: &[(&str, &str)],
) -> MetricPoint {
    MetricPoint {
        project_id,
        deployment_id: None,
        resource: ResourceInfo {
            service_name: "svc".to_string(),
            service_version: None,
            deployment_environment: None,
            attributes: std::collections::BTreeMap::new(),
        },
        metric_name: metric_name.to_string(),
        metric_type: MetricType::Gauge,
        unit: String::new(),
        description: None,
        timestamp,
        start_time: None,
        temporality: None,
        is_monotonic: None,
        flags: 0,
        value: Some(1.0),
        histogram_count: None,
        histogram_sum: None,
        histogram_min: None,
        histogram_max: None,
        histogram_bounds: None,
        histogram_bucket_counts: None,
        exp_scale: None,
        exp_zero_count: None,
        exp_zero_threshold: None,
        exp_positive_offset: None,
        exp_positive_counts: None,
        exp_negative_offset: None,
        exp_negative_counts: None,
        summary_quantiles: None,
        exemplars: Vec::new(),
        attributes: attributes
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

/// Build a single protobuf Span.
#[allow(clippy::too_many_arguments)]
pub fn span(
    trace_id: &[u8; 16],
    span_id: &[u8; 8],
    parent_span_id: &[u8],
    name: &str,
    kind: i32,
    start_nanos: u64,
    end_nanos: u64,
    status_code: i32,
) -> proto::trace::v1::Span {
    proto::trace::v1::Span {
        trace_id: trace_id.to_vec(),
        span_id: span_id.to_vec(),
        parent_span_id: parent_span_id.to_vec(),
        name: name.to_string(),
        kind,
        start_time_unix_nano: start_nanos,
        end_time_unix_nano: end_nanos,
        attributes: vec![],
        dropped_attributes_count: 0,
        events: vec![],
        dropped_events_count: 0,
        links: vec![],
        dropped_links_count: 0,
        status: Some(proto::trace::v1::Status {
            code: status_code,
            message: String::new(),
        }),
        trace_state: String::new(),
        flags: 0,
    }
}

/// Build an ExportTraceServiceRequest from spans grouped under one resource.
pub fn trace_request(
    resource_proto: proto::resource::v1::Resource,
    spans: Vec<proto::trace::v1::Span>,
) -> proto::collector::trace::v1::ExportTraceServiceRequest {
    proto::collector::trace::v1::ExportTraceServiceRequest {
        resource_spans: vec![proto::trace::v1::ResourceSpans {
            resource: Some(resource_proto),
            scope_spans: vec![proto::trace::v1::ScopeSpans {
                scope: None,
                spans,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

/// Encode a protobuf message to bytes.
pub fn encode_proto<M: Message>(msg: &M) -> Vec<u8> {
    msg.encode_to_vec()
}

/// Build a 3-span trace tree:
///
/// ```text
/// root-span (SERVER)
///   ├── child-db-query (CLIENT)
///   └── child-http-call (CLIENT)
///        └── grandchild-parse (INTERNAL)
/// ```
///
/// Returns (trace_id_hex, encoded_protobuf_bytes).
pub fn build_sample_trace_tree() -> (String, Vec<u8>) {
    let trace_id: [u8; 16] = [
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99,
    ];
    let root_id: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let child_db_id: [u8; 8] = [0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18];
    let child_http_id: [u8; 8] = [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28];
    let grandchild_id: [u8; 8] = [0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38];

    let base_time: u64 = 1_700_000_000_000_000_000; // ~2023-11-14

    let root = span(
        &trace_id,
        &root_id,
        &[], // no parent
        "GET /api/users",
        2, // SERVER
        base_time,
        base_time + 100_000_000, // 100ms
        1,                       // OK
    );

    let child_db = span(
        &trace_id,
        &child_db_id,
        &root_id,
        "SELECT * FROM users",
        3, // CLIENT
        base_time + 5_000_000,
        base_time + 25_000_000, // 20ms
        1,                      // OK
    );

    let child_http = span(
        &trace_id,
        &child_http_id,
        &root_id,
        "POST /external/validate",
        3, // CLIENT
        base_time + 30_000_000,
        base_time + 80_000_000, // 50ms
        1,                      // OK
    );

    let grandchild = span(
        &trace_id,
        &grandchild_id,
        &child_http_id,
        "parse_response",
        1, // INTERNAL
        base_time + 60_000_000,
        base_time + 75_000_000, // 15ms
        1,                      // OK
    );

    let res = resource("my-api-service");
    let request = trace_request(res, vec![root, child_db, child_http, grandchild]);
    let trace_id_hex = hex::encode(trace_id);

    (trace_id_hex, encode_proto(&request))
}

/// Reconstruct a trace tree from flat spans, returning a map of span_id -> children.
pub fn build_tree(spans: &[SpanRecord]) -> HashMap<String, Vec<String>> {
    let mut tree: HashMap<String, Vec<String>> = HashMap::new();
    for span in spans {
        if let Some(parent) = &span.parent_span_id {
            tree.entry(parent.clone())
                .or_default()
                .push(span.span_id.clone());
        }
    }
    tree
}

/// Find root spans (those with no parent_span_id).
pub fn find_roots(spans: &[SpanRecord]) -> Vec<&SpanRecord> {
    spans
        .iter()
        .filter(|s| s.parent_span_id.is_none())
        .collect()
}
