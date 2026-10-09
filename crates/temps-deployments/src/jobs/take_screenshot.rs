// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Take Screenshot Job
//!
//! Captures a screenshot of the deployed application

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::{JobResult, UtcDateTime, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_entities::{deployments, prelude::*};
use temps_logs::{LogLevel, LogService};
use temps_screenshots::ScreenshotServiceTrait;

/// Job for capturing screenshots of deployed applications
pub struct TakeScreenshotJob {
    job_id: String,
    deployment_id: i32,
    screenshot_service: Arc<dyn ScreenshotServiceTrait>,
    config_service: Arc<ConfigService>,
    db: Arc<DbConnection>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for TakeScreenshotJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TakeScreenshotJob")
            .field("job_id", &self.job_id)
            .field("deployment_id", &self.deployment_id)
            .field("screenshot_service", &"ScreenshotService")
            .finish()
    }
}

impl TakeScreenshotJob {
    pub fn new(
        job_id: String,
        deployment_id: i32,
        screenshot_service: Arc<dyn ScreenshotServiceTrait>,
        config_service: Arc<ConfigService>,
        db: Arc<DbConnection>,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            screenshot_service,
            config_service,
            db,
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
        if message.contains("✅")
            || message.contains("💾")
            || message.contains("Complete")
            || message.contains("success")
            || message.contains("captured")
        {
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
}

/// Why capturing a deployment's screenshot failed.
#[derive(Debug, thiserror::Error)]
pub enum DeploymentScreenshotError {
    #[error("Failed to resolve the public URL of deployment {deployment_id}: {reason}")]
    Url { deployment_id: i32, reason: String },

    #[error("Screenshot provider '{provider}' failed to capture deployment {deployment_id} at {url}: {source}")]
    Capture {
        deployment_id: i32,
        provider: &'static str,
        url: String,
        #[source]
        source: temps_screenshots::ScreenshotError,
    },

    #[error("Screenshot of deployment {deployment_id} did not finish within {timeout_secs}s")]
    TimedOut {
        deployment_id: i32,
        timeout_secs: u64,
    },

    #[error("Deployment {deployment_id} no longer exists; its screenshot was not recorded")]
    DeploymentNotFound { deployment_id: i32 },

    #[error("Failed to record screenshot location for deployment {deployment_id}: {source}")]
    Database {
        deployment_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// A screenshot that was captured, validated as an image, stored, and
/// recorded on the deployment.
#[derive(Debug, Clone)]
pub struct CapturedScreenshot {
    pub url: String,
    /// Path relative to the static directory, as stored on the deployment.
    pub screenshot_location: String,
    pub captured_at: UtcDateTime,
}

/// Capture the deployment's public URL, store the image and point
/// `deployments.screenshot_location` at it.
///
/// Returns only after all three happened: the screenshot service rejects
/// anything that is not an image before storing it, and the location is
/// written last, so `Ok` means a real image is readable at the recorded path.
pub async fn capture_deployment_screenshot(
    deployment_id: i32,
    screenshot_service: &dyn ScreenshotServiceTrait,
    config_service: &ConfigService,
    db: &DbConnection,
) -> Result<CapturedScreenshot, DeploymentScreenshotError> {
    let url = config_service
        .get_deployment_url(deployment_id)
        .await
        .map_err(|e| DeploymentScreenshotError::Url {
            deployment_id,
            reason: e.to_string(),
        })?;

    let captured_at = chrono::Utc::now();
    let screenshot_location = format!(
        "screenshots/deployment-{}-{}.png",
        deployment_id,
        captured_at.format("%Y%m%d-%H%M%S")
    );

    screenshot_service
        .capture_and_save(&url, &screenshot_location)
        .await
        .map_err(|source| DeploymentScreenshotError::Capture {
            deployment_id,
            provider: screenshot_service.provider_name(),
            url: url.clone(),
            source,
        })?;

    let deployment = Deployments::find_by_id(deployment_id)
        .one(db)
        .await
        .map_err(|source| DeploymentScreenshotError::Database {
            deployment_id,
            source,
        })?
        .ok_or(DeploymentScreenshotError::DeploymentNotFound { deployment_id })?;

    let mut active_deployment: deployments::ActiveModel = deployment.into();
    active_deployment.screenshot_location = Set(Some(screenshot_location.clone()));
    active_deployment.updated_at = Set(chrono::Utc::now());
    active_deployment
        .update(db)
        .await
        .map_err(|source| DeploymentScreenshotError::Database {
            deployment_id,
            source,
        })?;

    Ok(CapturedScreenshot {
        url,
        screenshot_location,
        captured_at,
    })
}

#[async_trait]
impl WorkflowTask for TakeScreenshotJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Take Screenshot"
    }

    fn description(&self) -> &str {
        "Captures a screenshot of the deployed application"
    }

    fn depends_on(&self) -> Vec<String> {
        // No specific job dependencies - just needs deployment to be complete
        vec![]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Wait for the application to fully initialize after deployment
        // The route table needs time to be ready, and the application needs to start serving content.
        // 15 seconds provides enough time for most applications to complete their startup sequence
        // and avoids capturing "building" or "loading" states in the screenshot.
        self.log("Waiting for application to fully initialize...".to_string())
            .await?;
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        self.log(format!(
            "Taking screenshot for deployment ID: {}",
            self.deployment_id
        ))
        .await?;

        self.log(format!(
            "Using screenshot service: {}",
            self.screenshot_service.provider_name()
        ))
        .await?;

        let captured = capture_deployment_screenshot(
            self.deployment_id,
            self.screenshot_service.as_ref(),
            self.config_service.as_ref(),
            self.db.as_ref(),
        )
        .await
        .map_err(|e| WorkflowError::JobExecutionFailed(e.to_string()))?;

        self.log(format!("Deployment URL: {}", captured.url))
            .await?;
        self.log(format!(
            "Screenshot captured: {}",
            captured.screenshot_location
        ))
        .await?;

        // Set job outputs
        context.set_output(
            &self.job_id,
            "captured_at",
            captured.captured_at.timestamp(),
        )?;
        context.set_output(&self.job_id, "deployment_id", self.deployment_id)?;
        context.set_output(
            &self.job_id,
            "screenshot_location",
            captured.screenshot_location,
        )?;

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(
        &self,
        _context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        // Basic validation
        if self.job_id.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "job_id cannot be empty".to_string(),
            ));
        }
        if self.deployment_id <= 0 {
            return Err(WorkflowError::JobValidationFailed(
                "deployment_id must be positive".to_string(),
            ));
        }

        // Check if screenshot service is available, surfacing *why* it is not so the
        // operator gets an actionable message instead of a silent skip.
        if let Err(e) = self.screenshot_service.check_provider_availability().await {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Screenshot provider '{}' is not available: {}",
                self.screenshot_service.provider_name(),
                e
            )));
        }

        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Screenshots persist after job completion
        Ok(())
    }
}

/// Builder for TakeScreenshotJob
pub struct TakeScreenshotJobBuilder {
    job_id: Option<String>,
    deployment_id: Option<i32>,
    screenshot_service: Option<Arc<dyn ScreenshotServiceTrait>>,
    config_service: Option<Arc<ConfigService>>,
    db: Option<Arc<DbConnection>>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl TakeScreenshotJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            deployment_id: None,
            screenshot_service: None,
            config_service: None,
            db: None,
            log_id: None,
            log_service: None,
        }
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn deployment_id(mut self, deployment_id: i32) -> Self {
        self.deployment_id = Some(deployment_id);
        self
    }

    pub fn screenshot_service(
        mut self,
        screenshot_service: Arc<dyn ScreenshotServiceTrait>,
    ) -> Self {
        self.screenshot_service = Some(screenshot_service);
        self
    }

    pub fn config_service(mut self, config_service: Arc<ConfigService>) -> Self {
        self.config_service = Some(config_service);
        self
    }

    pub fn db(mut self, db: Arc<DbConnection>) -> Self {
        self.db = Some(db);
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

    pub fn build(self) -> Result<TakeScreenshotJob, WorkflowError> {
        let job_id = self.job_id.unwrap_or_else(|| "take_screenshot".to_string());
        let deployment_id = self.deployment_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("deployment_id is required".to_string())
        })?;
        let screenshot_service = self.screenshot_service.ok_or_else(|| {
            WorkflowError::JobValidationFailed("screenshot_service is required".to_string())
        })?;
        let config_service = self.config_service.ok_or_else(|| {
            WorkflowError::JobValidationFailed("config_service is required".to_string())
        })?;
        let db = self
            .db
            .ok_or_else(|| WorkflowError::JobValidationFailed("db is required".to_string()))?;

        let mut job = TakeScreenshotJob::new(
            job_id,
            deployment_id,
            screenshot_service,
            config_service,
            db,
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

impl Default for TakeScreenshotJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::path::PathBuf;
    use temps_screenshots::{ScreenshotError, ScreenshotResult};

    /// Screenshot service whose provider is unavailable, mirroring a server where
    /// Chrome is installed but its shared libraries are missing.
    struct UnavailableScreenshotService;

    #[async_trait]
    impl ScreenshotServiceTrait for UnavailableScreenshotService {
        async fn capture_and_save(&self, _url: &str, _filename: &str) -> ScreenshotResult<PathBuf> {
            Err(ScreenshotError::ProviderNotConfigured)
        }

        async fn capture(&self, _url: &str) -> ScreenshotResult<Vec<u8>> {
            Err(ScreenshotError::ProviderNotConfigured)
        }

        async fn is_enabled(&self) -> bool {
            true
        }

        fn provider_name(&self) -> &'static str {
            "local-headless-chrome"
        }

        async fn is_provider_available(&self) -> bool {
            false
        }

        async fn check_provider_availability(&self) -> ScreenshotResult<()> {
            Err(ScreenshotError::ChromeError(
                "Failed to launch Chrome browser: libnss3.so: cannot open shared object file"
                    .to_string(),
            ))
        }
    }

    fn test_config_service(db: Arc<DbConnection>) -> Arc<ConfigService> {
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: "postgres://test".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:0".to_string(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            docker_extra_networks: Vec::new(),
            data_dir: std::path::PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
        });
        Arc::new(ConfigService::new(server_config, db))
    }

    struct NoopLogWriter;

    #[async_trait]
    impl temps_core::LogWriter for NoopLogWriter {
        async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            1
        }
    }

    fn test_context() -> WorkflowContext {
        WorkflowContext::new(
            "test-workflow".to_string(),
            1,
            1,
            1,
            Arc::new(NoopLogWriter),
        )
    }

    /// Regression test for #477: the prerequisite failure must name the provider
    /// and carry the underlying reason, so the deployment job's error message tells
    /// the operator what to actually fix.
    #[tokio::test]
    async fn test_validate_prerequisites_surfaces_provider_failure_reason() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let job = TakeScreenshotJob::new(
            "take_screenshot".to_string(),
            1,
            Arc::new(UnavailableScreenshotService),
            test_config_service(db.clone()),
            db,
        );

        let err = job
            .validate_prerequisites(&test_context())
            .await
            .expect_err("unavailable provider must fail prerequisites");

        let message = err.to_string();
        assert!(
            message.contains("local-headless-chrome"),
            "error should name the provider, got: {}",
            message
        );
        assert!(
            message.contains("libnss3.so"),
            "error should carry the underlying reason, got: {}",
            message
        );
    }
}
