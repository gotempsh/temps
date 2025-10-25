//! Take Screenshot Job
//!
//! Captures a screenshot of the deployed application

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::{JobResult, UtcDateTime, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_entities::{deployments, prelude::*};
use temps_logs::{LogLevel, LogService};
use temps_screenshots::ScreenshotServiceTrait;

/// Output from TakeScreenshotJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotOutput {
    pub captured_at: UtcDateTime,
}

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
        if message.contains("✅") || message.contains("💾") || message.contains("Complete") || message.contains("success") || message.contains("captured") {
            LogLevel::Success
        } else if message.contains("❌") || message.contains("Failed") || message.contains("Error") || message.contains("error") {
            LogLevel::Error
        } else {
            LogLevel::Info
        }
    }

    /// Capture screenshot using the screenshot service and save to disk
    async fn capture_screenshot(
        &self,
        deployment_url: &str,
        filename: &str,
    ) -> Result<ScreenshotOutput, WorkflowError> {
        self.log(format!("Capturing screenshot of: {}", deployment_url))
            .await?;

        // Generate screenshot path with timestamp structure
        let now = chrono::Utc::now();

        self.log(format!(
            "Using screenshot service: {}",
            self.screenshot_service.provider_name()
        ))
        .await?;

        // Capture and save screenshot using the screenshot service
        let screenshot_path = self
            .screenshot_service
            .capture_and_save(deployment_url, filename)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to capture screenshot: {}", e))
            })?;

        // Log relative path for cleaner output
        let static_dir = self.config_service.static_dir();
        let relative_display = screenshot_path
            .strip_prefix(&static_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| screenshot_path.display().to_string());

        self.log(format!("Screenshot saved to: {}", relative_display))
            .await?;

        Ok(ScreenshotOutput { captured_at: now })
    }
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
        // it takes a bit of time for the route table to be ready
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        self.log(format!(
            "Taking screenshot for deployment ID: {}",
            self.deployment_id
        ))
        .await?;

        // Get deployment URL from config service using deployment_id
        let deployment_url = self
            .config_service
            .get_deployment_url(self.deployment_id)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to get deployment URL: {}", e))
            })?;

        self.log(format!("Deployment URL: {}", deployment_url))
            .await?;

        // Generate screenshot filename with timestamp
        let now = chrono::Utc::now();
        let filename = format!(
            "screenshots/deployment-{}-{}.png",
            self.deployment_id,
            now.format("%Y%m%d-%H%M%S")
        );

        // Capture screenshot
        let screenshot_output = self.capture_screenshot(&deployment_url, &filename).await?;

        self.log(format!("Screenshot captured: {}", filename))
            .await?;

        // Update deployment with screenshot location (relative path)
        let deployment = Deployments::find_by_id(self.deployment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to find deployment: {}", e)))?
            .ok_or_else(|| {
                WorkflowError::Other(format!("Deployment {} not found", self.deployment_id))
            })?;

        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.screenshot_location = Set(Some(filename.clone()));
        active_deployment.updated_at = Set(chrono::Utc::now());

        active_deployment
            .update(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::Other(format!(
                    "Failed to update deployment screenshot_location: {}",
                    e
                ))
            })?;

        self.log(format!(
            "Updated deployment screenshot_location: {}",
            filename
        ))
        .await?;

        // Set job outputs
        context.set_output(
            &self.job_id,
            "captured_at",
            screenshot_output.captured_at.timestamp(),
        )?;
        context.set_output(&self.job_id, "deployment_id", self.deployment_id)?;
        context.set_output(&self.job_id, "screenshot_location", filename)?;

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

        // Check if screenshot service is available
        if !self.screenshot_service.is_provider_available().await {
            return Err(WorkflowError::JobValidationFailed(
                "Screenshot provider is not available".to_string(),
            ));
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
