//! CRUD handlers for first-class metric alert rules.
//!
//! Authenticated via the standard `RequireAuth` flow (JWT/session) since these
//! are managed by the Temps dashboard UI, not by OTel collectors. GET uses the
//! `OtelRead` permission; writes use `OtelWrite` and are audit-logged best-effort.
//! All by-id endpoints are scoped by `project_id` to prevent cross-tenant IDOR.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use tracing::{error, warn};
use utoipa::ToSchema;

use crate::detectors::DetectionConfig;
use crate::error::OtelError;
use crate::handlers::audit::{
    OtelMetricAlertCreatedAudit, OtelMetricAlertDeletedAudit, OtelMetricAlertUpdatedAudit,
};
use crate::services::anomaly_preview::compute_anomaly_preview;
use crate::services::metric_alert_evaluator::SeriesStateEntry;
use crate::OtelAppState;
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, ProblemDetails, RequestMetadata};
use temps_entities::metric_alert_rules::Model;

// ── Request DTOs ────────────────────────────────────────────────────

/// Default for `max_series` when a create request omits it (ADR-026 Phase 3).
fn default_max_series() -> i32 {
    20
}

/// Default for `grouped_notification_threshold` when a create request omits it.
/// Matches the value that was previously a hardcoded evaluator constant.
fn default_grouped_notification_threshold() -> i32 {
    5
}

#[derive(Debug, Deserialize)]
pub struct ListMetricAlertsParams {
    pub project_id: i32,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMetricAlertRequest {
    pub project_id: i32,
    pub name: String,
    pub metric_name: String,
    /// One of `avg|sum|min|max|count|rate|p50|p90|p95|p99`.
    pub aggregation: String,
    /// The detector: a discriminated union keyed by `kind`. Today only
    /// `{ "kind": "static", "comparator": "gt", "threshold": 500 }` is evaluable.
    pub detection_config: DetectionConfig,
    pub window_secs: i32,
    pub for_duration_secs: i32,
    /// One of `info|warning|critical`.
    pub severity: String,
    pub enabled: bool,
    /// AND-combined label equality filters: `[["key","value"],…]`. Empty = no
    /// filtering (the default). Max 10 pairs; keys must match `[a-zA-Z0-9_.:-]`;
    /// values capped at 500 characters.
    #[serde(default)]
    pub label_filters: Vec<(String, String)>,
    /// Label keys to break the metric down by, e.g. `["endpoint","region"]`. Empty
    /// (the default) = one aggregate stream. Max 2 keys; keys must match
    /// `[a-zA-Z0-9_.:-]`.
    #[serde(default)]
    pub group_by: Vec<String>,
    /// When true (and `group_by` is set) fire one independent alarm per breaching
    /// series. Static detectors only. Default false.
    #[serde(default)]
    pub dynamic_alerts: bool,
    /// Cardinality cap for dynamic alerting: at most this many series (top by
    /// `|value|`). Range 1–100, default 20.
    #[serde(default = "default_max_series")]
    pub max_series: i32,
    /// When more than this many series transition to firing in the same tick, only
    /// the first gets the expensive chart/AI enrichment. Range 1–1000, default 5.
    #[serde(default = "default_grouped_notification_threshold")]
    pub grouped_notification_threshold: i32,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMetricAlertRequest {
    pub name: Option<String>,
    pub metric_name: Option<String>,
    pub aggregation: Option<String>,
    /// Replaces the detector wholesale when present (absent = leave unchanged).
    pub detection_config: Option<DetectionConfig>,
    pub window_secs: Option<i32>,
    pub for_duration_secs: Option<i32>,
    pub severity: Option<String>,
    pub enabled: Option<bool>,
    /// Replaces the label filters wholesale when present (absent = leave unchanged).
    pub label_filters: Option<Vec<(String, String)>>,
    /// Replaces the group_by keys wholesale when present (absent = leave unchanged).
    pub group_by: Option<Vec<String>>,
    /// Toggles per-series ("dynamic") alerting (absent = leave unchanged).
    pub dynamic_alerts: Option<bool>,
    /// Updates the dynamic-alerting cardinality cap (absent = leave unchanged).
    pub max_series: Option<i32>,
    /// Updates the notification-grouping threshold (absent = leave unchanged).
    pub grouped_notification_threshold: Option<i32>,
}

/// Query params scoping a by-id alert operation to a project. Required on
/// get/update/delete so a caller cannot touch another project's rule by guessing
/// its id (cross-tenant IDOR).
#[derive(Debug, Deserialize)]
pub struct MetricAlertScopeParams {
    pub project_id: i32,
}

// ── Response DTOs ───────────────────────────────────────────────────

/// A single currently-firing series for a dynamic alert rule, snapshotted from
/// the evaluator's in-memory per-series firing map at read time (ADR-026 Phase 3).
#[derive(Debug, Serialize, ToSchema)]
pub struct FiringSeriesEntry {
    /// The series' label pairs, e.g. `[["endpoint","/checkout"],["region","eu-west"]]`.
    pub series_key: Vec<(String, String)>,
    /// The human-readable joined label, e.g. `endpoint=/checkout, region=eu-west`.
    pub series_label: String,
    /// The open alarm's id, when one was created (absent if suppressed).
    pub alarm_id: Option<i32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OtelMetricAlertRuleResponse {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub metric_name: String,
    pub aggregation: String,
    /// Coarse detector discriminator: `static|anomaly|forecast|outlier|auto_watch`.
    pub detection_kind: String,
    /// The typed detector definition (discriminated union keyed by `kind`).
    pub detection_config: DetectionConfig,
    pub window_secs: i32,
    pub for_duration_secs: i32,
    pub severity: String,
    pub enabled: bool,
    /// One of `ok|firing|unknown`.
    pub last_state: String,
    pub last_value: Option<f64>,
    /// AND-combined label equality filters applied when evaluating this rule.
    /// Empty = no filtering (matches all series).
    pub label_filters: Vec<(String, String)>,
    /// Label keys the rule breaks the metric down by. Empty = one aggregate stream.
    pub group_by: Vec<String>,
    /// Whether per-series ("dynamic") alerting is enabled for this rule.
    pub dynamic_alerts: bool,
    /// Cardinality cap for dynamic alerting (1–100).
    pub max_series: i32,
    /// Notification-grouping threshold: when more than this many series fire in the
    /// same tick, only the first gets chart/AI enrichment (1–1000).
    pub grouped_notification_threshold: i32,
    /// Number of series dropped by the cardinality cap on the latest dynamic tick
    /// (0 when nothing was dropped or for static/aggregate rules). Lets a UI warn
    /// "N series were dropped this tick" without reading server logs.
    pub last_dropped_series_count: i32,
    /// Full per-series state snapshot persisted after the latest dynamic-rule tick,
    /// keyed by the human-readable series label (`endpoint=/checkout`). Empty for
    /// static/aggregate rules. Unlike `firing_series` (a live in-memory snapshot),
    /// this is decoded from the persisted `series_states` jsonb column, so an
    /// external consumer that only reads the rule row still sees per-series detail.
    pub series_states: std::collections::HashMap<String, SeriesStateEntry>,
    /// Currently-firing series for a dynamic rule, snapshotted from the evaluator's
    /// in-memory firing map at read time. Empty for static/aggregate rules or when
    /// nothing is firing.
    #[serde(default)]
    pub firing_series: Vec<FiringSeriesEntry>,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub last_evaluated_at: Option<String>,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub updated_at: String,
}

impl From<Model> for OtelMetricAlertRuleResponse {
    fn from(model: Model) -> Self {
        // The stored blob is always valid (every write round-trips through the
        // typed enum); a decode failure means DB corruption — log and fall back
        // to a default static config so the API stays typed and serving.
        let detection_config =
            DetectionConfig::from_value(&model.detection_config).unwrap_or_else(|e| {
                warn!(
                    rule_id = model.id,
                    error = %e,
                    "metric_alert_rules.detection_config failed to decode; using default"
                );
                DetectionConfig::default_static()
            });
        let label_filters: Vec<(String, String)> =
            serde_json::from_value(model.label_filters).unwrap_or_default();
        let group_by: Vec<String> = serde_json::from_value(model.group_by).unwrap_or_default();
        // Decoded from the persisted jsonb the same way as label_filters/group_by;
        // malformed/legacy jsonb decodes to an empty map rather than failing.
        let series_states: std::collections::HashMap<String, SeriesStateEntry> =
            serde_json::from_value(model.series_states).unwrap_or_default();
        Self {
            id: model.id,
            project_id: model.project_id,
            name: model.name,
            metric_name: model.metric_name,
            aggregation: model.aggregation,
            detection_kind: model.detection_kind,
            detection_config,
            window_secs: model.window_secs,
            for_duration_secs: model.for_duration_secs,
            severity: model.severity,
            enabled: model.enabled,
            last_state: model.last_state,
            last_value: model.last_value,
            label_filters,
            group_by,
            dynamic_alerts: model.dynamic_alerts,
            max_series: model.max_series,
            grouped_notification_threshold: model.grouped_notification_threshold,
            last_dropped_series_count: model.last_dropped_series_count,
            series_states,
            // Populated by the read handlers from the evaluator's in-memory
            // snapshot (`firing_series_for`); empty by default so create/update
            // (which just mutated config) don't imply a firing state.
            firing_series: Vec::new(),
            last_evaluated_at: model.last_evaluated_at.map(|d| d.to_rfc3339()),
            created_at: model.created_at.to_rfc3339(),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OtelMetricAlertsResponse {
    pub data: Vec<OtelMetricAlertRuleResponse>,
    pub total: u64,
}

// ── Anomaly preview / backtest ──────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct AnomalyPreviewRequest {
    pub project_id: i32,
    pub metric_name: String,
    /// One of `avg|sum|min|max|count|rate|p50|p90|p95|p99`.
    pub aggregation: String,
    pub window_secs: i32,
    /// Must be an `anomaly` detector — the band to backtest.
    pub detection_config: DetectionConfig,
    /// RFC 3339; defaults to 7 days before `end_time`.
    #[schema(example = "2025-10-12T12:15:47Z")]
    pub start_time: Option<String>,
    /// RFC 3339; defaults to now.
    #[schema(example = "2025-10-12T12:15:47Z")]
    pub end_time: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnomalyPreviewPointResponse {
    #[schema(example = "2025-10-12T12:15:47Z")]
    pub bucket: String,
    pub value: f64,
    /// Lower edge of the expected band at this point.
    pub lower: f64,
    /// Upper edge of the expected band at this point.
    pub upper: f64,
    pub breaching: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnomalyPreviewResponse {
    pub points: Vec<AnomalyPreviewPointResponse>,
    /// How many points in the range would have fired.
    pub breach_count: i64,
    /// Baseline sample count (drives the `sufficient` flag).
    pub baseline_samples: i64,
    /// Whether the baseline had enough history for a trustworthy band.
    pub sufficient: bool,
}

fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Snapshot the evaluator's in-memory per-series firing map for `rule_id` into
/// response DTOs. Reads an in-memory map (no DB), so it is cheap to call per rule.
/// Labels are sorted by key so they match the per-series alarm titles.
async fn firing_series_entries(state: &OtelAppState, rule_id: i32) -> Vec<FiringSeriesEntry> {
    state
        .metric_alert_evaluator
        .firing_series_for(rule_id)
        .await
        .into_iter()
        .map(|(series_key, alarm_id)| {
            let mut pairs: Vec<&(String, String)> = series_key.iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let series_label = pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            FiringSeriesEntry {
                series_key,
                series_label,
                alarm_id: Some(alarm_id),
            }
        })
        .collect()
}

// ── Handlers ────────────────────────────────────────────────────────

/// List alert rules for a project (newest first, paginated).
#[utoipa::path(
    tag = "Alerts",
    get,
    path = "/otel/alerts",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)"),
    ),
    responses(
        (status = 200, description = "Alert rules for the project", body = OtelMetricAlertsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_alerts(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<ListMetricAlertsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    project_access_guard!(auth, params.project_id, state.project_access_checker);

    let (items, total) = state
        .metric_alert_service
        .list(params.project_id, params.page, params.page_size)
        .await?;

    let mut data = Vec::with_capacity(items.len());
    for item in items {
        let rule_id = item.id;
        let mut resp = OtelMetricAlertRuleResponse::from(item);
        resp.firing_series = firing_series_entries(&state, rule_id).await;
        data.push(resp);
    }
    Ok(Json(OtelMetricAlertsResponse { data, total }))
}

/// Create a new alert rule for a project.
#[utoipa::path(
    tag = "Alerts",
    post,
    path = "/otel/alerts",
    request_body = CreateMetricAlertRequest,
    responses(
        (status = 201, description = "Alert rule created", body = OtelMetricAlertRuleResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_alert(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateMetricAlertRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);
    project_access_guard!(auth, request.project_id, state.project_access_checker);

    let model = state
        .metric_alert_service
        .create(
            request.project_id,
            request.name,
            request.metric_name,
            request.aggregation,
            request.detection_config,
            request.window_secs,
            request.for_duration_secs,
            request.severity,
            request.enabled,
            request.label_filters,
            request.group_by,
            request.dynamic_alerts,
            request.max_series,
            request.grouped_notification_threshold,
        )
        .await?;

    let audit = OtelMetricAlertCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        rule_id: model.id,
        project_id: model.project_id,
        name: model.name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((
        StatusCode::CREATED,
        Json(OtelMetricAlertRuleResponse::from(model)),
    ))
}

/// Fetch a single alert rule by id.
#[utoipa::path(
    tag = "Alerts",
    get,
    path = "/otel/alerts/{id}",
    params(
        ("id" = i32, Path, description = "Alert rule ID"),
        ("project_id" = i32, Query, description = "Owning project ID (scopes the lookup)"),
    ),
    responses(
        (status = 200, description = "Alert rule", body = OtelMetricAlertRuleResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Alert rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_alert(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(id): Path<i32>,
    Query(scope): Query<MetricAlertScopeParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    project_access_guard!(auth, scope.project_id, state.project_access_checker);

    let model = state.metric_alert_service.get(scope.project_id, id).await?;
    let mut resp = OtelMetricAlertRuleResponse::from(model);
    resp.firing_series = firing_series_entries(&state, id).await;
    Ok(Json(resp))
}

/// Update an alert rule's fields.
#[utoipa::path(
    tag = "Alerts",
    patch,
    path = "/otel/alerts/{id}",
    params(
        ("id" = i32, Path, description = "Alert rule ID"),
        ("project_id" = i32, Query, description = "Owning project ID (scopes the update)"),
    ),
    request_body = UpdateMetricAlertRequest,
    responses(
        (status = 200, description = "Alert rule updated", body = OtelMetricAlertRuleResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Alert rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_alert(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Query(scope): Query<MetricAlertScopeParams>,
    Json(request): Json<UpdateMetricAlertRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);
    project_access_guard!(auth, scope.project_id, state.project_access_checker);

    let model = state
        .metric_alert_service
        .update(
            scope.project_id,
            id,
            request.name,
            request.metric_name,
            request.aggregation,
            request.detection_config,
            request.window_secs,
            request.for_duration_secs,
            request.severity,
            request.enabled,
            request.label_filters,
            request.group_by,
            request.dynamic_alerts,
            request.max_series,
            request.grouped_notification_threshold,
        )
        .await?;

    let audit = OtelMetricAlertUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        rule_id: model.id,
        project_id: model.project_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let rule_id = model.id;
    let mut resp = OtelMetricAlertRuleResponse::from(model);
    resp.firing_series = firing_series_entries(&state, rule_id).await;
    Ok(Json(resp))
}

/// Delete an alert rule.
#[utoipa::path(
    tag = "Alerts",
    delete,
    path = "/otel/alerts/{id}",
    params(
        ("id" = i32, Path, description = "Alert rule ID"),
        ("project_id" = i32, Query, description = "Owning project ID (scopes the delete)"),
    ),
    responses(
        (status = 204, description = "Alert rule deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Alert rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_alert(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Query(scope): Query<MetricAlertScopeParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);
    project_access_guard!(auth, scope.project_id, state.project_access_checker);

    // Verify ownership FIRST (404s if `id` isn't in `scope.project_id`): the
    // evaluator's in-memory maps below are keyed only by `rule_id`, not
    // `project_id`, so calling resolve_all_for_rule before this check would let
    // a caller with OtelWrite on their own project wipe another project's
    // per-series firing state just by guessing a foreign rule_id.
    let rule = state.metric_alert_service.get(scope.project_id, id).await?;

    // Resolve any open alarms (aggregate or per-series) and drop the evaluator's
    // in-memory state for this rule BEFORE removing the row: once the row is gone
    // the evaluator never runs for it again, so an open alarm would be orphaned as
    // permanently `firing`. Fire-and-forget — never blocks or fails the delete.
    state
        .metric_alert_evaluator
        .resolve_all_for_rule(rule.id, rule.project_id)
        .await;

    state
        .metric_alert_service
        .delete(scope.project_id, id)
        .await?;

    let audit = OtelMetricAlertDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        rule_id: id,
        project_id: scope.project_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Backtest an anomaly detector over a time range without saving a rule.
///
/// Replays the metric against the same band the evaluator would use, returning
/// the per-bucket band + which points would have fired. Powers the form's
/// "would this have fired?" preview and the explorer band overlay. Read-only.
#[utoipa::path(
    tag = "Alerts",
    post,
    path = "/otel/alerts/preview",
    request_body = AnomalyPreviewRequest,
    responses(
        (status = 200, description = "Per-bucket band + breach points", body = AnomalyPreviewResponse),
        (status = 400, description = "Not an anomaly detector / bad input", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn preview_alert(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Json(req): Json<AnomalyPreviewRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    project_access_guard!(auth, req.project_id, state.project_access_checker);

    // Preview only makes sense for a band-based (anomaly) detector.
    let params = match &req.detection_config {
        DetectionConfig::Anomaly(p) => p.clone(),
        other => {
            return Err(OtelError::Validation {
                message: format!(
                    "preview is only available for anomaly detectors, not '{}'",
                    other.kind_str()
                ),
            }
            .into());
        }
    };

    let end = req
        .end_time
        .as_deref()
        .and_then(parse_rfc3339)
        .unwrap_or_else(chrono::Utc::now);
    let start = req
        .start_time
        .as_deref()
        .and_then(parse_rfc3339)
        .unwrap_or_else(|| end - chrono::Duration::days(7));

    let preview = compute_anomaly_preview(
        &state.otel_service,
        req.project_id,
        &req.metric_name,
        &req.aggregation,
        req.window_secs,
        &params,
        start,
        end,
    )
    .await?;

    let points = preview
        .points
        .into_iter()
        .map(|p| AnomalyPreviewPointResponse {
            bucket: p.bucket.to_rfc3339(),
            value: p.value,
            lower: p.lower,
            upper: p.upper,
            breaching: p.breaching,
        })
        .collect();

    Ok(Json(AnomalyPreviewResponse {
        points,
        breach_count: preview.breach_count,
        baseline_samples: preview.baseline_samples,
        sufficient: preview.sufficient,
    }))
}
