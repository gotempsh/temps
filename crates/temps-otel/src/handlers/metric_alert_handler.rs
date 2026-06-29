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
use crate::OtelAppState;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, ProblemDetails, RequestMetadata};
use temps_entities::metric_alert_rules::Model;

// ── Request DTOs ────────────────────────────────────────────────────

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
}

/// Query params scoping a by-id alert operation to a project. Required on
/// get/update/delete so a caller cannot touch another project's rule by guessing
/// its id (cross-tenant IDOR).
#[derive(Debug, Deserialize)]
pub struct MetricAlertScopeParams {
    pub project_id: i32,
}

// ── Response DTOs ───────────────────────────────────────────────────

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

// ── Handlers ────────────────────────────────────────────────────────

/// List alert rules for a project (newest first, paginated).
#[utoipa::path(
    tag = "OTel",
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

    let (items, total) = state
        .metric_alert_service
        .list(params.project_id, params.page, params.page_size)
        .await?;

    let data = items
        .into_iter()
        .map(OtelMetricAlertRuleResponse::from)
        .collect();
    Ok(Json(OtelMetricAlertsResponse { data, total }))
}

/// Create a new alert rule for a project.
#[utoipa::path(
    tag = "OTel",
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
    tag = "OTel",
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

    let model = state.metric_alert_service.get(scope.project_id, id).await?;
    Ok(Json(OtelMetricAlertRuleResponse::from(model)))
}

/// Update an alert rule's fields.
#[utoipa::path(
    tag = "OTel",
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

    Ok(Json(OtelMetricAlertRuleResponse::from(model)))
}

/// Delete an alert rule.
#[utoipa::path(
    tag = "OTel",
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
    tag = "OTel",
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
