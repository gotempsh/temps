//! Deploy Image Job
//!
//! Deploys built container images to target environments

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::{
    ContainerDeployer, ContainerStatus as DeployerContainerStatus, DeployRequest, PortMapping,
    Protocol, ResourceLimits, RestartPolicy,
};
use temps_logs::{LogLevel, LogService};

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
    fn default() -> Self {
        Self {
            cpu_limit: Some("1000m".to_string()),
            memory_limit: Some("512Mi".to_string()),
            cpu_request: Some("100m".to_string()),
            memory_request: Some("128Mi".to_string()),
        }
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
    pub resources: ResourceUsage,
    pub health_check_path: Option<String>,
    pub ingress_enabled: bool,
    pub ingress_host: Option<String>,
}

impl Default for DeploymentJobConfig {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            service_name: "app".to_string(),
            replicas: 1,
            port: 8080,
            environment_variables: HashMap::new(),
            resources: ResourceUsage::default(),
            health_check_path: Some("/".to_string()),
            ingress_enabled: false,
            ingress_host: None,
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
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    /// Container IDs stored as soon as containers are created for cleanup on failure
    container_ids: Arc<Mutex<Vec<String>>>,
    /// Background task handle for log streaming (aborted on cleanup)
    log_stream_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Optional: directly provided image tag (for external/pre-built images, bypasses BuildImageJob lookup)
    external_image_tag: Option<String>,
}

impl std::fmt::Debug for DeployImageJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeployImageJob")
            .field("job_id", &self.job_id)
            .field("build_job_id", &self.build_job_id)
            .field("target", &self.target)
            .field("config", &self.config)
            .field("container_deployer", &"<ContainerDeployer>")
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
            log_id: None,
            log_service: None,
            container_ids: Arc::new(Mutex::new(Vec::new())),
            log_stream_task: Arc::new(Mutex::new(None)),
            external_image_tag: None,
        }
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

    pub fn with_external_image_tag(mut self, image_tag: String) -> Self {
        self.external_image_tag = Some(image_tag);
        self
    }

    /// Write log message to job-specific log file
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

        // Try to bind to port 0, which tells the OS to assign an available port
        let listener = TcpListener::bind("127.0.0.1:0")
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

                if let Err(e) = self.container_deployer.remove_container(container_id).await {
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

        // Deploy multiple replicas
        let mut all_container_ids = Vec::new();
        let mut all_host_ports = Vec::new();
        let mut deployment_error: Option<WorkflowError> = None;

        for replica_index in 0..self.config.replicas {
            self.log(
                context,
                format!(
                    "🚀 Deploying replica {}/{}...",
                    replica_index + 1,
                    self.config.replicas
                ),
            )
            .await?;

            match self
                .deploy_single_replica(image_output, context, replica_index)
                .await
            {
                Ok((container_id, host_port)) => {
                    all_container_ids.push(container_id);
                    all_host_ports.push(host_port);
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
            status: DeploymentStatus::Running, // All replicas deployed successfully
            replicas: all_container_ids.len() as u32,
            resources: self.config.resources.clone(),
            container_ids: all_container_ids,
            host_ports: all_host_ports,
        })
    }

    /// Deploy a single replica of the container
    async fn deploy_single_replica(
        &self,
        image_output: &BuildImageOutput,
        context: &WorkflowContext,
        replica_index: u32,
    ) -> Result<(String, u16), WorkflowError> {
        // Prepare deployment request using temps-deployer types
        self.log(context, "Deploying container image...".to_string())
            .await?;

        let log_path = std::env::temp_dir().join(format!("deploy_{}.log", self.job_id));

        // Determine the actual container port to expose
        // Priority: Image EXPOSE directive > configured port (from environment/project/default)
        let container_port = self
            .resolve_container_port(&image_output.image_tag, context)
            .await;

        // Allocate a random available port on the host
        let host_port = Self::find_available_port()?;
        self.log(
            context,
            format!(
                "🔌 Allocated host port: {} → container port: {}",
                host_port, container_port
            ),
        )
        .await?;

        let port_mappings = vec![PortMapping {
            host_port,
            container_port,
            protocol: Protocol::Tcp,
        }];

        let resource_limits = ResourceLimits {
            cpu_limit: self
                .config
                .resources
                .cpu_limit
                .as_ref()
                .and_then(|s| s.parse::<f64>().ok()),
            memory_limit_mb: self
                .config
                .resources
                .memory_limit
                .as_ref()
                .and_then(|s| s.trim_end_matches("Mi").parse::<u64>().ok()),
            disk_limit_mb: None,
        };

        // Use environment variables from config (PORT and HOST already included from workflow planner)
        let environment_vars = self.config.environment_variables.clone();

        tracing::debug!(
            "🌍 Deploying container with {} environment variables: {}",
            environment_vars.len(),
            environment_vars
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Create unique container name for each replica
        let container_name = if self.config.replicas > 1 {
            format!("{}-{}", self.config.service_name, replica_index + 1)
        } else {
            self.config.service_name.clone()
        };

        let deploy_request = DeployRequest {
            image_name: image_output.image_tag.clone(),
            container_name,
            environment_vars,
            port_mappings,
            network_name: None,
            resource_limits,
            restart_policy: RestartPolicy::Always,
            log_path,
            command: None,
        };

        let deploy_result = self
            .container_deployer
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
        let max_wait_time = std::time::Duration::from_secs(300); // 5 minutes
        let start_time = std::time::Instant::now();

        // Phase 1: Wait for container to be running
        loop {
            // Try to get container info, but don't fail hard if it can't be found
            // (container might have been removed by Docker or other processes)
            let container_info = match self
                .container_deployer
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
        // This runs in parallel with log streaming
        self.log(
            context,
            "Waiting for application to be ready...".to_string(),
        )
        .await?;
        let health_check_url = format!(
            "http://localhost:{}{}",
            host_port,
            self.config.health_check_path.as_deref().unwrap_or("/")
        );
        self.log(context, format!("Health check URL: {}", health_check_url))
            .await?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to create HTTP client: {}", e))
            })?;

        let mut consecutive_successes = 0;
        let required_successes = 2; // Require 2 consecutive successful connections

        loop {
            if start_time.elapsed() > max_wait_time {
                self.log(
                    context,
                    "⏱️  Application readiness timeout - connectivity checks failed".to_string(),
                )
                .await?;
                // Clean up container on connectivity timeout
                self.cleanup_container(context).await?;
                return Err(WorkflowError::JobExecutionFailed(
                    "Application timeout - connectivity checks did not pass in time".to_string(),
                ));
            }

            // Check if container is still running (it may have crashed)
            // This prevents waiting 5 minutes for a container that already exited
            if let Ok(container_info) = self
                .container_deployer
                .get_container_info(&deploy_result.container_id)
                .await
            {
                match container_info.status {
                    DeployerContainerStatus::Exited | DeployerContainerStatus::Dead => {
                        self.log(
                            context,
                            "❌ Container crashed during startup - application failed to start"
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
                    // Any response (including 404, 500, etc.) means the server is responding
                    consecutive_successes += 1;
                    self.log(
                        context,
                        format!(
                            "Connectivity check passed - server responding with status {} ({}/{})",
                            response.status(),
                            consecutive_successes,
                            required_successes
                        ),
                    )
                    .await?;

                    if consecutive_successes >= required_successes {
                        self.log(context, "Application is ready and responding!".to_string())
                            .await?;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => {
                    consecutive_successes = 0; // Reset counter on connection error
                    self.log(
                        context,
                        format!("Connectivity check failed ({}), retrying...", e),
                    )
                    .await?;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }

        let endpoint_url = format!("http://localhost:{}", deploy_result.host_port);
        self.log(
            context,
            format!("✅ Replica {} ready at {}", replica_index + 1, endpoint_url),
        )
        .await?;

        // Return container ID and host port
        Ok((deploy_result.container_id, deploy_result.host_port))
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

        // Deploy the image (logs written in real-time)
        let deployment_output = self.deploy_image(&image_output, &context).await?;

        // Set typed job outputs
        context.set_output(&self.job_id, "status", &deployment_output.status)?;
        context.set_output(&self.job_id, "replicas", deployment_output.replicas)?;
        context.set_output(
            &self.job_id,
            "container_ids",
            &deployment_output.container_ids,
        )?;
        context.set_output(&self.job_id, "host_ports", &deployment_output.host_ports)?;

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
                deployment_output.host_ports[0] as i32,
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
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl DeployImageJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            build_job_id: None,
            target: None,
            config: DeploymentJobConfig::default(),
            log_id: None,
            log_service: None,
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

    pub fn resources(mut self, resources: ResourceUsage) -> Self {
        self.config.resources = resources;
        self
    }

    pub fn ingress(mut self, enabled: bool, host: Option<String>) -> Self {
        self.config.ingress_enabled = enabled;
        self.config.ingress_host = host;
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

        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
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
}
