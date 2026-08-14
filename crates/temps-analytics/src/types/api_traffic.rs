//! Response types for API traffic analytics endpoints.
//!
//! These endpoints query `proxy_logs` (a TimescaleDB hypertable) at query time
//! with GROUP BY, giving raw route/caller breakdowns without pre-aggregation.
//!
//! ## Known limitation: raw path cardinality
//! Routes are grouped by the raw `(method, path)` pair from `proxy_logs`. No
//! path-template normalization is applied (e.g. `/users/123` and `/users/456`
//! remain separate rows). This is intentional for v1 — keep the implementation
//! simple and avoid introducing regex/pattern-matching infrastructure. The
//! top-routes endpoint limits results so the high-cardinality tail is naturally
//! suppressed. Path normalization can be layered on in a later iteration once
//! routing metadata is available.

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

/// A single time bucket in the API request timeseries.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ApiTimeseriesPoint {
    /// Bucket start timestamp (ISO 8601 with Z suffix).
    #[schema(value_type = String, format = "date-time")]
    pub timestamp: UtcDateTime,
    /// Total request count in this bucket.
    pub request_count: i64,
    /// Number of requests with status >= 400.
    pub error_count: i64,
    /// Error rate: error_count / request_count (0.0–1.0). Zero when
    /// request_count == 0.
    pub error_rate: f64,
    /// Mean response time in milliseconds. Null when no rows with a recorded
    /// response_time_ms exist in the bucket.
    pub avg_latency_ms: Option<f64>,
    /// p95 response time in milliseconds. Null when insufficient data.
    pub p95_latency_ms: Option<f64>,
    /// p99 response time in milliseconds. Null when insufficient data.
    pub p99_latency_ms: Option<f64>,
}

/// Timeseries of API request volume, error rate, and latency percentiles.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ApiTimeseriesResponse {
    /// Time-bucketed data points ordered ascending by timestamp.
    pub points: Vec<ApiTimeseriesPoint>,
    /// Aggregate request count over the full period.
    pub total_requests: i64,
    /// Aggregate error count (status >= 400) over the full period.
    pub total_errors: i64,
    /// Overall error rate over the full period (0.0–1.0).
    pub overall_error_rate: f64,
    /// Overall mean response time in milliseconds over the full period.
    pub overall_avg_latency_ms: Option<f64>,
    /// Bucket interval used for time series (e.g. "1 hour", "6 hours", "1 day").
    pub bucket_interval: String,
}

/// A single route entry in the top-routes breakdown.
///
/// Routes are grouped by raw `(method, path)` — no template normalization.
/// See the module-level note on path cardinality.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ApiRouteEntry {
    /// HTTP method (e.g. "GET", "POST").
    pub method: String,
    /// Raw request path (e.g. "/api/users/123"). High-cardinality paths with
    /// dynamic IDs will appear as separate rows until path normalization is
    /// implemented.
    pub path: String,
    /// Total request count for this route in the period.
    pub request_count: i64,
    /// Mean response time in milliseconds for this route.
    pub avg_latency_ms: Option<f64>,
    /// Error rate for this route (0.0–1.0): requests with status >= 400.
    pub error_rate: f64,
}

/// Top routes by request count for a given project + time window.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ApiRoutesResponse {
    /// Top routes, ordered by request_count descending.
    pub routes: Vec<ApiRouteEntry>,
    /// Total distinct (method, path) pairs in the period (before the limit).
    pub total_routes: i64,
}

/// A single caller entry in the top-callers breakdown.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ApiCallerEntry {
    /// Caller IP address as recorded in proxy_logs.client_ip.
    pub client_ip: String,
    /// Total request count from this IP in the period.
    pub request_count: i64,
    /// Error rate for this caller (0.0–1.0).
    pub error_rate: f64,
    /// Timestamp of the most recent request from this IP in the period.
    #[schema(value_type = String, format = "date-time")]
    pub last_seen: UtcDateTime,
}

/// Top callers (by IP) ranked by request count for a given project + time window.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ApiCallersResponse {
    /// Top callers, ordered by request_count descending.
    pub callers: Vec<ApiCallerEntry>,
    /// Total distinct client IPs seen in the period (before the limit).
    pub total_callers: i64,
}

/// Structured AI summary of API traffic for a given project + time window.
///
/// Derives `JsonSchema` so `complete_typed` can request this shape from the
/// configured AI provider. All fields are intentionally short — the prompt
/// feeds aggregated stats (not raw log lines) to keep token usage bounded.
///
/// When AI is not configured or the project has not opted in, the summary
/// endpoint returns `null` for this field rather than an error.
#[derive(Debug, Serialize, Deserialize, ToSchema, schemars::JsonSchema, Clone)]
pub struct ApiTrafficSummary {
    /// One-sentence headline describing the traffic pattern.
    pub headline: String,
    /// Bullet-point findings (2–4 items). Each finding is a single sentence.
    pub findings: Vec<String>,
    /// Anomalies or concerns worth investigating (0–3 items). Empty when
    /// traffic appears normal.
    pub anomalies: Vec<String>,
    /// Optional single actionable recommendation.
    pub recommendation: Option<String>,
}

/// Response from the AI traffic summary endpoint.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ApiTrafficSummaryResponse {
    /// The AI-generated summary, or `null` when AI is not configured or the
    /// project has not opted in (`ai_api_traffic_summary_enabled = false`).
    pub summary: Option<ApiTrafficSummary>,
    /// Whether the project has `ai_api_traffic_summary_enabled = true`.
    pub enabled: bool,
    /// Why the summary is null when `summary` is None and `enabled` is true:
    /// either AI is not configured or the call failed/timed out.
    pub unavailable_reason: Option<String>,
    /// Whether this response was served from the backend AI-result cache.
    pub cached: bool,
}
