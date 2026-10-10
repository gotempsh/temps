// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Take Screenshot Job
//!
//! Captures a screenshot of the deployed application

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use temps_config::ConfigService;
use temps_core::{JobResult, UtcDateTime, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_entities::{deployments, prelude::*};
use temps_logs::{LogLevel, LogService};
use temps_screenshots::ScreenshotServiceTrait;

/// Job for capturing screenshots of deployed applications
/// Hard ceiling for one on-demand capture. Providers bound page loads
/// themselves; this guarantees a capture always ends, so it cannot hold its
/// deployment's capture slot forever.
pub const SCREENSHOT_CAPTURE_TIMEOUT: Duration = Duration::from_secs(180);

/// How long the deployment pipeline waits for a capture of the same
/// deployment that is already running (an on-demand one) before giving up.
const RUNNING_CAPTURE_WAIT: Duration = Duration::from_secs(200);

/// One screenshot capture per deployment at a time, shared by the deployment
/// pipeline's [`TakeScreenshotJob`] and on-demand `take_screenshot`
/// operations, so two captures never race to record a deployment's
/// screenshot.
///
/// A single instance is registered by the deployments plugin and handed to
/// both paths.
#[derive(Debug, Default)]
pub struct DeploymentCaptureGuard {
    in_flight: Arc<Mutex<HashMap<i32, UtcDateTime>>>,
}

/// A deployment's claim on its capture slot; dropping it frees the slot.
#[derive(Debug)]
pub struct DeploymentCaptureSlot {
    in_flight: Arc<Mutex<HashMap<i32, UtcDateTime>>>,
    deployment_id: i32,
}

impl Drop for DeploymentCaptureSlot {
    fn drop(&mut self) {
        lock_in_flight(&self.in_flight).remove(&self.deployment_id);
    }
}

fn lock_in_flight(
    in_flight: &Mutex<HashMap<i32, UtcDateTime>>,
) -> MutexGuard<'_, HashMap<i32, UtcDateTime>> {
    // Every critical section is a single insert or remove, so a panic while
    // holding the lock cannot leave the map half-updated; keep using it.
    in_flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl DeploymentCaptureGuard {
    /// Claim `deployment_id`'s slot, or return when the capture holding it
    /// started.
    pub fn try_claim(&self, deployment_id: i32) -> Result<DeploymentCaptureSlot, UtcDateTime> {
        match lock_in_flight(&self.in_flight).entry(deployment_id) {
            Entry::Occupied(running) => Err(*running.get()),
            Entry::Vacant(slot) => {
                slot.insert(chrono::Utc::now());
                Ok(DeploymentCaptureSlot {
                    in_flight: self.in_flight.clone(),
                    deployment_id,
                })
            }
        }
    }

    /// Claim `deployment_id`'s slot, waiting up to `max_wait` for a running
    /// capture to finish. Returns when the blocking capture started if it is
    /// still running after that.
    pub async fn claim_within(
        &self,
        deployment_id: i32,
        max_wait: Duration,
    ) -> Result<DeploymentCaptureSlot, UtcDateTime> {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            match self.try_claim(deployment_id) {
                Ok(slot) => return Ok(slot),
                Err(started_at) if tokio::time::Instant::now() >= deadline => {
                    return Err(started_at)
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
    }
}

/// Storage path for a new screenshot of `deployment_id`. Millisecond
/// precision plus a random suffix, so two captures can never write the same
/// file even when they start in the same instant.
fn screenshot_location(deployment_id: i32, captured_at: UtcDateTime) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "screenshots/deployment-{}-{}-{}.png",
        deployment_id,
        captured_at.format("%Y%m%d-%H%M%S-%3f"),
        &suffix[..8]
    )
}

pub struct TakeScreenshotJob {
    job_id: String,
    deployment_id: i32,
    screenshot_service: Arc<dyn ScreenshotServiceTrait>,
    capture_guard: Arc<DeploymentCaptureGuard>,
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
        capture_guard: Arc<DeploymentCaptureGuard>,
        config_service: Arc<ConfigService>,
        db: Arc<DbConnection>,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            screenshot_service,
            capture_guard,
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
    let screenshot_location = screenshot_location(deployment_id, captured_at);

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

        // An on-demand capture of this deployment may be running; wait for it
        // rather than race it to record the deployment's screenshot.
        let _slot = match self.capture_guard.try_claim(self.deployment_id) {
            Ok(slot) => slot,
            Err(started_at) => {
                self.log(format!(
                    "Waiting for the screenshot of deployment {} already being captured (started {})...",
                    self.deployment_id,
                    started_at.to_rfc3339()
                ))
                .await?;
                self.capture_guard
                    .claim_within(self.deployment_id, RUNNING_CAPTURE_WAIT)
                    .await
                    .map_err(|started_at| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Another screenshot of deployment {} has been running since {} and did not finish within {}s",
                            self.deployment_id,
                            started_at.to_rfc3339(),
                            RUNNING_CAPTURE_WAIT.as_secs()
                        ))
                    })?
            }
        };

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
    capture_guard: Option<Arc<DeploymentCaptureGuard>>,
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
            capture_guard: None,
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

    pub fn capture_guard(mut self, capture_guard: Arc<DeploymentCaptureGuard>) -> Self {
        self.capture_guard = Some(capture_guard);
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
        let capture_guard = self.capture_guard.ok_or_else(|| {
            WorkflowError::JobValidationFailed("capture_guard is required".to_string())
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
            capture_guard,
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
            Arc::new(DeploymentCaptureGuard::default()),
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

    #[test]
    fn a_deployment_has_one_capture_slot_until_it_is_released() {
        let guard = DeploymentCaptureGuard::default();

        let slot = guard.try_claim(7).expect("free slot");
        let started_at = guard
            .try_claim(7)
            .expect_err("a second capture of the same deployment must wait");
        assert!(started_at <= chrono::Utc::now());
        // Other deployments are unaffected.
        assert!(guard.try_claim(8).is_ok());

        drop(slot);
        assert!(guard.try_claim(7).is_ok(), "dropping the slot frees it");
    }

    #[tokio::test]
    async fn the_pipeline_waits_for_a_running_capture_to_finish() {
        let guard = Arc::new(DeploymentCaptureGuard::default());
        let running = guard.try_claim(7).unwrap();

        let waiter = {
            let guard = guard.clone();
            tokio::spawn(async move { guard.claim_within(7, Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiter.is_finished(), "must wait while the capture runs");

        drop(running);
        let claimed = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("claims once the running capture ends")
            .unwrap();
        assert!(claimed.is_ok());
    }

    #[tokio::test]
    async fn the_pipeline_gives_up_on_a_capture_that_never_ends() {
        let guard = DeploymentCaptureGuard::default();
        let _running = guard.try_claim(7).unwrap();

        let result = guard.claim_within(7, Duration::from_millis(300)).await;

        assert!(result.is_err());
    }

    #[test]
    fn captures_in_the_same_instant_get_different_files() {
        let now = chrono::Utc::now();

        let first = screenshot_location(7, now);
        let second = screenshot_location(7, now);

        assert_ne!(first, second);
        assert!(
            first.starts_with("screenshots/deployment-7-") && first.ends_with(".png"),
            "{first}"
        );
    }

    #[test]
    fn builder_requires_the_shared_capture_guard() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let err = TakeScreenshotJobBuilder::new()
            .deployment_id(7)
            .screenshot_service(Arc::new(UnavailableScreenshotService))
            .config_service(test_config_service(db.clone()))
            .db(db)
            .build()
            .expect_err("a job without the shared guard could race on-demand captures");

        assert!(err.to_string().contains("capture_guard"), "{err}");
    }
}
