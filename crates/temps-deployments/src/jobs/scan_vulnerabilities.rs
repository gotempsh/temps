// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Vulnerability Scan Job
//!
//! Scans the deployed application for security vulnerabilities using Trivy.
//! This job runs after deployment is complete to identify any known vulnerabilities
//! in the application dependencies and container images.
//!
//! An image built on a worker node is scanned on that node, through the agent
//! that already holds it: the control plane neither copies the image nor runs
//! the scanner. A node that cannot scan (an agent that predates the endpoint)
//! falls back to copying the image and scanning it here, as before. A scan
//! that cannot run at all fails its own scan record and never the deployment.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use temps_core::{DockerHandle, JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_deployer::ImageBuilder;
use temps_deployment_logs::{LogLevel, LogService};
use temps_vulnerability_scanner::{
    node_report::result_from_report,
    scanner::{ScanResult, ScannerError, VulnerabilityScanner},
    service::VulnerabilityScanService,
    trivy::TrivyScanner,
};
use tracing::{debug, error, info, warn};

use crate::jobs::builder_node::{image_builder_for_build, BuilderNodeResolver};
use crate::jobs::ImageOutput;

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
    _download_job_id: String,
    build_job_id: String,
    db: Arc<DbConnection>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    /// Process-wide Docker handle. Always present in the service registry;
    /// [`TrivyScanner`] resolves it lazily and returns a typed
    /// `ScannerError::DockerUnavailable` at scan time if this process was
    /// started with `--profile control-plane` (no local daemon).
    docker_handle: Arc<DockerHandle>,
    /// This process's own image store, used to copy a node-built image over
    /// when its node cannot scan it.
    image_builder: Option<Arc<dyn ImageBuilder>>,
    /// Reaches the node that built an image. Without it a node-built image
    /// can only be scanned from a copy in the local image store.
    builder_node_resolver: Option<Arc<dyn BuilderNodeResolver>>,
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
    #[allow(clippy::too_many_arguments)]
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
        docker_handle: Arc<DockerHandle>,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            project_id,
            environment_id,
            branch,
            commit_hash,
            _download_job_id: download_job_id,
            build_job_id,
            db,
            log_id: None,
            log_service: None,
            docker_handle,
            image_builder: None,
            builder_node_resolver: None,
        }
    }

    pub fn with_image_builder(mut self, image_builder: Arc<dyn ImageBuilder>) -> Self {
        self.image_builder = Some(image_builder);
        self
    }

    pub fn with_builder_node_resolver(mut self, resolver: Arc<dyn BuilderNodeResolver>) -> Self {
        self.builder_node_resolver = Some(resolver);
        self
    }

    /// The scanner for `image_output`: the build node's, when a node built
    /// the image and this job can reach it; this process's own otherwise.
    async fn scanner_for(
        &self,
        context: &WorkflowContext,
        image_output: &ImageOutput,
    ) -> Result<(Arc<dyn VulnerabilityScanner>, Option<i32>), WorkflowError> {
        let local: Arc<dyn VulnerabilityScanner> =
            Arc::new(TrivyScanner::new(self.docker_handle.clone()));
        let (Some(image_builder), Some(resolver)) = (
            self.image_builder.as_ref(),
            self.builder_node_resolver.as_ref(),
        ) else {
            return Ok((local, None));
        };
        let (builder, node_id) =
            image_builder_for_build(context, &self.build_job_id, image_builder, Some(resolver))
                .await?;
        let Some(node_id) = node_id else {
            return Ok((local, None));
        };
        let scanner = NodeImageScanner {
            node: builder,
            node_id,
            expected_image_id: image_output.expected_image_id().map(str::to_string),
            local,
            local_builder: image_builder.clone(),
        };
        Ok((Arc::new(scanner), Some(node_id)))
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

/// Scans an image through the node that built it.
///
/// `scan_image` asks the node's agent, which runs the scanner against the
/// image it holds and returns the findings. Only when the agent cannot scan
/// does the image come to this process (once, identity-checked) to be scanned
/// by `local`.
struct NodeImageScanner {
    node: Arc<dyn ImageBuilder>,
    node_id: i32,
    /// The id recorded when the image was built; the node refuses a tag that
    /// now names something else.
    expected_image_id: Option<String>,
    local: Arc<dyn VulnerabilityScanner>,
    local_builder: Arc<dyn ImageBuilder>,
}

#[async_trait]
impl VulnerabilityScanner for NodeImageScanner {
    fn name(&self) -> &str {
        "trivy"
    }

    /// The node reports its scanner's version with the findings; asking for
    /// it here would start a scanner container on the control plane.
    async fn version(&self) -> Result<Option<String>, ScannerError> {
        Ok(None)
    }

    async fn scan_path(&self, path: &Path) -> Result<ScanResult, ScannerError> {
        self.local.scan_path(path).await
    }

    async fn scan_image(&self, image_name: &str) -> Result<ScanResult, ScannerError> {
        match self
            .node
            .scan_image_vulnerabilities(image_name, self.expected_image_id.as_deref())
            .await
        {
            Ok(Some(report)) => Ok(result_from_report(report)),
            Ok(None) => self.scan_copy_locally(image_name).await,
            Err(error) => Err(ScannerError::Execution(format!(
                "Scan of '{image_name}' on build node {} failed: {error}",
                self.node_id
            ))),
        }
    }

    async fn is_available(&self) -> bool {
        true
    }
}

impl NodeImageScanner {
    /// The node's agent cannot scan: bring the image here and scan it.
    async fn scan_copy_locally(&self, image_name: &str) -> Result<ScanResult, ScannerError> {
        let node_name = format!("node {}", self.node_id);
        match super::node_image::copy_node_built_image(
            self.node.as_ref(),
            &node_name,
            self.local_builder.as_ref(),
            image_name,
            self.expected_image_id.as_deref(),
        )
        .await
        {
            Ok(_) => {}
            Err(error) => {
                let reason = match &error {
                    WorkflowError::JobExecutionFailed(message) => message.clone(),
                    other => other.to_string(),
                };
                return Err(ScannerError::Execution(format!(
                    "Build node {} cannot scan '{image_name}' itself (its agent predates \
                     image scans) and the image could not be copied to this control plane \
                     to scan it here: {reason}",
                    self.node_id
                )));
            }
        }
        self.local.scan_image(image_name).await
    }
}

/// Where a deployment's image was built, read back from its build job's
/// saved outputs. Only the fields a later scan needs.
#[derive(Debug, Deserialize)]
struct RecordedBuild {
    image_tag: String,
    #[serde(default)]
    image_id: String,
    #[serde(default)]
    image_tags_by_platform: std::collections::HashMap<String, String>,
    #[serde(default)]
    image_ids_by_platform: std::collections::HashMap<String, String>,
    #[serde(default)]
    builder_node_id: Option<i32>,
}

impl RecordedBuild {
    /// The node that holds `image_name`, with the id it was built as; `None`
    /// for a build on this host, or outputs that describe another image.
    fn node_holding(
        outputs: &serde_json::Value,
        image_name: &str,
    ) -> Option<(i32, Option<String>)> {
        let build: RecordedBuild = serde_json::from_value(outputs.clone()).ok()?;
        if build.image_tag != image_name {
            return None;
        }
        let node_id = build.builder_node_id?;
        let expected = super::build_image::primary_image_id(
            &build.image_tag,
            &build.image_id,
            &build.image_tags_by_platform,
            &build.image_ids_by_platform,
        )
        .map(str::to_string);
        Some((node_id, expected))
    }
}

/// Points scans that run after the deployment -- the daily rescan, a manual
/// one -- at the build node that holds a worker-built image.
///
/// The deployment-time scan knows the node from its workflow context; these
/// have only a deployment id, so the node is read back from the build job's
/// persisted outputs.
pub struct BuildNodeScannerResolver {
    db: Arc<DbConnection>,
    image_builder: Arc<dyn ImageBuilder>,
    nodes: Arc<dyn BuilderNodeResolver>,
    local: Arc<dyn VulnerabilityScanner>,
}

impl BuildNodeScannerResolver {
    pub fn new(
        db: Arc<DbConnection>,
        image_builder: Arc<dyn ImageBuilder>,
        nodes: Arc<dyn BuilderNodeResolver>,
        docker_handle: Arc<DockerHandle>,
    ) -> Self {
        Self {
            db,
            image_builder,
            nodes,
            local: Arc::new(TrivyScanner::new(docker_handle)),
        }
    }
}

/// Rollbacks and promotions chain to the deployment they reuse; a chain is
/// a few hops at most, and a longer one is a cycle or corrupt metadata.
const MAX_REUSE_HOPS: usize = 8;

impl BuildNodeScannerResolver {
    /// The persisted outputs of the build behind `deployment_id`. A
    /// rollback or promotion has no build job of its own: its image is the
    /// one built by the deployment named in its `source_deployment_id`
    /// context, so follow that to the build.
    async fn build_outputs_of(
        &self,
        deployment_id: i32,
    ) -> Result<Option<serde_json::Value>, sea_orm::DbErr> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
        use temps_entities::{deployment_jobs, deployments};

        let mut current = deployment_id;
        let mut project_id = None;
        for _ in 0..MAX_REUSE_HOPS {
            let outputs = deployment_jobs::Entity::find()
                .filter(deployment_jobs::Column::DeploymentId.eq(current))
                .filter(deployment_jobs::Column::JobType.eq("BuildImageJob"))
                .filter(deployment_jobs::Column::Outputs.is_not_null())
                .order_by_desc(deployment_jobs::Column::Id)
                .one(self.db.as_ref())
                .await?
                .and_then(|job| job.outputs);
            if outputs.is_some() {
                return Ok(outputs);
            }
            let Some(deployment) = deployments::Entity::find_by_id(current)
                .one(self.db.as_ref())
                .await?
            else {
                return Ok(None);
            };
            // Only follow reuse within the project that owns the deployment.
            if *project_id.get_or_insert(deployment.project_id) != deployment.project_id {
                return Ok(None);
            }
            let source = deployment
                .context_vars
                .as_ref()
                .and_then(|vars| vars.get("source_deployment_id"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|id| i32::try_from(id).ok());
            match source {
                Some(source) if source != current => current = source,
                _ => return Ok(None),
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl temps_vulnerability_scanner::service::DeploymentScannerResolver for BuildNodeScannerResolver {
    async fn scanner_for_deployment(
        &self,
        deployment_id: i32,
        image_name: &str,
    ) -> Option<Arc<dyn VulnerabilityScanner>> {
        let outputs = match self.build_outputs_of(deployment_id).await {
            Ok(outputs) => outputs?,
            Err(error) => {
                warn!(
                    deployment_id,
                    "Cannot read the build of deployment {deployment_id} to find the node \
                     holding '{image_name}'; scanning on this host: {error}"
                );
                return None;
            }
        };
        let (node_id, expected_image_id) = RecordedBuild::node_holding(&outputs, image_name)?;
        let node = match self.nodes.image_builder_for_node(node_id).await {
            Ok(node) => node,
            Err(error) => {
                warn!(
                    deployment_id,
                    node_id,
                    "Cannot reach build node {node_id} holding '{image_name}' of deployment \
                     {deployment_id}; scanning on this host: {error}"
                );
                return None;
            }
        };
        Some(Arc::new(NodeImageScanner {
            node,
            node_id,
            expected_image_id,
            local: self.local.clone(),
            local_builder: self.image_builder.clone(),
        }))
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

        // Pick the scanner. For an image built on a worker node this is the
        // node's own: nothing is copied and no scanner runs on the control
        // plane. Otherwise a `TrivyScanner` is built from the process-wide
        // `DockerHandle`, which never fails at construction -- a
        // control-plane process (no local Docker daemon) surfaces a typed
        // `ScannerError::DockerUnavailable` from the scan call instead.
        let (scanner, scanned_on_node) = self.scanner_for(&context, &image_output).await?;

        if let Some(node_id) = scanned_on_node {
            self.log(format!(
                "Scanning on build node {node_id}; the image is not copied to the control plane"
            ))
            .await?;
        } else {
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
                Some(self.deployment_id),
                Some(self.branch.clone()),
                (!self.commit_hash.is_empty()).then(|| self.commit_hash.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::node_image::fake::FakeImageStore;
    use std::sync::Mutex;
    use temps_deployer::image_scan::{ImageScanFinding, ImageScanReport};
    use temps_deployer::{
        BuildRequest, BuildRequestWithCallback, BuildResult, BuilderError, ImageImportStream,
        ImageInfo,
    };

    /// A build node: an image store plus a scripted answer to a scan request.
    struct ScanningNode {
        store: FakeImageStore,
        answer: Mutex<Option<Result<Option<ImageScanReport>, String>>>,
        requests: Mutex<Vec<(String, Option<String>)>>,
    }

    impl ScanningNode {
        fn new(
            store: FakeImageStore,
            answer: Result<Option<ImageScanReport>, String>,
        ) -> Arc<Self> {
            Arc::new(Self {
                store,
                answer: Mutex::new(Some(answer)),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl ImageBuilder for ScanningNode {
        async fn build_image(&self, _: BuildRequest) -> Result<BuildResult, BuilderError> {
            unimplemented!("not used")
        }
        async fn build_image_with_callback(
            &self,
            _: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            unimplemented!("not used")
        }
        async fn import_image(
            &self,
            _: std::path::PathBuf,
            _: &str,
        ) -> Result<String, BuilderError> {
            unimplemented!("not used")
        }
        async fn import_image_stream(
            &self,
            stream: ImageImportStream,
            tag: &str,
        ) -> Result<String, BuilderError> {
            self.store.import_image_stream(stream, tag).await
        }
        async fn export_image_stream(
            &self,
            image_name: &str,
        ) -> Result<ImageImportStream, BuilderError> {
            self.store.export_image_stream(image_name).await
        }
        async fn save_image(&self, _: &str, _: &Path) -> Result<(), BuilderError> {
            unimplemented!("not used")
        }
        async fn extract_from_image(&self, _: &str, _: &str, _: &Path) -> Result<(), BuilderError> {
            unimplemented!("not used")
        }
        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            unimplemented!("not used")
        }
        async fn remove_image(&self, _: &str) -> Result<(), BuilderError> {
            unimplemented!("not used")
        }
        async fn inspect_image(&self, image_name: &str) -> Result<ImageInfo, BuilderError> {
            self.store.inspect_image(image_name).await
        }
        async fn scan_image_vulnerabilities(
            &self,
            image_name: &str,
            expected_image_id: Option<&str>,
        ) -> Result<Option<ImageScanReport>, BuilderError> {
            self.requests.lock().unwrap().push((
                image_name.to_string(),
                expected_image_id.map(str::to_string),
            ));
            match self.answer.lock().unwrap().take() {
                Some(Ok(report)) => Ok(report),
                Some(Err(message)) => Err(BuilderError::Other(message)),
                None => panic!("scan requested twice"),
            }
        }
        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }

    /// The control plane's own scanner; records what it is asked to scan.
    #[derive(Default)]
    struct RecordingScanner {
        scanned: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl VulnerabilityScanner for RecordingScanner {
        fn name(&self) -> &str {
            "recording"
        }
        async fn version(&self) -> Result<Option<String>, ScannerError> {
            panic!("a node scan must not start a scanner on the control plane")
        }
        async fn scan_path(&self, _: &Path) -> Result<ScanResult, ScannerError> {
            unimplemented!("not used")
        }
        async fn scan_image(&self, image_name: &str) -> Result<ScanResult, ScannerError> {
            self.scanned.lock().unwrap().push(image_name.to_string());
            Ok(ScanResult::new("trivy".to_string(), None))
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    fn report(findings: usize) -> ImageScanReport {
        ImageScanReport {
            image_id: "sha256:built".to_string(),
            scanner: "trivy".to_string(),
            scanner_version: Some("0.58.1".to_string()),
            findings: (0..findings)
                .map(|index| ImageScanFinding {
                    vulnerability_id: format!("CVE-2000-{index:04}"),
                    package_name: "libexample".to_string(),
                    installed_version: "1.0.0".to_string(),
                    fixed_version: None,
                    severity: if index == 0 { "CRITICAL" } else { "LOW" }.to_string(),
                    title: "synthetic finding".to_string(),
                    description: None,
                    references: Vec::new(),
                    cvss_score: None,
                    primary_url: None,
                    published_date: None,
                    last_modified_date: None,
                    class: None,
                    vuln_type: None,
                    target: None,
                })
                .collect(),
        }
    }

    struct Fixture {
        scanner: NodeImageScanner,
        node: Arc<ScanningNode>,
        local_store: Arc<FakeImageStore>,
        local_scanner: Arc<RecordingScanner>,
    }

    fn fixture(node: Arc<ScanningNode>) -> Fixture {
        let local_store = Arc::new(FakeImageStore::default());
        let local_scanner = Arc::new(RecordingScanner::default());
        Fixture {
            scanner: NodeImageScanner {
                node: node.clone(),
                node_id: 7,
                expected_image_id: Some("sha256:built".to_string()),
                local: local_scanner.clone(),
                local_builder: local_store.clone(),
            },
            node,
            local_store,
            local_scanner,
        }
    }

    /// The scan runs on the node: its findings come back, the image never
    /// reaches this process, and no scanner starts here.
    #[tokio::test]
    async fn node_findings_are_used_without_copying_or_scanning_locally() {
        let node = ScanningNode::new(
            FakeImageStore::holding("app:1", "sha256:built"),
            Ok(Some(report(3))),
        );
        let fixture = fixture(node);

        let result = fixture.scanner.scan_image("app:1").await.unwrap();

        assert_eq!(result.total_count, 3);
        assert_eq!(result.critical_count, 1);
        assert_eq!(result.low_count, 2);
        assert_eq!(result.scanner_version.as_deref(), Some("0.58.1"));
        assert_eq!(
            *fixture.node.requests.lock().unwrap(),
            vec![("app:1".to_string(), Some("sha256:built".to_string()))],
            "the node must be told which image id the findings must describe"
        );
        assert!(fixture.local_store.images.lock().unwrap().is_empty());
        assert!(fixture.local_scanner.scanned.lock().unwrap().is_empty());
    }

    /// A clean image is a successful scan with no findings.
    #[tokio::test]
    async fn node_report_without_findings_is_a_completed_scan() {
        let node = ScanningNode::new(FakeImageStore::default(), Ok(Some(report(0))));
        let result = fixture(node).scanner.scan_image("app:1").await.unwrap();
        assert_eq!(result.total_count, 0);
    }

    /// An agent that predates the endpoint: the image is copied once,
    /// identity-checked, and scanned by this process's scanner.
    #[tokio::test]
    async fn node_that_cannot_scan_falls_back_to_a_verified_local_copy() {
        let node = ScanningNode::new(FakeImageStore::holding("app:1", "sha256:built"), Ok(None));
        let fixture = fixture(node);

        fixture.scanner.scan_image("app:1").await.unwrap();

        assert_eq!(
            fixture.local_store.images.lock().unwrap().get("app:1"),
            Some(&"sha256:built".to_string())
        );
        assert_eq!(
            *fixture.local_scanner.scanned.lock().unwrap(),
            vec!["app:1".to_string()]
        );
    }

    /// The fallback cannot run (no copy to scan): the scan fails with a
    /// message that names the node and the reason -- never a silent pass.
    #[tokio::test]
    async fn fallback_that_cannot_copy_the_image_fails_the_scan_with_the_reason() {
        let node = ScanningNode::new(FakeImageStore::default(), Ok(None));
        let fixture = fixture(node);

        let error = fixture
            .scanner
            .scan_image("app:1")
            .await
            .expect_err("nothing to scan");
        let message = error.to_string();

        assert!(message.contains("Build node 7"), "{message}");
        assert!(message.contains("predates image scans"), "{message}");
        assert!(fixture.local_scanner.scanned.lock().unwrap().is_empty());
    }

    /// A scan that was attempted and failed is reported as failed. Copying
    /// the image over and scanning again would double the work and hide why.
    #[tokio::test]
    async fn failed_node_scan_is_reported_not_retried_locally() {
        let node = ScanningNode::new(
            FakeImageStore::holding("app:1", "sha256:built"),
            Err("trivy image pull failed".to_string()),
        );
        let fixture = fixture(node);

        let error = fixture
            .scanner
            .scan_image("app:1")
            .await
            .expect_err("node scan failed");
        let message = error.to_string();

        assert!(message.contains("build node 7"), "{message}");
        assert!(message.contains("trivy image pull failed"), "{message}");
        assert!(fixture.local_store.images.lock().unwrap().is_empty());
        assert!(fixture.local_scanner.scanned.lock().unwrap().is_empty());
    }

    /// The version comes with the node's findings; asking for it would start
    /// a scanner container on the control plane.
    #[tokio::test]
    async fn node_scanner_does_not_query_a_local_version() {
        let node = ScanningNode::new(FakeImageStore::default(), Ok(None));
        let fixture = fixture(node);
        assert_eq!(fixture.scanner.version().await.unwrap(), None);
    }

    // ---- rescans: the build node is read back from the persisted build ----

    fn build_outputs(node: Option<i32>) -> serde_json::Value {
        serde_json::json!({
            "image_tag": "app:1",
            "image_id": "sha256:built",
            "builder_node_id": node,
        })
    }

    #[test]
    fn recorded_build_names_the_node_and_the_id_it_was_built_as() {
        let outputs = build_outputs(Some(7));
        assert_eq!(
            RecordedBuild::node_holding(&outputs, "app:1"),
            Some((7, Some("sha256:built".to_string())))
        );
    }

    #[test]
    fn recorded_build_of_another_image_or_the_control_plane_names_no_node() {
        assert_eq!(
            RecordedBuild::node_holding(&build_outputs(Some(7)), "app:2"),
            None
        );
        assert_eq!(
            RecordedBuild::node_holding(&build_outputs(None), "app:1"),
            None
        );
        assert_eq!(
            RecordedBuild::node_holding(&serde_json::json!({"unrelated": true}), "app:1"),
            None
        );
    }

    /// A mixed-architecture build keeps one image id per platform; the one
    /// for the tag being scanned is the identity to require.
    #[test]
    fn recorded_mixed_architecture_build_uses_its_own_platform_id() {
        let outputs = serde_json::json!({
            "image_tag": "app:1",
            "image_id": "sha256:other",
            "builder_node_id": 7,
            "image_tags_by_platform": {"linux/amd64": "app:1", "linux/arm64": "app:1-arm64"},
            "image_ids_by_platform": {"linux/amd64": "sha256:amd", "linux/arm64": "sha256:arm"},
        });
        assert_eq!(
            RecordedBuild::node_holding(&outputs, "app:1"),
            Some((7, Some("sha256:amd".to_string())))
        );
    }

    fn deployment_build_job(
        outputs: Option<serde_json::Value>,
    ) -> temps_entities::deployment_jobs::Model {
        temps_entities::deployment_jobs::Model {
            id: 1,
            deployment_id: 42,
            job_id: "build_image".to_string(),
            job_type: "BuildImageJob".to_string(),
            name: "build_image".to_string(),
            description: None,
            status: temps_entities::types::JobStatus::Success,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            started_at: None,
            finished_at: None,
            log_id: "log".to_string(),
            error_message: None,
            job_config: None,
            outputs,
            dependencies: None,
            execution_order: None,
        }
    }

    fn rescan_resolver(
        jobs: Vec<temps_entities::deployment_jobs::Model>,
        node: Arc<ScanningNode>,
        local_scanner: Arc<RecordingScanner>,
    ) -> BuildNodeScannerResolver {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_core::{DockerHandle, PROFILE_CONTROL_PLANE};
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![jobs])
            .into_connection();
        let nodes: Arc<dyn BuilderNodeResolver> = Arc::new(
            crate::jobs::builder_node::tests::StaticResolver::new(7, node),
        );
        let mut resolver = BuildNodeScannerResolver::new(
            Arc::new(db),
            Arc::new(FakeImageStore::default()),
            nodes,
            Arc::new(DockerHandle::disabled(PROFILE_CONTROL_PLANE, "unit test")),
        );
        resolver.local = local_scanner;
        resolver
    }

    /// The daily and manual rescans have only a deployment id. For a
    /// worker-built image whose copy never came to the control plane, the
    /// scan must go to the build node instead of failing "not found locally".
    #[tokio::test]
    async fn rescan_of_a_worker_built_image_is_routed_to_its_build_node() {
        use temps_vulnerability_scanner::service::DeploymentScannerResolver;
        let node = ScanningNode::new(
            FakeImageStore::holding("app:1", "sha256:built"),
            Ok(Some(report(2))),
        );
        let local_scanner = Arc::new(RecordingScanner::default());
        let resolver = rescan_resolver(
            vec![deployment_build_job(Some(build_outputs(Some(7))))],
            node.clone(),
            local_scanner.clone(),
        );

        let scanner = resolver
            .scanner_for_deployment(42, "app:1")
            .await
            .expect("a node holds this image");
        let result = scanner.scan_image("app:1").await.unwrap();

        assert_eq!(result.total_count, 2);
        assert_eq!(
            *node.requests.lock().unwrap(),
            vec![("app:1".to_string(), Some("sha256:built".to_string()))]
        );
        assert!(local_scanner.scanned.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rescan_of_a_control_plane_build_keeps_the_local_scanner() {
        use temps_vulnerability_scanner::service::DeploymentScannerResolver;
        let node = ScanningNode::new(FakeImageStore::default(), Ok(None));
        let resolver = rescan_resolver(
            vec![deployment_build_job(Some(build_outputs(None)))],
            node,
            Arc::new(RecordingScanner::default()),
        );
        assert!(resolver.scanner_for_deployment(42, "app:1").await.is_none());
    }

    #[tokio::test]
    async fn rescan_of_a_deployment_without_a_build_keeps_the_local_scanner() {
        use temps_vulnerability_scanner::service::DeploymentScannerResolver;
        let node = ScanningNode::new(FakeImageStore::default(), Ok(None));
        let resolver = rescan_resolver(Vec::new(), node, Arc::new(RecordingScanner::default()));
        assert!(resolver.scanner_for_deployment(42, "app:1").await.is_none());
    }

    fn promoted_deployment(id: i32, source: i32) -> temps_entities::deployments::Model {
        temps_entities::deployments::Model {
            id,
            project_id: 1,
            environment_id: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: format!("d{id}"),
            state: "deployed".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: None,
            context_vars: Some(serde_json::json!({ "source_deployment_id": source })),
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: Some(source),
            upload_request_id: None,
            docker_socket_mounted: false,
        }
    }

    /// A promoted or rolled-back deployment has no build job of its own: its
    /// image is the one built by the deployment it reuses, which is where
    /// the scan has to go.
    #[tokio::test]
    async fn rescan_of_a_reused_deployment_follows_it_to_the_original_build() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_core::{DockerHandle, PROFILE_CONTROL_PLANE};
        use temps_vulnerability_scanner::service::DeploymentScannerResolver;

        let node = ScanningNode::new(
            FakeImageStore::holding("app:1", "sha256:built"),
            Ok(Some(report(1))),
        );
        let local_scanner = Arc::new(RecordingScanner::default());
        // Deployment 43 reuses 42: no build job of its own, then its
        // context, then the original build's outputs.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::deployment_jobs::Model>::new()])
            .append_query_results(vec![vec![promoted_deployment(43, 42)]])
            .append_query_results(vec![vec![deployment_build_job(Some(build_outputs(Some(
                7,
            ))))]])
            .into_connection();
        let nodes: Arc<dyn BuilderNodeResolver> = Arc::new(
            crate::jobs::builder_node::tests::StaticResolver::new(7, node.clone()),
        );
        let mut resolver = BuildNodeScannerResolver::new(
            Arc::new(db),
            Arc::new(FakeImageStore::default()),
            nodes,
            Arc::new(DockerHandle::disabled(PROFILE_CONTROL_PLANE, "unit test")),
        );
        resolver.local = local_scanner.clone();

        let scanner = resolver
            .scanner_for_deployment(43, "app:1")
            .await
            .expect("the original build ran on a node");
        let result = scanner.scan_image("app:1").await.unwrap();

        assert_eq!(result.total_count, 1);
        assert_eq!(node.requests.lock().unwrap().len(), 1);
        assert!(local_scanner.scanned.lock().unwrap().is_empty());
    }
}
