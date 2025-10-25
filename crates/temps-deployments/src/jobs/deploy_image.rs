//! Deploy Image Job
//!
//! Deploys built container images to target environments

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::{
    ContainerDeployer, ContainerStatus as DeployerContainerStatus, DeployRequest, PortMapping,
    Protocol, ResourceLimits, RestartPolicy,
};
use temps_logs::LogService;

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
    pub deployment_id: String,
    pub service_name: String,
    pub namespace: String,
    pub endpoint_url: Option<String>,
    pub status: DeploymentStatus,
    pub replicas: u32,
    pub resources: ResourceUsage,
    pub host_port: u16, // The external port exposed on the host
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

/// Configuration for deployment
#[derive(Debug, Clone)]
pub struct DeploymentConfig {
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

impl Default for DeploymentConfig {
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
    Kubernetes {
        cluster_name: String,
        kubeconfig_path: Option<String>,
    },
    Docker {
        registry_url: String,
        network: Option<String>,
    },
    CloudRun {
        project_id: String,
        region: String,
    },
}

/// Job for deploying container images to target environments
pub struct DeployImageJob {
    job_id: String,
    build_job_id: String,
    target: DeploymentTarget,
    config: DeploymentConfig,
    container_deployer: Arc<dyn ContainerDeployer>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
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
            config: DeploymentConfig::default(),
            container_deployer,
            log_id: None,
            log_service: None,
        }
    }

    pub fn with_config(mut self, config: DeploymentConfig) -> Self {
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

    /// Write log message to job-specific log file
    /// Write log message to both job-specific log file and context log writer
    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        // Write to job-specific log file
        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_to_log(log_id, &format!("{}\n", message))
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        // Also write to context log writer (for real-time streaming and test capture)
        context.log(&message).await?;
        Ok(())
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
                                format!("✅ Detected EXPOSE directive in image: port {}", port),
                            )
                            .await;
                        return port;
                    }
                    Ok(None) => {
                        let _ = self.log(
                            context,
                            format!("ℹ️  No EXPOSE directive found in image, using configured port: {}", self.config.port),
                        ).await;
                    }
                    Err(e) => {
                        let _ = self
                            .log(
                                context,
                                format!(
                                    "⚠️  Failed to inspect image: {}, using configured port: {}",
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
                            "⚠️  Failed to connect to Docker: {}, using configured port: {}",
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
    pub fn config(&self) -> &DeploymentConfig {
        &self.config
    }

    /// Public getter for target to allow test access
    pub fn target(&self) -> &DeploymentTarget {
        &self.target
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
                "🚀 Starting deployment of image: {}",
                image_output.image_tag
            ),
        )
        .await?;
        self.log(context, format!("🎯 Target: {:?}", self.target))
            .await?;
        self.log(
            context,
            format!(
                "⚙️  Service: {} in namespace: {}",
                self.config.service_name, self.config.namespace
            ),
        )
        .await?;

        // Pre-deployment validation
        self.log(
            context,
            "🔍 Validating deployment configuration...".to_string(),
        )
        .await?;
        self.validate_deployment_config(context).await?;

        // Prepare deployment request using temps-deployer types
        self.log(context, "📦 Deploying container image...".to_string())
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

        let deploy_request = DeployRequest {
            image_name: image_output.image_tag.clone(),
            container_name: self.config.service_name.clone(),
            environment_vars: self.config.environment_variables.clone(),
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

        self.log(
            context,
            format!("✅ Deployment created: {}", deploy_result.container_id),
        )
        .await?;

        // Wait for deployment to be ready (with timeout)
        self.log(context, "⏳ Waiting for container to start...".to_string())
            .await?;
        let max_wait_time = std::time::Duration::from_secs(300); // 5 minutes
        let start_time = std::time::Instant::now();

        // Phase 1: Wait for container to be running
        loop {
            let container_info = self
                .container_deployer
                .get_container_info(&deploy_result.container_id)
                .await
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to check deployment status: {}",
                        e
                    ))
                })?;

            match container_info.status {
                DeployerContainerStatus::Running => {
                    self.log(context, "✅ Container is running".to_string())
                        .await?;
                    break;
                }
                DeployerContainerStatus::Exited | DeployerContainerStatus::Dead => {
                    self.log(context, "❌ Container failed to start".to_string())
                        .await?;
                    return Err(WorkflowError::JobExecutionFailed(
                        "Container failed to start".to_string(),
                    ));
                }
                DeployerContainerStatus::Created => {
                    if start_time.elapsed() > max_wait_time {
                        self.log(context, "❌ Container start timeout".to_string())
                            .await?;
                        return Err(WorkflowError::JobExecutionFailed(
                            "Container timeout - took too long to start".to_string(),
                        ));
                    }
                    self.log(
                        context,
                        format!(
                            "⏳ Container status: {:?}, waiting...",
                            container_info.status
                        ),
                    )
                    .await?;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                _ => {
                    self.log(
                        context,
                        format!(
                            "⏳ Container status: {:?}, waiting...",
                            container_info.status
                        ),
                    )
                    .await?;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }

        // Phase 2: Wait for application to be ready (connectivity check)
        self.log(
            context,
            "⏳ Waiting for application to be ready...".to_string(),
        )
        .await?;
        let health_check_url = format!(
            "http://localhost:{}{}",
            host_port,
            self.config.health_check_path.as_deref().unwrap_or("/")
        );
        self.log(
            context,
            format!("🔍 Health check URL: {}", health_check_url),
        )
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
                self.log(context, "❌ Application readiness timeout".to_string())
                    .await?;
                return Err(WorkflowError::JobExecutionFailed(
                    "Application timeout - connectivity checks did not pass in time".to_string(),
                ));
            }

            match client.get(&health_check_url).send().await {
                Ok(response) => {
                    // Any response (including 404, 500, etc.) means the server is responding
                    consecutive_successes += 1;
                    self.log(
                        context,
                        format!(
                        "✅ Connectivity check passed - server responding with status {} ({}/{})",
                        response.status(),
                        consecutive_successes,
                        required_successes
                    ),
                    )
                    .await?;

                    if consecutive_successes >= required_successes {
                        self.log(
                            context,
                            "🎉 Application is ready and responding!".to_string(),
                        )
                        .await?;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => {
                    consecutive_successes = 0; // Reset counter on connection error
                    self.log(
                        context,
                        format!("⏳ Connectivity check failed ({}), retrying...", e),
                    )
                    .await?;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }

        let endpoint_url = Some(format!("http://localhost:{}", deploy_result.host_port));
        self.log(
            context,
            format!("🌐 Service endpoint: {}", endpoint_url.as_ref().unwrap()),
        )
        .await?;
        self.log(
            context,
            format!(
                "📊 Replicas: {}, Resources: {:?}",
                self.config.replicas, self.config.resources
            ),
        )
        .await?;

        // Convert deployer status to deployment status
        let status = match deploy_result.status {
            DeployerContainerStatus::Running => DeploymentStatus::Running,
            DeployerContainerStatus::Stopped => DeploymentStatus::Stopped,
            DeployerContainerStatus::Paused => DeploymentStatus::Pending,
            _ => DeploymentStatus::Failed,
        };

        Ok(DeploymentOutput {
            deployment_id: deploy_result.container_id,
            service_name: deploy_result.container_name,
            namespace: self.config.namespace.clone(),
            endpoint_url,
            status,
            replicas: self.config.replicas,
            resources: self.config.resources.clone(),
            host_port: deploy_result.host_port,
        })
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

        self.log(context, "✅ Deployment configuration is valid".to_string())
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
        vec![self.build_job_id.clone()]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Get typed output from the build job
        let image_output = BuildImageOutput::from_context(&context, &self.build_job_id)?;

        // Deploy the image (logs written in real-time)
        let deployment_output = self.deploy_image(&image_output, &context).await?;

        // Set typed job outputs
        context.set_output(
            &self.job_id,
            "deployment_id",
            &deployment_output.deployment_id,
        )?;
        context.set_output(
            &self.job_id,
            "service_name",
            &deployment_output.service_name,
        )?;
        context.set_output(&self.job_id, "namespace", &deployment_output.namespace)?;
        context.set_output(&self.job_id, "status", &deployment_output.status)?;
        context.set_output(&self.job_id, "replicas", deployment_output.replicas)?;
        if let Some(ref endpoint) = deployment_output.endpoint_url {
            context.set_output(&self.job_id, "endpoint_url", endpoint)?;
        }

        // Store container info for database update (deployment_id is container_id, service_name is container_name)
        context.set_output(
            &self.job_id,
            "container_id",
            &deployment_output.deployment_id,
        )?;
        context.set_output(
            &self.job_id,
            "container_name",
            &deployment_output.service_name,
        )?;
        context.set_output(
            &self.job_id,
            "container_port",
            deployment_output.host_port as i32,
        )?; // Store host port (external port)
        context.set_output(&self.job_id, "host_port", deployment_output.host_port)?; // Also set as host_port for other jobs

        // Set artifacts
        context.set_artifact(
            &self.job_id,
            "deployment",
            PathBuf::from(&deployment_output.deployment_id),
        );

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Verify that the build job output is available
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
        // Get deployment ID from outputs if available
        if let Ok(Some(deployment_id)) = context.get_output::<String>(&self.job_id, "deployment_id")
        {
            // Note: In a real implementation, you might want to conditionally clean up
            // based on deployment lifecycle configuration (e.g., cleanup on failure only)
            if let Err(e) = self
                .container_deployer
                .remove_container(&deployment_id)
                .await
            {
                // Log but don't fail cleanup
                eprintln!(
                    "Warning: Failed to cleanup deployment {}: {}",
                    deployment_id, e
                );
            }
        }
        Ok(())
    }
}

/// Builder for DeployImageJob
pub struct DeployImageJobBuilder {
    job_id: Option<String>,
    build_job_id: Option<String>,
    target: Option<DeploymentTarget>,
    config: DeploymentConfig,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl DeployImageJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            build_job_id: None,
            target: None,
            config: DeploymentConfig::default(),
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
        ContainerDeployer, ContainerInfo, ContainerStatus as DeployerContainerStatus,
        DeployRequest, DeployResult, DeployerError,
    };

    // Mock ContainerDeployer for testing
    struct MockContainerDeployer;

    #[async_trait]
    impl ContainerDeployer for MockContainerDeployer {
        async fn deploy_container(
            &self,
            request: DeployRequest,
        ) -> Result<DeployResult, DeployerError> {
            Ok(DeployResult {
                container_id: "test_container_123".to_string(),
                container_name: request.container_name,
                container_port: 8080,
                host_port: 8080,
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
        let container_deployer: Arc<dyn ContainerDeployer> = Arc::new(MockContainerDeployer);
        let target = DeploymentTarget::Kubernetes {
            cluster_name: "test-cluster".to_string(),
            kubeconfig_path: None,
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
        let container_deployer: Arc<dyn ContainerDeployer> = Arc::new(MockContainerDeployer);
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
