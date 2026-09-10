// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Configure Metric Alerts Job
//!
//! Reconciles metric alert rules declared in `.temps.yaml` with the database
//! after a deployment completes. Follows the same structure as
//! `ConfigureCronsJob`.
//!
//! ## Reconciliation policy
//!
//! Rules are upserted by `(project_id, environment_id, name)`:
//! - A rule in `.temps.yaml` that already exists in the DB for this
//!   `(project, environment)` is **updated** in place (config change on
//!   redeploy).
//! - A rule in the DB for this environment that is no longer present in
//!   `.temps.yaml` is **disabled** (`enabled = false`), not deleted. This
//!   preserves alert history and evaluator state. Operators can hard-delete
//!   orphaned rules manually via the UI/API if desired.
//! - Project-scoped rules (`environment_id = NULL`) are never touched by this
//!   job — they remain exclusively under UI/API control.

use async_trait::async_trait;
use sea_orm::EntityTrait;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use temps_core::{JobResult, TempsConfig, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_entities::projects;
use temps_logs::{LogLevel, LogService};
use tracing::warn;

use crate::jobs::RepositoryOutput;

/// Hard cap on the number of `alerts:` entries a single `.temps.yaml` may
/// declare. See the check in `configure_alerts` for why this exists.
const MAX_ALERTS_PER_TEMPS_YAML: usize = 100;

/// Pure validation for the cap, split out from `configure_alerts` so it's
/// unit-testable without a `RepositoryOutput`/DB fixture.
fn validate_alert_count(count: usize) -> Result<(), String> {
    if count > MAX_ALERTS_PER_TEMPS_YAML {
        Err(format!(
            ".temps.yaml declares {} alert(s), exceeding the limit of {}",
            count, MAX_ALERTS_PER_TEMPS_YAML
        ))
    } else {
        Ok(())
    }
}

/// The parsed detector for a [`MetricAlertConfig`] — either a static
/// threshold or an anomaly band. `forecast`/`outlier`/`auto_watch` are not
/// representable here: they're rejected at YAML-parse time (see
/// `MetricAlertYamlConfig::detection_source`) since `temps-otel` has no
/// evaluator for them yet.
#[derive(Debug, Clone)]
pub enum AlertDetectionSpec {
    Static {
        comparator: String,
        threshold: f64,
    },
    Anomaly {
        algorithm: String,
        deviations: f64,
        direction: String,
        seasonality: String,
        pct_anomalous: f64,
        baseline_lookback_days: Option<i32>,
    },
}

/// A metric alert rule in the service-layer DTO format.
#[derive(Debug, Clone)]
pub struct MetricAlertConfig {
    /// Human-readable name; used as the upsert key within the environment.
    pub name: String,
    /// Metric name (e.g. `http.server.errors`).
    pub metric_name: String,
    /// Aggregation function: `avg|sum|min|max|count|rate|p50|p90|p95|p99`.
    pub aggregation: String,
    /// The detector for this rule.
    pub detection: AlertDetectionSpec,
    /// Evaluation window in seconds.
    pub window_secs: i32,
    /// Minimum breach duration in seconds before the rule fires.
    pub for_duration_secs: i32,
    /// Severity: `info|warning|critical`.
    pub severity: String,
    /// Whether the rule is active.
    pub enabled: bool,
    /// AND-combined label equality filters as `(key, value)` pairs.
    pub label_filters: Vec<(String, String)>,
    /// Label keys to break the metric down by.
    pub group_by: Vec<String>,
}

/// Service interface for metric alert reconciliation.
///
/// Concrete implementations live in `temps-deployments`
/// (`DatabaseMetricAlertConfigService`) to avoid circular dependencies with
/// `temps-otel`.
#[async_trait]
pub trait MetricAlertConfigService: Send + Sync {
    /// Upsert the supplied alert configs for `(project_id, environment_id)` and
    /// disable any rules that are currently in the DB for that scope but absent
    /// from `configs`. Project-scoped rules (NULL environment) are never touched.
    async fn configure_alerts(
        &self,
        project_id: i32,
        environment_id: i32,
        configs: Vec<MetricAlertConfig>,
    ) -> Result<(), MetricAlertConfigError>;
}

/// Errors that can occur during metric alert reconciliation.
#[derive(Debug, thiserror::Error)]
pub enum MetricAlertConfigError {
    #[error("Database error while reconciling metric alerts for project {project_id}, environment {environment_id}: {message}")]
    DatabaseError {
        project_id: i32,
        environment_id: i32,
        message: String,
    },

    #[error("Invalid alert config for rule '{name}': {message}")]
    InvalidConfig { name: String, message: String },

    #[error("Configuration error: {0}")]
    ConfigError(String),
}

/// No-op fallback for when no concrete metric alert service is wired.
pub struct NoOpMetricAlertConfigService;

#[async_trait]
impl MetricAlertConfigService for NoOpMetricAlertConfigService {
    async fn configure_alerts(
        &self,
        _project_id: i32,
        _environment_id: i32,
        _configs: Vec<MetricAlertConfig>,
    ) -> Result<(), MetricAlertConfigError> {
        warn!("Metric alert configuration skipped - no metric alert service available");
        Ok(())
    }
}

/// Job for reconciling metric alert rules from `.temps.yaml` after deployment.
pub struct ConfigureMetricAlertsJob {
    job_id: String,
    download_job_id: String,
    deploy_container_job_id: String,
    project_id: i32,
    environment_id: i32,
    db: Arc<DbConnection>,
    alert_service: Arc<dyn MetricAlertConfigService>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for ConfigureMetricAlertsJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigureMetricAlertsJob")
            .field("job_id", &self.job_id)
            .field("download_job_id", &self.download_job_id)
            .field("deploy_container_job_id", &self.deploy_container_job_id)
            .field("project_id", &self.project_id)
            .field("environment_id", &self.environment_id)
            .finish()
    }
}

impl ConfigureMetricAlertsJob {
    pub fn new(
        job_id: String,
        download_job_id: String,
        deploy_container_job_id: String,
        project_id: i32,
        environment_id: i32,
        db: Arc<DbConnection>,
        alert_service: Arc<dyn MetricAlertConfigService>,
    ) -> Self {
        Self {
            job_id,
            download_job_id,
            deploy_container_job_id,
            project_id,
            environment_id,
            db,
            alert_service,
            log_id: None,
            log_service: None,
        }
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        let level = Self::detect_log_level(&message);
        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        Ok(())
    }

    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌")
            || message.contains("Failed")
            || message.contains("Error")
            || message.contains("error")
        {
            LogLevel::Error
        } else {
            LogLevel::Info
        }
    }

    async fn load_temps_config(
        &self,
        repo_dir: &Path,
        project: &projects::Model,
    ) -> Result<Option<TempsConfig>, WorkflowError> {
        let project_dir = repo_dir.join(&project.directory);
        let config_path = project_dir.join(".temps.yaml");

        if !config_path.exists() {
            self.log(format!(
                "No .temps.yaml found at {:?}, skipping metric alert configuration",
                config_path
            ))
            .await?;
            return Ok(None);
        }

        self.log(format!("Found .temps.yaml at {:?}", config_path))
            .await?;

        let config_contents = fs::read_to_string(&config_path).map_err(WorkflowError::IoError)?;

        let config = TempsConfig::from_yaml(&config_contents).map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to parse .temps.yaml: {}", e))
        })?;

        Ok(Some(config))
    }

    async fn configure_alerts(&self, repo_output: &RepositoryOutput) -> Result<(), WorkflowError> {
        self.log("Starting metric alert configuration".to_string())
            .await?;

        let project = projects::Entity::find_by_id(self.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to load project: {}", e)))?
            .ok_or_else(|| {
                WorkflowError::Other(format!("Project {} not found", self.project_id))
            })?;

        let config = match self
            .load_temps_config(&repo_output.repo_dir, &project)
            .await?
        {
            Some(config) => config,
            None => {
                self.log("No metric alert configuration needed".to_string())
                    .await?;
                return Ok(());
            }
        };

        // Even when has_alerts() is false we still call configure_alerts with
        // an empty list so the reconciler can disable any orphaned rules that
        // were previously deployed but have since been removed from the YAML.
        let alert_yaml_configs = config.alert_configs();
        self.log(format!(
            "Found {} metric alert rule(s) to configure",
            alert_yaml_configs.len()
        ))
        .await?;

        // Cap the number of rules a single .temps.yaml can declare. Without
        // this, an unbounded `alerts:` list would fan out into that many
        // serial DB upserts on every deploy — a self-inflicted control-plane
        // DoS vector unique to config-as-code (the UI/API path is naturally
        // rate-limited by a human clicking "create"; a git commit is not).
        if let Err(e) = validate_alert_count(alert_yaml_configs.len()) {
            let error_msg = format!("❌ {}", e);
            self.log(error_msg.clone()).await?;
            return Err(WorkflowError::JobExecutionFailed(error_msg));
        }

        // Parse YAML configs into service-layer DTOs; fail early on parse errors.
        let mut alert_configs: Vec<MetricAlertConfig> =
            Vec::with_capacity(alert_yaml_configs.len());
        for alert in &alert_yaml_configs {
            let window_secs =
                temps_core::repo_config::parse_duration_secs(&alert.window).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Invalid `window` for alert '{}': {}",
                        alert.name, e
                    ))
                })?;

            let for_duration_secs =
                temps_core::repo_config::parse_duration_secs(&alert.for_duration).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Invalid `for` for alert '{}': {}",
                        alert.name, e
                    ))
                })?;

            alert
                .detection_source()
                .map_err(WorkflowError::JobExecutionFailed)?;

            let detection = match &alert.detection {
                None => {
                    // detection_source() already guaranteed condition.is_some().
                    let (comparator, threshold) = alert.parsed_condition().map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Invalid `condition` for alert '{}': {}",
                            alert.name, e
                        ))
                    })?;
                    AlertDetectionSpec::Static {
                        comparator,
                        threshold,
                    }
                }
                Some(d) => AlertDetectionSpec::Anomaly {
                    algorithm: d.algorithm.clone().unwrap_or_else(|| "robust".to_string()),
                    deviations: d.deviations.unwrap_or(3.0),
                    direction: d.direction.clone().unwrap_or_else(|| "both".to_string()),
                    seasonality: d.seasonality.clone().unwrap_or_else(|| "none".to_string()),
                    pct_anomalous: d.pct_anomalous.unwrap_or(1.0),
                    baseline_lookback_days: d.baseline_lookback_days,
                },
            };

            alert_configs.push(MetricAlertConfig {
                name: alert.name.clone(),
                metric_name: alert.metric.clone(),
                aggregation: alert.aggregation.clone(),
                detection,
                window_secs,
                for_duration_secs,
                severity: alert.severity.clone(),
                enabled: alert.enabled,
                label_filters: alert.label_filters_pairs(),
                group_by: alert.group_by_keys(),
            });
        }

        match self
            .alert_service
            .configure_alerts(self.project_id, self.environment_id, alert_configs)
            .await
        {
            Ok(()) => {
                self.log("✅ Metric alert configuration completed successfully".to_string())
                    .await?;
            }
            Err(e) => {
                let error_msg = format!("❌ Failed to configure metric alerts: {}", e);
                self.log(error_msg.clone()).await?;
                return Err(WorkflowError::JobExecutionFailed(error_msg));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl WorkflowTask for ConfigureMetricAlertsJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Configure Metric Alerts"
    }

    fn description(&self) -> &str {
        "Reconciles metric alert rules from .temps.yaml with the database"
    }

    fn depends_on(&self) -> Vec<String> {
        // Depend on deploy_container so we only run once the deployment is live.
        // The download_job_id is consumed via context inside execute().
        vec![self.deploy_container_job_id.clone()]
    }

    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let repo_output = RepositoryOutput::from_context(&context, &self.download_job_id)?;
        self.configure_alerts(&repo_output).await?;
        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        RepositoryOutput::from_context(context, &self.download_job_id)?;
        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}

/// Builder for `ConfigureMetricAlertsJob`.
pub struct ConfigureMetricAlertsJobBuilder {
    job_id: Option<String>,
    download_job_id: Option<String>,
    deploy_container_job_id: Option<String>,
    project_id: Option<i32>,
    environment_id: Option<i32>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl ConfigureMetricAlertsJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            download_job_id: None,
            deploy_container_job_id: None,
            project_id: None,
            environment_id: None,
            log_id: None,
            log_service: None,
        }
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn download_job_id(mut self, download_job_id: String) -> Self {
        self.download_job_id = Some(download_job_id);
        self
    }

    pub fn deploy_container_job_id(mut self, deploy_container_job_id: String) -> Self {
        self.deploy_container_job_id = Some(deploy_container_job_id);
        self
    }

    pub fn project_id(mut self, project_id: i32) -> Self {
        self.project_id = Some(project_id);
        self
    }

    pub fn environment_id(mut self, environment_id: i32) -> Self {
        self.environment_id = Some(environment_id);
        self
    }

    pub fn log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    pub fn build(
        self,
        db: Arc<DbConnection>,
        alert_service: Arc<dyn MetricAlertConfigService>,
    ) -> Result<ConfigureMetricAlertsJob, WorkflowError> {
        let job_id = self
            .job_id
            .unwrap_or_else(|| "configure_metric_alerts".to_string());
        let download_job_id = self.download_job_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("download_job_id is required".to_string())
        })?;
        let deploy_container_job_id = self.deploy_container_job_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("deploy_container_job_id is required".to_string())
        })?;
        let project_id = self.project_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("project_id is required".to_string())
        })?;
        let environment_id = self.environment_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("environment_id is required".to_string())
        })?;

        let mut job = ConfigureMetricAlertsJob::new(
            job_id,
            download_job_id,
            deploy_container_job_id,
            project_id,
            environment_id,
            db,
            alert_service,
        );

        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }

        Ok(job)
    }
}

impl Default for ConfigureMetricAlertsJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ── Mock service ─────────────────────────────────────────────────────────

    struct MockMetricAlertConfigService {
        should_fail: bool,
        captured: Arc<std::sync::Mutex<Vec<MetricAlertConfig>>>,
        captured_project_id: Arc<std::sync::Mutex<Option<i32>>>,
        captured_environment_id: Arc<std::sync::Mutex<Option<i32>>>,
    }

    impl MockMetricAlertConfigService {
        fn new() -> Self {
            Self {
                should_fail: false,
                captured: Arc::new(std::sync::Mutex::new(Vec::new())),
                captured_project_id: Arc::new(std::sync::Mutex::new(None)),
                captured_environment_id: Arc::new(std::sync::Mutex::new(None)),
            }
        }

        fn with_failure() -> Self {
            Self {
                should_fail: true,
                captured: Arc::new(std::sync::Mutex::new(Vec::new())),
                captured_project_id: Arc::new(std::sync::Mutex::new(None)),
                captured_environment_id: Arc::new(std::sync::Mutex::new(None)),
            }
        }

        fn captured_configs(&self) -> Vec<MetricAlertConfig> {
            self.captured.lock().unwrap().clone()
        }

        fn captured_project_id(&self) -> Option<i32> {
            *self.captured_project_id.lock().unwrap()
        }

        fn captured_environment_id(&self) -> Option<i32> {
            *self.captured_environment_id.lock().unwrap()
        }
    }

    #[async_trait]
    impl MetricAlertConfigService for MockMetricAlertConfigService {
        async fn configure_alerts(
            &self,
            project_id: i32,
            environment_id: i32,
            configs: Vec<MetricAlertConfig>,
        ) -> Result<(), MetricAlertConfigError> {
            if self.should_fail {
                return Err(MetricAlertConfigError::ConfigError(
                    "Mock failure".to_string(),
                ));
            }
            *self.captured_project_id.lock().unwrap() = Some(project_id);
            *self.captured_environment_id.lock().unwrap() = Some(environment_id);
            self.captured.lock().unwrap().extend(configs);
            Ok(())
        }
    }

    // ── Unit tests ────────────────────────────────────────────────────────────

    #[test]
    fn test_noop_service() {
        let service = NoOpMetricAlertConfigService;
        let result = tokio_test::block_on(service.configure_alerts(1, 1, vec![]));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mock_service_success() {
        let service = MockMetricAlertConfigService::new();
        let configs = vec![MetricAlertConfig {
            name: "High error rate".to_string(),
            metric_name: "http.server.errors".to_string(),
            aggregation: "rate".to_string(),
            detection: AlertDetectionSpec::Static {
                comparator: ">".to_string(),
                threshold: 0.05,
            },
            window_secs: 300,
            for_duration_secs: 120,
            severity: "warning".to_string(),
            enabled: true,
            label_filters: vec![],
            group_by: vec![],
        }];

        let result = service.configure_alerts(7, 42, configs).await;
        assert!(result.is_ok());
        assert_eq!(service.captured_project_id(), Some(7));
        assert_eq!(service.captured_environment_id(), Some(42));
        let captured = service.captured_configs();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].name, "High error rate");
        match &captured[0].detection {
            AlertDetectionSpec::Static {
                comparator,
                threshold,
            } => {
                assert_eq!(comparator, ">");
                assert!((threshold - 0.05).abs() < f64::EPSILON);
            }
            AlertDetectionSpec::Anomaly { .. } => panic!("expected Static detection"),
        }
    }

    #[tokio::test]
    async fn test_mock_service_failure() {
        let service = MockMetricAlertConfigService::with_failure();
        let result = service.configure_alerts(1, 1, vec![]).await;
        assert!(result.is_err());
        match result {
            Err(MetricAlertConfigError::ConfigError(msg)) => {
                assert_eq!(msg, "Mock failure");
            }
            _ => panic!("Expected ConfigError"),
        }
    }

    #[test]
    fn test_builder_fields() {
        let builder = ConfigureMetricAlertsJobBuilder::new()
            .job_id("test_job".to_string())
            .download_job_id("download".to_string())
            .deploy_container_job_id("deploy_container".to_string())
            .project_id(7)
            .environment_id(42)
            .log_id("log_id".to_string());

        assert_eq!(builder.job_id, Some("test_job".to_string()));
        assert_eq!(builder.download_job_id, Some("download".to_string()));
        assert_eq!(
            builder.deploy_container_job_id,
            Some("deploy_container".to_string())
        );
        assert_eq!(builder.project_id, Some(7));
        assert_eq!(builder.environment_id, Some(42));
        assert_eq!(builder.log_id, Some("log_id".to_string()));
    }

    #[test]
    fn test_builder_defaults() {
        let builder = ConfigureMetricAlertsJobBuilder::default();
        assert!(builder.job_id.is_none());
        assert!(builder.download_job_id.is_none());
        assert!(builder.project_id.is_none());
        assert!(builder.environment_id.is_none());
    }

    #[test]
    fn test_error_display() {
        let err = MetricAlertConfigError::DatabaseError {
            project_id: 1,
            environment_id: 2,
            message: "connection failed".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("project 1"));
        assert!(msg.contains("environment 2"));
        assert!(msg.contains("connection failed"));

        let err = MetricAlertConfigError::InvalidConfig {
            name: "High error rate".to_string(),
            message: "bad comparator".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("High error rate"));
        assert!(msg.contains("bad comparator"));
    }

    // ── YAML parsing integration tests ────────────────────────────────────────

    #[test]
    fn test_parse_alert_yaml_config() {
        use temps_core::TempsConfig;

        let yaml = r#"
alerts:
  - name: High error rate
    metric: http.server.errors
    aggregation: rate
    condition: "> 0.05"
    window: 5m
    for: 2m
    severity: warning
  - name: Low throughput
    metric: http.server.requests
    condition: "< 10"
    window: 10m
    severity: info
"#;
        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(config.has_alerts());
        let alerts = config.alert_configs();
        assert_eq!(alerts.len(), 2);

        let a0 = &alerts[0];
        assert_eq!(a0.window_secs().unwrap(), 300);
        assert_eq!(a0.for_duration_secs().unwrap(), 120);
        let (cmp, t) = a0.parsed_condition().unwrap();
        assert_eq!(cmp, ">");
        assert!((t - 0.05).abs() < f64::EPSILON);

        // defaults on second alert
        let a1 = &alerts[1];
        assert_eq!(a1.aggregation, "avg"); // default
        assert_eq!(a1.for_duration, "0s"); // default
    }

    #[test]
    fn test_parse_invalid_condition_caught_before_service_call() {
        use temps_core::repo_config::parse_condition;
        assert!(parse_condition("!= 5").is_err());
        assert!(parse_condition("> abc").is_err());
    }

    #[test]
    fn test_parse_anomaly_alert_yaml_config() {
        use temps_core::TempsConfig;

        let yaml = r#"
alerts:
  - name: CPU usage anomaly
    metric: system.cpu.utilization
    aggregation: avg
    window: 5m
    severity: warning
    detection:
      kind: anomaly
      algorithm: robust
      deviations: 3.0
      direction: both
      seasonality: daily
      pct_anomalous: 1.0
      baseline_lookback_days: 14
"#;
        let config = TempsConfig::from_yaml(yaml).unwrap();
        let alerts = config.alert_configs();
        assert_eq!(alerts.len(), 1);
        let a0 = alerts[0];
        assert!(a0.condition.is_none());
        assert!(a0.detection_source().is_ok());
        let d = a0.detection.as_ref().unwrap();
        assert_eq!(d.kind, "anomaly");
        assert_eq!(d.algorithm.as_deref(), Some("robust"));
        assert_eq!(d.baseline_lookback_days, Some(14));
    }

    #[test]
    fn test_detection_source_rejects_both_and_neither() {
        use temps_core::TempsConfig;

        let both = r#"
alerts:
  - name: Bad rule
    metric: http.server.errors
    condition: "> 0.05"
    window: 5m
    detection:
      kind: anomaly
"#;
        let config = TempsConfig::from_yaml(both).unwrap();
        let err = config.alert_configs()[0].detection_source().unwrap_err();
        assert!(err.contains("both"), "unexpected error: {err}");

        let neither = r#"
alerts:
  - name: Bad rule
    metric: http.server.errors
    window: 5m
"#;
        let config = TempsConfig::from_yaml(neither).unwrap();
        let err = config.alert_configs()[0].detection_source().unwrap_err();
        assert!(err.contains("neither"), "unexpected error: {err}");
    }

    #[test]
    fn test_detection_source_rejects_unevaluated_kinds() {
        use temps_core::TempsConfig;

        for kind in ["forecast", "outlier", "auto_watch"] {
            let yaml = format!(
                r#"
alerts:
  - name: Unsupported rule
    metric: http.server.errors
    window: 5m
    detection:
      kind: {kind}
"#
            );
            let config = TempsConfig::from_yaml(&yaml).unwrap();
            let err = config.alert_configs()[0].detection_source().unwrap_err();
            assert!(
                err.contains("no evaluator yet"),
                "kind '{kind}' should be rejected: {err}"
            );
        }
    }

    #[tokio::test]
    async fn test_configure_alerts_builds_anomaly_detection_spec() {
        // End-to-end through the job's parsing path: YAML detection block ->
        // AlertDetectionSpec::Anomaly with the right fields threaded through.
        use temps_core::TempsConfig;

        let yaml = r#"
alerts:
  - name: CPU usage anomaly
    metric: system.cpu.utilization
    window: 5m
    detection:
      kind: anomaly
      deviations: 4.0
      direction: above
"#;
        let config = TempsConfig::from_yaml(yaml).unwrap();
        let alert = &config.alert_configs()[0];
        assert!(alert.detection_source().is_ok());

        let d = alert.detection.as_ref().unwrap();
        let spec = AlertDetectionSpec::Anomaly {
            algorithm: d.algorithm.clone().unwrap_or_else(|| "robust".to_string()),
            deviations: d.deviations.unwrap_or(3.0),
            direction: d.direction.clone().unwrap_or_else(|| "both".to_string()),
            seasonality: d.seasonality.clone().unwrap_or_else(|| "none".to_string()),
            pct_anomalous: d.pct_anomalous.unwrap_or(1.0),
            baseline_lookback_days: d.baseline_lookback_days,
        };
        match spec {
            AlertDetectionSpec::Anomaly {
                deviations,
                direction,
                ..
            } => {
                assert_eq!(deviations, 4.0);
                assert_eq!(direction, "above");
            }
            AlertDetectionSpec::Static { .. } => panic!("expected Anomaly detection"),
        }
    }

    #[test]
    fn test_validate_alert_count_within_limit() {
        assert!(validate_alert_count(0).is_ok());
        assert!(validate_alert_count(MAX_ALERTS_PER_TEMPS_YAML).is_ok());
    }

    #[test]
    fn test_validate_alert_count_rejects_over_limit() {
        let err = validate_alert_count(MAX_ALERTS_PER_TEMPS_YAML + 1).unwrap_err();
        assert!(err.contains(&(MAX_ALERTS_PER_TEMPS_YAML + 1).to_string()));
        assert!(err.contains(&MAX_ALERTS_PER_TEMPS_YAML.to_string()));
    }
}
