//! Vulnerability Scan Job
//!
//! Scans the deployed application for security vulnerabilities using Trivy.
//! This job runs after deployment is complete to identify any known vulnerabilities
//! in the application dependencies and container images.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_logs::{LogLevel, LogService};
use temps_vulnerability_scanner::{
    scanner::VulnerabilityScanner, service::VulnerabilityScanService, trivy::TrivyScanner,
};
use tracing::{debug, error, info, warn};

use crate::jobs::{ImageOutput, RepositoryOutput};

/// Output from ScanVulnerabilitiesJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanVulnerabilitiesOutput {
    pub scan_id: i32,
    pub total_vulnerabilities: i32,
    pub critical_count: i32,
    pub high_count: i32,
    pub medium_count: i32,
    pub low_count: i32,
}

/// Job that scans deployed application for vulnerabilities
pub struct ScanVulnerabilitiesJob {
    job_id: String,
    deployment_id: i32,
    project_id: i32,
    environment_id: i32,
    branch: String,
    commit_hash: String,
    download_job_id: String,
    build_job_id: String,
    db: Arc<DbConnection>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for ScanVulnerabilitiesJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanVulnerabilitiesJob")
            .field("job_id", &self.job_id)
            .field("deployment_id", &self.deployment_id)
            .field("project_id", &self.project_id)
            .field("environment_id", &self.environment_id)
            .field("branch", &self.branch)
            .field("commit_hash", &self.commit_hash)
            .field("build_job_id", &self.build_job_id)
            .finish()
    }
}

impl ScanVulnerabilitiesJob {
    pub fn new(
        job_id: String,
        deployment_id: i32,
        project_id: i32,
        environment_id: i32,
        branch: String,
        commit_hash: String,
        download_job_id: String,
        build_job_id: String,
        db: Arc<DbConnection>,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            project_id,
            environment_id,
            branch,
            commit_hash,
            download_job_id,
            build_job_id,
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
        let level = Self::detect_log_level(&message);

        if let (Some(log_id), Some(log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message)
                .await
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!("Failed to write log: {}", e))
                })?;
        }

        Ok(())
    }

    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌") || message.contains("Failed") || message.contains("Error")
        {
            LogLevel::Error
        } else if message.contains("⚠️")
            || message.contains("Warning")
            || message.contains("CRITICAL")
            || message.contains("HIGH")
        {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }
}

#[async_trait]
impl WorkflowTask for ScanVulnerabilitiesJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Scan Vulnerabilities"
    }

    fn description(&self) -> &str {
        "Scan deployed application for security vulnerabilities using Trivy"
    }

    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        info!(
            "Starting vulnerability scan for deployment {} (project: {}, env: {}, branch: {})",
            self.deployment_id, self.project_id, self.environment_id, self.branch
        );

        self.log("🔍 Starting vulnerability scan...".to_string())
            .await?;

        // Get image tag from build job output
        let image_output = ImageOutput::from_context(&context, &self.build_job_id)?;
        let image_tag = &image_output.image_tag;

        self.log(format!("Scanning Docker image: {}", image_tag))
            .await?;

        // Initialize Trivy scanner
        let scanner = match TrivyScanner::new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                let error_msg = format!("Failed to initialize Trivy scanner: {}", e);
                error!("{}", error_msg);
                self.log(format!("❌ {}", error_msg)).await?;
                return Err(WorkflowError::JobExecutionFailed(error_msg));
            }
        };

        // Check scanner version
        match scanner.version().await {
            Ok(Some(version)) => {
                self.log(format!("Using Trivy version: {}", version))
                    .await?;
                debug!("Trivy version: {}", version);
            }
            Ok(None) => {
                warn!("Could not determine Trivy version");
            }
            Err(e) => {
                warn!("Failed to get Trivy version: {}", e);
            }
        }

        // Create scan service with scanner
        let scan_service = VulnerabilityScanService::new(self.db.clone(), scanner);

        // Create scan record
        self.log("Creating vulnerability scan record...".to_string())
            .await?;

        let scan = match scan_service
            .create_scan(
                self.project_id,
                Some(self.environment_id),
                Some(self.branch.clone()),
                Some(self.commit_hash.clone()),
            )
            .await
        {
            Ok(scan) => {
                self.log(format!("✅ Created scan record (ID: {})", scan.id))
                    .await?;
                scan
            }
            Err(e) => {
                let error_msg = format!("Failed to create scan record: {}", e);
                error!("{}", error_msg);
                self.log(format!("❌ {}", error_msg)).await?;
                return Err(WorkflowError::JobExecutionFailed(error_msg));
            }
        };

        let scan_id = scan.id;

        // Execute image scan (this handles everything: scanning, saving vulnerabilities, updating scan record)
        self.log("Running Trivy image scan (this may take a few minutes)...".to_string())
            .await?;

        let scan_result = match scan_service.execute_image_scan(scan_id, image_tag).await {
            Ok(result) => {
                self.log(format!(
                    "✅ Scan complete - Found {} vulnerabilities",
                    result.total_count
                ))
                .await?;
                result
            }
            Err(e) => {
                let error_msg = format!("Image scan execution failed: {}", e);
                error!("{}", error_msg);
                self.log(format!("❌ {}", error_msg)).await?;
                return Err(WorkflowError::JobExecutionFailed(error_msg));
            }
        };

        // Log summary by severity
        if scan_result.critical_count > 0 {
            self.log(format!(
                "⚠️  CRITICAL: {} vulnerabilities",
                scan_result.critical_count
            ))
            .await?;
        }
        if scan_result.high_count > 0 {
            self.log(format!(
                "⚠️  HIGH: {} vulnerabilities",
                scan_result.high_count
            ))
            .await?;
        }
        if scan_result.medium_count > 0 {
            self.log(format!(
                "MEDIUM: {} vulnerabilities",
                scan_result.medium_count
            ))
            .await?;
        }
        if scan_result.low_count > 0 {
            self.log(format!("LOW: {} vulnerabilities", scan_result.low_count))
                .await?;
        }

        info!(
            "Vulnerability scan completed for deployment {}: {} total vulnerabilities ({} critical, {} high)",
            self.deployment_id, scan_result.total_count, scan_result.critical_count, scan_result.high_count
        );

        self.log("✅ Vulnerability scan completed successfully".to_string())
            .await?;

        // Set output in context
        let mut updated_context = context.clone();
        updated_context.set_output(&self.job_id, "scan_id", scan_id)?;
        updated_context.set_output(
            &self.job_id,
            "total_vulnerabilities",
            scan_result.total_count,
        )?;
        updated_context.set_output(&self.job_id, "critical_count", scan_result.critical_count)?;
        updated_context.set_output(&self.job_id, "high_count", scan_result.high_count)?;

        Ok(JobResult::success(updated_context))
    }
}
