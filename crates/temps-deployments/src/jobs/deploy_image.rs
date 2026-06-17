//! Deploy Image Job
//!
//! Deploys built container images to target environments

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use temps_core::{
    JobResult, WorkflowCancellationProvider, WorkflowContext, WorkflowError, WorkflowTask,
};
use temps_deployer::{
    ContainerDeployer, ContainerLogConfig, ContainerStatus as DeployerContainerStatus,
    DeployRequest, ImageBuilder, PortMapping, Protocol, ResourceLimits, RestartPolicy,
};
use temps_logs::{LogLevel, LogService};
use tokio::time::{sleep, Duration};

/// Typed output from BuildImageJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildImageOutput {
    pub image_tag: String,
    pub image_id: String,
    pub size_bytes: u64,
    pub build_context: PathBuf,
    pub dockerfile_path: PathBuf,
}

impl BuildImageOutput {
    /// Extract ImageOutput from WorkflowContext
    pub fn from_context(
        context: &WorkflowContext,
        build_job_id: &str,
    ) -> Result<Self, WorkflowError> {
        let image_tag: String =
            context
                .get_output(build_job_id, "image_tag")?
                .ok_or_else(|| {
                    WorkflowError::JobValidationFailed("image_tag output not found".to_string())
                })?;
        let image_id: String = context
            .get_output(build_job_id, "image_id")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("image_id output not found".to_string())
            })?;
        let size_bytes: u64 = context
            .get_output(build_job_id, "size_bytes")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("size_bytes output not found".to_string())
            })?;
        let build_context_str: String = context
            .get_output(build_job_id, "build_context")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("build_context output not found".to_string())
            })?;
        let dockerfile_path_str: String = context
            .get_output(build_job_id, "dockerfile_path")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("dockerfile_path output not found".to_string())
            })?;

        Ok(Self {
            image_tag,
            image_id,
            size_bytes,
            build_context: PathBuf::from(build_context_str),
            dockerfile_path: PathBuf::from(dockerfile_path_str),
        })
    }
}

/// Typed output from DeployImageJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentOutput {
    pub status: DeploymentStatus,
    pub replicas: u32,
    pub resources: ResourceUsage,
    /// List of all deployed container IDs (for multi-replica deployments)
    pub container_ids: Vec<String>,
    /// List of all allocated host ports (one per replica)
    pub host_ports: Vec<u16>,
    /// The resolved container port (from image EXPOSE, config, or default)
    pub container_port: u16,
    /// Node IDs for each replica (None = local node). Parallel to container_ids.
    #[serde(default)]
    pub node_ids: Vec<Option<i32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeploymentStatus {
    Pending,
    Deploying,
    Running,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub cpu_limit: Option<String>,
    pub memory_limit: Option<String>,
    pub cpu_request: Option<String>,
    pub memory_request: Option<String>,
}

impl Default for ResourceUsage {
    /// No limits by default — an environment is uncapped unless the operator
    /// opts in by configuring `cpu_limit`/`memory_limit` on the project or
    /// environment `deployment_config`. A `None` here flows through
    /// `parse_cpu_cores`/`parse_memory_mb` (→ `None`) to the Docker
    /// `HostConfig`, where `nano_cpus`/`memory` are left unset (= uncapped).
    ///
    /// Historically this seeded `1000000u`/`512Mi`, which silently capped any
    /// deploy path that built a job without calling `.resources(...)` (e.g.
    /// rollback/promote) even when no limit was configured.
    fn default() -> Self {
        Self {
            cpu_limit: None,
            memory_limit: None,
            cpu_request: None,
            memory_request: None,
        }
    }
}

/// Parse a CPU quantity into whole cores. Accepts:
///   - microcores via `u` suffix: "2000000u" → 2.0 (1_000_000u = 1 core)
///   - millicores via `m` suffix: "1000m"   → 1.0 (1_000m     = 1 core)
///   - bare cores:                "2"        → 2.0
///
/// Returns None on unrecognized input so the caller can fall back to "no limit".
pub(crate) fn parse_cpu_cores(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(micro) = trimmed.strip_suffix('u') {
        return micro.parse::<f64>().ok().map(|v| v / 1_000_000.0);
    }
    if let Some(milli) = trimmed.strip_suffix('m') {
        return milli.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    trimmed.parse::<f64>().ok()
}

/// Parse a Kubernetes-style memory quantity into megabytes (binary units, so
/// "1Gi" → 1024 MB). Accepts Ki/Mi/Gi/Ti and the decimal K/M/G/T variants;
/// bare numbers are interpreted as bytes and rounded up to the nearest MB.
pub(crate) fn parse_memory_mb(s: &str) -> Option<u64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (num, factor_to_bytes): (&str, f64) = if let Some(v) = trimmed.strip_suffix("Ki") {
        (v, 1024.0)
    } else if let Some(v) = trimmed.strip_suffix("Mi") {
        (v, 1024.0 * 1024.0)
    } else if let Some(v) = trimmed.strip_suffix("Gi") {
        (v, 1024.0 * 1024.0 * 1024.0)
    } else if let Some(v) = trimmed.strip_suffix("Ti") {
        (v, 1024.0 * 1024.0 * 1024.0 * 1024.0)
    } else if let Some(v) = trimmed.strip_suffix('K') {
        (v, 1000.0)
    } else if let Some(v) = trimmed.strip_suffix('M') {
        (v, 1_000_000.0)
    } else if let Some(v) = trimmed.strip_suffix('G') {
        (v, 1_000_000_000.0)
    } else if let Some(v) = trimmed.strip_suffix('T') {
        (v, 1_000_000_000_000.0)
    } else {
        (trimmed, 1.0)
    };
    let value = num.parse::<f64>().ok()?;
    let mb = (value * factor_to_bytes) / (1024.0 * 1024.0);
    if mb.is_finite() && mb >= 0.0 {
        Some(mb.ceil() as u64)
    } else {
        None
    }
}

/// Configuration for deployment job execution
/// This is built from the entity's DeploymentConfig + runtime values
#[derive(Debug, Clone)]
pub struct DeploymentJobConfig {
    pub namespace: String,
    pub service_name: String,
    pub replicas: u32,
    pub port: u32,
    pub environment_variables: HashMap<String, String>,
    /// Secret values (decrypted plaintext) mounted into the container as
    /// files under `/run/secrets/<KEY>` by the deployer. Never injected as
    /// environment variables; never visible via `docker inspect`.
    pub secrets: HashMap<String, String>,
    pub resources: ResourceUsage,
    /// When `None`, HTTP health checks are skipped entirely (only container
    /// running status is verified). Set to `Some("/")` or a custom path to
    /// enable HTTP health checks after the container starts.
    pub health_check_path: Option<String>,
    /// Explicit deploy-time health-check path override. When `Some`, this wins
    /// over both `.temps.yaml` `health.path` (build job output) and the default
    /// `health_check_path`. Used by image/static deploys that can't read
    /// `.temps.yaml`. `None` means "no deploy-time override" (fall back to the
    /// usual `.temps.yaml`/default resolution).
    pub health_check_path_override: Option<String>,
    pub ingress_enabled: bool,
    pub ingress_host: Option<String>,
    /// Maximum time to wait for the application to become ready (container
    /// start + health checks). Defaults to 300 seconds (5 minutes).
    pub health_check_timeout_secs: u64,
    /// Optional list of node IDs to deploy to. When set, replicas are distributed
    /// only across these specific nodes. When None, all active nodes are eligible.
    pub target_nodes: Option<Vec<i32>>,
    /// Label selector for node-based scheduling. Nodes whose labels match
    /// the selector are eligible. Applied after `target_nodes` filtering.
    pub target_labels: Option<serde_json::Value>,
    /// Environment variables with connection strings rewritten for remote nodes.
    /// Used instead of `environment_variables` when a replica deploys to a worker node
    /// (container names are replaced with the control plane's private address + host port).
    pub remote_environment_variables: Option<HashMap<String, String>>,
    /// Anti-affinity: avoid placing two replicas on the same node.
    /// When true, the scheduler spreads replicas across different nodes.
    pub anti_affinity: bool,
    /// Node IDs that already host containers for the current environment.
    /// During rolling updates, the outgoing containers haven't been removed yet.
    /// When anti-affinity is enabled, these nodes are excluded from scheduling
    /// to prevent new replicas from landing on the same nodes as old ones.
    pub exclude_node_ids: Vec<i32>,
}

impl Default for DeploymentJobConfig {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            service_name: "app".to_string(),
            replicas: 1,
            port: 8080,
            environment_variables: HashMap::new(),
            secrets: HashMap::new(),
            resources: ResourceUsage::default(),
            health_check_path: Some("/".to_string()),
            health_check_path_override: None,
            ingress_enabled: false,
            ingress_host: None,
            health_check_timeout_secs: 300,
            target_nodes: None,
            target_labels: None,
            remote_environment_variables: None,
            anti_affinity: true,
            exclude_node_ids: Vec::new(),
        }
    }
}

/// Target environment for deployment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeploymentTarget {
    Docker {
        registry_url: String,
        network: Option<String>,
    },
}

/// Job for deploying container images to target environments
pub struct DeployImageJob {
    job_id: String,
    build_job_id: String,
    target: DeploymentTarget,
    config: DeploymentJobConfig,
    container_deployer: Arc<dyn ContainerDeployer>,
    /// Node scheduler for distributing replicas across the cluster.
    /// When None, all replicas deploy locally (single-node mode).
    node_scheduler: Option<Arc<crate::services::NodeScheduler>>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    /// Container IDs stored as soon as containers are created for cleanup on failure
    container_ids: Arc<Mutex<Vec<String>>>,
    /// Per-replica deployers: maps container_id → deployer for cleanup on correct node
    replica_deployers: Arc<Mutex<HashMap<String, Arc<dyn ContainerDeployer>>>>,
    /// Background task handle for log streaming (aborted on cleanup)
    log_stream_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Optional: directly provided image tag (for external/pre-built images, bypasses BuildImageJob lookup)
    external_image_tag: Option<String>,
    /// Docker log rotation config to prevent unbounded log growth
    log_config: Option<ContainerLogConfig>,
    /// Encryption service for decrypting node tokens during remote deployments
    encryption_service: Option<Arc<temps_core::EncryptionService>>,
    /// Local image builder — used to `save_image()` before transferring to remote nodes
    image_builder: Option<Arc<dyn temps_deployer::ImageBuilder>>,
}

impl std::fmt::Debug for DeployImageJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeployImageJob")
            .field("job_id", &self.job_id)
            .field("build_job_id", &self.build_job_id)
            .field("target", &self.target)
            .field("config", &self.config)
            .field("container_deployer", &"<ContainerDeployer>")
            .field("node_scheduler", &self.node_scheduler.is_some())
            .finish()
    }
}

impl DeployImageJob {
    pub fn new(
        job_id: String,
        build_job_id: String,
        target: DeploymentTarget,
        container_deployer: Arc<dyn ContainerDeployer>,
    ) -> Self {
        Self {
            job_id,
            build_job_id,
            target,
            config: DeploymentJobConfig::default(),
            container_deployer,
            node_scheduler: None,
            log_id: None,
            log_service: None,
            container_ids: Arc::new(Mutex::new(Vec::new())),
            replica_deployers: Arc::new(Mutex::new(HashMap::new())),
            log_stream_task: Arc::new(Mutex::new(None)),
            external_image_tag: None,
            log_config: None,
            encryption_service: None,
            image_builder: None,
        }
    }

    pub fn with_log_config(mut self, log_config: ContainerLogConfig) -> Self {
        self.log_config = Some(log_config);
        self
    }

    pub fn with_config(mut self, config: DeploymentJobConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_service_name(mut self, service_name: String) -> Self {
        self.config.service_name = service_name;
        self
    }

    pub fn with_namespace(mut self, namespace: String) -> Self {
        self.config.namespace = namespace;
        self
    }

    pub fn with_replicas(mut self, replicas: u32) -> Self {
        self.config.replicas = replicas;
        self
    }

    pub fn with_environment_variables(mut self, env_vars: HashMap<String, String>) -> Self {
        self.config.environment_variables = env_vars;
        self
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    pub fn with_node_scheduler(mut self, scheduler: Arc<crate::services::NodeScheduler>) -> Self {
        self.node_scheduler = Some(scheduler);
        self
    }

    pub fn with_external_image_tag(mut self, image_tag: String) -> Self {
        self.external_image_tag = Some(image_tag);
        self
    }

    pub fn with_encryption_service(mut self, service: Arc<temps_core::EncryptionService>) -> Self {
        self.encryption_service = Some(service);
        self
    }

    pub fn with_image_builder(mut self, builder: Arc<dyn temps_deployer::ImageBuilder>) -> Self {
        self.image_builder = Some(builder);
        self
    }

    /// Write log message to job-specific log file
    /// Ensure the image exists on a remote node, transferring it if needed.
    ///
    /// 1. Checks if the image already exists on the remote node (via agent API).
    /// 2. If not, saves the image as a tar on the control plane (`docker save`).
    /// 3. Streams the tar to the remote agent (`POST /agent/images/import`).
    /// 4. Cleans up the local tar file.
    async fn ensure_image_on_remote(
        &self,
        image_tag: &str,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        node_name: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        // Check if image already exists on the remote node
        match remote.image_exists(image_tag).await {
            Ok(true) => {
                self.log(
                    context,
                    format!(
                        "Image '{}' already exists on node '{}'",
                        image_tag, node_name
                    ),
                )
                .await?;
                return Ok(());
            }
            Ok(false) => {
                self.log(
                    context,
                    format!(
                        "Image '{}' not found on node '{}', transferring...",
                        image_tag, node_name
                    ),
                )
                .await?;
            }
            Err(e) => {
                // If we can't check, try transferring anyway
                tracing::warn!(
                    image = %image_tag,
                    node = %node_name,
                    "Failed to check image existence on remote node, will attempt transfer: {}",
                    e
                );
            }
        }

        let image_builder = match self.image_builder.as_ref() {
            Some(b) => b,
            None => {
                let msg = format!(
                    "Cannot transfer image '{}' to node '{}': no image builder configured. \
                     Multi-node deployments require an image builder for image transfer.",
                    image_tag, node_name
                );
                self.log(context, format!("ERROR: {}", msg)).await?;
                return Err(WorkflowError::JobExecutionFailed(msg));
            }
        };

        // Save image to temp tar file
        let tar_path =
            std::env::temp_dir().join(format!("temps-image-transfer-{}.tar", uuid::Uuid::new_v4()));

        self.log(
            context,
            format!(
                "Saving image '{}' to tar for transfer to '{}'...",
                image_tag, node_name
            ),
        )
        .await?;

        if let Err(e) = image_builder.save_image(image_tag, &tar_path).await {
            let msg = format!(
                "Failed to save image '{}' for transfer to node '{}': {}",
                image_tag, node_name, e
            );
            self.log(context, format!("ERROR: {}", msg)).await?;
            return Err(WorkflowError::JobExecutionFailed(msg));
        }

        // Transfer to remote node
        self.log(
            context,
            format!("Transferring image to node '{}'...", node_name),
        )
        .await?;

        let import_result = remote.import_image(tar_path.clone(), image_tag).await;

        // Clean up local tar file regardless of result
        if let Err(e) = tokio::fs::remove_file(&tar_path).await {
            tracing::warn!("Failed to clean up image tar {:?}: {}", tar_path, e);
        }

        if let Err(e) = import_result {
            let msg = format!(
                "Failed to transfer image '{}' to node '{}': {}",
                image_tag, node_name, e
            );
            self.log(context, format!("ERROR: {}", msg)).await?;
            return Err(WorkflowError::JobExecutionFailed(msg));
        }

        self.log(
            context,
            format!(
                "Image '{}' transferred to node '{}' successfully",
                image_tag, node_name
            ),
        )
        .await?;

        Ok(())
    }

    /// Write log message to both job-specific log file and context log writer
    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content/emojis
        let level = Self::detect_log_level(&message);

        // Write structured log to job-specific log file
        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        // Also write to context log writer (for real-time streaming and test capture)
        context.log(&message).await?;
        Ok(())
    }

    /// Detect log level from message content
    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌")
            || message.contains("Failed")
            || message.contains("Error")
            || message.contains("error")
        {
            LogLevel::Error
        } else if message.contains("⏳")
            || message.contains("Waiting")
            || message.contains("warning")
        {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }

    /// Find an available port on the host machine
    fn find_available_port() -> Result<u16, WorkflowError> {
        use std::net::TcpListener;

        // Bind to 0.0.0.0:0 to match Docker's binding address and avoid port collisions
        // where a port appears free on 127.0.0.1 but is occupied on 0.0.0.0
        let listener = TcpListener::bind("0.0.0.0:0")
            .map_err(|e| WorkflowError::Other(format!("Failed to find available port: {}", e)))?;

        let port = listener
            .local_addr()
            .map_err(|e| WorkflowError::Other(format!("Failed to get port: {}", e)))?
            .port();

        Ok(port)
    }

    /// Resolve the actual container port to expose
    ///
    /// Priority order:
    /// 1. Auto-detected from Docker image EXPOSE directive (source of truth)
    /// 2. Configured port from environment/project/default (fallback)
    ///
    /// This method inspects the built image and extracts exposed ports.
    async fn resolve_container_port(&self, image_tag: &str, context: &WorkflowContext) -> u16 {
        // Try to inspect the image and get exposed ports
        match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => {
                match crate::utils::docker_inspect::get_primary_port(&docker, image_tag).await {
                    Ok(Some(port)) => {
                        let _ = self
                            .log(
                                context,
                                format!("Detected EXPOSE directive in image: port {}", port),
                            )
                            .await;
                        return port;
                    }
                    Ok(None) => {
                        let _ = self
                            .log(
                                context,
                                format!(
                                    "No EXPOSE directive found in image, using configured port: {}",
                                    self.config.port
                                ),
                            )
                            .await;
                    }
                    Err(e) => {
                        let _ = self
                            .log(
                                context,
                                format!(
                                    "Failed to inspect image: {}, using configured port: {}",
                                    e, self.config.port
                                ),
                            )
                            .await;
                    }
                }
            }
            Err(e) => {
                let _ = self
                    .log(
                        context,
                        format!(
                            "Failed to connect to Docker: {}, using configured port: {}",
                            e, self.config.port
                        ),
                    )
                    .await;
            }
        }

        // Fallback to configured port (from environment/project/default)
        self.config.port as u16
    }

    /// Public getter for config to allow test access
    pub fn config(&self) -> &DeploymentJobConfig {
        &self.config
    }

    /// Public getter for target to allow test access
    pub fn target(&self) -> &DeploymentTarget {
        &self.target
    }

    /// Remove all containers if they exist (called on timeout/failure/cancellation)
    async fn cleanup_container(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // First, abort the background log streaming task if running
        let should_log = {
            let mut task_handle = self.log_stream_task.lock().unwrap();
            if let Some(handle) = task_handle.take() {
                handle.abort();
                true
            } else {
                false
            }
        };

        if should_log {
            self.log(context, "🧹 Stopped background log streaming".to_string())
                .await?;
        }

        // Then clean up all containers
        let container_ids = {
            let guard = self.container_ids.lock().unwrap();
            guard.clone()
        };

        if !container_ids.is_empty() {
            self.log(
                context,
                format!("🧹 Cleaning up {} container(s)", container_ids.len()),
            )
            .await?;

            for container_id in &container_ids {
                self.log(context, format!("🧹 Removing container: {}", container_id))
                    .await?;

                // Use per-replica deployer if available, otherwise fall back to local
                let deployer = {
                    let deployers = self.replica_deployers.lock().unwrap();
                    deployers
                        .get(container_id)
                        .cloned()
                        .unwrap_or_else(|| self.container_deployer.clone())
                };

                if let Err(e) = deployer.remove_container(container_id).await {
                    self.log(
                        context,
                        format!(
                            "⚠️  Warning: Failed to remove container {}: {}",
                            container_id, e
                        ),
                    )
                    .await?;
                } else {
                    self.log(
                        context,
                        format!("✅ Container {} removed successfully", container_id),
                    )
                    .await?;
                }
            }
        }

        Ok(())
    }

    /// Deploy the container image with real-time logging
    async fn deploy_image(
        &self,
        image_output: &BuildImageOutput,
        context: &WorkflowContext,
        health_check_override: Option<String>,
    ) -> Result<DeploymentOutput, WorkflowError> {
        self.log(
            context,
            format!(
                "Starting deployment of {} replica(s) for image: {}",
                self.config.replicas, image_output.image_tag
            ),
        )
        .await?;
        self.log(context, format!("Target: {:?}", self.target))
            .await?;
        self.log(
            context,
            format!(
                "Service: {} in namespace: {}",
                self.config.service_name, self.config.namespace
            ),
        )
        .await?;

        // Pre-deployment validation
        self.log(
            context,
            "Validating deployment configuration...".to_string(),
        )
        .await?;
        self.validate_deployment_config(context).await?;

        // Schedule replicas across nodes (or deploy locally if no scheduler/no nodes)
        let node_assignments = if let Some(ref scheduler) = self.node_scheduler {
            let target_ids = self.config.target_nodes.as_deref();
            let target_labels = self.config.target_labels.as_ref();
            match scheduler
                .schedule_replicas_excluding(
                    self.config.replicas,
                    target_labels,
                    target_ids,
                    self.config.anti_affinity,
                    &self.config.exclude_node_ids,
                )
                .await
            {
                Ok(assignments) => {
                    // Log where replicas will be deployed
                    for (i, assignment) in assignments.iter().enumerate() {
                        match assignment {
                            crate::services::NodeAssignment::Local => {
                                self.log(
                                    context,
                                    format!("Replica {} scheduled on local node", i + 1),
                                )
                                .await?;
                            }
                            crate::services::NodeAssignment::Remote {
                                node_name, node_id, ..
                            } => {
                                self.log(
                                    context,
                                    format!(
                                        "Replica {} scheduled on node '{}' (id={})",
                                        i + 1,
                                        node_name,
                                        node_id
                                    ),
                                )
                                .await?;
                            }
                        }
                    }
                    assignments
                }
                Err(e) => {
                    self.log(
                        context,
                        format!(
                            "Node scheduling failed ({}), falling back to local deployment",
                            e
                        ),
                    )
                    .await?;
                    vec![crate::services::NodeAssignment::Local; self.config.replicas as usize]
                }
            }
        } else {
            // No scheduler injected — pure single-node mode
            vec![crate::services::NodeAssignment::Local; self.config.replicas as usize]
        };

        // Deploy multiple replicas
        let mut all_container_ids = Vec::new();
        let mut all_host_ports = Vec::new();
        let mut all_node_ids: Vec<Option<i32>> = Vec::new();
        let mut resolved_container_port: Option<u16> = None;
        let mut deployment_error: Option<WorkflowError> = None;

        for (replica_index, assignment) in node_assignments.iter().enumerate() {
            self.log(
                context,
                format!(
                    "🚀 Deploying replica {}/{}...",
                    replica_index + 1,
                    self.config.replicas
                ),
            )
            .await?;

            // Select deployer based on node assignment
            let deployer: Arc<dyn ContainerDeployer> = match assignment {
                crate::services::NodeAssignment::Local => self.container_deployer.clone(),
                crate::services::NodeAssignment::Remote {
                    address, node_name, ..
                } => {
                    // Look up the node's token from the node service
                    let token = self.get_node_token(assignment).await?;
                    let remote = match temps_deployer::remote::RemoteNodeDeployer::new(
                        address.clone(),
                        token,
                        node_name.clone(),
                    ) {
                        Ok(remote) => Arc::new(remote),
                        Err(e) => {
                            self.log(
                                context,
                                format!(
                                    "Failed to create remote deployer for node '{}': {}",
                                    node_name, e
                                ),
                            )
                            .await?;
                            return Err(WorkflowError::JobExecutionFailed(format!(
                                "Failed to create remote deployer for node '{}': {}",
                                node_name, e
                            )));
                        }
                    };

                    // Transfer image to remote node if it doesn't already exist there
                    self.ensure_image_on_remote(
                        &image_output.image_tag,
                        &remote,
                        node_name,
                        context,
                    )
                    .await?;

                    remote
                }
            };

            match self
                .deploy_single_replica(
                    image_output,
                    context,
                    replica_index as u32,
                    health_check_override.as_deref(),
                    &deployer,
                    assignment,
                )
                .await
            {
                Ok((container_id, host_port, container_port)) => {
                    // Track the deployer for this container (used for cleanup)
                    {
                        let mut deployers = self.replica_deployers.lock().unwrap();
                        deployers.insert(container_id.clone(), deployer);
                    }
                    all_container_ids.push(container_id);
                    all_host_ports.push(host_port);
                    all_node_ids.push(assignment.node_id());
                    // All replicas share the same container port
                    resolved_container_port = Some(container_port);
                }
                Err(e) => {
                    self.log(
                        context,
                        format!("❌ Failed to deploy replica {}: {}", replica_index + 1, e),
                    )
                    .await?;

                    // Clean up all successfully deployed containers before failing
                    self.log(
                        context,
                        format!(
                            "🧹 Cleaning up {} successfully deployed container(s) due to failure",
                            all_container_ids.len()
                        ),
                    )
                    .await?;

                    self.cleanup_container(context).await?;

                    deployment_error = Some(e);
                    break;
                }
            }
        }

        // If we encountered an error during deployment, return it
        if let Some(error) = deployment_error {
            return Err(error);
        }

        if all_container_ids.is_empty() {
            return Err(WorkflowError::JobExecutionFailed(
                "Failed to deploy any replicas".to_string(),
            ));
        }

        self.log(
            context,
            format!(
                "✅ Successfully deployed {}/{} replicas",
                all_container_ids.len(),
                self.config.replicas
            ),
        )
        .await?;

        Ok(DeploymentOutput {
            status: DeploymentStatus::Running,
            replicas: all_container_ids.len() as u32,
            resources: self.config.resources.clone(),
            container_ids: all_container_ids,
            host_ports: all_host_ports,
            container_port: resolved_container_port.unwrap_or(self.config.port as u16),
            node_ids: all_node_ids,
        })
    }

    /// Get the token for a remote node by decrypting the stored encrypted token.
    async fn get_node_token(
        &self,
        assignment: &crate::services::NodeAssignment,
    ) -> Result<String, WorkflowError> {
        match assignment {
            crate::services::NodeAssignment::Local => Err(WorkflowError::JobExecutionFailed(
                "Cannot get token for local node assignment".to_string(),
            )),
            crate::services::NodeAssignment::Remote {
                node_id, node_name, ..
            } => {
                let scheduler = self.node_scheduler.as_ref().ok_or_else(|| {
                    WorkflowError::JobExecutionFailed(
                        "Node scheduler not available for remote deployment".to_string(),
                    )
                })?;

                let node = scheduler
                    .node_service()
                    .get_by_id(*node_id)
                    .await
                    .map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to get node '{}' (id={}): {}",
                            node_name, node_id, e
                        ))
                    })?;

                let encrypted_token = node.token_encrypted.ok_or_else(|| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Node '{}' (id={}) has no encrypted token — re-register the node",
                        node_name, node_id
                    ))
                })?;

                let encryption_service = self.encryption_service.as_ref().ok_or_else(|| {
                    WorkflowError::JobExecutionFailed(
                        "Encryption service not available for token decryption".to_string(),
                    )
                })?;

                let decrypted_bytes =
                    encryption_service.decrypt(&encrypted_token).map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to decrypt token for node '{}' (id={}): {}",
                            node_name, node_id, e
                        ))
                    })?;

                String::from_utf8(decrypted_bytes).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Decrypted token for node '{}' (id={}) is not valid UTF-8: {}",
                        node_name, node_id, e
                    ))
                })
            }
        }
    }

    /// Deploy a single replica of the container
    async fn deploy_single_replica(
        &self,
        image_output: &BuildImageOutput,
        context: &WorkflowContext,
        replica_index: u32,
        health_check_override: Option<&str>,
        deployer: &Arc<dyn ContainerDeployer>,
        assignment: &crate::services::NodeAssignment,
    ) -> Result<(String, u16, u16), WorkflowError> {
        // Prepare deployment request using temps-deployer types
        self.log(context, "Deploying container image...".to_string())
            .await?;

        let log_path = std::env::temp_dir().join(format!("deploy_{}.log", self.job_id));

        // Determine the actual container port to expose
        // Priority: Image EXPOSE directive > configured port (from environment/project/default)
        let container_port = self
            .resolve_container_port(&image_output.image_tag, context)
            .await;

        // For local deployments, allocate a port on this host.
        // For remote deployments, set host_port=0 so Docker on the agent picks an available port.
        let host_port = if assignment.is_local() {
            Self::find_available_port()?
        } else {
            0
        };
        self.log(
            context,
            format!(
                "🔌 {} host port: {} → container port: {}",
                if assignment.is_local() {
                    "Allocated"
                } else {
                    "Requesting dynamic"
                },
                if host_port == 0 {
                    "auto".to_string()
                } else {
                    host_port.to_string()
                },
                container_port
            ),
        )
        .await?;

        let port_mappings = vec![PortMapping {
            host_port,
            container_port,
            protocol: Protocol::Tcp,
        }];

        // Convert k8s-style strings ("1000m", "512Mi", "2", "1Gi") into the
        // numeric ResourceLimits the deployer feeds to bollard. The previous
        // implementation called `parse::<f64>()` directly, which silently
        // returned None for any value containing a unit suffix — so even the
        // builder defaults ("1000m" / "512Mi") never made it to the container.
        let resource_limits = ResourceLimits {
            cpu_limit: self
                .config
                .resources
                .cpu_limit
                .as_ref()
                .and_then(|s| parse_cpu_cores(s)),
            memory_limit_mb: self
                .config
                .resources
                .memory_limit
                .as_ref()
                .and_then(|s| parse_memory_mb(s)),
            disk_limit_mb: None,
        };

        // Use remote environment variables for remote deployments (connection strings
        // rewritten with control plane's private address), fall back to local env vars.
        let environment_vars = if !assignment.is_local() {
            if let Some(ref remote_vars) = self.config.remote_environment_variables {
                tracing::info!(
                    "Using REMOTE environment variables for non-local assignment (has {} remote vars)",
                    remote_vars.len()
                );
                remote_vars.clone()
            } else {
                tracing::warn!(
                    "Non-local assignment but no remote_environment_variables available, using local vars"
                );
                self.config.environment_variables.clone()
            }
        } else {
            tracing::info!("Using LOCAL environment variables for local assignment");
            self.config.environment_variables.clone()
        };

        tracing::info!(
            "Deploying container with {} env vars, POSTGRES_HOST={:?}, POSTGRES_URL={:?}",
            environment_vars.len(),
            environment_vars.get("POSTGRES_HOST"),
            environment_vars.get("POSTGRES_URL").map(|u| {
                // Truncate for logging (may contain password)
                if u.len() > 60 {
                    format!("{}...", &u[..60])
                } else {
                    u.clone()
                }
            })
        );

        // Create unique container name for each replica
        let container_name = if self.config.replicas > 1 {
            format!("{}-{}", self.config.service_name, replica_index + 1)
        } else {
            self.config.service_name.clone()
        };

        // Build Docker labels for the log aggregator to discover this container.
        // The collector inspects these labels to enrich log lines with project/env/service context.
        // `sh.temps.managed` marks this as a Temps-managed container for reconciliation.
        let mut labels = HashMap::new();
        labels.insert("sh.temps.managed".to_string(), "true".to_string());
        labels.insert(
            "sh.temps.project_id".to_string(),
            context.project_id.to_string(),
        );
        labels.insert(
            "sh.temps.environment".to_string(),
            context.environment_id.to_string(),
        );
        labels.insert(
            "sh.temps.service".to_string(),
            self.config.service_name.clone(),
        );
        labels.insert(
            "sh.temps.deploy_id".to_string(),
            context.deployment_id.to_string(),
        );

        let deploy_request = DeployRequest {
            image_name: image_output.image_tag.clone(),
            container_name,
            environment_vars,
            secrets: self.config.secrets.clone(),
            port_mappings,
            network_name: None,
            resource_limits,
            restart_policy: RestartPolicy::Always,
            log_path,
            command: None,
            log_config: self.log_config.clone(),
            labels,
        };

        let deploy_result = deployer
            .deploy_container(deploy_request)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to deploy container: {}", e))
            })?;

        // CRITICAL: Store container_id immediately for cleanup on failure/cancellation
        {
            let mut container_ids = self.container_ids.lock().unwrap();
            container_ids.push(deploy_result.container_id.clone());
        }

        self.log(
            context,
            format!("Deployment created: {}", deploy_result.container_id),
        )
        .await?;

        // Wait for deployment to be ready (with timeout)
        self.log(context, "Waiting for container to start...".to_string())
            .await?;
        let max_wait_time = std::time::Duration::from_secs(self.config.health_check_timeout_secs);
        let start_time = std::time::Instant::now();

        // Phase 1: Wait for container to be running
        loop {
            // Try to get container info, but don't fail hard if it can't be found
            // (container might have been removed by Docker or other processes)
            let container_info = match deployer
                .get_container_info(&deploy_result.container_id)
                .await
            {
                Ok(info) => info,
                Err(e) => {
                    // Container not found - might have been removed, but that's okay
                    // Log a warning but don't fail the deployment
                    tracing::warn!(
                        "Cannot verify container {} during deployment - container may have been removed: {}",
                        deploy_result.container_id,
                        e
                    );
                    self.log(
                        context,
                        format!(
                            "⏳ Container status check failed (may have been removed): {}",
                            e
                        ),
                    )
                    .await?;

                    // Wait a bit and try again, but don't fail if we can't verify
                    if start_time.elapsed() > max_wait_time {
                        self.log(context, "Container verification timeout - proceeding anyway (container may be running)".to_string())
                            .await?;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            match container_info.status {
                DeployerContainerStatus::Running => {
                    self.log(context, "✅ Container is running".to_string())
                        .await?;
                    break;
                }
                DeployerContainerStatus::Exited | DeployerContainerStatus::Dead => {
                    self.log(context, "❌ Container failed to start".to_string())
                        .await?;
                    // Clean up failed container
                    self.cleanup_container(context).await?;
                    return Err(WorkflowError::JobExecutionFailed(
                        "Container failed to start".to_string(),
                    ));
                }
                DeployerContainerStatus::Created => {
                    if start_time.elapsed() > max_wait_time {
                        self.log(context, "⏱️  Container start timeout".to_string())
                            .await?;
                        // Clean up timed-out container
                        self.cleanup_container(context).await?;
                        return Err(WorkflowError::JobExecutionFailed(
                            "Container timeout - took too long to start".to_string(),
                        ));
                    }
                    self.log(
                        context,
                        format!("Container status: {:?}, waiting...", container_info.status),
                    )
                    .await?;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                _ => {
                    self.log(
                        context,
                        format!("Container status: {:?}, waiting...", container_info.status),
                    )
                    .await?;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }

        // Stream container logs in background (non-blocking)
        // This runs concurrently with health checks
        let container_id_for_logs = deploy_result.container_id.clone();
        let log_id = self.log_id.clone();
        let log_service = self.log_service.clone();
        let context_for_logs = context.clone();

        let log_task = tokio::spawn(async move {
            // Helper macro to write logs in the background task
            macro_rules! write_log {
                ($level:expr, $msg:expr) => {
                    if let (Some(ref log_id), Some(ref log_service)) = (&log_id, &log_service) {
                        let _ = log_service
                            .append_structured_log(log_id, $level, $msg.clone())
                            .await;
                    }
                    let _ = context_for_logs.log(&$msg).await;
                };
            }

            write_log!(
                LogLevel::Info,
                format!("📋 Streaming container logs for 15s...")
            );

            // Connect to Docker
            let docker = match bollard::Docker::connect_with_local_defaults() {
                Ok(d) => d,
                Err(e) => {
                    write_log!(
                        LogLevel::Warning,
                        format!("⚠️  Cannot stream logs - Docker connection failed: {}", e)
                    );
                    return;
                }
            };

            // Configure log options
            let log_options = bollard::query_parameters::LogsOptions {
                stdout: true,
                stderr: true,
                follow: true,
                timestamps: false,
                ..Default::default()
            };

            // Stream logs with timeout
            let mut log_stream = docker.logs(&container_id_for_logs, Some(log_options));
            let mut line_count = 0;
            let max_lines = 100;
            let timeout = tokio::time::sleep(std::time::Duration::from_secs(15));
            tokio::pin!(timeout);

            loop {
                tokio::select! {
                    _ = &mut timeout => {
                        write_log!(LogLevel::Info,
                            format!("📋 Log streaming complete ({} lines captured)", line_count));
                        break;
                    }
                    log_result = log_stream.next() => {
                        match log_result {
                            Some(Ok(log_output)) => {
                                let clean_msg = log_output.to_string().trim().to_string();
                                if !clean_msg.is_empty() {
                                    write_log!(LogLevel::Info,
                                        format!("🐳 {}", clean_msg));
                                    line_count += 1;

                                    if line_count >= max_lines {
                                        write_log!(LogLevel::Info,
                                            format!("📋 Log limit reached ({} lines), stopping stream...", max_lines));
                                        break;
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                write_log!(LogLevel::Warning,
                                    format!("⚠️  Log stream error: {}", e));
                                break;
                            }
                            None => {
                                write_log!(LogLevel::Info,
                                    format!("📋 Log streaming complete ({} lines captured)", line_count));
                                break;
                            }
                        }
                    }
                }
            }
        });

        // Store the task handle for cleanup on cancellation
        {
            let mut task_handle = self.log_stream_task.lock().unwrap();
            *task_handle = Some(log_task);
        }

        // Phase 2: Wait for application to be ready (connectivity check)
        // When health_check_path is None, skip HTTP health checks entirely --
        // the container running status from Phase 1 is sufficient (useful for
        // rollbacks where the image was already verified, or services without
        // an HTTP endpoint).
        // Precedence (highest first):
        //   1. explicit deploy-time override (config.health_check_path_override) —
        //      set by image/static deploys that can't read .temps.yaml
        //   2. .temps.yaml health.path (build job output, passed as health_check_override)
        //   3. default config.health_check_path ("/" unless explicitly cleared)
        let effective_health_path = self
            .config
            .health_check_path_override
            .clone()
            .or_else(|| health_check_override.map(String::from))
            .or_else(|| self.config.health_check_path.clone());
        if let Some(ref health_path) = effective_health_path {
            self.log(
                context,
                "Waiting for application to be ready...".to_string(),
            )
            .await?;
            // For remote nodes, health check via the node's private IP and host port
            // since the control plane can't reach the container by Docker network name.
            // For local deployments, use the standard container URL resolution.
            let health_check_url = if let Some(private_addr) = assignment.private_address() {
                format!(
                    "http://{}:{}{}",
                    private_addr, deploy_result.host_port, health_path
                )
            } else {
                temps_core::DeploymentMode::build_container_url(
                    &deploy_result.container_name,
                    deploy_result.container_port,
                    deploy_result.host_port,
                    Some(health_path),
                )
            };
            self.log(context, format!("Health check URL: {}", health_check_url))
                .await?;

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to create HTTP client: {}",
                        e
                    ))
                })?;

            let mut consecutive_successes = 0;
            let required_successes = 2; // Require 2 consecutive successful connections
            let mut first_error_time: Option<std::time::Instant> = None;
            let max_error_duration = std::time::Duration::from_secs(60); // Only retry errors for 60 seconds

            loop {
                // Check for overall timeout
                if start_time.elapsed() > max_wait_time {
                    self.log(
                        context,
                        "Application readiness timeout - connectivity checks failed".to_string(),
                    )
                    .await?;
                    // Clean up container on connectivity timeout
                    self.cleanup_container(context).await?;
                    return Err(WorkflowError::JobExecutionFailed(
                        "Application timeout - connectivity checks did not pass in time"
                            .to_string(),
                    ));
                }

                // Check for error timeout (60 seconds of consecutive 4xx/5xx errors)
                if let Some(error_start) = first_error_time {
                    if error_start.elapsed() > max_error_duration {
                        self.log(
                            context,
                            "Application health check failed - server returning errors for too long"
                                .to_string(),
                        )
                        .await?;
                        // Clean up container on health check failure
                        self.cleanup_container(context).await?;
                        return Err(WorkflowError::JobExecutionFailed(
                            "Application health check failed - server returned error status codes for 60 seconds".to_string(),
                        ));
                    }
                }

                // Check if container is still running (it may have crashed)
                // This prevents waiting the full timeout for a container that already exited
                if let Ok(container_info) = deployer
                    .get_container_info(&deploy_result.container_id)
                    .await
                {
                    match container_info.status {
                        DeployerContainerStatus::Exited | DeployerContainerStatus::Dead => {
                            self.log(
                                context,
                                "Container crashed during startup - application failed to start"
                                    .to_string(),
                            )
                            .await?;
                            // Clean up crashed container
                            self.cleanup_container(context).await?;
                            return Err(WorkflowError::JobExecutionFailed(
                                "Container crashed during startup - check container logs for details"
                                    .to_string(),
                            ));
                        }
                        _ => {
                            // Container is still running, continue with connectivity checks
                        }
                    }
                }

                match client.get(&health_check_url).send().await {
                    Ok(response) => {
                        let status = response.status();

                        // Any HTTP response means the server is running.
                        // 2xx, 3xx, 404, and 405 are all valid — the health check
                        // path may not exist but the server is up and responding.
                        // Only 5xx indicates a real problem.
                        let is_healthy = status.is_success()
                            || status.is_redirection()
                            || status.as_u16() == 404
                            || status.as_u16() == 405;
                        if is_healthy {
                            consecutive_successes += 1;
                            first_error_time = None; // Reset error timer on success

                            let message = format!(
                                "Health check passed - server healthy with status {} ({}/{})",
                                status, consecutive_successes, required_successes
                            );
                            if let (Some(ref log_id), Some(ref log_service)) =
                                (&self.log_id, &self.log_service)
                            {
                                log_service
                                    .append_structured_log(
                                        log_id,
                                        LogLevel::Success,
                                        message.clone(),
                                    )
                                    .await
                                    .map_err(|e| {
                                        WorkflowError::Other(format!("Failed to write log: {}", e))
                                    })?;
                            }
                            context.log(&message).await?;

                            if consecutive_successes >= required_successes {
                                self.log(context, "Application is ready and healthy!".to_string())
                                    .await?;
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        } else {
                            // 4xx, 5xx = application error
                            consecutive_successes = 0;

                            // Start error timer if this is the first error
                            if first_error_time.is_none() {
                                first_error_time = Some(std::time::Instant::now());
                            }

                            let elapsed = first_error_time.unwrap().elapsed().as_secs();
                            self.log(
                                context,
                                format!(
                                    "Health check failed - server returned error status {} (not healthy), retrying... ({}/60s)",
                                    status, elapsed
                                ),
                            )
                            .await?;
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                    Err(e) => {
                        consecutive_successes = 0; // Reset counter on connection error
                        first_error_time = None; // Reset error timer - connection errors are expected during startup
                        self.log(
                            context,
                            format!("Connectivity check failed ({}), retrying...", e),
                        )
                        .await?;
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        } else {
            self.log(
                context,
                "Health check path not configured - skipping HTTP health checks (container is running)".to_string(),
            )
            .await?;
        }

        let endpoint_url = if let Some(private_addr) = assignment.private_address() {
            format!("http://{}:{}", private_addr, deploy_result.host_port)
        } else {
            temps_core::DeploymentMode::build_container_url(
                &deploy_result.container_name,
                deploy_result.container_port,
                deploy_result.host_port,
                None,
            )
        };
        self.log(
            context,
            format!("✅ Replica {} ready at {}", replica_index + 1, endpoint_url),
        )
        .await?;

        // Return container ID, host port, and container port
        Ok((
            deploy_result.container_id,
            deploy_result.host_port,
            deploy_result.container_port,
        ))
    }

    async fn validate_deployment_config(
        &self,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        if self.config.service_name.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "service_name cannot be empty".to_string(),
            ));
        }

        if self.config.namespace.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "namespace cannot be empty".to_string(),
            ));
        }

        if self.config.replicas == 0 {
            return Err(WorkflowError::JobValidationFailed(
                "replicas must be greater than 0".to_string(),
            ));
        }

        self.log(context, "Deployment configuration is valid".to_string())
            .await?;
        Ok(())
    }
}

#[async_trait]
impl WorkflowTask for DeployImageJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Deploy Image"
    }

    fn description(&self) -> &str {
        "Deploys a built container image to the target environment"
    }

    fn depends_on(&self) -> Vec<String> {
        // If external image is provided, no dependencies on build job
        if self.external_image_tag.is_some() {
            vec![]
        } else {
            vec![self.build_job_id.clone()]
        }
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Get image output either from external tag or from build job
        let image_output = if let Some(ref external_tag) = self.external_image_tag {
            // External image provided directly - create synthetic BuildImageOutput
            self.log(&context, format!("Using external image: {}", external_tag))
                .await?;
            BuildImageOutput {
                image_tag: external_tag.clone(),
                image_id: format!("external-{}", external_tag.replace(":", "-")),
                size_bytes: 0, // Not applicable for external images
                build_context: std::path::PathBuf::from("."),
                dockerfile_path: std::path::PathBuf::from("."),
            }
        } else {
            // Standard workflow - get from build job output
            BuildImageOutput::from_context(&context, &self.build_job_id)?
        };

        // Apply .temps.yaml health config if the build job found one.
        // The BuildImageJob reads .temps.yaml and stores health_check_path as an output.
        // We override the default config before deploying.
        let health_override: Option<String> = context
            .get_output::<String>(&self.build_job_id, "health_check_path")
            .ok()
            .flatten();

        if let Some(ref health_path) = health_override {
            self.log(
                &context,
                format!("Using health check path from .temps.yaml: {}", health_path),
            )
            .await?;
        }

        // An explicit deploy-time override (set by image/static deploys that can't
        // read .temps.yaml) wins over the .temps.yaml value.
        if let Some(ref override_path) = self.config.health_check_path_override {
            self.log(
                &context,
                format!(
                    "Using deploy-time health check path override: {}",
                    override_path
                ),
            )
            .await?;
        }

        // Persist the effective health-check path as this job's output so
        // MarkDeploymentCompleteJob can propagate it to the environment's monitor
        // (its check_path). For image/static deploys there is no build job output,
        // so this is the only place the path becomes available downstream.
        let effective_health_path = self
            .config
            .health_check_path_override
            .clone()
            .or_else(|| health_override.clone())
            .or_else(|| self.config.health_check_path.clone());
        if let Some(ref effective) = effective_health_path {
            context.set_output(&self.job_id, "health_check_path", effective)?;
        }

        // Deploy the image (logs written in real-time)
        let deployment_output = self
            .deploy_image(&image_output, &context, health_override)
            .await?;

        // Set typed job outputs
        context.set_output(&self.job_id, "status", &deployment_output.status)?;
        context.set_output(&self.job_id, "replicas", deployment_output.replicas)?;
        context.set_output(
            &self.job_id,
            "container_ids",
            &deployment_output.container_ids,
        )?;
        context.set_output(&self.job_id, "host_ports", &deployment_output.host_ports)?;
        context.set_output(&self.job_id, "node_ids", &deployment_output.node_ids)?;

        // For backward compatibility, also set singular fields using the first container
        if !deployment_output.container_ids.is_empty() {
            context.set_output(
                &self.job_id,
                "container_id",
                &deployment_output.container_ids[0],
            )?;
            context.set_output(&self.job_id, "container_name", &self.config.service_name)?;
            context.set_output(&self.job_id, "host_port", deployment_output.host_ports[0])?;
            context.set_output(
                &self.job_id,
                "container_port",
                deployment_output.container_port as i32,
            )?;

            // Set artifact for first container
            context.set_artifact(
                &self.job_id,
                "deployment",
                PathBuf::from(&deployment_output.container_ids[0]),
            );
        }

        Ok(JobResult::success(context))
    }

    async fn execute_with_cancellation(
        &self,
        context: WorkflowContext,
        cancellation_provider: &dyn WorkflowCancellationProvider,
    ) -> Result<JobResult, WorkflowError> {
        let workflow_run_id = context.workflow_run_id.clone();

        // Check if already cancelled before starting
        if cancellation_provider.is_cancelled(&workflow_run_id).await? {
            self.log(
                &context,
                "Deploy cancelled before starting - deployment was cancelled by user".to_string(),
            )
            .await
            .ok();
            return Err(WorkflowError::BuildCancelled);
        }

        // Create cancellation check future that polls every 2 seconds
        let cancellation_check = async {
            loop {
                sleep(Duration::from_secs(2)).await;

                match cancellation_provider.is_cancelled(&workflow_run_id).await {
                    Ok(true) => {
                        // Cancellation detected
                        return;
                    }
                    Ok(false) => {
                        // Continue checking
                    }
                    Err(_) => {
                        // Error checking cancellation - stop polling
                        break;
                    }
                }
            }
        };

        // Race between deploy execution and cancellation detection
        let deploy_future = self.execute(context.clone());

        tokio::select! {
            result = deploy_future => {
                // Deploy completed (success or failure)
                result
            }
            _ = cancellation_check => {
                // Cancellation detected during deploy
                self.log(
                    &context,
                    "Deploy cancelled by user - stopping container deployment".to_string(),
                )
                .await
                .ok();

                Err(WorkflowError::BuildCancelled)
            }
        }
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // If external image is provided, skip build job validation
        if self.external_image_tag.is_some() {
            return Ok(());
        }

        // Verify that the build job output is available (for standard workflow)
        BuildImageOutput::from_context(context, &self.build_job_id)?;

        // Basic validation
        if self.build_job_id.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "build_job_id cannot be empty".to_string(),
            ));
        }

        // Note: validate_deployment_config requires context for logging,
        // so we skip it here and rely on execute to validate

        Ok(())
    }

    async fn cleanup(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Use the stored container_id (set immediately after container creation)
        // This ensures cleanup works even if deployment fails before setting outputs
        self.cleanup_container(context).await
    }
}

/// Builder for DeployImageJob
pub struct DeployImageJobBuilder {
    job_id: Option<String>,
    build_job_id: Option<String>,
    target: Option<DeploymentTarget>,
    config: DeploymentJobConfig,
    node_scheduler: Option<Arc<crate::services::NodeScheduler>>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    external_image_tag: Option<String>,
    log_config: Option<ContainerLogConfig>,
    encryption_service: Option<Arc<temps_core::EncryptionService>>,
    image_builder: Option<Arc<dyn temps_deployer::ImageBuilder>>,
}

impl DeployImageJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            build_job_id: None,
            target: None,
            config: DeploymentJobConfig::default(),
            node_scheduler: None,
            log_id: None,
            log_service: None,
            external_image_tag: None,
            log_config: None,
            encryption_service: None,
            image_builder: None,
        }
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn build_job_id(mut self, build_job_id: String) -> Self {
        self.build_job_id = Some(build_job_id);
        self
    }

    pub fn target(mut self, target: DeploymentTarget) -> Self {
        self.target = Some(target);
        self
    }

    pub fn service_name(mut self, service_name: String) -> Self {
        self.config.service_name = service_name;
        self
    }

    pub fn namespace(mut self, namespace: String) -> Self {
        self.config.namespace = namespace;
        self
    }

    pub fn replicas(mut self, replicas: u32) -> Self {
        self.config.replicas = replicas;
        self
    }

    pub fn port(mut self, port: u32) -> Self {
        self.config.port = port;
        self
    }

    pub fn environment_variables(mut self, env_vars: HashMap<String, String>) -> Self {
        self.config.environment_variables = env_vars;
        self
    }

    /// Sets decrypted secret values for this deployment. They will be
    /// materialized as files under `/run/secrets/<KEY>` (tmpfs, mode 0400)
    /// inside the container.
    pub fn secrets(mut self, secrets: HashMap<String, String>) -> Self {
        self.config.secrets = secrets;
        self
    }

    pub fn resources(mut self, resources: ResourceUsage) -> Self {
        self.config.resources = resources;
        self
    }

    pub fn ingress(mut self, enabled: bool, host: Option<String>) -> Self {
        self.config.ingress_enabled = enabled;
        self.config.ingress_host = host;
        self
    }

    /// Set the health check path. When `None`, HTTP health checks are skipped
    /// entirely after the container reaches running state. Useful for rollbacks
    /// or services without an HTTP endpoint.
    pub fn health_check_path(mut self, path: Option<String>) -> Self {
        self.config.health_check_path = path;
        self
    }

    /// Set an explicit deploy-time health-check path override. When `Some`, it
    /// takes priority over both `.temps.yaml` `health.path` and the default
    /// `health_check_path`. Used by image/static deploys that can't read
    /// `.temps.yaml`. Passing `None` leaves the usual resolution untouched.
    pub fn health_check_path_override(mut self, path: Option<String>) -> Self {
        self.config.health_check_path_override = path;
        self
    }

    /// Set the maximum time (in seconds) to wait for the application to become
    /// ready. Defaults to 300 seconds (5 minutes).
    pub fn health_check_timeout_secs(mut self, secs: u64) -> Self {
        self.config.health_check_timeout_secs = secs;
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

    /// Set external image tag (for pre-built images, bypasses build job dependency)
    pub fn external_image_tag(mut self, image_tag: String) -> Self {
        self.external_image_tag = Some(image_tag);
        self
    }

    /// Set Docker log rotation config to prevent unbounded log growth
    pub fn container_log_config(mut self, log_config: ContainerLogConfig) -> Self {
        self.log_config = Some(log_config);
        self
    }

    /// Set the node scheduler for multi-node deployments
    pub fn node_scheduler(mut self, scheduler: Arc<crate::services::NodeScheduler>) -> Self {
        self.node_scheduler = Some(scheduler);
        self
    }

    /// Set the target node IDs for this deployment
    pub fn target_nodes(mut self, node_ids: Vec<i32>) -> Self {
        self.config.target_nodes = Some(node_ids);
        self
    }

    /// Set the label selector for node-based scheduling
    pub fn target_labels(mut self, labels: serde_json::Value) -> Self {
        self.config.target_labels = Some(labels);
        self
    }

    /// Enable or disable anti-affinity (spread replicas across different nodes)
    pub fn anti_affinity(mut self, enabled: bool) -> Self {
        self.config.anti_affinity = enabled;
        self
    }

    /// Set node IDs to exclude from scheduling (rolling update awareness).
    /// These are nodes that still host containers from the previous deployment.
    pub fn exclude_node_ids(mut self, node_ids: Vec<i32>) -> Self {
        self.config.exclude_node_ids = node_ids;
        self
    }

    /// Set remote environment variables (connection strings rewritten for worker nodes)
    pub fn remote_environment_variables(
        mut self,
        remote_vars: Option<HashMap<String, String>>,
    ) -> Self {
        self.config.remote_environment_variables = remote_vars;
        self
    }

    /// Set the encryption service for decrypting node tokens during remote deployments
    pub fn encryption_service(mut self, service: Arc<temps_core::EncryptionService>) -> Self {
        self.encryption_service = Some(service);
        self
    }

    /// Set the local image builder for transferring images to remote nodes
    pub fn image_builder(mut self, builder: Arc<dyn temps_deployer::ImageBuilder>) -> Self {
        self.image_builder = Some(builder);
        self
    }

    pub fn build(
        self,
        container_deployer: Arc<dyn ContainerDeployer>,
    ) -> Result<DeployImageJob, WorkflowError> {
        let job_id = self.job_id.unwrap_or_else(|| "deploy_image".to_string());
        let build_job_id = self.build_job_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("build_job_id is required".to_string())
        })?;
        let target = self.target.ok_or_else(|| {
            WorkflowError::JobValidationFailed("deployment target is required".to_string())
        })?;

        let mut job = DeployImageJob::new(job_id, build_job_id, target, container_deployer)
            .with_config(self.config);

        if let Some(scheduler) = self.node_scheduler {
            job = job.with_node_scheduler(scheduler);
        }
        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }
        if let Some(external_image_tag) = self.external_image_tag {
            job = job.with_external_image_tag(external_image_tag);
        }
        if let Some(log_config) = self.log_config {
            job = job.with_log_config(log_config);
        }
        if let Some(encryption_service) = self.encryption_service {
            job = job.with_encryption_service(encryption_service);
        }
        if let Some(image_builder) = self.image_builder {
            job = job.with_image_builder(image_builder);
        }

        Ok(job)
    }
}

impl Default for DeployImageJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[test]
    fn resource_usage_default_is_uncapped() {
        // Regression guard: the default MUST be all-None so any deploy path that
        // builds a job without calling `.resources(...)` (rollback/promote)
        // leaves the container uncapped rather than silently applying a phantom
        // CPU/memory limit. `None` here → `parse_cpu_cores`/`parse_memory_mb`
        // return None → Docker `HostConfig` nano_cpus/memory stay unset.
        let d = ResourceUsage::default();
        assert_eq!(
            d.cpu_limit, None,
            "default cpu_limit must be None (uncapped)"
        );
        assert_eq!(
            d.memory_limit, None,
            "default memory_limit must be None (uncapped)"
        );
        assert_eq!(d.cpu_request, None);
        assert_eq!(d.memory_request, None);
        // And it must parse to no limit at the deployer boundary.
        assert_eq!(d.cpu_limit.as_deref().and_then(parse_cpu_cores), None);
        assert_eq!(d.memory_limit.as_deref().and_then(parse_memory_mb), None);
    }

    #[test]
    fn parse_cpu_cores_handles_millicores_and_whole_cores() {
        assert_eq!(parse_cpu_cores("1000m"), Some(1.0));
        assert_eq!(parse_cpu_cores("500m"), Some(0.5));
        assert_eq!(parse_cpu_cores("2"), Some(2.0));
        assert_eq!(parse_cpu_cores("0.25"), Some(0.25));
        assert_eq!(parse_cpu_cores("  1500m  "), Some(1.5));
        assert_eq!(parse_cpu_cores(""), None);
        assert_eq!(parse_cpu_cores("garbage"), None);
    }

    #[test]
    fn parse_cpu_cores_handles_microcores() {
        // 1_000_000u = 1 core (the storage convention used by temps DB).
        assert_eq!(parse_cpu_cores("1000000u"), Some(1.0));
        assert_eq!(parse_cpu_cores("500000u"), Some(0.5));
        assert_eq!(parse_cpu_cores("2000000u"), Some(2.0));
        assert_eq!(parse_cpu_cores("100000u"), Some(0.1));
        assert_eq!(parse_cpu_cores("  2000000u  "), Some(2.0));
    }

    #[test]
    fn parse_memory_mb_handles_binary_and_decimal_suffixes() {
        assert_eq!(parse_memory_mb("512Mi"), Some(512));
        assert_eq!(parse_memory_mb("1Gi"), Some(1024));
        assert_eq!(parse_memory_mb("2048Ki"), Some(2));
        assert_eq!(parse_memory_mb("1G"), Some(954)); // 1e9 / (1024*1024) ≈ 953.67 → ceil
        assert_eq!(parse_memory_mb("128"), Some(1)); // 128 bytes → ceil to 1 MB
        assert_eq!(parse_memory_mb(""), None);
        assert_eq!(parse_memory_mb("garbage"), None);
    }

    use temps_deployer::{
        ContainerDeployer, ContainerInfo, ContainerStats,
        ContainerStatus as DeployerContainerStatus, DeployRequest, DeployResult, DeployerError,
    };

    // Mock ContainerDeployer for testing multi-replica deployments
    use std::sync::Mutex as StdMutex;

    struct TrackingMockContainerDeployer {
        deployed_containers: Arc<StdMutex<Vec<String>>>,
    }

    impl TrackingMockContainerDeployer {
        fn new() -> Self {
            Self {
                deployed_containers: Arc::new(StdMutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl ContainerDeployer for TrackingMockContainerDeployer {
        async fn deploy_container(
            &self,
            request: DeployRequest,
        ) -> Result<DeployResult, DeployerError> {
            // Generate unique container ID based on container name
            let container_id = format!("container_{}", request.container_name);

            // Track this deployment
            self.deployed_containers
                .lock()
                .unwrap()
                .push(container_id.clone());

            // Use the port from request
            let host_port = request
                .port_mappings
                .first()
                .map(|p| p.host_port)
                .unwrap_or(8080);
            let container_port = request
                .port_mappings
                .first()
                .map(|p| p.container_port)
                .unwrap_or(8080);

            Ok(DeployResult {
                container_id,
                container_name: request.container_name,
                container_port,
                host_port,
                status: DeployerContainerStatus::Running,
            })
        }

        async fn start_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn stop_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn pause_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn resume_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn remove_container(&self, _container_id: &str) -> Result<(), DeployerError> {
            Ok(())
        }

        async fn get_container_info(
            &self,
            _container_id: &str,
        ) -> Result<ContainerInfo, DeployerError> {
            Ok(ContainerInfo {
                container_id: "test_container_123".to_string(),
                container_name: "test_container".to_string(),
                image_name: "test:latest".to_string(),
                status: DeployerContainerStatus::Running,
                created_at: chrono::Utc::now(),
                ports: vec![],
                environment_vars: HashMap::new(),
                restart_count: Some(0),
                labels: HashMap::new(),
                ..Default::default()
            })
        }

        async fn get_container_stats(
            &self,
            container_id: &str,
        ) -> Result<ContainerStats, DeployerError> {
            Ok(ContainerStats {
                container_id: container_id.to_string(),
                container_name: "test_container".to_string(),
                cpu_percent: 25.0,
                memory_bytes: 268435456,
                memory_limit_bytes: Some(2147483648),
                memory_percent: Some(12.5),
                network_rx_bytes: 2048000,
                network_tx_bytes: 1024000,
                timestamp: chrono::Utc::now(),
                ..Default::default()
            })
        }

        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            Ok(vec![])
        }

        async fn get_container_logs(&self, _container_id: &str) -> Result<String, DeployerError> {
            Ok("test logs".to_string())
        }

        async fn stream_container_logs(
            &self,
            _container_id: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            Err(DeployerError::Other("Not implemented".to_string()))
        }
    }

    #[test]
    fn test_deploy_image_job_builder() {
        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());
        let target = DeploymentTarget::Docker {
            registry_url: "registry.test.com".to_string(),
            network: Some("test-network".to_string()),
        };

        let mut env_vars = HashMap::new();
        env_vars.insert("ENV".to_string(), "production".to_string());

        let job = DeployImageJobBuilder::new()
            .job_id("test_deploy".to_string())
            .build_job_id("build_image".to_string())
            .target(target)
            .service_name("myapp".to_string())
            .namespace("production".to_string())
            .replicas(3)
            .environment_variables(env_vars)
            .build(container_deployer)
            .unwrap();

        assert_eq!(job.job_id(), "test_deploy");
        assert_eq!(job.build_job_id, "build_image");
        assert_eq!(job.config.service_name, "myapp");
        assert_eq!(job.config.namespace, "production");
        assert_eq!(job.config.replicas, 3);
        assert_eq!(job.depends_on(), vec!["build_image".to_string()]);
    }

    #[test]
    fn test_health_check_path_override_default_is_none() {
        // By default there is no deploy-time override; only the standard
        // health_check_path ("/") is set.
        let job = DeployImageJobBuilder::new()
            .job_id("d".to_string())
            .build_job_id("build_image".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("app".to_string())
            .build(Arc::new(TrackingMockContainerDeployer::new()))
            .unwrap();

        assert_eq!(job.config.health_check_path_override, None);
        assert_eq!(job.config.health_check_path, Some("/".to_string()));
    }

    #[test]
    fn test_health_check_path_override_flows_to_config() {
        // An explicit deploy-time override is captured separately so it can win
        // over .temps.yaml at execution time.
        let job = DeployImageJobBuilder::new()
            .job_id("d".to_string())
            .build_job_id("build_image".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("app".to_string())
            .health_check_path_override(Some("/api/healthz".to_string()))
            .build(Arc::new(TrackingMockContainerDeployer::new()))
            .unwrap();

        assert_eq!(
            job.config.health_check_path_override,
            Some("/api/healthz".to_string())
        );
        // The default standard path is left untouched; precedence is resolved at
        // execution time (override > .temps.yaml > default).
        assert_eq!(job.config.health_check_path, Some("/".to_string()));
    }

    #[tokio::test]
    async fn test_multi_replica_deployment() {
        // This test verifies that DeployImageJob is configured to deploy multiple replicas
        // and that the configuration flows correctly through the system.
        //
        // Note: Full end-to-end execution is tested in integration tests since it requires
        // actual containers and health checks.

        let mock_deployer = Arc::new(TrackingMockContainerDeployer::new());
        let container_deployer: Arc<dyn ContainerDeployer> = mock_deployer.clone();

        let target = DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: Some(temps_core::NETWORK_NAME.to_string()),
        };

        // Create job with 2 replicas
        let job = DeployImageJobBuilder::new()
            .job_id("test_deploy".to_string())
            .build_job_id("build_image".to_string())
            .target(target)
            .service_name("myapp".to_string())
            .namespace("production".to_string())
            .replicas(2) // Deploy 2 replicas
            .port(3000)
            .build(container_deployer)
            .unwrap();

        // Verify job configuration
        assert_eq!(
            job.config.replicas, 2,
            "Job should be configured for 2 replicas"
        );
        assert_eq!(job.config.service_name, "myapp");
        assert_eq!(job.config.port, 3000);

        // Verify container naming for multi-replica deployment
        // Replica 1 should be named "myapp-1", replica 2 should be "myapp-2"
        // This is tested implicitly through the container deployment flow
    }

    #[test]
    fn test_image_output_from_context() {
        let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);

        // Set up outputs as the build job would
        context
            .set_output("build_image", "image_tag", "myapp:latest")
            .unwrap();
        context
            .set_output("build_image", "image_id", "sha256:abc123")
            .unwrap();
        context
            .set_output("build_image", "size_bytes", 104857600u64)
            .unwrap(); // 100MB
        context
            .set_output("build_image", "build_context", "/tmp/repo")
            .unwrap();
        context
            .set_output("build_image", "dockerfile_path", "/tmp/repo/Dockerfile")
            .unwrap();

        let image_output = BuildImageOutput::from_context(&context, "build_image").unwrap();
        assert_eq!(image_output.image_tag, "myapp:latest");
        assert_eq!(image_output.image_id, "sha256:abc123");
        assert_eq!(image_output.size_bytes, 104857600);
        assert_eq!(image_output.build_context, PathBuf::from("/tmp/repo"));
        assert_eq!(
            image_output.dockerfile_path,
            PathBuf::from("/tmp/repo/Dockerfile")
        );
    }

    #[tokio::test]
    async fn test_deployment_config_validation() {
        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());
        let target = DeploymentTarget::Docker {
            registry_url: "docker.io".to_string(),
            network: None,
        };

        let job = DeployImageJob::new(
            "test".to_string(),
            "build_job".to_string(),
            target,
            container_deployer,
        );

        let context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
        assert!(job.validate_deployment_config(&context).await.is_ok());
    }

    #[test]
    fn test_deploy_image_job_builder_with_node_scheduler() {
        use crate::services::{NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let job = DeployImageJobBuilder::new()
            .job_id("test_deploy".to_string())
            .build_job_id("build_image".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("myapp".to_string())
            .namespace("default".to_string())
            .replicas(2)
            .node_scheduler(scheduler)
            .target_nodes(vec![1, 3])
            .build(container_deployer)
            .unwrap();

        assert!(job.node_scheduler.is_some(), "Node scheduler should be set");
        assert_eq!(
            job.config.target_nodes,
            Some(vec![1, 3]),
            "Target nodes should be set"
        );
        assert_eq!(job.config.replicas, 2);
    }

    #[test]
    fn test_deploy_image_job_builder_without_node_scheduler() {
        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let job = DeployImageJobBuilder::new()
            .job_id("test_deploy".to_string())
            .build_job_id("build_image".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("myapp".to_string())
            .namespace("default".to_string())
            .replicas(3)
            .build(container_deployer)
            .unwrap();

        assert!(
            job.node_scheduler.is_none(),
            "Node scheduler should not be set when not provided"
        );
        assert_eq!(
            job.config.target_nodes, None,
            "Target nodes should be None by default"
        );
    }

    #[test]
    fn test_deployment_job_config_target_nodes_default() {
        let config = DeploymentJobConfig::default();
        assert_eq!(config.target_nodes, None);
    }

    /// Test that node scheduling produces correct assignments when integrated with DeployImageJob.
    /// We test the scheduling logic directly (not the full deploy flow which needs real containers).
    #[tokio::test]
    async fn test_node_scheduling_no_scheduler_returns_local_assignments() {
        // Without a node_scheduler, the deploy_image method creates Local assignments
        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let job = DeployImageJobBuilder::new()
            .job_id("test".to_string())
            .build_job_id("build".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("myapp".to_string())
            .namespace("default".to_string())
            .replicas(3)
            .build(container_deployer)
            .unwrap();

        // Verify no scheduler is set
        assert!(job.node_scheduler.is_none());
        // The deploy_image method will create vec![Local; 3] internally
    }

    /// Test that scheduler with no active nodes produces local assignments
    #[tokio::test]
    async fn test_node_scheduling_empty_nodes_returns_local() {
        use crate::services::{NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        // Schedule 3 replicas with no active nodes
        let assignments = scheduler
            .schedule_replicas(3, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 3);
        for a in &assignments {
            assert!(
                a.is_local(),
                "All assignments should be Local when no active nodes"
            );
        }
    }

    /// Test that scheduler distributes replicas across active nodes via round-robin
    #[tokio::test]
    async fn test_node_scheduling_round_robin_across_nodes() {
        use crate::services::{NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        fn make_node(id: i32, name: &str) -> nodes::Model {
            nodes::Model {
                id,
                name: name.to_string(),
                token_hash: format!("hash_{}", id),
                token_encrypted: None,
                address: format!("https://10.0.0.{}:3100", id),
                private_address: format!("10.0.0.{}", id),
                public_endpoint: None,
                wg_public_key: None,
                role: "worker".to_string(),
                status: "active".to_string(),
                labels: serde_json::json!({}),
                capacity: serde_json::json!({}),
                last_heartbeat: Some(chrono::Utc::now()),
                edge_public_key: None,
                compute_cidr: None,
                underlay_address: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }
        }

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                make_node(1, "worker-a"),
                make_node(2, "worker-b"),
            ]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        let assignments = scheduler
            .schedule_replicas(4, None, None, false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 4);

        // Pool is [Local, worker-a, worker-b] → round-robin: Local, A(1), B(2), Local
        assert!(assignments[0].is_local(), "First replica should be Local");
        assert_eq!(
            assignments[1].node_id(),
            Some(1),
            "Second should be worker-a"
        );
        assert_eq!(
            assignments[2].node_id(),
            Some(2),
            "Third should be worker-b"
        );
        assert!(
            assignments[3].is_local(),
            "Fourth should wrap back to Local"
        );
    }

    /// Test that target_nodes filters to only specified nodes
    #[tokio::test]
    async fn test_node_scheduling_with_target_nodes_filter() {
        use crate::services::{NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        fn make_node(id: i32, name: &str) -> nodes::Model {
            nodes::Model {
                id,
                name: name.to_string(),
                token_hash: format!("hash_{}", id),
                token_encrypted: None,
                address: format!("https://10.0.0.{}:3100", id),
                private_address: format!("10.0.0.{}", id),
                public_endpoint: None,
                wg_public_key: None,
                role: "worker".to_string(),
                status: "active".to_string(),
                labels: serde_json::json!({}),
                capacity: serde_json::json!({}),
                last_heartbeat: Some(chrono::Utc::now()),
                edge_public_key: None,
                compute_cidr: None,
                underlay_address: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }
        }

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                make_node(1, "worker-a"),
                make_node(2, "worker-b"),
                make_node(3, "worker-c"),
            ]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        // Target only nodes 1 and 3
        let target_ids = vec![1, 3];
        let assignments = scheduler
            .schedule_replicas(4, None, Some(&target_ids), false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 4);

        // Pool is [Local, worker-a(1), worker-c(3)] → round-robin includes Local
        for a in &assignments {
            match a {
                crate::services::NodeAssignment::Remote { node_id, .. } => {
                    assert!(
                        *node_id == 1 || *node_id == 3,
                        "Should only schedule on target nodes, got {}",
                        node_id
                    );
                }
                crate::services::NodeAssignment::Local => {
                    // Local (control plane) is always part of the pool
                }
            }
        }
    }

    /// Test that target_nodes with no matching active nodes falls back to local
    #[tokio::test]
    async fn test_node_scheduling_target_nodes_no_match_falls_back_to_local() {
        use crate::services::{NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        let node = nodes::Model {
            id: 1,
            name: "worker-1".to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: None,
            address: "https://10.0.0.1:3100".to_string(),
            private_address: "10.0.0.1".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(chrono::Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        // Target node 99 doesn't exist
        let target_ids = vec![99];
        let assignments = scheduler
            .schedule_replicas(2, None, Some(&target_ids), false)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 2);
        for a in &assignments {
            assert!(
                a.is_local(),
                "Should fall back to local when no target nodes match"
            );
        }
    }

    /// Test that DeployImageJob correctly passes target_nodes to scheduler
    #[tokio::test]
    async fn test_deploy_image_job_target_nodes_config_flows_to_scheduler() {
        use crate::services::{NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let job = DeployImageJobBuilder::new()
            .job_id("deploy".to_string())
            .build_job_id("build".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("app".to_string())
            .namespace("default".to_string())
            .replicas(2)
            .node_scheduler(scheduler)
            .target_nodes(vec![5, 10])
            .build(container_deployer)
            .unwrap();

        // Verify the config was set correctly
        assert_eq!(job.config.target_nodes, Some(vec![5, 10]));
        assert!(job.node_scheduler.is_some());

        // The target_nodes will be passed to scheduler.schedule_replicas
        // via self.config.target_nodes.as_deref() in deploy_image()
    }

    /// Test NodeAssignment accessor methods
    #[test]
    fn test_node_assignment_private_address() {
        use crate::services::NodeAssignment;

        let local = NodeAssignment::Local;
        assert!(local.private_address().is_none());

        let remote = NodeAssignment::Remote {
            node_id: 1,
            node_name: "w1".to_string(),
            address: "https://10.0.0.1:3100".to_string(),
            private_address: "10.0.0.1".to_string(),
        };
        assert_eq!(remote.private_address(), Some("10.0.0.1"));
        assert_eq!(remote.node_id(), Some(1));
        assert!(!remote.is_local());
    }

    /// Test get_node_token returns error for Local assignment
    #[tokio::test]
    async fn test_get_node_token_local_returns_error() {
        use crate::services::NodeAssignment;

        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let job = DeployImageJob::new(
            "test".to_string(),
            "build".to_string(),
            DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            },
            container_deployer,
        );

        let result = job.get_node_token(&NodeAssignment::Local).await;
        assert!(result.is_err(), "Should error for local assignment");
    }

    /// Test get_node_token returns error when no scheduler is set
    #[tokio::test]
    async fn test_get_node_token_no_scheduler_returns_error() {
        use crate::services::NodeAssignment;

        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let job = DeployImageJob::new(
            "test".to_string(),
            "build".to_string(),
            DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            },
            container_deployer,
        );

        let result = job
            .get_node_token(&NodeAssignment::Remote {
                node_id: 1,
                node_name: "worker-1".to_string(),
                address: "https://10.0.0.1:3100".to_string(),
                private_address: "10.0.0.1".to_string(),
            })
            .await;
        assert!(result.is_err(), "Should error when no scheduler available");
    }

    /// Test get_node_token decrypts the encrypted token from node service
    #[tokio::test]
    async fn test_get_node_token_success() {
        use crate::services::{NodeAssignment, NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        let enc_service = Arc::new(
            temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
        );
        let plaintext_token = "my-secret-agent-token";
        let encrypted = enc_service.encrypt(plaintext_token.as_bytes()).unwrap();

        let node = nodes::Model {
            id: 1,
            name: "worker-1".to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: Some(encrypted),
            address: "https://10.0.0.1:3100".to_string(),
            private_address: "10.0.0.1".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(chrono::Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let mut job = DeployImageJob::new(
            "test".to_string(),
            "build".to_string(),
            DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            },
            container_deployer,
        );
        job.node_scheduler = Some(scheduler);
        job.encryption_service = Some(enc_service);

        let result = job
            .get_node_token(&NodeAssignment::Remote {
                node_id: 1,
                node_name: "worker-1".to_string(),
                address: "https://10.0.0.1:3100".to_string(),
                private_address: "10.0.0.1".to_string(),
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), plaintext_token);
    }

    /// Test get_node_token fails when no encrypted token is stored
    #[tokio::test]
    async fn test_get_node_token_no_encrypted_token() {
        use crate::services::{NodeAssignment, NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        let node = nodes::Model {
            id: 1,
            name: "worker-1".to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: None,
            address: "https://10.0.0.1:3100".to_string(),
            private_address: "10.0.0.1".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(chrono::Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node]])
            .into_connection();
        let node_service = Arc::new(NodeService::new(Arc::new(db)));
        let scheduler = Arc::new(NodeScheduler::new(node_service));

        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());

        let enc_service = Arc::new(
            temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
        );

        let mut job = DeployImageJob::new(
            "test".to_string(),
            "build".to_string(),
            DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            },
            container_deployer,
        );
        job.node_scheduler = Some(scheduler);
        job.encryption_service = Some(enc_service);

        let result = job
            .get_node_token(&NodeAssignment::Remote {
                node_id: 1,
                node_name: "worker-1".to_string(),
                address: "https://10.0.0.1:3100".to_string(),
                private_address: "10.0.0.1".to_string(),
            })
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no encrypted token"),
            "Error should mention missing token: {}",
            err
        );
    }
}
