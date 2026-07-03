//! Service for first-class, metric-centric alert rules.
//!
//! An alert rule is defined on a *signal* (project + metric + aggregation +
//! threshold), independent of any dashboard. Rules are Postgres config/metadata
//! (the `metric_alert_rules` table), so this service owns its own
//! `Arc<DatabaseConnection>` rather than going through the ClickHouse/TimescaleDB
//! `OtelStorage` trait used by `OtelService`.
//!
//! The background `MetricAlertEvaluator` drives evaluation; this service owns CRUD
//! plus two evaluator-support methods (`list_enabled`, `persist_evaluation`).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder,
};

use temps_entities::metric_alert_rules::{ActiveModel, Column, Entity, Model};

use crate::detectors::DetectionConfig;
use crate::error::OtelError;

const MAX_LABEL_FILTERS: usize = 10;
const MAX_LABEL_VALUE_LEN: usize = 500;

/// Max `group_by` keys (ADR-026 Phase 3). More than two is unreadable in the
/// per-series alarm label and multiplies cardinality; the dashboard tile applies
/// the same limit (Phase 2).
const MAX_GROUP_BY_KEYS: usize = 2;

/// Inclusive bounds on `max_series` (the dynamic-alerting cardinality cap). The
/// hard upper bound of 100 is the ADR's ceiling on per-rule alarm fan-out.
const MIN_MAX_SERIES: i32 = 1;
const MAX_MAX_SERIES: i32 = 100;

/// Inclusive bounds on `grouped_notification_threshold` (the notification-grouping
/// threshold for dynamic alerting). The hard ceiling of 1000 prevents nonsense
/// values while still comfortably exceeding the 100-series cardinality cap.
const MIN_GROUPED_THRESHOLD: i32 = 1;
const MAX_GROUPED_THRESHOLD: i32 = 1000;

/// The allowlisted aggregations a rule may request. Mirrors the keyword/quantile
/// forms accepted by `MetricAggregation::parse`.
pub const ALLOWED_AGGREGATIONS: &[&str] = &[
    "avg", "sum", "min", "max", "count", "rate", "p50", "p90", "p95", "p99",
];

/// The allowlisted severities, mapped to `AlarmSeverity` by the evaluator.
pub const ALLOWED_SEVERITIES: &[&str] = &["info", "warning", "critical"];

const MAX_NAME_LEN: usize = 200;
const MAX_METRIC_NAME_LEN: usize = 256;

/// Validate `label_filters` pairs: at most [`MAX_LABEL_FILTERS`] pairs, each
/// key passing the OTel metric-name character allowlist `[a-zA-Z0-9_.:-]`, each
/// value at most [`MAX_LABEL_VALUE_LEN`] characters.
fn validate_label_filters(filters: &[(String, String)]) -> Result<(), OtelError> {
    if filters.len() > MAX_LABEL_FILTERS {
        return Err(OtelError::Validation {
            message: format!(
                "Too many label filters ({}, max {MAX_LABEL_FILTERS})",
                filters.len()
            ),
        });
    }
    for (key, value) in filters {
        if temps_metrics::validate_metric_name(key).is_err() {
            return Err(OtelError::Validation {
                message: format!(
                    "Label filter key '{key}' contains characters outside the allowed set [a-zA-Z0-9_.:-]"
                ),
            });
        }
        if value.len() > MAX_LABEL_VALUE_LEN {
            return Err(OtelError::Validation {
                message: format!(
                    "Label filter value for key '{key}' exceeds {MAX_LABEL_VALUE_LEN} characters"
                ),
            });
        }
    }
    Ok(())
}

/// Validate `group_by` keys: at most [`MAX_GROUP_BY_KEYS`] keys, each passing the
/// same OTel metric-name character allowlist as `label_filters`.
fn validate_group_by(group_by: &[String]) -> Result<(), OtelError> {
    if group_by.len() > MAX_GROUP_BY_KEYS {
        return Err(OtelError::Validation {
            message: format!(
                "Too many group_by keys ({}, max {MAX_GROUP_BY_KEYS})",
                group_by.len()
            ),
        });
    }
    for key in group_by {
        if temps_metrics::validate_metric_name(key).is_err() {
            return Err(OtelError::Validation {
                message: format!(
                    "group_by key '{key}' contains characters outside the allowed set [a-zA-Z0-9_.:-]"
                ),
            });
        }
    }
    Ok(())
}

/// Validate the cross-cutting rule fields against the allowlists and sane bounds,
/// then delegate detector-specific validation to the typed [`DetectionConfig`].
///
/// Returns [`OtelError::Validation`] on the first invalid field so the caller
/// surfaces a 400 rather than persisting an un-evaluable rule.
#[allow(clippy::too_many_arguments)]
fn validate_rule(
    name: &str,
    metric_name: &str,
    aggregation: &str,
    severity: &str,
    window_secs: i32,
    for_duration_secs: i32,
    detection_config: &DetectionConfig,
    label_filters: &[(String, String)],
    group_by: &[String],
    dynamic_alerts: bool,
    max_series: i32,
    grouped_notification_threshold: i32,
) -> Result<(), OtelError> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err(OtelError::Validation {
            message: "Alert rule name cannot be empty".to_string(),
        });
    }
    if trimmed_name.len() > MAX_NAME_LEN {
        return Err(OtelError::Validation {
            message: format!("Alert rule name exceeds {MAX_NAME_LEN} characters"),
        });
    }
    let trimmed_metric = metric_name.trim();
    if trimmed_metric.is_empty() {
        return Err(OtelError::Validation {
            message: "Alert rule metric_name cannot be empty".to_string(),
        });
    }
    if trimmed_metric.len() > MAX_METRIC_NAME_LEN {
        return Err(OtelError::Validation {
            message: format!("Alert rule metric_name exceeds {MAX_METRIC_NAME_LEN} characters"),
        });
    }
    let agg = aggregation.trim().to_ascii_lowercase();
    if !ALLOWED_AGGREGATIONS.contains(&agg.as_str()) {
        return Err(OtelError::Validation {
            message: format!(
                "Invalid aggregation '{}' (allowed: {})",
                aggregation,
                ALLOWED_AGGREGATIONS.join(", ")
            ),
        });
    }
    let sev = severity.trim().to_ascii_lowercase();
    if !ALLOWED_SEVERITIES.contains(&sev.as_str()) {
        return Err(OtelError::Validation {
            message: format!(
                "Invalid severity '{}' (allowed: {})",
                severity,
                ALLOWED_SEVERITIES.join(", ")
            ),
        });
    }
    if window_secs <= 0 {
        return Err(OtelError::Validation {
            message: "window_secs must be greater than 0".to_string(),
        });
    }
    if for_duration_secs <= 0 {
        return Err(OtelError::Validation {
            message: "for_duration_secs must be greater than 0".to_string(),
        });
    }
    // Detector-specific invariants (threshold finiteness for static rules; the
    // not-yet-supported guard for anomaly/forecast/outlier/auto_watch).
    detection_config.validate()?;
    validate_label_filters(label_filters)?;
    validate_group_by(group_by)?;
    if !(MIN_MAX_SERIES..=MAX_MAX_SERIES).contains(&max_series) {
        return Err(OtelError::Validation {
            message: format!(
                "max_series must be between {MIN_MAX_SERIES} and {MAX_MAX_SERIES} (got {max_series})"
            ),
        });
    }
    if !(MIN_GROUPED_THRESHOLD..=MAX_GROUPED_THRESHOLD).contains(&grouped_notification_threshold) {
        return Err(OtelError::Validation {
            message: format!(
                "grouped_notification_threshold must be between {MIN_GROUPED_THRESHOLD} and {MAX_GROUPED_THRESHOLD} (got {grouped_notification_threshold})"
            ),
        });
    }
    // Per-series ("dynamic") alerting is supported for static AND anomaly
    // detectors (each series gets its own independently-scoped, independently-
    // cached baseline). Forecast/outlier/auto_watch remain genuinely unsupported
    // per-series — those are already rejected wholesale by
    // `detection_config.validate()` above (regardless of `dynamic_alerts`), but
    // this guard keeps a clear, dynamic-specific error should any of them ever
    // become evaluable in aggregate before their per-series design lands.
    if dynamic_alerts
        && !matches!(
            detection_config,
            DetectionConfig::Static(_) | DetectionConfig::Anomaly(_)
        )
    {
        return Err(OtelError::Validation {
            message: format!(
                "dynamic_alerts is only supported for static and anomaly detectors, not '{}'",
                detection_config.kind_str()
            ),
        });
    }
    Ok(())
}

/// Service managing CRUD over `metric_alert_rules`, plus evaluator support.
pub struct MetricAlertService {
    db: Arc<DatabaseConnection>,
}

impl MetricAlertService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// List rules for a project, newest first, paginated.
    ///
    /// Returns `(items, total)` where `total` is the unpaginated count.
    pub async fn list(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<Model>, u64), OtelError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);
        let paginator = Entity::find()
            .filter(Column::ProjectId.eq(project_id))
            .order_by_desc(Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;
        Ok((items, total))
    }

    /// Create a rule. Validates every field before persisting. `last_state`
    /// defaults to `unknown` via the DB default.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        project_id: i32,
        name: String,
        metric_name: String,
        aggregation: String,
        detection_config: DetectionConfig,
        window_secs: i32,
        for_duration_secs: i32,
        severity: String,
        enabled: bool,
        label_filters: Vec<(String, String)>,
        group_by: Vec<String>,
        dynamic_alerts: bool,
        max_series: i32,
        grouped_notification_threshold: i32,
    ) -> Result<Model, OtelError> {
        validate_rule(
            &name,
            &metric_name,
            &aggregation,
            &severity,
            window_secs,
            for_duration_secs,
            &detection_config,
            &label_filters,
            &group_by,
            dynamic_alerts,
            max_series,
            grouped_notification_threshold,
        )?;

        let model = ActiveModel {
            project_id: Set(project_id),
            name: Set(name.trim().to_string()),
            metric_name: Set(metric_name.trim().to_string()),
            aggregation: Set(aggregation.trim().to_ascii_lowercase()),
            detection_kind: Set(detection_config.kind_str().to_string()),
            detection_config: Set(detection_config.to_value()?),
            window_secs: Set(window_secs),
            for_duration_secs: Set(for_duration_secs),
            severity: Set(severity.trim().to_ascii_lowercase()),
            enabled: Set(enabled),
            label_filters: Set(serde_json::to_value(&label_filters)?),
            group_by: Set(serde_json::to_value(&group_by)?),
            dynamic_alerts: Set(dynamic_alerts),
            max_series: Set(max_series),
            grouped_notification_threshold: Set(grouped_notification_threshold),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(model)
    }

    /// Fetch a single rule by id, SCOPED to `project_id`.
    ///
    /// Filtering by project_id (not a bare primary key) prevents a caller
    /// operating in one project from reading another project's rule by guessing
    /// its id (cross-tenant IDOR).
    pub async fn get(&self, project_id: i32, id: i32) -> Result<Model, OtelError> {
        Entity::find_by_id(id)
            .filter(Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(OtelError::MetricAlertNotFound { rule_id: id })
    }

    /// Update a rule (scoped to `project_id`). Validates any supplied fields by
    /// merging them onto the existing row and running the full validator.
    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        &self,
        project_id: i32,
        id: i32,
        name: Option<String>,
        metric_name: Option<String>,
        aggregation: Option<String>,
        detection_config: Option<DetectionConfig>,
        window_secs: Option<i32>,
        for_duration_secs: Option<i32>,
        severity: Option<String>,
        enabled: Option<bool>,
        label_filters: Option<Vec<(String, String)>>,
        group_by: Option<Vec<String>>,
        dynamic_alerts: Option<bool>,
        max_series: Option<i32>,
        grouped_notification_threshold: Option<i32>,
    ) -> Result<Model, OtelError> {
        // Ensure the row exists AND belongs to the project (typed 404 otherwise).
        let existing = self.get(project_id, id).await?;

        // Validate the merged effective field set so partial updates can't leave
        // the row in an un-evaluable state. A detector is replaced wholesale: the
        // supplied config, or the existing one decoded from the stored blob.
        let eff_name = name.clone().unwrap_or_else(|| existing.name.clone());
        let eff_metric = metric_name
            .clone()
            .unwrap_or_else(|| existing.metric_name.clone());
        let eff_agg = aggregation
            .clone()
            .unwrap_or_else(|| existing.aggregation.clone());
        let eff_sev = severity
            .clone()
            .unwrap_or_else(|| existing.severity.clone());
        let eff_window = window_secs.unwrap_or(existing.window_secs);
        let eff_for = for_duration_secs.unwrap_or(existing.for_duration_secs);
        let eff_config = match &detection_config {
            Some(c) => c.clone(),
            None => DetectionConfig::from_value(&existing.detection_config)?,
        };
        let eff_filters: Vec<(String, String)> = match &label_filters {
            Some(f) => f.clone(),
            None => serde_json::from_value(existing.label_filters.clone()).unwrap_or_default(),
        };
        let eff_group_by: Vec<String> = match &group_by {
            Some(g) => g.clone(),
            None => serde_json::from_value(existing.group_by.clone()).unwrap_or_default(),
        };
        let eff_dynamic = dynamic_alerts.unwrap_or(existing.dynamic_alerts);
        let eff_max_series = max_series.unwrap_or(existing.max_series);
        let eff_grouped_threshold =
            grouped_notification_threshold.unwrap_or(existing.grouped_notification_threshold);
        validate_rule(
            &eff_name,
            &eff_metric,
            &eff_agg,
            &eff_sev,
            eff_window,
            eff_for,
            &eff_config,
            &eff_filters,
            &eff_group_by,
            eff_dynamic,
            eff_max_series,
            eff_grouped_threshold,
        )?;

        let mut active: ActiveModel = existing.into();
        if let Some(n) = name {
            active.name = Set(n.trim().to_string());
        }
        if let Some(m) = metric_name {
            active.metric_name = Set(m.trim().to_string());
        }
        if let Some(a) = aggregation {
            active.aggregation = Set(a.trim().to_ascii_lowercase());
        }
        if let Some(c) = detection_config {
            active.detection_kind = Set(c.kind_str().to_string());
            active.detection_config = Set(c.to_value()?);
        }
        if let Some(w) = window_secs {
            active.window_secs = Set(w);
        }
        if let Some(f) = for_duration_secs {
            active.for_duration_secs = Set(f);
        }
        if let Some(s) = severity {
            active.severity = Set(s.trim().to_ascii_lowercase());
        }
        if let Some(e) = enabled {
            active.enabled = Set(e);
        }
        if let Some(lf) = label_filters {
            active.label_filters = Set(serde_json::to_value(&lf)?);
        }
        if let Some(gb) = group_by {
            active.group_by = Set(serde_json::to_value(&gb)?);
        }
        if let Some(d) = dynamic_alerts {
            active.dynamic_alerts = Set(d);
        }
        if let Some(ms) = max_series {
            active.max_series = Set(ms);
        }
        if let Some(gt) = grouped_notification_threshold {
            active.grouped_notification_threshold = Set(gt);
        }
        let model = active.update(self.db.as_ref()).await?;
        Ok(model)
    }

    /// Delete a rule (scoped to `project_id`). Returns
    /// [`OtelError::MetricAlertNotFound`] when no matching row was removed.
    pub async fn delete(&self, project_id: i32, id: i32) -> Result<(), OtelError> {
        let result = Entity::delete_many()
            .filter(Column::Id.eq(id))
            .filter(Column::ProjectId.eq(project_id))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(OtelError::MetricAlertNotFound { rule_id: id });
        }
        Ok(())
    }

    // ── Evaluator support ───────────────────────────────────────────

    /// All enabled rules across all projects, for the background evaluator scan.
    pub async fn list_enabled(&self) -> Result<Vec<Model>, OtelError> {
        let rules = Entity::find()
            .filter(Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;
        Ok(rules)
    }

    /// Persist the evaluator's observed state for a rule. Loads by id (no project
    /// scope — the evaluator already holds the rule it loaded via `list_enabled`)
    /// and records the three evaluation columns.
    pub async fn persist_evaluation(
        &self,
        id: i32,
        last_state: &str,
        last_value: Option<f64>,
        last_evaluated_at: DateTime<Utc>,
    ) -> Result<(), OtelError> {
        let existing = Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OtelError::MetricAlertNotFound { rule_id: id })?;
        let mut active: ActiveModel = existing.into();
        active.last_state = Set(last_state.to_string());
        active.last_value = Set(last_value);
        active.last_evaluated_at = Set(Some(last_evaluated_at));
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Persist a DYNAMIC (per-series) rule's evaluation: the aggregate-view columns
    /// (`last_state`/`last_value`/`last_evaluated_at`, exactly as
    /// [`Self::persist_evaluation`]) PLUS the full per-series snapshot
    /// (`series_states`) and the latest tick's cardinality-cap drop count
    /// (`last_dropped_series_count`). The static/aggregate path keeps using the
    /// narrower `persist_evaluation`, so those two columns stay at their DB
    /// defaults (`{}` / `0`) for non-dynamic rules.
    pub async fn persist_dynamic_evaluation(
        &self,
        id: i32,
        last_state: &str,
        last_value: Option<f64>,
        last_evaluated_at: DateTime<Utc>,
        series_states: serde_json::Value,
        dropped_series_count: i32,
    ) -> Result<(), OtelError> {
        let existing = Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OtelError::MetricAlertNotFound { rule_id: id })?;
        let mut active: ActiveModel = existing.into();
        active.last_state = Set(last_state.to_string());
        active.last_value = Set(last_value);
        active.last_evaluated_at = Set(Some(last_evaluated_at));
        active.series_states = Set(series_states);
        active.last_dropped_series_count = Set(dropped_series_count);
        active.update(self.db.as_ref()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::{Comparator, StaticParams};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;
    use temps_core::DBDateTime;

    /// A valid static detector config for test rules.
    fn static_cfg() -> DetectionConfig {
        DetectionConfig::Static(StaticParams {
            comparator: Comparator::Gt,
            threshold: 500.0,
        })
    }

    /// Build a MockRow representing a `COUNT(*) AS num_items` result for the
    /// sea-orm paginator. `num_items()` reads `try_get::<i64>("", "num_items")`.
    fn count_row(n: i64) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("num_items".to_string(), Value::BigInt(Some(n)));
        m
    }

    fn sample_model(id: i32) -> Model {
        let now: DBDateTime = chrono::Utc::now();
        Model {
            id,
            project_id: 7,
            name: "High latency".to_string(),
            metric_name: "http.server.duration".to_string(),
            aggregation: "p95".to_string(),
            detection_kind: "static".to_string(),
            detection_config: serde_json::json!({
                "kind": "static", "comparator": "gt", "threshold": 500.0
            }),
            label_filters: serde_json::json!([]),
            group_by: serde_json::json!([]),
            dynamic_alerts: false,
            max_series: 20,
            grouped_notification_threshold: 5,
            window_secs: 300,
            for_duration_secs: 120,
            severity: "warning".to_string(),
            enabled: true,
            last_state: "unknown".to_string(),
            last_value: None,
            series_states: serde_json::json!({}),
            last_dropped_series_count: 0,
            last_evaluated_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn test_create_success() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "High latency".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec![],
                false,
                20,
                5,
            )
            .await;

        assert!(result.is_ok());
        let model = result.unwrap();
        assert_eq!(model.id, 1);
        assert_eq!(model.project_id, 7);
        assert_eq!(model.detection_kind, "static");
    }

    #[tokio::test]
    async fn test_create_empty_name_validation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "   ".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_create_bad_aggregation_validation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "median".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_create_unsupported_kind_rejected() {
        // Forecast/outlier/auto_watch detectors are typed/schema-present but not
        // yet evaluable, so creation is rejected with a Validation error.
        // (Anomaly IS now supported — see the detectors module tests.)
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let forecast = DetectionConfig::from_value(&serde_json::json!({
            "kind": "forecast", "forecast_horizon_secs": 3600, "comparator": "gt", "threshold": 1.0
        }))
        .expect("forecast config parses");
        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                forecast,
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_create_zero_window_validation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                0,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<Model>::new()])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service.get(7, 999).await;
        assert!(matches!(
            result.unwrap_err(),
            OtelError::MetricAlertNotFound { rule_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_update_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<Model>::new()])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .update(
                7,
                999,
                Some("New Name".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(
            result.unwrap_err(),
            OtelError::MetricAlertNotFound { rule_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service.delete(7, 999).await;
        assert!(matches!(
            result.unwrap_err(),
            OtelError::MetricAlertNotFound { rule_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_delete_success() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service.delete(7, 1).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_pagination_caps_page_size() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(2)]])
            .append_query_results(vec![vec![sample_model(1), sample_model(2)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service.list(7, Some(1), Some(10_000)).await;
        assert!(result.is_ok());
        let (items, total) = result.unwrap();
        assert_eq!(total, 2);
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn test_list_enabled() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_model(1), sample_model(2)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let rules = service.list_enabled().await.unwrap();
        assert_eq!(rules.len(), 2);
    }

    #[tokio::test]
    async fn test_persist_evaluation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get-by-id load, then the update returns the row again
            .append_query_results(vec![vec![sample_model(1)]])
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .persist_evaluation(1, "firing", Some(600.0), chrono::Utc::now())
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_label_filters_too_many_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let too_many: Vec<(String, String)> = (0..11)
            .map(|i| (format!("key{i}"), format!("val{i}")))
            .collect();
        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                too_many,
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_label_filters_invalid_key_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![("bad key!".to_string(), "value".to_string())],
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_label_filters_value_too_long_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let long_value = "x".repeat(501);
        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![("region".to_string(), long_value)],
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_label_filters_valid_passes_validation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![
                    ("region".to_string(), "eu-west".to_string()),
                    ("endpoint".to_string(), "/checkout".to_string()),
                ],
                vec![],
                false,
                20,
                5,
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_group_by_too_many_keys_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_group_by_invalid_key_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["bad key!".to_string()],
                false,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_max_series_out_of_range_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        // 0 is below MIN_MAX_SERIES.
        let too_low = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string()],
                true,
                0,
                5,
            )
            .await;
        assert!(matches!(too_low.unwrap_err(), OtelError::Validation { .. }));

        // 101 is above the hard cap of 100.
        let too_high = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string()],
                true,
                101,
                5,
            )
            .await;
        assert!(matches!(
            too_high.unwrap_err(),
            OtelError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_dynamic_alerts_with_anomaly_accepted() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        // Robust anomaly + dynamic (per-series) alerting is now supported: each
        // series gets its own independently-scoped baseline (ADR-026 follow-up).
        let anomaly =
            DetectionConfig::from_value(&serde_json::json!({ "kind": "anomaly" })).unwrap();
        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                anomaly,
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string()],
                true,
                20,
                5,
            )
            .await;
        assert!(result.is_ok(), "anomaly + dynamic should now be accepted");
    }

    #[tokio::test]
    async fn test_dynamic_alerts_with_forecast_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        // Forecast (like outlier/auto_watch) remains genuinely unsupported both in
        // aggregate AND per-series, so combining it with dynamic_alerts is rejected.
        let forecast = DetectionConfig::from_value(&serde_json::json!({
            "kind": "forecast", "forecast_horizon_secs": 3600, "comparator": "gt", "threshold": 1.0
        }))
        .expect("forecast config parses");
        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                forecast,
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string()],
                true,
                20,
                5,
            )
            .await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_dynamic_static_valid_passes() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string(), "region".to_string()],
                true,
                50,
                5,
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_grouped_notification_threshold_valid_passes() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        // In-range custom threshold (1..=1000) is accepted.
        let result = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string()],
                true,
                20,
                250,
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_grouped_notification_threshold_out_of_range_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        // 0 is below MIN_GROUPED_THRESHOLD.
        let too_low = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string()],
                true,
                20,
                0,
            )
            .await;
        assert!(matches!(too_low.unwrap_err(), OtelError::Validation { .. }));

        // 1001 is above the hard ceiling of 1000.
        let too_high = service
            .create(
                7,
                "Rule".to_string(),
                "http.server.duration".to_string(),
                "p95".to_string(),
                static_cfg(),
                300,
                120,
                "warning".to_string(),
                true,
                vec![],
                vec!["endpoint".to_string()],
                true,
                20,
                1001,
            )
            .await;
        assert!(matches!(
            too_high.unwrap_err(),
            OtelError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_persist_dynamic_evaluation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get-by-id load, then the update returns the row again
            .append_query_results(vec![vec![sample_model(1)]])
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricAlertService::new(Arc::new(db));

        let series_states = serde_json::json!({
            "method=GET": {"state": "firing", "value": 12.5, "alarm_id": 259},
            "method=POST": {"state": "ok", "value": 3.1, "alarm_id": null},
        });
        let result = service
            .persist_dynamic_evaluation(
                1,
                "firing",
                Some(12.5),
                chrono::Utc::now(),
                series_states,
                2,
            )
            .await;
        assert!(result.is_ok());
    }
}
