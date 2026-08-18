//! HTTP handlers for the unified metrics API.
//!
//! Exposes time-series metric queries, latest-value lookups, and alert-rule
//! CRUD for external services, deployments, and nodes.  All endpoints require
//! standard bearer-token authentication via [`RequireAuth`] and check the
//! appropriate read/write permissions.
//!
//! # Route layout
//!
//! ```text
//! GET  /external-services/{id}/metrics            — range query (gauge + counter)
//! GET  /external-services/{id}/metrics/latest     — latest values for all metrics
//! GET  /external-services/{id}/metrics/alert-rules
//! POST /external-services/{id}/metrics/alert-rules
//! PUT  /external-services/{id}/metrics/alert-rules/{rule_id}
//! DEL  /external-services/{id}/metrics/alert-rules/{rule_id}
//! PATCH /external-services/{id}/metrics/enable
//!
//! GET  /deployments/{id}/metrics
//! GET  /deployments/{id}/metrics/latest
//! PATCH /deployments/{id}/metrics/enable
//!
//! GET  /nodes/{id}/metrics
//! ```
//!
//! The `range` query param accepts `"1h"` | `"6h"` | `"24h"` | `"7d"`.
//! An optional `percentile` param (0–100) switches the range query into
//! histogram-percentile mode using `service_metrics_histogram`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, put},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::{bad_request, forbidden, internal_server_error, not_found, ErrorBuilder},
    problemdetails::Problem,
    UtcDateTime,
};
use temps_entities::{external_services, monitoring_alert_rules};
use temps_metrics::{
    duration_to_step, is_monotonic_counter, range_to_step, LatestByLabelQuery, LatestQuery,
    RangeQuery, SourceKind,
};
use tracing::{error, info};
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::types::AppState;

// `is_monotonic_counter` / `range_to_step` live in `temps_metrics` (shared
// with the deployment container metrics handlers) — imported below.

// ---------------------------------------------------------------------------
// Helper: Prometheus-compatible histogram_quantile (linear interpolation).
// ---------------------------------------------------------------------------

/// Compute a histogram quantile via linear interpolation (same algorithm as
/// Prometheus `histogram_quantile()`).
///
/// This is exposed for future use when `MetricsStore::query_range_histogram()`
/// lands (tracked in issue #12).  Currently used only in tests.
#[allow(dead_code)]
fn histogram_quantile(q: f64, bounds: &[f64], counts: &[u64]) -> f64 {
    if bounds.is_empty() || counts.is_empty() {
        return f64::NAN;
    }

    let total: u64 = counts.iter().sum();
    if total == 0 {
        return f64::NAN;
    }

    let target = q * total as f64;
    let mut cumulative = 0u64;

    for (i, &count) in counts.iter().enumerate() {
        cumulative += count;
        if cumulative as f64 >= target {
            // Linear interpolation within this bucket.
            let lower_bound = if i == 0 { 0.0_f64 } else { bounds[i - 1] };
            let upper_bound = bounds[i];
            let lower_count = cumulative - count;
            let fraction = if count == 0 {
                0.0
            } else {
                (target - lower_count as f64) / count as f64
            };
            return lower_bound + fraction * (upper_bound - lower_bound);
        }
    }

    // All counts exhausted — return the last bound as the maximum.
    *bounds.last().unwrap_or(&f64::NAN)
}

// ---------------------------------------------------------------------------
// Request / response DTOs
// ---------------------------------------------------------------------------

/// Query params for range metric queries.
#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct MetricsRangeQuery {
    /// Metric name, e.g. `"pg.connections_active"`.
    pub metric: String,
    /// Time window: `"1h"` | `"6h"` | `"24h"` | `"7d"`.
    /// Ignored when `start_time` and `end_time` are both set.
    #[serde(default = "default_range")]
    pub range: String,
    /// Optional histogram percentile (0–100).  When provided, the endpoint
    /// fetches histogram buckets and computes the requested quantile.
    pub percentile: Option<f64>,
    /// Explicit window start (ISO 8601). Must be paired with `end_time`.
    #[param(value_type = Option<String>, example = "2026-08-13T00:00:00Z")]
    #[schema(value_type = Option<String>)]
    pub start_time: Option<UtcDateTime>,
    /// Explicit window end (ISO 8601). Must be paired with `start_time`.
    #[param(value_type = Option<String>, example = "2026-08-13T12:00:00Z")]
    #[schema(value_type = Option<String>)]
    pub end_time: Option<UtcDateTime>,
}

fn default_range() -> String {
    "1h".to_string()
}

/// Resolve `(from, to, step)` from either an explicit `[start_time, end_time]`
/// pair or a preset `range`. Explicit bounds win when both are present.
///
/// `step` is always server-derived via `duration_to_step` — callers cannot
/// request 1-minute buckets over a 7-day window.
fn resolve_range_window(
    params: &MetricsRangeQuery,
) -> Result<(DateTime<Utc>, DateTime<Utc>, Duration), Problem> {
    match (params.start_time, params.end_time) {
        (Some(start), Some(end)) => {
            if start >= end {
                return Err(bad_request()
                    .detail("start_time must be before end_time")
                    .build());
            }
            let span = end - start;
            let max = Duration::days(temps_core::time_window::MAX_WINDOW_DAYS);
            if span > max {
                return Err(bad_request()
                    .detail(format!(
                        "Requested time range spans {} days, which exceeds the {}-day maximum for this endpoint. Older data is still available — request it {} days at a time by moving start_time/end_time back.",
                        span.num_days().max(1),
                        temps_core::time_window::MAX_WINDOW_DAYS,
                        temps_core::time_window::MAX_WINDOW_DAYS
                    ))
                    .build());
            }
            Ok((start, end, duration_to_step(span)))
        }
        (None, None) => {
            let (window, step) = range_to_step(&params.range);
            let now = Utc::now();
            Ok((now - window, now, step))
        }
        _ => Err(bad_request()
            .detail("start_time and end_time must both be provided")
            .build()),
    }
}

/// A single `(timestamp, value)` data point in a metric series.
#[derive(Debug, Serialize, ToSchema)]
pub struct MetricDataPoint {
    /// ISO 8601 timestamp with `Z` suffix.
    pub time: String,
    /// Metric value at this bucket.
    pub value: f64,
}

/// Request body to toggle metric collection for an external service.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ToggleServiceMetricsRequest {
    /// Whether to enable (`true`) or disable (`false`) metric collection.
    pub enabled: bool,
}

/// Request body to toggle OTLP metric ingestion for a deployment.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ToggleDeploymentMetricsRequest {
    /// Whether to enable (`true`) or disable (`false`) metric ingestion.
    pub enabled: bool,
    /// Prometheus scrape port (optional).
    pub port: Option<u16>,
    /// Prometheus scrape path (optional, defaults to `/metrics`).
    pub path: Option<String>,
}

/// Wire representation of a monitoring alert rule.
///
/// Registered under a domain-prefixed OpenAPI schema name to avoid colliding
/// with `temps-error-tracking`'s unrelated `AlertRuleResponse` (utoipa keys
/// schemas by their bare struct name, so without `as = ...` the last crate to
/// register would silently shadow this one in the merged spec / generated SDK).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[schema(as = ServiceAlertRuleResponse)]
pub struct AlertRuleResponse {
    pub id: i32,
    pub service_id: Option<i32>,
    pub deployment_id: Option<i32>,
    /// Node the rule is scoped to. `0` is the synthetic control-plane node
    /// (see `CONTROL_PLANE_NODE_ID`), which owns the `proxy.*` and `node.*`
    /// rules. Exactly one of `service_id`/`deployment_id`/`node_id` is set.
    pub node_id: Option<i32>,
    pub name: String,
    pub metric_name: String,
    pub threshold: f64,
    pub comparator: String,
    pub severity: String,
    pub for_duration_secs: i32,
    pub enabled: bool,
    pub silenced_until: Option<String>,
}

impl From<monitoring_alert_rules::Model> for AlertRuleResponse {
    fn from(m: monitoring_alert_rules::Model) -> Self {
        Self {
            id: m.id,
            service_id: m.service_id,
            deployment_id: m.deployment_id,
            node_id: m.node_id,
            name: m.name,
            metric_name: m.metric_name,
            threshold: m.threshold,
            comparator: m.comparator,
            severity: m.severity,
            for_duration_secs: m.for_duration_secs,
            enabled: m.enabled,
            silenced_until: m.silenced_until.map(|t| t.to_rfc3339()),
        }
    }
}

/// Request body for creating an alert rule on an external service.
///
/// Domain-prefixed schema name — see [`AlertRuleResponse`] for why.
#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = ServiceCreateAlertRuleRequest)]
pub struct CreateAlertRuleRequest {
    pub name: String,
    pub metric_name: String,
    pub threshold: f64,
    /// One of `>`, `<`, `>=`, `<=`.
    pub comparator: String,
    /// `"warning"` or `"critical"`.
    pub severity: String,
    /// Seconds the breach must persist before the alarm fires (0 = immediate).
    #[serde(default)]
    pub for_duration_secs: i32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Request body for updating an existing alert rule.
///
/// Domain-prefixed schema name — see [`AlertRuleResponse`] for why.
#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = ServiceUpdateAlertRuleRequest)]
pub struct UpdateAlertRuleRequest {
    pub name: Option<String>,
    pub metric_name: Option<String>,
    pub threshold: Option<f64>,
    pub comparator: Option<String>,
    pub severity: Option<String>,
    pub for_duration_secs: Option<i32>,
    pub enabled: Option<bool>,
}

// ---------------------------------------------------------------------------
// External Services — metric range query
// ---------------------------------------------------------------------------

/// Fetch a time-series range for a single metric on an external service.
///
/// Pass `percentile` to compute a histogram quantile instead of a plain
/// gauge/counter average.
#[utoipa::path(
    get,
    path = "/external-services/{id}/metrics",
    operation_id = "ExternalServiceMetricsGetRange",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "External service ID"),
        MetricsRangeQuery,
    ),
    responses(
        (status = 200, description = "Metric time series data points", body = Vec<MetricDataPoint>),
        (status = 400, description = "Invalid query parameters"),
        (status = 401, description = "Unauthorized"),
        (status = 503, description = "Metrics store not available"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_service_metrics_range(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(params): Query<MetricsRangeQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    // SECURITY(metrics-security-6): verify the service belongs to the caller
    // before returning any metric data.
    assert_service_owned_by_caller(id, &auth, &state).await?;

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this server")
            .build()
    })?;

    let (from, to, step) = resolve_range_window(&params)?;

    let query = RangeQuery {
        source_kind: SourceKind::Database,
        source_id: id,
        monotonic: is_monotonic_counter(&params.metric),
        name: params.metric.clone(),
        from,
        to,
        step,
    };

    let points = store.query_range(query).await.map_err(|e| {
        error!(service_id = id, metric = %params.metric, error = %e, "Failed to query metric range");
        internal_server_error()
            .detail(format!("Failed to query metrics: {}", e))
            .build()
    })?;

    let response: Vec<MetricDataPoint> = points
        .into_iter()
        .map(|(ts, v)| MetricDataPoint {
            time: ts.to_rfc3339(),
            value: v,
        })
        .collect();

    Ok((StatusCode::OK, Json(response)))
}

// ---------------------------------------------------------------------------
// External Services — latest values
// ---------------------------------------------------------------------------

/// Fetch the most-recent value for every tracked metric on an external service.
#[utoipa::path(
    get,
    path = "/external-services/{id}/metrics/latest",
    operation_id = "ExternalServiceMetricsGetLatest",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "External service ID"),
    ),
    responses(
        (status = 200, description = "Map of metric name to latest value", body = HashMap<String, f64>),
        (status = 401, description = "Unauthorized"),
        (status = 503, description = "Metrics store not available"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_service_metrics_latest(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    // SECURITY(metrics-security-6): verify the service belongs to the caller.
    assert_service_owned_by_caller(id, &auth, &state).await?;

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this server")
            .build()
    })?;

    // Check that metrics are enabled for this specific service.
    let service = external_services::Entity::find_by_id(id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| internal_server_error().detail(e.to_string()).build())?
        .ok_or_else(|| not_found().detail("Service not found").build())?;

    if !service.metrics_enabled {
        return Err(ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this service")
            .build());
    }

    // Empty names list → the store returns all known metrics for this source.
    let query = LatestQuery {
        source_kind: SourceKind::Database,
        source_id: id,
        names: vec![],
    };

    let values = store.query_latest(query).await.map_err(|e| {
        error!(service_id = id, error = %e, "Failed to query latest metrics");
        internal_server_error()
            .detail(format!("Failed to query latest metrics: {}", e))
            .build()
    })?;

    Ok((StatusCode::OK, Json(values)))
}

/// Freshness status: when metrics were last received for this service.
#[derive(Debug, Serialize, ToSchema)]
pub struct MetricsStatusResponse {
    /// ISO 8601 timestamp of the most recent metric row, or null if none yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_received_at: Option<String>,
}

/// Return the freshness status (last-received timestamp) for a service.
///
/// Cheap O(1) lookup against `service_metrics_status` — used by the UI to show
/// "last received at …" without scanning the metrics hypertable.
#[utoipa::path(
    get,
    path = "/external-services/{id}/metrics/status",
    operation_id = "ExternalServiceMetricsStatus",
    tag = "Metrics",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "Metrics freshness status", body = MetricsStatusResponse),
        (status = 503, description = "Metrics not available"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_service_metrics_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    assert_service_owned_by_caller(id, &auth, &state).await?;

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this server")
            .build()
    })?;

    let last = store
        .latest_timestamp(SourceKind::Database, id)
        .await
        .map_err(|e| {
            error!(service_id = id, error = %e, "Failed to query metrics status");
            internal_server_error()
                .detail(format!("Failed to query metrics status: {}", e))
                .build()
        })?;

    Ok((
        StatusCode::OK,
        Json(MetricsStatusResponse {
            last_received_at: last.map(|t| t.to_rfc3339()),
        }),
    ))
}

/// Per-database metric values for a Postgres service.
///
/// A Postgres instance can host many databases (some unrelated to this
/// service). The collector records per-`datname` series; this groups the
/// latest value of each requested metric by database so the UI can render a
/// "Databases" breakdown table instead of one collapsed number.
#[derive(Debug, Serialize, ToSchema)]
pub struct DatabaseMetricsRow {
    /// Database name (`datname`).
    pub database: String,
    /// Latest value of each requested metric for this database
    /// (e.g. `{"pg.database_size_bytes": 7943871, "pg.cache_hit_ratio": 0.99}`).
    pub metrics: HashMap<String, f64>,
}

/// Response for the per-database metrics breakdown.
#[derive(Debug, Serialize, ToSchema)]
pub struct DatabaseMetricsResponse {
    /// One entry per database, sorted by the first metric descending
    /// (largest first) so the biggest database leads the table.
    pub databases: Vec<DatabaseMetricsRow>,
}

/// Metrics surfaced per database in the breakdown, in display order.
/// All are gauges/counters that `pg_stat_database` / `pg_database_size` emit
/// once per `datname`.
// Must stay in sync with the web `PG_PER_DATABASE_GROUPS` list in
// ServiceMonitoring.tsx — every metric the per-database section displays has to
// be requested here, or the UI shows "—" for the missing ones.
const PER_DATABASE_METRICS: &[&str] = &[
    "pg.database_size_bytes",
    "pg.cache_hit_ratio",
    "pg.tuple_fetch_ratio",
    "pg.commits_total",
    "pg.rollbacks_total",
    "pg.deadlocks_total",
    "pg.temp_files_total",
    "pg.temp_bytes_total",
    "pg.tuples_inserted_total",
    "pg.tuples_updated_total",
    "pg.tuples_deleted_total",
];

/// Return the latest per-database metric values for a Postgres service.
///
/// Groups `pg_stat_database` / size metrics by `datname` so the UI can show a
/// breakdown table (each database with its own size, cache-hit ratio, etc.)
/// rather than collapsing every database into one value.
#[utoipa::path(
    get,
    path = "/external-services/{id}/metrics/by-database",
    operation_id = "ExternalServiceMetricsByDatabase",
    tag = "Metrics",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "Per-database metric breakdown", body = DatabaseMetricsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 503, description = "Metrics not available"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_service_metrics_by_database(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    assert_service_owned_by_caller(id, &auth, &state).await?;

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this server")
            .build()
    })?;

    let rows = store
        .query_latest_by_label(LatestByLabelQuery {
            source_kind: SourceKind::Database,
            source_id: id,
            names: PER_DATABASE_METRICS.iter().map(|s| s.to_string()).collect(),
            label_key: "datname".to_string(),
        })
        .await
        .map_err(|e| {
            error!(service_id = id, error = %e, "Failed to query per-database metrics");
            internal_server_error()
                .detail(format!("Failed to query per-database metrics: {}", e))
                .build()
        })?;

    // Fold the flat (database, metric, value) list into one map per database.
    let mut by_db: std::collections::BTreeMap<String, HashMap<String, f64>> =
        std::collections::BTreeMap::new();
    for r in rows {
        by_db
            .entry(r.label_value)
            .or_default()
            .insert(r.name, r.value);
    }

    let mut databases: Vec<DatabaseMetricsRow> = by_db
        .into_iter()
        .map(|(database, metrics)| DatabaseMetricsRow { database, metrics })
        .collect();

    // Largest database first (by the headline size metric); fall back to name.
    databases.sort_by(|a, b| {
        let av = a
            .metrics
            .get("pg.database_size_bytes")
            .copied()
            .unwrap_or(0.0);
        let bv = b
            .metrics
            .get("pg.database_size_bytes")
            .copied()
            .unwrap_or(0.0);
        bv.partial_cmp(&av)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.database.cmp(&b.database))
    });

    Ok((StatusCode::OK, Json(DatabaseMetricsResponse { databases })))
}

// ---------------------------------------------------------------------------
// External Services — alert rules
// ---------------------------------------------------------------------------

/// List all monitoring alert rules for an external service.
#[utoipa::path(
    get,
    path = "/external-services/{id}/metrics/alert-rules",
    operation_id = "ExternalServiceMetricsGetAlertRules",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "External service ID"),
    ),
    responses(
        (status = 200, description = "List of alert rules", body = Vec<AlertRuleResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_service_alert_rules(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    // SECURITY(metrics-security-6): verify the service belongs to the caller.
    assert_service_owned_by_caller(id, &auth, &state).await?;

    let rules = monitoring_alert_rules::Entity::find()
        .filter(monitoring_alert_rules::Column::ServiceId.eq(id))
        .all(state.db.as_ref())
        .await
        .map_err(|e| {
            error!(service_id = id, error = %e, "Failed to list alert rules");
            internal_server_error()
                .detail(format!("Failed to list alert rules: {}", e))
                .build()
        })?;

    let response: Vec<AlertRuleResponse> = rules.into_iter().map(AlertRuleResponse::from).collect();
    Ok((StatusCode::OK, Json(response)))
}

/// Create a monitoring alert rule for an external service.
///
/// If metric collection is enabled and the service engine has default rules,
/// seeding is idempotent (ON CONFLICT DO NOTHING).
#[utoipa::path(
    post,
    path = "/external-services/{id}/metrics/alert-rules",
    operation_id = "ExternalServiceMetricsCreateAlertRule",
    tag = "Metrics",
    request_body = CreateAlertRuleRequest,
    params(
        ("id" = i32, Path, description = "External service ID"),
    ),
    responses(
        (status = 201, description = "Alert rule created", body = AlertRuleResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn create_service_alert_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Json(request): Json<CreateAlertRuleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    // SECURITY(metrics-security-6): verify the service belongs to the caller.
    assert_service_owned_by_caller(id, &auth, &state).await?;

    validate_comparator(&request.comparator)?;
    validate_severity(&request.severity)?;

    // SECURITY(metrics-security-1): validate metric_name against allowlist to
    // prevent SQL injection via user-supplied metric names in alert rules.
    temps_metrics::store::timescale::validate_metric_name(&request.metric_name).map_err(|_| {
        bad_request()
            .detail(format!(
                "metric_name '{}' contains invalid characters; \
                 only [a-zA-Z0-9_.:−] are allowed",
                request.metric_name
            ))
            .build()
    })?;

    let active_model = monitoring_alert_rules::ActiveModel {
        service_id: Set(Some(id)),
        deployment_id: Set(None),
        name: Set(request.name.clone()),
        metric_name: Set(request.metric_name.clone()),
        threshold: Set(request.threshold),
        comparator: Set(request.comparator.clone()),
        severity: Set(request.severity.clone()),
        for_duration_secs: Set(request.for_duration_secs),
        enabled: Set(request.enabled),
        silenced_until: Set(None),
        ..Default::default()
    };

    let rule = active_model.insert(state.db.as_ref()).await.map_err(|e| {
        error!(service_id = id, error = %e, "Failed to create alert rule");
        internal_server_error()
            .detail(format!("Failed to create alert rule: {}", e))
            .build()
    })?;

    Ok((StatusCode::CREATED, Json(AlertRuleResponse::from(rule))))
}

/// Update an existing monitoring alert rule for an external service.
#[utoipa::path(
    put,
    path = "/external-services/{id}/metrics/alert-rules/{rule_id}",
    operation_id = "ExternalServiceMetricsUpdateAlertRule",
    tag = "Metrics",
    request_body = UpdateAlertRuleRequest,
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("rule_id" = i32, Path, description = "Alert rule ID"),
    ),
    responses(
        (status = 200, description = "Updated alert rule", body = AlertRuleResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Alert rule not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn update_service_alert_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((id, rule_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateAlertRuleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    // SECURITY(metrics-security-6): verify the service belongs to the caller.
    assert_service_owned_by_caller(id, &auth, &state).await?;

    if let Some(ref comp) = request.comparator {
        validate_comparator(comp)?;
    }
    if let Some(ref sev) = request.severity {
        validate_severity(sev)?;
    }
    // SECURITY(metrics-security-1): validate updated metric_name if provided.
    if let Some(ref mn) = request.metric_name {
        temps_metrics::store::timescale::validate_metric_name(mn).map_err(|_| {
            bad_request()
                .detail(format!(
                    "metric_name '{}' contains invalid characters; \
                     only [a-zA-Z0-9_.:−] are allowed",
                    mn
                ))
                .build()
        })?;
    }

    let rule = monitoring_alert_rules::Entity::find_by_id(rule_id)
        .filter(monitoring_alert_rules::Column::ServiceId.eq(id))
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!(rule_id, service_id = id, error = %e, "Failed to load alert rule");
            internal_server_error()
                .detail(format!("Failed to load alert rule: {}", e))
                .build()
        })?
        .ok_or_else(|| not_found().detail("Alert rule not found").build())?;

    let mut active: monitoring_alert_rules::ActiveModel = rule.into();
    if let Some(name) = request.name {
        active.name = Set(name);
    }
    if let Some(metric_name) = request.metric_name {
        active.metric_name = Set(metric_name);
    }
    if let Some(threshold) = request.threshold {
        active.threshold = Set(threshold);
    }
    if let Some(comparator) = request.comparator {
        active.comparator = Set(comparator);
    }
    if let Some(severity) = request.severity {
        active.severity = Set(severity);
    }
    if let Some(for_duration_secs) = request.for_duration_secs {
        active.for_duration_secs = Set(for_duration_secs);
    }
    if let Some(enabled) = request.enabled {
        active.enabled = Set(enabled);
    }

    let updated = active.update(state.db.as_ref()).await.map_err(|e| {
        error!(rule_id, service_id = id, error = %e, "Failed to update alert rule");
        internal_server_error()
            .detail(format!("Failed to update alert rule: {}", e))
            .build()
    })?;

    Ok((StatusCode::OK, Json(AlertRuleResponse::from(updated))))
}

/// Delete a monitoring alert rule for an external service.
#[utoipa::path(
    delete,
    path = "/external-services/{id}/metrics/alert-rules/{rule_id}",
    operation_id = "ExternalServiceMetricsDeleteAlertRule",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("rule_id" = i32, Path, description = "Alert rule ID"),
    ),
    responses(
        (status = 204, description = "Alert rule deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Alert rule not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn delete_service_alert_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((id, rule_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    // SECURITY(metrics-security-6): verify the service belongs to the caller.
    assert_service_owned_by_caller(id, &auth, &state).await?;

    let result = monitoring_alert_rules::Entity::delete_many()
        .filter(monitoring_alert_rules::Column::Id.eq(rule_id))
        .filter(monitoring_alert_rules::Column::ServiceId.eq(id))
        .exec(state.db.as_ref())
        .await
        .map_err(|e| {
            error!(rule_id, service_id = id, error = %e, "Failed to delete alert rule");
            internal_server_error()
                .detail(format!("Failed to delete alert rule: {}", e))
                .build()
        })?;

    if result.rows_affected == 0 {
        return Err(not_found().detail("Alert rule not found").build());
    }

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// External Services — enable/disable metric collection
// ---------------------------------------------------------------------------

/// Enable or disable metric collection for an external service.
///
/// When `enabled=true`, seeds the default alert rules for the service's engine
/// via [`temps_monitoring::seed_default_rules`] (idempotent).
#[utoipa::path(
    patch,
    path = "/external-services/{id}/metrics/enable",
    operation_id = "ExternalServiceMetricsToggle",
    tag = "Metrics",
    request_body = ToggleServiceMetricsRequest,
    params(
        ("id" = i32, Path, description = "External service ID"),
    ),
    responses(
        (status = 200, description = "Metrics toggle applied"),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn toggle_service_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Json(request): Json<ToggleServiceMetricsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    // SECURITY(metrics-security-6): verify the service belongs to the caller
    // before mutating it. Without this, any ExternalServicesWrite holder could
    // toggle monitoring on another tenant's service — and enabling it
    // provisions a new `si_` ingest key and restarts that service's container.
    assert_service_owned_by_caller(id, &auth, &state).await?;

    if request.enabled {
        // Look up the service engine so we can seed the right default rules.
        let service = state
            .external_service_manager
            .get_service(id)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("not found") {
                    not_found().detail("Service not found").build()
                } else {
                    internal_server_error()
                        .detail(format!("Failed to load service: {}", e))
                        .build()
                }
            })?;

        if let Err(e) =
            temps_monitoring::seed_default_rules(state.db.as_ref(), id, &service.service_type).await
        {
            // Non-fatal — the user can create rules manually. Log but continue.
            error!(
                service_id = id,
                engine = %service.service_type,
                error = %e,
                "Failed to seed default alert rules; continuing"
            );
        }

        // For OTLP-push services (RustFS): provision an si_ ingest key and
        // restart the container so the OTLP env vars take effect.
        // Both "rustfs" and "s3" service types use RustFS containers and
        // support OTLP push — provision an ingest key for either.
        if matches!(
            service.service_type.to_lowercase().as_str(),
            "rustfs" | "s3"
        ) {
            provision_otlp_ingest_key(&state, &service, auth.user_id()).await;
        }
    }

    // Persist the metrics_enabled flag on the service row.
    let active = external_services::ActiveModel {
        id: Set(id),
        metrics_enabled: Set(request.enabled),
        ..Default::default()
    };
    active.update(state.db.as_ref()).await.map_err(|e| {
        error!(service_id = id, error = %e, "Failed to update metrics_enabled");
        internal_server_error()
            .detail(format!("Failed to update service: {}", e))
            .build()
    })?;

    Ok(StatusCode::OK)
}

/// Provision a `si_` metrics ingest key for an OTLP-push service and apply it.
///
/// Non-fatal — failures are logged but do not fail the metrics-enable request.
/// The user can retry by toggling metrics off and on again.
///
/// Also called by the create-service handler so newly-created OTLP-push services
/// (rustfs/s3) come up with metrics already wired, matching `metrics_enabled`
/// defaulting to `true` at creation.
pub(crate) async fn provision_otlp_ingest_key(
    state: &AppState,
    service: &external_services::Model,
    user_id: i32,
) {
    // Generate the si_ key tied to this service.
    let ingest_key = match state
        .api_key_service
        .create_service_ingest_key(service.id, &service.name, user_id)
        .await
    {
        Ok(k) => k,
        Err(e) => {
            error!(
                service_id = service.id,
                service_name = %service.name,
                error = %e,
                "Failed to create metrics ingest key; OTLP push will not work"
            );
            return;
        }
    };

    // Resolve the internal URL containers use to reach the Temps API. Falls
    // back to the host.docker.internal default with the console port when no
    // ConfigService is wired.
    let internal_url = match &state.config_service {
        Some(cfg) => cfg.resolve_internal_url().await,
        None => "http://host.docker.internal:8080".to_string(),
    };

    // Persist the key + ingest URL into the encrypted config, then restart.
    if let Err(e) = state
        .external_service_manager
        .store_and_apply_ingest_key(service.id, ingest_key, internal_url)
        .await
    {
        error!(
            service_id = service.id,
            service_name = %service.name,
            error = %e,
            "Failed to apply ingest key; key was created but not injected into container"
        );
    } else {
        tracing::info!(
            service_id = service.id,
            service_name = %service.name,
            "OTLP metrics ingest key applied and container restarted"
        );
    }
}

// ---------------------------------------------------------------------------
// Deployments — metric range query
// ---------------------------------------------------------------------------

/// Fetch a time-series range for a single metric on a deployment.
#[utoipa::path(
    get,
    path = "/deployments/{id}/metrics",
    operation_id = "DeploymentMetricsGetRange",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "Deployment ID"),
        MetricsRangeQuery,
    ),
    responses(
        (status = 200, description = "Metric time series data points", body = Vec<MetricDataPoint>),
        (status = 400, description = "Invalid query parameters"),
        (status = 401, description = "Unauthorized"),
        (status = 503, description = "Metrics store not available"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_deployment_metrics_range(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(params): Query<MetricsRangeQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    assert_deployment_owned_by_caller(id, &auth, &state).await?;

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this server")
            .build()
    })?;

    let (from, to, step) = resolve_range_window(&params)?;

    let query = RangeQuery {
        source_kind: SourceKind::Deployment,
        source_id: id,
        monotonic: is_monotonic_counter(&params.metric),
        name: params.metric.clone(),
        from,
        to,
        step,
    };

    let points = store.query_range(query).await.map_err(|e| {
        error!(deployment_id = id, metric = %params.metric, error = %e, "Failed to query deployment metric range");
        internal_server_error()
            .detail(format!("Failed to query metrics: {}", e))
            .build()
    })?;

    let response: Vec<MetricDataPoint> = points
        .into_iter()
        .map(|(ts, v)| MetricDataPoint {
            time: ts.to_rfc3339(),
            value: v,
        })
        .collect();

    Ok((StatusCode::OK, Json(response)))
}

// ---------------------------------------------------------------------------
// Deployments — latest values
// ---------------------------------------------------------------------------

/// Fetch the most-recent metric values for a deployment.
#[utoipa::path(
    get,
    path = "/deployments/{id}/metrics/latest",
    operation_id = "DeploymentMetricsGetLatest",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "Deployment ID"),
    ),
    responses(
        (status = 200, description = "Map of metric name to latest value", body = HashMap<String, f64>),
        (status = 401, description = "Unauthorized"),
        (status = 503, description = "Metrics store not available"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_deployment_metrics_latest(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    assert_deployment_owned_by_caller(id, &auth, &state).await?;

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this server")
            .build()
    })?;

    let query = LatestQuery {
        source_kind: SourceKind::Deployment,
        source_id: id,
        names: vec![],
    };

    let values = store.query_latest(query).await.map_err(|e| {
        error!(deployment_id = id, error = %e, "Failed to query latest deployment metrics");
        internal_server_error()
            .detail(format!("Failed to query latest metrics: {}", e))
            .build()
    })?;

    Ok((StatusCode::OK, Json(values)))
}

// ---------------------------------------------------------------------------
// Deployments — enable/disable OTLP metric ingestion
// ---------------------------------------------------------------------------

/// Enable or disable OTLP metric ingestion for a deployment.
///
/// When `enabled=true`, seeds the default container alert rules for the
/// deployment via [`temps_monitoring::seed_default_container_rules`] (idempotent).
#[utoipa::path(
    patch,
    path = "/deployments/{id}/metrics/enable",
    operation_id = "DeploymentMetricsToggle",
    tag = "Metrics",
    request_body = ToggleDeploymentMetricsRequest,
    params(
        ("id" = i32, Path, description = "Deployment ID"),
    ),
    responses(
        (status = 200, description = "Metrics toggle applied"),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn toggle_deployment_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Json(request): Json<ToggleDeploymentMetricsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    assert_deployment_owned_by_caller(id, &auth, &state).await?;

    if request.enabled {
        if let Err(e) = temps_monitoring::seed_default_container_rules(state.db.as_ref(), id).await
        {
            error!(
                deployment_id = id,
                error = %e,
                "Failed to seed default container alert rules; continuing"
            );
        }
    }

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Nodes — metric range query
// ---------------------------------------------------------------------------

/// Fetch a time-series range for a single metric on a node.
#[utoipa::path(
    get,
    path = "/nodes/{id}/metrics",
    operation_id = "NodeMetricsGetRange",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "Node ID"),
        MetricsRangeQuery,
    ),
    responses(
        (status = 200, description = "Metric time series data points", body = Vec<MetricDataPoint>),
        (status = 400, description = "Invalid query parameters"),
        (status = 401, description = "Unauthorized"),
        (status = 503, description = "Metrics store not available"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_node_metrics_range(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(params): Query<MetricsRangeQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Metrics Unavailable")
            .detail("Metric collection is not enabled on this server")
            .build()
    })?;

    let (from, to, step) = resolve_range_window(&params)?;

    let query = RangeQuery {
        source_kind: SourceKind::Node,
        source_id: id,
        monotonic: is_monotonic_counter(&params.metric),
        name: params.metric.clone(),
        from,
        to,
        step,
    };

    let points = store.query_range(query).await.map_err(|e| {
        error!(node_id = id, metric = %params.metric, error = %e, "Failed to query node metric range");
        internal_server_error()
            .detail(format!("Failed to query metrics: {}", e))
            .build()
    })?;

    let response: Vec<MetricDataPoint> = points
        .into_iter()
        .map(|(ts, v)| MetricDataPoint {
            time: ts.to_rfc3339(),
            value: v,
        })
        .collect();

    Ok((StatusCode::OK, Json(response)))
}

// ---------------------------------------------------------------------------
// Nodes — alert rules
// ---------------------------------------------------------------------------

/// List the monitoring alert rules scoped to a node.
///
/// Node `0` is the synthetic control-plane node, which owns the seeded
/// `proxy.*` (error rate, p99 latency) and `node.*` (file-descriptor /
/// socket exhaustion) defaults. Without this endpoint those rules exist only
/// in the database and the operator has no way to see that they are watching,
/// let alone retune or silence them.
#[utoipa::path(
    get,
    path = "/nodes/{id}/metrics/alert-rules",
    operation_id = "NodeMetricsGetAlertRules",
    tag = "Metrics",
    params(
        ("id" = i32, Path, description = "Node ID (0 = control plane)"),
    ),
    responses(
        (status = 200, description = "List of node alert rules", body = Vec<AlertRuleResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_node_alert_rules(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    // Node metrics are platform-level, not project-scoped — same guard as
    // `get_node_metrics_range`.
    permission_guard!(auth, SettingsRead);

    let rules = monitoring_alert_rules::Entity::find()
        .filter(monitoring_alert_rules::Column::NodeId.eq(id))
        .order_by_asc(monitoring_alert_rules::Column::MetricName)
        .all(state.db.as_ref())
        .await
        .map_err(|e| {
            error!(node_id = id, error = %e, "Failed to list node alert rules");
            internal_server_error()
                .detail(format!("Failed to list node alert rules: {}", e))
                .build()
        })?;

    let response: Vec<AlertRuleResponse> = rules.into_iter().map(AlertRuleResponse::from).collect();
    Ok((StatusCode::OK, Json(response)))
}

/// Update a node-scoped monitoring alert rule.
///
/// Deliberately update-only: there is no delete. The control-plane defaults
/// are re-seeded on every startup (`ON CONFLICT DO NOTHING`), so a deleted
/// rule would silently reappear on the next restart, whereas `enabled: false`
/// survives re-seeding — disabling is the operation that actually means
/// "stop alerting on this".
#[utoipa::path(
    patch,
    path = "/nodes/{id}/metrics/alert-rules/{rule_id}",
    operation_id = "NodeMetricsUpdateAlertRule",
    tag = "Metrics",
    request_body = UpdateAlertRuleRequest,
    params(
        ("id" = i32, Path, description = "Node ID (0 = control plane)"),
        ("rule_id" = i32, Path, description = "Alert rule ID"),
    ),
    responses(
        (status = 200, description = "Updated alert rule", body = AlertRuleResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Alert rule not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
async fn update_node_alert_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((id, rule_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateAlertRuleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    if let Some(ref comp) = request.comparator {
        validate_comparator(comp)?;
    }
    if let Some(ref sev) = request.severity {
        validate_severity(sev)?;
    }
    // SECURITY(metrics-security-1): validate updated metric_name if provided —
    // metric names reach the metrics store's query builder.
    if let Some(ref mn) = request.metric_name {
        temps_metrics::store::timescale::validate_metric_name(mn).map_err(|_| {
            bad_request()
                .detail(format!(
                    "metric_name '{}' contains invalid characters; \
                     only [a-zA-Z0-9_.:−] are allowed",
                    mn
                ))
                .build()
        })?;
    }

    let rule = monitoring_alert_rules::Entity::find_by_id(rule_id)
        .filter(monitoring_alert_rules::Column::NodeId.eq(id))
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!(rule_id, node_id = id, error = %e, "Failed to load node alert rule");
            internal_server_error()
                .detail(format!("Failed to load node alert rule: {}", e))
                .build()
        })?
        .ok_or_else(|| {
            not_found()
                .detail(format!("Alert rule {rule_id} not found on node {id}"))
                .build()
        })?;

    let mut active: monitoring_alert_rules::ActiveModel = rule.into();
    if let Some(name) = request.name {
        active.name = Set(name);
    }
    if let Some(metric_name) = request.metric_name {
        active.metric_name = Set(metric_name);
    }
    if let Some(threshold) = request.threshold {
        active.threshold = Set(threshold);
    }
    if let Some(comparator) = request.comparator {
        active.comparator = Set(comparator);
    }
    if let Some(severity) = request.severity {
        active.severity = Set(severity);
    }
    if let Some(for_duration_secs) = request.for_duration_secs {
        active.for_duration_secs = Set(for_duration_secs);
    }
    if let Some(enabled) = request.enabled {
        active.enabled = Set(enabled);
    }

    let updated = active.update(state.db.as_ref()).await.map_err(|e| {
        error!(rule_id, node_id = id, error = %e, "Failed to update node alert rule");
        internal_server_error()
            .detail(format!("Failed to update node alert rule: {}", e))
            .build()
    })?;

    info!(
        rule_id,
        node_id = id,
        metric_name = %updated.metric_name,
        threshold = updated.threshold,
        enabled = updated.enabled,
        "Node alert rule updated"
    );

    Ok((StatusCode::OK, Json(AlertRuleResponse::from(updated))))
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_comparator(comp: &str) -> Result<(), Problem> {
    match comp {
        ">" | "<" | ">=" | "<=" => Ok(()),
        _ => Err(bad_request()
            .detail("comparator must be one of: >, <, >=, <=")
            .build()),
    }
}

fn validate_severity(sev: &str) -> Result<(), Problem> {
    match sev {
        "warning" | "critical" => Ok(()),
        _ => Err(bad_request()
            .detail("severity must be one of: warning, critical")
            .build()),
    }
}

// ---------------------------------------------------------------------------
// Authorization helpers
// ---------------------------------------------------------------------------

/// Verify that `service_id` belongs to the requesting user's project.
///
/// # SECURITY(metrics-security-6): IDOR on metrics endpoints
///
/// Without this check, any authenticated user with `ExternalServicesRead`
/// permission can fetch metrics for any service ID — including services owned
/// by other projects/tenants.  This violates row-level access control.
///
/// The check joins `project_services` against the service ID.  If no row
/// matches, it means either:
/// - The service does not exist (return 404), or
/// - The service exists but is not linked to the user's project (return 404
///   — do not distinguish these cases to avoid leaking service existence).
///
/// For deployment tokens, the token's bound `project_id` is checked directly
/// against `project_services`. For session/API-key/CLI callers there is no
/// single bound project, so ownership is expressed through team-based access:
/// the caller must be able to access at least one of the projects the
/// service is linked to, via the [`temps_core::ProjectAccessChecker`]
/// extension point (ADR-028) — a no-op in OSS (no team-access concept
/// exists), real enforcement in EE when a team-access checker is registered.
/// This mirrors [`require_service_parameter_project_access`] in
/// `handlers.rs`, which enforces the same rule for the credential-reveal
/// endpoint.
///
/// Returns `Ok(())` if the caller may access the service, or `Err(Problem)`
/// with a 404 (service not found / not linked to caller's project) or 403
/// (linked, but caller's team has no grant on any of its projects).
pub(crate) async fn assert_service_owned_by_caller(
    service_id: i32,
    auth: &temps_auth::AuthContext,
    state: &AppState,
) -> Result<(), Problem> {
    use temps_entities::project_services;

    // For deployment tokens, check against the token's bound project.
    if let Some(token_project_id) = auth.project_id() {
        let linked = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id))
            .filter(project_services::Column::ProjectId.eq(token_project_id))
            .one(state.db.as_ref())
            .await
            .map_err(|e| {
                error!(service_id, error = %e, "assert_service_owned: DB error");
                internal_server_error()
                    .detail("Failed to verify service ownership")
                    .build()
            })?;

        if linked.is_none() {
            return Err(not_found()
                .detail(format!("External service {} not found", service_id))
                .build());
        }
        return Ok(());
    }

    // Session/API-key/CLI caller: verify the service exists (same check as
    // before this fix), then resolve which projects it's linked to for the
    // access decision below.
    //
    // Deliberately a direct `project_services` query, not
    // `ExternalServiceManager::list_service_projects` — that also fetches
    // full project metadata with a `projects` table lookup per linked row,
    // which is unneeded N+1 cost here (only the IDs matter) on a path now
    // shared by every session/API-key/CLI request across 30+ handlers.
    let service_exists = state
        .external_service_manager
        .get_service(service_id)
        .await
        .is_ok();
    if !service_exists {
        return Err(not_found()
            .detail(format!("External service {} not found", service_id))
            .build());
    }

    let project_ids: Vec<i32> = project_services::Entity::find()
        .filter(project_services::Column::ServiceId.eq(service_id))
        .all(state.db.as_ref())
        .await
        .map_err(|e| {
            error!(service_id, error = %e, "assert_service_owned: failed to resolve linked projects");
            internal_server_error()
                .detail("Failed to verify service ownership")
                .build()
        })?
        .into_iter()
        .map(|link| link.project_id)
        .collect();

    session_caller_may_access_linked_projects(
        auth,
        &project_ids,
        state.project_access_checker.as_deref(),
    )
    .await
}

/// Pure authorization decision for the session/API-key/CLI branch of
/// [`assert_service_owned_by_caller`]: given the projects a service is
/// linked to, may this caller access it?
///
/// * Instance-wide Admin/PlatformAdmin bypasses, matching the documented
///   contract of [`temps_core::ProjectAccessChecker`] (implementations are
///   not required to duplicate that check themselves).
/// * No checker registered (plain OSS, or EE before any team-access grant
///   exists) → allow. This "fail-open-when-unconfigured" behaviour is a
///   documented property of the extension point, not a bug: it's what keeps
///   an EE binary with no grants configured yet behaving identically to OSS.
/// * Checker registered → allow iff the caller can access at least one of
///   the linked projects.
///
/// Extracted as its own function — taking already-resolved data instead of
/// `AppState` — so the decision is unit-testable without a database round
/// trip, the same reasoning as [`deployment_visible_to_caller`] below.
async fn session_caller_may_access_linked_projects(
    auth: &temps_auth::AuthContext,
    project_ids: &[i32],
    checker: Option<&dyn temps_core::ProjectAccessChecker>,
) -> Result<(), Problem> {
    if auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin) {
        return Ok(());
    }

    match checker {
        Some(checker) => {
            require_access_to_any_linked_project(auth.user_id(), project_ids, checker).await
        }
        None => Ok(()),
    }
}

/// Allow iff `user_id` can access at least one of `project_ids`, per the
/// registered [`temps_core::ProjectAccessChecker`]. Fails closed (denies) on
/// any infrastructure error from the checker — a checker that can't verify
/// access must never be treated as "access granted".
///
/// Shared with the credential-reveal path
/// (`handlers::require_service_parameter_project_access`), which enforces
/// the identical rule for a different endpoint.
pub(crate) async fn require_access_to_any_linked_project(
    user_id: i32,
    project_ids: &[i32],
    checker: &dyn temps_core::ProjectAccessChecker,
) -> Result<(), Problem> {
    let mut infrastructure_error = None;
    for project_id in project_ids {
        match checker.user_can_access_project(user_id, *project_id).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => infrastructure_error = Some(error),
        }
    }

    if let Some(error) = infrastructure_error {
        error!(user_id, error = %error, "Project access check failed");
        return Err(internal_server_error()
            .title("Project Access Check Failed")
            .detail("Could not verify project access; please try again")
            .build());
    }

    Err(forbidden()
        .title("Project Access Denied")
        .detail("Your team membership does not include access to this service")
        .build())
}

/// Pure authorization policy: may a caller see/act on a deployment?
///
/// `caller_project_id` is the project a deployment token is bound to
/// (`AuthContext::project_id()`), or `None` for a session user.
///
/// * `None` (session user) — visible. For the current single-project model a
///   session user may access any project; the strict multi-tenant
///   `project_members` check is tracked in the FIXME on
///   `assert_service_owned_by_caller`.
/// * `Some(pid)` (deployment token) — visible only if the deployment lives in
///   that bound project.
///
/// Extracted as a pure fn so the security rule is unit-testable without
/// constructing an `AppState`.
fn deployment_visible_to_caller(
    deployment_project_id: i32,
    caller_project_id: Option<i32>,
) -> bool {
    match caller_project_id {
        None => true,
        Some(pid) => deployment_project_id == pid,
    }
}

/// Verify the caller is allowed to act on the given deployment.
///
/// SECURITY(metrics-security-6): deployment metrics handlers take a deployment
/// `{id}` from the path and check a `Deployments*` permission, but without this
/// they never verify the deployment belongs to the caller — letting any holder
/// of the permission read or toggle metrics on another tenant's deployment by
/// changing the URL id (IDOR). Mirrors `assert_service_owned_by_caller`.
///
/// Returns 404 (not 403) when the deployment isn't visible to the caller, so we
/// don't leak the existence of other tenants' deployments.
async fn assert_deployment_owned_by_caller(
    deployment_id: i32,
    auth: &temps_auth::AuthContext,
    state: &AppState,
) -> Result<(), Problem> {
    use temps_entities::deployments;

    let deployment = deployments::Entity::find_by_id(deployment_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!(deployment_id, error = %e, "assert_deployment_owned: DB error");
            internal_server_error()
                .detail("Failed to verify deployment ownership")
                .build()
        })?;

    let Some(deployment) = deployment else {
        return Err(not_found()
            .detail(format!("Deployment {} not found", deployment_id))
            .build());
    };

    if !deployment_visible_to_caller(deployment.project_id, auth.project_id()) {
        return Err(not_found()
            .detail(format!("Deployment {} not found", deployment_id))
            .build());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Route builder
// ---------------------------------------------------------------------------

pub fn configure_metrics_routes() -> Router<Arc<AppState>> {
    Router::new()
        // External service metrics
        .route(
            "/external-services/{id}/metrics",
            get(get_service_metrics_range),
        )
        .route(
            "/external-services/{id}/metrics/latest",
            get(get_service_metrics_latest),
        )
        .route(
            "/external-services/{id}/metrics/status",
            get(get_service_metrics_status),
        )
        .route(
            "/external-services/{id}/metrics/by-database",
            get(get_service_metrics_by_database),
        )
        .route(
            "/external-services/{id}/metrics/alert-rules",
            get(list_service_alert_rules).post(create_service_alert_rule),
        )
        .route(
            "/external-services/{id}/metrics/alert-rules/{rule_id}",
            put(update_service_alert_rule).delete(delete_service_alert_rule),
        )
        .route(
            "/external-services/{id}/metrics/enable",
            patch(toggle_service_metrics),
        )
        // Deployment metrics
        .route(
            "/deployments/{id}/metrics",
            get(get_deployment_metrics_range),
        )
        .route(
            "/deployments/{id}/metrics/latest",
            get(get_deployment_metrics_latest),
        )
        .route(
            "/deployments/{id}/metrics/enable",
            patch(toggle_deployment_metrics),
        )
        // Node metrics
        .route("/nodes/{id}/metrics", get(get_node_metrics_range))
        .route(
            "/nodes/{id}/metrics/alert-rules",
            get(list_node_alert_rules),
        )
        .route(
            "/nodes/{id}/metrics/alert-rules/{rule_id}",
            patch(update_node_alert_rule),
        )
}

// ---------------------------------------------------------------------------
// OpenAPI schema contribution
// ---------------------------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    paths(
        get_service_metrics_range,
        get_service_metrics_latest,
        get_service_metrics_status,
        get_service_metrics_by_database,
        list_service_alert_rules,
        create_service_alert_rule,
        update_service_alert_rule,
        delete_service_alert_rule,
        toggle_service_metrics,
        get_deployment_metrics_range,
        get_deployment_metrics_latest,
        toggle_deployment_metrics,
        get_node_metrics_range,
        list_node_alert_rules,
        update_node_alert_rule,
    ),
    components(schemas(
        MetricDataPoint,
        MetricsRangeQuery,
        MetricsStatusResponse,
        DatabaseMetricsRow,
        DatabaseMetricsResponse,
        AlertRuleResponse,
        CreateAlertRuleRequest,
        UpdateAlertRuleRequest,
        ToggleServiceMetricsRequest,
        ToggleDeploymentMetricsRequest,
    )),
    info(
        title = "Metrics API",
        description = "Time-series metric queries and alert rule management for external services, \
                        deployments, and nodes.",
        version = "1.0.0"
    ),
    tags(
        (name = "Metrics", description = "Metrics query and alert rule endpoints")
    )
)]
pub struct MetricsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_histogram_quantile_p50() {
        // Simple 3-bucket histogram: [0,1), [1,2), [2,3)
        let bounds = vec![1.0, 2.0, 3.0];
        let counts = vec![10u64, 20, 10];
        let q50 = histogram_quantile(0.5, &bounds, &counts);
        // 50% of 40 = 20 — falls inside [1,2)
        assert!(
            (1.0..=2.0).contains(&q50),
            "p50 should be in [1,2), got {}",
            q50
        );
    }

    #[test]
    fn test_histogram_quantile_empty_returns_nan() {
        let result = histogram_quantile(0.99, &[], &[]);
        assert!(result.is_nan());
    }

    #[test]
    fn test_histogram_quantile_all_zero_counts_returns_nan() {
        let result = histogram_quantile(0.5, &[1.0, 2.0], &[0, 0]);
        assert!(result.is_nan());
    }

    fn range_query(start: DateTime<Utc>, end: DateTime<Utc>) -> MetricsRangeQuery {
        MetricsRangeQuery {
            metric: "node.cpu_percent".into(),
            range: "1h".into(),
            percentile: None,
            start_time: Some(start),
            end_time: Some(end),
        }
    }

    #[test]
    fn custom_seven_day_window_uses_hourly_step() {
        let start = DateTime::parse_from_rfc3339("2026-08-06T00:00:00Z")
            .expect("valid fixture")
            .with_timezone(&Utc);
        let end = start + Duration::days(7);
        let (_, _, step) = resolve_range_window(&range_query(start, end)).expect("within cap");
        assert_eq!(step, Duration::hours(1));
        let points = (end - start).num_seconds() / step.num_seconds().max(1);
        assert!(points <= temps_core::time_window::MAX_SERIES_POINTS);
    }

    #[test]
    fn custom_one_hour_window_uses_minute_step() {
        let start = DateTime::parse_from_rfc3339("2026-08-13T11:00:00Z")
            .expect("valid fixture")
            .with_timezone(&Utc);
        let end = start + Duration::hours(1);
        let (_, _, step) = resolve_range_window(&range_query(start, end)).expect("within cap");
        assert_eq!(step, Duration::minutes(1));
    }

    #[test]
    fn test_validate_comparator_valid() {
        assert!(validate_comparator(">").is_ok());
        assert!(validate_comparator("<").is_ok());
        assert!(validate_comparator(">=").is_ok());
        assert!(validate_comparator("<=").is_ok());
    }

    #[test]
    fn test_validate_comparator_invalid() {
        assert!(validate_comparator("!=").is_err());
        assert!(validate_comparator("==").is_err());
        assert!(validate_comparator("").is_err());
    }

    #[test]
    fn test_validate_severity_valid() {
        assert!(validate_severity("warning").is_ok());
        assert!(validate_severity("critical").is_ok());
    }

    #[test]
    fn test_validate_severity_invalid() {
        assert!(validate_severity("info").is_err());
        assert!(validate_severity("").is_err());
    }

    // ── deployment ownership policy (SECURITY metrics-security-6 / IDOR) ───────
    //
    // `deployment_visible_to_caller` is the pure policy behind
    // `assert_deployment_owned_by_caller`. The IDOR fix hinges on a
    // deployment-token caller only seeing deployments in its own bound project,
    // so these assert that rule directly (no AppState needed).

    #[test]
    fn deployment_token_can_see_its_own_project_deployment() {
        // Token bound to project 7, deployment lives in project 7 → visible.
        assert!(deployment_visible_to_caller(7, Some(7)));
    }

    #[test]
    fn deployment_token_cannot_see_other_project_deployment() {
        // Token bound to project 7, deployment lives in project 8 → IDOR blocked.
        assert!(!deployment_visible_to_caller(8, Some(7)));
        // And the symmetric case, to be explicit.
        assert!(!deployment_visible_to_caller(7, Some(8)));
    }

    #[test]
    fn session_user_can_see_any_deployment() {
        // No bound project (session user) → visible under the current
        // single-project model, regardless of the deployment's project.
        assert!(deployment_visible_to_caller(1, None));
        assert!(deployment_visible_to_caller(999, None));
    }

    // ── session-caller project access (SECURITY metrics-security-6) ────────
    //
    // `session_caller_may_access_linked_projects` is the pure policy behind
    // the session/API-key/CLI branch of `assert_service_owned_by_caller`.
    // Unlike deployment tokens (checked above via a direct project_services
    // row match), these callers have no single bound project, so ownership
    // is expressed through the `ProjectAccessChecker` EE extension point —
    // a no-op in OSS, real team-based enforcement in EE.

    struct TestProjectAccessChecker {
        allowed_project_ids: Vec<i32>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for TestProjectAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            if self.fail {
                return Err("checker infrastructure failure".into());
            }
            Ok(self.allowed_project_ids.contains(&project_id))
        }
    }

    fn test_session_auth(role: temps_auth::Role) -> temps_auth::AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 1,
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
        };
        temps_auth::AuthContext::new_session(user, role)
    }

    #[tokio::test]
    async fn session_user_allowed_with_no_checker_registered() {
        // OSS has no team-access concept: an unregistered checker must
        // fail open, identical to the pre-fix behaviour (existence-only).
        let result = session_caller_may_access_linked_projects(
            &test_session_auth(temps_auth::Role::User),
            &[10, 11],
            None,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn session_admin_bypasses_a_denying_checker() {
        let checker = TestProjectAccessChecker {
            allowed_project_ids: vec![],
            fail: false,
        };
        let result = session_caller_may_access_linked_projects(
            &test_session_auth(temps_auth::Role::Admin),
            &[10, 11],
            Some(&checker),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn session_user_denied_when_checker_grants_no_linked_project() {
        // This is the actual EE-enforced fix: a non-admin member whose team
        // has no grant on any project the service is linked to is denied,
        // instead of the pre-fix behaviour of "any session user may access
        // any service that exists".
        let checker = TestProjectAccessChecker {
            allowed_project_ids: vec![99],
            fail: false,
        };
        let problem = session_caller_may_access_linked_projects(
            &test_session_auth(temps_auth::Role::User),
            &[10, 11],
            Some(&checker),
        )
        .await
        .expect_err("no linked project is accessible — must deny");
        assert_eq!(problem.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn session_user_allowed_when_checker_grants_one_linked_project() {
        let checker = TestProjectAccessChecker {
            allowed_project_ids: vec![11],
            fail: false,
        };
        let result = session_caller_may_access_linked_projects(
            &test_session_auth(temps_auth::Role::User),
            &[10, 11],
            Some(&checker),
        )
        .await;
        assert!(result.is_ok());
    }

    // ── OpenAPI registration ───────────────────────────────────────────────
    //
    // `MetricsApiDoc` is NOT the document the server serves. The plugin
    // contributes `ExternalServiceApiDoc` (handlers.rs), which re-lists every
    // metrics operation by hand. A handler added to `MetricsApiDoc` alone
    // compiles, routes, and answers requests — but is absent from
    // `/api-docs/openapi.json`, so `bun run openapi-ts` generates no binding
    // for it and neither the console nor the CLI can call it. That is a silent
    // failure with no compile error and no runtime error; these tests are the
    // only thing that catches it.

    /// `operation_id`s declared by an OpenAPI document, keyed by the path they
    /// sit on so a failure message points at the endpoint, not just the name.
    fn operation_keys(doc: &utoipa::openapi::OpenApi) -> std::collections::BTreeSet<String> {
        doc.paths
            .paths
            .iter()
            .flat_map(|(path, item)| {
                [
                    &item.get,
                    &item.put,
                    &item.post,
                    &item.delete,
                    &item.patch,
                    &item.head,
                    &item.options,
                    &item.trace,
                ]
                .into_iter()
                .flatten()
                .filter_map(move |op| op.operation_id.as_ref().map(|id| format!("{id} ({path})")))
            })
            .collect()
    }

    #[test]
    fn served_openapi_doc_declares_every_metrics_operation() {
        use utoipa::OpenApi as _;

        let metrics = operation_keys(&MetricsApiDoc::openapi());
        let served = operation_keys(&crate::handlers::handlers::ExternalServiceApiDoc::openapi());

        let missing: Vec<_> = metrics.difference(&served).cloned().collect();
        assert!(
            missing.is_empty(),
            "these metrics operations are routed but absent from the served OpenAPI \
             document (ExternalServiceApiDoc in handlers.rs), so no SDK binding is \
             generated for them: {missing:?}"
        );
    }

    #[test]
    fn node_alert_rule_endpoints_are_in_the_served_openapi_doc() {
        use utoipa::OpenApi as _;

        let served = operation_keys(&crate::handlers::handlers::ExternalServiceApiDoc::openapi());
        for expected in [
            "NodeMetricsGetAlertRules (/nodes/{id}/metrics/alert-rules)",
            "NodeMetricsUpdateAlertRule (/nodes/{id}/metrics/alert-rules/{rule_id})",
        ] {
            assert!(
                served.contains(expected),
                "{expected} missing from the served OpenAPI document; \
                 declared operations: {served:?}"
            );
        }
    }

    #[tokio::test]
    async fn session_user_denied_when_checker_infrastructure_fails() {
        // Fail-closed: a checker that cannot verify access must never be
        // treated as "access granted".
        let checker = TestProjectAccessChecker {
            allowed_project_ids: vec![],
            fail: true,
        };
        let problem = session_caller_may_access_linked_projects(
            &test_session_auth(temps_auth::Role::User),
            &[10],
            Some(&checker),
        )
        .await
        .expect_err("checker infrastructure failure must deny, not allow");
        assert_eq!(
            problem.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
