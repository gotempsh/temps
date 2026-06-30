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

/// The allowlisted aggregations a rule may request. Mirrors the keyword/quantile
/// forms accepted by `MetricAggregation::parse`.
pub const ALLOWED_AGGREGATIONS: &[&str] = &[
    "avg", "sum", "min", "max", "count", "rate", "p50", "p90", "p95", "p99",
];

/// The allowlisted severities, mapped to `AlarmSeverity` by the evaluator.
pub const ALLOWED_SEVERITIES: &[&str] = &["info", "warning", "critical"];

const MAX_NAME_LEN: usize = 200;
const MAX_METRIC_NAME_LEN: usize = 256;

/// Validate the cross-cutting rule fields against the allowlists and sane bounds,
/// then delegate detector-specific validation to the typed [`DetectionConfig`].
///
/// Returns [`OtelError::Validation`] on the first invalid field so the caller
/// surfaces a 400 rather than persisting an un-evaluable rule.
fn validate_rule(
    name: &str,
    metric_name: &str,
    aggregation: &str,
    severity: &str,
    window_secs: i32,
    for_duration_secs: i32,
    detection_config: &DetectionConfig,
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
    ) -> Result<Model, OtelError> {
        validate_rule(
            &name,
            &metric_name,
            &aggregation,
            &severity,
            window_secs,
            for_duration_secs,
            &detection_config,
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
        validate_rule(
            &eff_name,
            &eff_metric,
            &eff_agg,
            &eff_sev,
            eff_window,
            eff_for,
            &eff_config,
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
            window_secs: 300,
            for_duration_secs: 120,
            severity: "warning".to_string(),
            enabled: true,
            last_state: "unknown".to_string(),
            last_value: None,
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
}
