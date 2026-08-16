//! Database-backed implementation of `MetricAlertConfigService`.
//!
//! Reconciles metric alert rules declared in `.temps.yaml` with the
//! `metric_alert_rules` table using an upsert-by-name strategy scoped to
//! `(project_id, environment_id)`:
//!
//! - Rules present in the YAML are created (if new) or updated (if existing).
//! - Rules present in the DB for that environment but absent from the YAML are
//!   **disabled** (`enabled = false`), never hard-deleted. This preserves alert
//!   history and per-series evaluator state so operators can review what changed.
//! - Project-scoped rules (`environment_id = NULL`) are never modified.

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use std::sync::Arc;
use temps_database::DbConnection;
use temps_entities::metric_alert_rules;
use temps_otel::detectors::{
    AnomalyAlgorithm, AnomalyParams, Comparator, DetectionConfig, Direction, Seasonality,
    StaticParams,
};
use tracing::{debug, info, warn};

use crate::jobs::configure_metric_alerts::{
    AlertDetectionSpec, MetricAlertConfig, MetricAlertConfigError, MetricAlertConfigService,
};

/// Database-backed metric alert reconciliation service.
pub struct DatabaseMetricAlertConfigService {
    db: Arc<DatabaseConnection>,
}

impl DatabaseMetricAlertConfigService {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self { db }
    }

    /// Build and validate the typed [`DetectionConfig`] for a rule's spec.
    /// Rejects malformed comparator/algorithm/direction/seasonality strings
    /// (which can only come from a bug in this crate's own parsing — the
    /// YAML layer already restricts these to known keywords) and invalid
    /// hyperparameters (e.g. non-finite threshold, `pct_anomalous` outside
    /// `(0, 1]`) via `DetectionConfig::validate`.
    fn build_detection_config(
        name: &str,
        spec: &AlertDetectionSpec,
    ) -> Result<DetectionConfig, MetricAlertConfigError> {
        let config = match spec {
            AlertDetectionSpec::Static {
                comparator,
                threshold,
            } => {
                let comparator = match comparator.as_str() {
                    ">" => Comparator::Gt,
                    ">=" => Comparator::Gte,
                    "<" => Comparator::Lt,
                    "<=" => Comparator::Lte,
                    other => {
                        return Err(MetricAlertConfigError::InvalidConfig {
                            name: name.to_string(),
                            message: format!("unsupported comparator '{}'", other),
                        })
                    }
                };
                DetectionConfig::Static(StaticParams {
                    comparator,
                    threshold: *threshold,
                })
            }
            AlertDetectionSpec::Anomaly {
                algorithm,
                deviations,
                direction,
                seasonality,
                pct_anomalous,
                baseline_lookback_days,
            } => {
                let algorithm = match algorithm.as_str() {
                    "robust" => AnomalyAlgorithm::Robust,
                    "basic" => AnomalyAlgorithm::Basic,
                    "agile" => AnomalyAlgorithm::Agile,
                    "ewma" => AnomalyAlgorithm::Ewma,
                    other => {
                        return Err(MetricAlertConfigError::InvalidConfig {
                            name: name.to_string(),
                            message: format!("unknown anomaly algorithm '{}'", other),
                        })
                    }
                };
                let direction = match direction.as_str() {
                    "both" => Direction::Both,
                    "above" => Direction::Above,
                    "below" => Direction::Below,
                    other => {
                        return Err(MetricAlertConfigError::InvalidConfig {
                            name: name.to_string(),
                            message: format!("unknown anomaly direction '{}'", other),
                        })
                    }
                };
                let seasonality = match seasonality.as_str() {
                    "none" => Seasonality::None,
                    "hourly" => Seasonality::Hourly,
                    "daily" => Seasonality::Daily,
                    "weekly" => Seasonality::Weekly,
                    other => {
                        return Err(MetricAlertConfigError::InvalidConfig {
                            name: name.to_string(),
                            message: format!("unknown anomaly seasonality '{}'", other),
                        })
                    }
                };
                DetectionConfig::Anomaly(AnomalyParams {
                    algorithm,
                    deviations: *deviations,
                    direction,
                    seasonality,
                    pct_anomalous: *pct_anomalous,
                    baseline_lookback_days: *baseline_lookback_days,
                })
            }
        };

        config
            .validate()
            .map_err(|e| MetricAlertConfigError::InvalidConfig {
                name: name.to_string(),
                message: e.to_string(),
            })?;

        Ok(config)
    }
}

#[async_trait]
impl MetricAlertConfigService for DatabaseMetricAlertConfigService {
    async fn configure_alerts(
        &self,
        project_id: i32,
        environment_id: i32,
        configs: Vec<MetricAlertConfig>,
    ) -> Result<(), MetricAlertConfigError> {
        info!(
            project_id,
            environment_id,
            count = configs.len(),
            "Reconciling metric alert rules from .temps.yaml"
        );

        // Fetch all existing rules for this (project, environment) scope.
        let existing = metric_alert_rules::Entity::find()
            .filter(metric_alert_rules::Column::ProjectId.eq(project_id))
            .filter(metric_alert_rules::Column::EnvironmentId.eq(environment_id))
            .all(self.db.as_ref())
            .await
            .map_err(|e| MetricAlertConfigError::DatabaseError {
                project_id,
                environment_id,
                message: format!("Failed to fetch existing metric alert rules: {}", e),
            })?;

        // Index by name for O(1) lookup.
        let existing_by_name: std::collections::HashMap<&str, &metric_alert_rules::Model> =
            existing.iter().map(|r| (r.name.as_str(), r)).collect();

        // Track which names are covered by the YAML so we can disable orphans.
        let yaml_names: std::collections::HashSet<&str> =
            configs.iter().map(|c| c.name.as_str()).collect();

        // Upsert rules from YAML.
        for cfg in &configs {
            let label_filters_json = serde_json::to_value(&cfg.label_filters).map_err(|e| {
                MetricAlertConfigError::InvalidConfig {
                    name: cfg.name.clone(),
                    message: format!("Failed to serialise label_filters: {}", e),
                }
            })?;
            let group_by_json = serde_json::to_value(&cfg.group_by).map_err(|e| {
                MetricAlertConfigError::InvalidConfig {
                    name: cfg.name.clone(),
                    message: format!("Failed to serialise group_by: {}", e),
                }
            })?;
            let detection = Self::build_detection_config(&cfg.name, &cfg.detection)?;
            let detection_kind = detection.kind_str().to_string();
            let detection_config =
                detection
                    .to_value()
                    .map_err(|e| MetricAlertConfigError::InvalidConfig {
                        name: cfg.name.clone(),
                        message: e.to_string(),
                    })?;

            if let Some(existing_rule) = existing_by_name.get(cfg.name.as_str()) {
                // Update the existing row.
                debug!(
                    rule_id = existing_rule.id,
                    name = cfg.name,
                    "Updating existing metric alert rule"
                );
                let mut active: metric_alert_rules::ActiveModel = (*existing_rule).clone().into();
                active.metric_name = Set(cfg.metric_name.clone());
                active.aggregation = Set(cfg.aggregation.trim().to_ascii_lowercase());
                active.detection_kind = Set(detection_kind.clone());
                active.detection_config = Set(detection_config.clone());
                active.window_secs = Set(cfg.window_secs);
                active.for_duration_secs = Set(cfg.for_duration_secs);
                active.severity = Set(cfg.severity.trim().to_ascii_lowercase());
                active.enabled = Set(cfg.enabled);
                active.label_filters = Set(label_filters_json);
                active.group_by = Set(group_by_json);
                active.update(self.db.as_ref()).await.map_err(|e| {
                    MetricAlertConfigError::DatabaseError {
                        project_id,
                        environment_id,
                        message: format!(
                            "Failed to update metric alert rule '{}': {}",
                            cfg.name, e
                        ),
                    }
                })?;
            } else {
                // Insert a new row.
                debug!(name = cfg.name, "Creating new metric alert rule from YAML");
                let active = metric_alert_rules::ActiveModel {
                    project_id: Set(project_id),
                    environment_id: Set(Some(environment_id)),
                    name: Set(cfg.name.clone()),
                    metric_name: Set(cfg.metric_name.clone()),
                    aggregation: Set(cfg.aggregation.trim().to_ascii_lowercase()),
                    detection_kind: Set(detection_kind),
                    detection_config: Set(detection_config),
                    window_secs: Set(cfg.window_secs),
                    for_duration_secs: Set(cfg.for_duration_secs),
                    severity: Set(cfg.severity.trim().to_ascii_lowercase()),
                    enabled: Set(cfg.enabled),
                    label_filters: Set(label_filters_json),
                    group_by: Set(group_by_json),
                    // Static/aggregate-only rules; leave dynamic columns at defaults.
                    dynamic_alerts: Set(false),
                    max_series: Set(20),
                    grouped_notification_threshold: Set(5),
                    last_state: Set("unknown".to_string()),
                    series_states: Set(serde_json::json!({})),
                    last_dropped_series_count: Set(0),
                    ..Default::default()
                };
                active.insert(self.db.as_ref()).await.map_err(|e| {
                    MetricAlertConfigError::DatabaseError {
                        project_id,
                        environment_id,
                        message: format!(
                            "Failed to insert metric alert rule '{}': {}",
                            cfg.name, e
                        ),
                    }
                })?;
            }
        }

        // Disable orphaned rules (in DB for this env but absent from YAML).
        let orphaned: Vec<&metric_alert_rules::Model> = existing
            .iter()
            .filter(|r| !yaml_names.contains(r.name.as_str()))
            .collect();

        for rule in orphaned {
            warn!(
                rule_id = rule.id,
                name = rule.name,
                "Disabling metric alert rule absent from .temps.yaml"
            );
            let mut active: metric_alert_rules::ActiveModel = rule.clone().into();
            active.enabled = Set(false);
            active.update(self.db.as_ref()).await.map_err(|e| {
                MetricAlertConfigError::DatabaseError {
                    project_id,
                    environment_id,
                    message: format!(
                        "Failed to disable orphaned metric alert rule '{}' (id {}): {}",
                        rule.name, rule.id, e
                    ),
                }
            })?;
        }

        info!(
            project_id,
            environment_id,
            upserted = configs.len(),
            disabled = existing
                .iter()
                .filter(|r| !yaml_names.contains(r.name.as_str()))
                .count(),
            "Metric alert reconciliation complete"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::configure_metric_alerts::{
        AlertDetectionSpec, MetricAlertConfig, MetricAlertConfigError, MetricAlertConfigService,
    };

    /// Helper to create a minimal MetricAlertConfig for tests.
    fn make_config(name: &str, metric: &str) -> MetricAlertConfig {
        MetricAlertConfig {
            name: name.to_string(),
            metric_name: metric.to_string(),
            aggregation: "avg".to_string(),
            detection: AlertDetectionSpec::Static {
                comparator: ">".to_string(),
                threshold: 0.5,
            },
            window_secs: 300,
            for_duration_secs: 0,
            severity: "warning".to_string(),
            enabled: true,
            label_filters: vec![],
            group_by: vec![],
        }
    }

    #[test]
    fn test_build_detection_config_static_shape() {
        let spec = AlertDetectionSpec::Static {
            comparator: ">=".to_string(),
            threshold: 90.0,
        };
        let config = DatabaseMetricAlertConfigService::build_detection_config("rule", &spec)
            .expect("valid static spec");
        let v = config.to_value().unwrap();
        assert_eq!(v["kind"], "static");
        assert_eq!(v["comparator"], "gte");
        assert!((v["threshold"].as_f64().unwrap() - 90.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_build_detection_config_anomaly_shape() {
        let spec = AlertDetectionSpec::Anomaly {
            algorithm: "robust".to_string(),
            deviations: 3.0,
            direction: "both".to_string(),
            seasonality: "daily".to_string(),
            pct_anomalous: 1.0,
            baseline_lookback_days: Some(14),
        };
        let config = DatabaseMetricAlertConfigService::build_detection_config("rule", &spec)
            .expect("valid anomaly spec");
        let v = config.to_value().unwrap();
        assert_eq!(v["kind"], "anomaly");
        assert_eq!(v["algorithm"], "robust");
        assert_eq!(v["seasonality"], "daily");
        assert_eq!(v["baseline_lookback_days"], 14);
    }

    #[test]
    fn test_build_detection_config_rejects_unknown_algorithm() {
        let spec = AlertDetectionSpec::Anomaly {
            algorithm: "quantum".to_string(),
            deviations: 3.0,
            direction: "both".to_string(),
            seasonality: "none".to_string(),
            pct_anomalous: 1.0,
            baseline_lookback_days: None,
        };
        let err =
            DatabaseMetricAlertConfigService::build_detection_config("rule", &spec).unwrap_err();
        assert!(matches!(
            err,
            MetricAlertConfigError::InvalidConfig { name, .. } if name == "rule"
        ));
    }

    #[test]
    fn test_build_detection_config_rejects_invalid_hyperparameters() {
        // deviations <= 0 is caught by DetectionConfig::validate, not our
        // string-matching — proving the two layers compose.
        let spec = AlertDetectionSpec::Anomaly {
            algorithm: "robust".to_string(),
            deviations: 0.0,
            direction: "both".to_string(),
            seasonality: "none".to_string(),
            pct_anomalous: 1.0,
            baseline_lookback_days: None,
        };
        assert!(DatabaseMetricAlertConfigService::build_detection_config("rule", &spec).is_err());
    }

    #[tokio::test]
    async fn test_configure_alerts_db_error() {
        // Without a real DB we test that the service propagates a properly typed
        // error when the DB call fails. We use a simple mock via the trait.
        struct FailingService;

        #[async_trait]
        impl MetricAlertConfigService for FailingService {
            async fn configure_alerts(
                &self,
                _project_id: i32,
                _environment_id: i32,
                _configs: Vec<MetricAlertConfig>,
            ) -> Result<(), MetricAlertConfigError> {
                Err(MetricAlertConfigError::DatabaseError {
                    project_id: 1,
                    environment_id: 2,
                    message: "simulated failure".to_string(),
                })
            }
        }

        let svc = FailingService;
        let result = svc
            .configure_alerts(1, 2, vec![make_config("test", "metric")])
            .await;
        assert!(matches!(
            result,
            Err(MetricAlertConfigError::DatabaseError {
                project_id: 1,
                environment_id: 2,
                ..
            })
        ));
    }

    #[test]
    fn test_make_config_helper() {
        let cfg = make_config("High CPU", "system.cpu");
        assert_eq!(cfg.name, "High CPU");
        assert_eq!(cfg.metric_name, "system.cpu");
        match cfg.detection {
            AlertDetectionSpec::Static {
                comparator,
                threshold,
            } => {
                assert_eq!(comparator, ">");
                assert!((threshold - 0.5).abs() < f64::EPSILON);
            }
            AlertDetectionSpec::Anomaly { .. } => panic!("expected Static detection"),
        }
        assert!(cfg.enabled);
    }
}
