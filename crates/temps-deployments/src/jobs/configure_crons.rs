//! Configure Crons Job
//!
//! Configures cron jobs from .temps.yaml configuration file after deployment

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

/// Service interface for cron configuration
#[async_trait]
pub trait CronConfigService: Send + Sync {
    async fn configure_crons(
        &self,
        project_id: i32,
        environment_id: i32,
        cron_configs: Vec<CronConfig>,
    ) -> Result<(), CronConfigError>;
}

/// Cron configuration for service layer
#[derive(Debug, Clone)]
pub struct CronConfig {
    pub path: String,
    pub schedule: String,
}

/// Errors that can occur during cron configuration
#[derive(Debug, thiserror::Error)]
pub enum CronConfigError {
    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Invalid cron schedule: {0}")]
    InvalidSchedule(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),
}

/// No-op implementation of CronConfigService for when cron service is not available
pub struct NoOpCronConfigService;

#[async_trait]
impl CronConfigService for NoOpCronConfigService {
    async fn configure_crons(
        &self,
        _project_id: i32,
        _environment_id: i32,
        _cron_configs: Vec<CronConfig>,
    ) -> Result<(), CronConfigError> {
        // No-op: cron service not available
        warn!("Cron configuration skipped - no cron service available");
        Ok(())
    }
}

/// Job for configuring cron jobs from repository configuration
pub struct ConfigureCronsJob {
    job_id: String,
    download_job_id: String,
    deploy_container_job_id: String,
    project_id: i32,
    environment_id: i32,
    db: Arc<DbConnection>,
    cron_service: Arc<dyn CronConfigService>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for ConfigureCronsJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigureCronsJob")
            .field("job_id", &self.job_id)
            .field("download_job_id", &self.download_job_id)
            .field("deploy_container_job_id", &self.deploy_container_job_id)
            .field("project_id", &self.project_id)
            .field("environment_id", &self.environment_id)
            .finish()
    }
}

impl ConfigureCronsJob {
    pub fn new(
        job_id: String,
        download_job_id: String,
        deploy_container_job_id: String,
        project_id: i32,
        environment_id: i32,
        db: Arc<DbConnection>,
        cron_service: Arc<dyn CronConfigService>,
    ) -> Self {
        Self {
            job_id,
            download_job_id,
            deploy_container_job_id,
            project_id,
            environment_id,
            db,
            cron_service,
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

    /// Write log message to job-specific log file
    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content/emojis
        let level = Self::detect_log_level(&message);

        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        Ok(())
    }

    /// Detect log level from message content
    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌") || message.contains("Failed") || message.contains("Error") || message.contains("error") {
            LogLevel::Error
        } else {
            LogLevel::Info
        }
    }

    /// Load and parse .temps.yaml configuration
    async fn load_temps_config(
        &self,
        repo_dir: &Path,
        project: &projects::Model,
    ) -> Result<Option<TempsConfig>, WorkflowError> {
        let project_dir = repo_dir.join(&project.directory);
        let config_path = project_dir.join(".temps.yaml");

        if !config_path.exists() {
            self.log(format!(
                "No .temps.yaml found at {:?}, skipping cron configuration",
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

    /// Configure cron jobs based on repository configuration
    async fn configure_crons(&self, repo_output: &RepositoryOutput) -> Result<(), WorkflowError> {
        self.log("Starting cron configuration".to_string())
            .await?;

        // Load project to get directory
        let project = projects::Entity::find_by_id(self.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to load project: {}", e)))?
            .ok_or_else(|| {
                WorkflowError::Other(format!("Project {} not found", self.project_id))
            })?;

        // Load .temps.yaml configuration
        let config = match self
            .load_temps_config(&repo_output.repo_dir, &project)
            .await?
        {
            Some(config) => config,
            None => {
                self.log("No cron configuration needed".to_string())
                    .await?;
                return Ok(());
            }
        };

        // Check if config has cron jobs
        if !config.has_crons() {
            self.log("No cron jobs defined in .temps.yaml".to_string())
                .await?;
            return Ok(());
        }

        let cron_jobs = config.cron_jobs();
        self.log(format!(
            "Found {} cron job(s) to configure",
            cron_jobs.len()
        ))
        .await?;

        // Convert to service layer format
        let cron_configs: Vec<CronConfig> = cron_jobs
            .iter()
            .map(|job| CronConfig {
                path: job.path.clone(),
                schedule: job.schedule.clone(),
            })
            .collect();

        // Configure crons via service
        self.cron_service
            .configure_crons(self.project_id, self.environment_id, cron_configs)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to configure cron jobs: {}", e))
            })?;

        self.log("Cron configuration completed successfully".to_string())
            .await?;

        Ok(())
    }
}

#[async_trait]
impl WorkflowTask for ConfigureCronsJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Configure Crons"
    }

    fn description(&self) -> &str {
        "Configures scheduled cron jobs from repository configuration"
    }

    fn depends_on(&self) -> Vec<String> {
        // Depend on deploy_container to ensure deployment is complete before configuring crons
        // We still need download_job_id to access repository outputs in execute()
        vec![self.deploy_container_job_id.clone()]
    }

    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Get typed output from the download job
        let repo_output = RepositoryOutput::from_context(&context, &self.download_job_id)?;

        // Configure cron jobs
        self.configure_crons(&repo_output).await?;

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Verify that the download job output is available
        RepositoryOutput::from_context(context, &self.download_job_id)?;

        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        // No cleanup needed for cron configuration
        Ok(())
    }
}

/// Builder for ConfigureCronsJob
pub struct ConfigureCronsJobBuilder {
    job_id: Option<String>,
    download_job_id: Option<String>,
    deploy_container_job_id: Option<String>,
    project_id: Option<i32>,
    environment_id: Option<i32>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl ConfigureCronsJobBuilder {
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
        cron_service: Arc<dyn CronConfigService>,
    ) -> Result<ConfigureCronsJob, WorkflowError> {
        let job_id = self.job_id.unwrap_or_else(|| "configure_crons".to_string());
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

        let mut job = ConfigureCronsJob::new(
            job_id,
            download_job_id,
            deploy_container_job_id,
            project_id,
            environment_id,
            db,
            cron_service,
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

impl Default for ConfigureCronsJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Mock CronConfigService for testing
    struct MockCronConfigService {
        should_fail: bool,
        captured_configs: Arc<std::sync::Mutex<Vec<CronConfig>>>,
    }

    impl MockCronConfigService {
        fn new() -> Self {
            Self {
                should_fail: false,
                captured_configs: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn with_failure() -> Self {
            Self {
                should_fail: true,
                captured_configs: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn get_captured_configs(&self) -> Vec<CronConfig> {
            self.captured_configs.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl CronConfigService for MockCronConfigService {
        async fn configure_crons(
            &self,
            _project_id: i32,
            _environment_id: i32,
            cron_configs: Vec<CronConfig>,
        ) -> Result<(), CronConfigError> {
            if self.should_fail {
                return Err(CronConfigError::ConfigError("Mock failure".to_string()));
            }
            self.captured_configs.lock().unwrap().extend(cron_configs);
            Ok(())
        }
    }

    #[test]
    fn test_parse_temps_config() {
        let yaml = r#"
cron:
  - path: /api/cron/cleanup
    schedule: "0 0 * * *"
  - path: /api/cron/reports
    schedule: "0 9 * * 1"
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(config.has_crons());

        let crons = config.cron_jobs();
        assert_eq!(crons.len(), 2);
        assert_eq!(crons[0].path, "/api/cron/cleanup");
        assert_eq!(crons[0].schedule, "0 0 * * *");
        assert_eq!(crons[1].path, "/api/cron/reports");
        assert_eq!(crons[1].schedule, "0 9 * * 1");
    }

    #[test]
    fn test_parse_temps_config_no_crons() {
        let yaml = r#"
# No cron configuration
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(!config.has_crons());
    }

    #[test]
    fn test_parse_temps_config_empty_crons() {
        let yaml = r#"
cron: []
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(!config.has_crons());
        assert_eq!(config.cron_jobs().len(), 0);
    }

    #[test]
    fn test_parse_temps_config_with_names() {
        let yaml = r#"
cron:
  - path: /api/cron/cleanup
    schedule: "0 0 * * *"
    name: "Daily Cleanup"
  - path: /api/cron/backup
    schedule: "0 2 * * *"
    name: "Nightly Backup"
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(config.has_crons());

        let crons = config.cron_jobs();
        assert_eq!(crons.len(), 2);
        assert_eq!(crons[0].name.as_deref(), Some("Daily Cleanup"));
        assert_eq!(crons[1].name.as_deref(), Some("Nightly Backup"));
    }

    #[test]
    fn test_parse_temps_config_mixed_with_other_sections() {
        let yaml = r#"
cron:
  - path: /api/cron/task
    schedule: "*/5 * * * *"

build:
  dockerfile: Dockerfile
  context: .

env:
  NODE_ENV: production
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(config.has_crons());
        assert!(config.has_build_config());
        assert!(config.env.is_some());

        let crons = config.cron_jobs();
        assert_eq!(crons.len(), 1);
        assert_eq!(crons[0].path, "/api/cron/task");
    }

    #[test]
    fn test_cron_config_conversion() {
        let yaml = r#"
cron:
  - path: /api/health
    schedule: "*/1 * * * *"
  - path: /api/cleanup
    schedule: "0 0 * * *"
  - path: /api/reports
    schedule: "0 9 * * 1"
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        let cron_jobs = config.cron_jobs();

        // Convert to CronConfig format
        let cron_configs: Vec<CronConfig> = cron_jobs
            .iter()
            .map(|job| CronConfig {
                path: job.path.clone(),
                schedule: job.schedule.clone(),
            })
            .collect();

        assert_eq!(cron_configs.len(), 3);
        assert_eq!(cron_configs[0].path, "/api/health");
        assert_eq!(cron_configs[0].schedule, "*/1 * * * *");
        assert_eq!(cron_configs[1].path, "/api/cleanup");
        assert_eq!(cron_configs[1].schedule, "0 0 * * *");
        assert_eq!(cron_configs[2].path, "/api/reports");
        assert_eq!(cron_configs[2].schedule, "0 9 * * 1");
    }

    #[test]
    fn test_noop_cron_service() {
        let service = NoOpCronConfigService;
        let configs = vec![CronConfig {
            path: "/test".to_string(),
            schedule: "* * * * *".to_string(),
        }];

        // Should succeed without doing anything
        let result = tokio_test::block_on(service.configure_crons(1, 1, configs));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mock_cron_service_success() {
        let service = MockCronConfigService::new();
        let configs = vec![
            CronConfig {
                path: "/api/cron/task1".to_string(),
                schedule: "0 0 * * *".to_string(),
            },
            CronConfig {
                path: "/api/cron/task2".to_string(),
                schedule: "0 12 * * *".to_string(),
            },
        ];

        let result = service.configure_crons(1, 1, configs).await;
        assert!(result.is_ok());

        let captured = service.get_captured_configs();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].path, "/api/cron/task1");
        assert_eq!(captured[1].path, "/api/cron/task2");
    }

    #[tokio::test]
    async fn test_mock_cron_service_failure() {
        let service = MockCronConfigService::with_failure();
        let configs = vec![CronConfig {
            path: "/api/cron/task".to_string(),
            schedule: "0 0 * * *".to_string(),
        }];

        let result = service.configure_crons(1, 1, configs).await;
        assert!(result.is_err());

        match result {
            Err(CronConfigError::ConfigError(msg)) => {
                assert_eq!(msg, "Mock failure");
            }
            _ => panic!("Expected ConfigError"),
        }
    }

    #[test]
    fn test_cron_config_builder() {
        let builder = ConfigureCronsJobBuilder::new()
            .job_id("test_job".to_string())
            .download_job_id("download".to_string())
            .project_id(1)
            .environment_id(2)
            .log_id("test_log".to_string());

        // Verify builder fields are set (can't access private fields, but can verify build works)
        // This would require a mock DB and service to fully test
        assert_eq!(builder.job_id, Some("test_job".to_string()));
        assert_eq!(builder.download_job_id, Some("download".to_string()));
        assert_eq!(builder.project_id, Some(1));
        assert_eq!(builder.environment_id, Some(2));
        assert_eq!(builder.log_id, Some("test_log".to_string()));
    }

    #[test]
    fn test_cron_config_builder_defaults() {
        let builder = ConfigureCronsJobBuilder::default();
        assert!(builder.job_id.is_none());
        assert!(builder.download_job_id.is_none());
        assert!(builder.project_id.is_none());
        assert!(builder.environment_id.is_none());
    }

    #[test]
    fn test_cron_config_error_display() {
        let err = CronConfigError::DatabaseError("Connection failed".to_string());
        assert_eq!(err.to_string(), "Database error: Connection failed");

        let err = CronConfigError::InvalidSchedule("Bad format".to_string());
        assert_eq!(err.to_string(), "Invalid cron schedule: Bad format");

        let err = CronConfigError::ConfigError("Missing field".to_string());
        assert_eq!(err.to_string(), "Configuration error: Missing field");
    }

    #[test]
    fn test_parse_invalid_yaml() {
        let yaml = r#"
cron:
  - path: /test
    schedule: "0 0 * * *"
    invalid_field_that_breaks_parsing: [[[
"#;

        let result = TempsConfig::from_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_malformed_yaml() {
        let yaml = r#"
cron:
  - path
    schedule
"#;

        let result = TempsConfig::from_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_cron_config_vec() {
        let yaml = r#"
cron: []
"#;
        let config = TempsConfig::from_yaml(yaml).unwrap();

        // Should handle empty cron array gracefully
        let cron_jobs = config.cron_jobs();
        assert_eq!(cron_jobs.len(), 0);

        let cron_configs: Vec<CronConfig> = cron_jobs
            .iter()
            .map(|job| CronConfig {
                path: job.path.clone(),
                schedule: job.schedule.clone(),
            })
            .collect();

        assert_eq!(cron_configs.len(), 0);
    }

    #[test]
    fn test_single_cron_job() {
        let yaml = r#"
cron:
  - path: /api/single
    schedule: "0 0 * * *"
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(config.has_crons());

        let crons = config.cron_jobs();
        assert_eq!(crons.len(), 1);
        assert_eq!(crons[0].path, "/api/single");
        assert_eq!(crons[0].schedule, "0 0 * * *");
    }

    #[test]
    fn test_complex_cron_schedules() {
        let yaml = r#"
cron:
  - path: /api/every-minute
    schedule: "* * * * *"
  - path: /api/every-hour
    schedule: "0 * * * *"
  - path: /api/daily-midnight
    schedule: "0 0 * * *"
  - path: /api/weekly-monday
    schedule: "0 0 * * 1"
  - path: /api/monthly-first
    schedule: "0 0 1 * *"
  - path: /api/complex
    schedule: "*/15 9-17 * * 1-5"
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        let crons = config.cron_jobs();

        assert_eq!(crons.len(), 6);
        assert_eq!(crons[0].schedule, "* * * * *");
        assert_eq!(crons[1].schedule, "0 * * * *");
        assert_eq!(crons[2].schedule, "0 0 * * *");
        assert_eq!(crons[3].schedule, "0 0 * * 1");
        assert_eq!(crons[4].schedule, "0 0 1 * *");
        assert_eq!(crons[5].schedule, "*/15 9-17 * * 1-5");
    }
}
