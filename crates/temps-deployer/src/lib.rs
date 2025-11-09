//! Temps Deployer - Abstract container building and deployment
//!
//! This crate provides a unified interface for:
//! - Building OCI images from Dockerfiles
//! - Deploying containers to various runtimes
//! - Managing container lifecycle (start, stop, pause, etc.)
//! - Extracting files from images
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use temps_core::UtcDateTime;
use thiserror::Error;

/// Callback function type for processing build logs in real-time
pub type LogCallback =
    std::sync::Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

pub mod docker;
pub mod plugin;
pub mod static_deployer;

#[derive(Error, Debug)]
pub enum BuilderError {
    #[error("Build failed: {0}")]
    BuildFailed(String),

    #[error("Build cancelled by user")]
    BuildCancelled,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Invalid context: {0}")]
    InvalidContext(String),

    #[error("Missing dockerfile: {0}")]
    MissingDockerfile(String),

    #[error("Resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),

    #[error("Other error: {0}")]
    Other(String),
}

#[derive(Error, Debug)]
pub enum DeployerError {
    #[error("Deployment failed: {0}")]
    DeploymentFailed(String),

    #[error("Container not found: {0}")]
    ContainerNotFound(String),

    #[error("Image not found: {0}")]
    ImageNotFound(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Resource allocation failed: {0}")]
    ResourceAllocationFailed(String),

    #[error("Other error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRequest {
    pub image_name: String,
    pub context_path: PathBuf,
    pub dockerfile_path: Option<PathBuf>,
    pub build_args: HashMap<String, String>,
    pub build_args_buildkit: HashMap<String, String>,
    pub platform: Option<String>,
    pub log_path: PathBuf,
}

/// Build request with optional log callback for real-time log streaming
pub struct BuildRequestWithCallback {
    pub request: BuildRequest,
    pub log_callback: Option<LogCallback>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub image_id: String,
    pub image_name: String,
    pub size_bytes: u64,
    pub build_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRequest {
    pub image_name: String,
    pub container_name: String,
    pub environment_vars: HashMap<String, String>,
    pub port_mappings: Vec<PortMapping>,
    pub network_name: Option<String>,
    pub resource_limits: ResourceLimits,
    pub restart_policy: RestartPolicy,
    pub log_path: PathBuf,
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: Protocol,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_limit: Option<f64>, // CPU cores
    pub memory_limit_mb: Option<u64>,
    pub disk_limit_mb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RestartPolicy {
    Never,
    Always,
    OnFailure,
    UnlessStopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployResult {
    pub container_id: String,
    pub container_name: String,
    pub container_port: u16,
    pub host_port: u16,
    pub status: ContainerStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContainerStatus {
    Created,
    Running,
    Paused,
    Stopped,
    Exited,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub container_id: String,
    pub container_name: String,
    pub image_name: String,
    pub status: ContainerStatus,
    pub created_at: UtcDateTime,
    pub ports: Vec<PortMapping>,
    pub environment_vars: HashMap<String, String>,
}

/// Container performance statistics (CPU, memory, network)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerStats {
    pub container_id: String,
    pub container_name: String,
    /// CPU usage percentage (0-100)
    pub cpu_percent: f64,
    /// Memory usage in bytes
    pub memory_bytes: u64,
    /// Memory limit in bytes (if set)
    pub memory_limit_bytes: Option<u64>,
    /// Memory usage percentage (0-100) if limit is set
    pub memory_percent: Option<f64>,
    /// Network bytes received
    pub network_rx_bytes: u64,
    /// Network bytes transmitted
    pub network_tx_bytes: u64,
    /// Timestamp of metrics collection
    pub timestamp: UtcDateTime,
}

/// Configuration for stopping containers
#[derive(Debug, Clone)]
pub struct ContainerStopSpec {
    /// Container identifier (ID or name)
    pub identifier: String,
    /// Whether to remove the container after stopping
    pub remove_after_stop: bool,
    /// Whether to fail the entire operation if this container fails to stop
    pub fail_on_error: bool,
}

impl ContainerStopSpec {
    pub fn new(identifier: String) -> Self {
        Self {
            identifier,
            remove_after_stop: false,
            fail_on_error: true,
        }
    }

    pub fn with_removal(mut self) -> Self {
        self.remove_after_stop = true;
        self
    }

    pub fn allow_failure(mut self) -> Self {
        self.fail_on_error = false;
        self
    }
}

/// Configuration for launching containers
#[derive(Debug, Clone)]
pub struct ContainerLaunchSpec {
    /// Docker image name to deploy
    pub image_name: String,
    /// Container name (optional)
    pub container_name: Option<String>,
    /// Environment variables
    pub environment_variables: Vec<(String, String)>,
    /// Number of replicas
    pub replicas: Option<i32>,
    /// CPU request in millicores
    pub cpu_request: Option<i64>,
    /// CPU limit in millicores
    pub cpu_limit: Option<i64>,
    /// Memory request in MB
    pub memory_request: Option<i64>,
    /// Memory limit in MB
    pub memory_limit: Option<i64>,
    /// Port mappings (host_port, container_port)
    pub port_mappings: Option<Vec<(u16, u16)>>,
    /// Whether to fail the entire operation if this container fails to launch
    pub fail_on_error: bool,
}

impl ContainerLaunchSpec {
    pub fn new(image_name: String) -> Self {
        Self {
            image_name,
            container_name: None,
            environment_variables: Vec::new(),
            replicas: Some(1),
            cpu_request: None,
            cpu_limit: None,
            memory_request: None,
            memory_limit: None,
            port_mappings: None,
            fail_on_error: true,
        }
    }

    pub fn with_name(mut self, name: String) -> Self {
        self.container_name = Some(name);
        self
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.environment_variables = env;
        self
    }

    pub fn with_replicas(mut self, replicas: i32) -> Self {
        self.replicas = Some(replicas);
        self
    }

    pub fn with_resources(
        mut self,
        cpu_request: Option<i64>,
        cpu_limit: Option<i64>,
        memory_request: Option<i64>,
        memory_limit: Option<i64>,
    ) -> Self {
        self.cpu_request = cpu_request;
        self.cpu_limit = cpu_limit;
        self.memory_request = memory_request;
        self.memory_limit = memory_limit;
        self
    }

    pub fn with_ports(mut self, ports: Vec<(u16, u16)>) -> Self {
        self.port_mappings = Some(ports);
        self
    }

    pub fn allow_failure(mut self) -> Self {
        self.fail_on_error = false;
        self
    }
}

/// Trait for building OCI images from source code and Dockerfiles
#[async_trait]
pub trait ImageBuilder: Send + Sync {
    /// Build an OCI image from a Dockerfile and context
    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError>;

    /// Build an OCI image with real-time log callback
    async fn build_image_with_callback(
        &self,
        request: BuildRequestWithCallback,
    ) -> Result<BuildResult, BuilderError>;

    /// Import an image from a tar archive
    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError>;

    /// Extract files from an image to a destination path
    async fn extract_from_image(
        &self,
        image_name: &str,
        source_path: &str,
        destination_path: &Path,
    ) -> Result<(), BuilderError>;

    /// List available images
    async fn list_images(&self) -> Result<Vec<String>, BuilderError>;

    /// Remove an image
    async fn remove_image(&self, image_name: &str) -> Result<(), BuilderError>;
}

/// Trait for deploying and managing containers
#[async_trait]
pub trait ContainerDeployer: Send + Sync {
    /// Deploy a container from an image
    async fn deploy_container(&self, request: DeployRequest)
        -> Result<DeployResult, DeployerError>;

    /// Start a stopped container
    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError>;

    /// Stop a running container
    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError>;

    /// Pause a running container
    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError>;

    /// Resume a paused container
    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError>;

    /// Remove a container
    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError>;

    /// Get container information
    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError>;

    /// Get container performance metrics (CPU, memory, network)
    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<ContainerStats, DeployerError>;

    /// List running containers
    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError>;

    /// Get container logs
    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError>;

    /// Stream container logs
    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError>;
}

/// Combined trait for both building and deploying
#[async_trait]
pub trait ContainerRuntime: ImageBuilder + ContainerDeployer + Send + Sync {
    /// Get runtime information (Docker version, available resources, etc.)
    async fn get_runtime_info(&self) -> Result<RuntimeInfo, DeployerError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub runtime_type: String,
    pub version: String,
    pub available_cpu_cores: usize,
    pub available_memory_mb: u64,
    pub available_disk_mb: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_limit: Some(1.0),       // 1 CPU core
            memory_limit_mb: Some(512), // 512 MB
            disk_limit_mb: Some(1024),  // 1 GB
        }
    }
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::OnFailure
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Tcp => write!(f, "tcp"),
            Protocol::Udp => write!(f, "udp"),
        }
    }
}

impl std::fmt::Display for ContainerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerStatus::Created => write!(f, "created"),
            ContainerStatus::Running => write!(f, "running"),
            ContainerStatus::Paused => write!(f, "paused"),
            ContainerStatus::Stopped => write!(f, "stopped"),
            ContainerStatus::Exited => write!(f, "exited"),
            ContainerStatus::Dead => write!(f, "dead"),
        }
    }
}

impl std::fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestartPolicy::Never => write!(f, "no"),
            RestartPolicy::Always => write!(f, "always"),
            RestartPolicy::OnFailure => write!(f, "on-failure"),
            RestartPolicy::UnlessStopped => write!(f, "unless-stopped"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_build_request_creation() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().to_path_buf();
        let log_path = temp_dir.path().join("build.log");

        let mut build_args = HashMap::new();
        build_args.insert("ENV".to_string(), "production".to_string());

        let request = BuildRequest {
            image_name: "test-image:latest".to_string(),
            context_path: context_path.clone(),
            dockerfile_path: Some(context_path.join("Dockerfile")),
            build_args: build_args.clone(),
            build_args_buildkit: build_args.clone(),
            platform: Some("linux/amd64".to_string()),
            log_path,
        };

        assert_eq!(request.image_name, "test-image:latest");
        assert_eq!(request.context_path, context_path);
        assert!(request.dockerfile_path.is_some());
        assert_eq!(request.build_args.get("ENV").unwrap(), "production");
        assert_eq!(request.platform.as_ref().unwrap(), "linux/amd64");
    }

    #[test]
    fn test_deploy_request_creation() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("deploy.log");

        let mut env_vars = HashMap::new();
        env_vars.insert("PORT".to_string(), "3000".to_string());

        let port_mappings = vec![PortMapping {
            host_port: 8080,
            container_port: 3000,
            protocol: Protocol::Tcp,
        }];

        let request = DeployRequest {
            image_name: "test-image:latest".to_string(),
            container_name: "test-container".to_string(),
            environment_vars: env_vars,
            port_mappings,
            network_name: Some("test-network".to_string()),
            resource_limits: ResourceLimits::default(),
            restart_policy: RestartPolicy::Always,
            log_path,
            command: Some(vec!["node".to_string(), "server.js".to_string()]),
        };

        assert_eq!(request.image_name, "test-image:latest");
        assert_eq!(request.container_name, "test-container");
        assert_eq!(request.environment_vars.get("PORT").unwrap(), "3000");
        assert_eq!(request.port_mappings.len(), 1);
        assert_eq!(request.port_mappings[0].host_port, 8080);
        assert_eq!(request.port_mappings[0].container_port, 3000);
        assert!(matches!(request.port_mappings[0].protocol, Protocol::Tcp));
        assert_eq!(request.network_name.as_ref().unwrap(), "test-network");
        assert_eq!(request.command.as_ref().unwrap().len(), 2);
        assert_eq!(request.command.as_ref().unwrap()[0], "node");
        assert_eq!(request.command.as_ref().unwrap()[1], "server.js");
    }

    #[test]
    fn test_resource_limits_default() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.cpu_limit, Some(1.0));
        assert_eq!(limits.memory_limit_mb, Some(512));
        assert_eq!(limits.disk_limit_mb, Some(1024));
    }

    #[test]
    fn test_restart_policy_display() {
        assert_eq!(RestartPolicy::Never.to_string(), "no");
        assert_eq!(RestartPolicy::Always.to_string(), "always");
        assert_eq!(RestartPolicy::OnFailure.to_string(), "on-failure");
        assert_eq!(RestartPolicy::UnlessStopped.to_string(), "unless-stopped");
    }

    #[test]
    fn test_protocol_display() {
        assert_eq!(Protocol::Tcp.to_string(), "tcp");
        assert_eq!(Protocol::Udp.to_string(), "udp");
    }

    #[test]
    fn test_container_status_display() {
        assert_eq!(ContainerStatus::Created.to_string(), "created");
        assert_eq!(ContainerStatus::Running.to_string(), "running");
        assert_eq!(ContainerStatus::Paused.to_string(), "paused");
        assert_eq!(ContainerStatus::Stopped.to_string(), "stopped");
        assert_eq!(ContainerStatus::Exited.to_string(), "exited");
        assert_eq!(ContainerStatus::Dead.to_string(), "dead");
    }

    #[test]
    fn test_port_mapping() {
        let mapping = PortMapping {
            host_port: 8080,
            container_port: 80,
            protocol: Protocol::Tcp,
        };

        assert_eq!(mapping.host_port, 8080);
        assert_eq!(mapping.container_port, 80);
        assert!(matches!(mapping.protocol, Protocol::Tcp));
    }

    #[test]
    fn test_container_info_creation() {
        let now = chrono::Utc::now();
        let mut env_vars = HashMap::new();
        env_vars.insert("APP_ENV".to_string(), "test".to_string());

        let info = ContainerInfo {
            container_id: "abc123".to_string(),
            container_name: "test-container".to_string(),
            image_name: "test-image:latest".to_string(),
            status: ContainerStatus::Running,
            created_at: now,
            ports: vec![PortMapping {
                host_port: 8080,
                container_port: 3000,
                protocol: Protocol::Tcp,
            }],
            environment_vars: env_vars,
        };

        assert_eq!(info.container_id, "abc123");
        assert_eq!(info.container_name, "test-container");
        assert_eq!(info.image_name, "test-image:latest");
        assert!(matches!(info.status, ContainerStatus::Running));
        assert_eq!(info.created_at, now);
        assert_eq!(info.ports.len(), 1);
        assert_eq!(info.environment_vars.get("APP_ENV").unwrap(), "test");
    }

    #[test]
    fn test_build_result() {
        let result = BuildResult {
            image_id: "sha256:abc123".to_string(),
            image_name: "test-image:latest".to_string(),
            size_bytes: 1024 * 1024 * 100, // 100MB
            build_duration_ms: 5000,
        };

        assert_eq!(result.image_id, "sha256:abc123");
        assert_eq!(result.image_name, "test-image:latest");
        assert_eq!(result.size_bytes, 104_857_600);
        assert_eq!(result.build_duration_ms, 5000);
    }

    #[test]
    fn test_deploy_result() {
        let result = DeployResult {
            container_id: "xyz789".to_string(),
            container_name: "test-container".to_string(),
            container_port: 3000,
            host_port: 8080,
            status: ContainerStatus::Running,
        };

        assert_eq!(result.container_id, "xyz789");
        assert_eq!(result.container_name, "test-container");
        assert_eq!(result.container_port, 3000);
        assert_eq!(result.host_port, 8080);
        assert!(matches!(result.status, ContainerStatus::Running));
    }

    #[test]
    fn test_runtime_info() {
        let info = RuntimeInfo {
            runtime_type: "Docker".to_string(),
            version: "20.10.7".to_string(),
            available_cpu_cores: 8,
            available_memory_mb: 16384,
            available_disk_mb: 102400,
        };

        assert_eq!(info.runtime_type, "Docker");
        assert_eq!(info.version, "20.10.7");
        assert_eq!(info.available_cpu_cores, 8);
        assert_eq!(info.available_memory_mb, 16384);
        assert_eq!(info.available_disk_mb, 102400);
    }

    #[test]
    fn test_builder_error_types() {
        let build_failed = BuilderError::BuildFailed("Build error".to_string());
        let io_error = BuilderError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "File not found",
        ));
        let invalid_context = BuilderError::InvalidContext("Invalid context".to_string());

        assert!(matches!(build_failed, BuilderError::BuildFailed(_)));
        assert!(matches!(io_error, BuilderError::IoError(_)));
        assert!(matches!(invalid_context, BuilderError::InvalidContext(_)));
    }

    #[test]
    fn test_deployer_error_types() {
        let deploy_failed = DeployerError::DeploymentFailed("Deploy error".to_string());
        let container_not_found = DeployerError::ContainerNotFound("Container missing".to_string());
        let network_error = DeployerError::NetworkError("Network issue".to_string());

        assert!(matches!(deploy_failed, DeployerError::DeploymentFailed(_)));
        assert!(matches!(
            container_not_found,
            DeployerError::ContainerNotFound(_)
        ));
        assert!(matches!(network_error, DeployerError::NetworkError(_)));
    }

    #[test]
    fn test_serde_serialization() {
        let request = BuildRequest {
            image_name: "test:latest".to_string(),
            context_path: PathBuf::from("/tmp/build"),
            dockerfile_path: None,
            build_args: HashMap::new(),
            build_args_buildkit: HashMap::new(),
            platform: None,
            log_path: PathBuf::from("/tmp/build.log"),
        };

        // Test serialization
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("test:latest"));

        // Test deserialization
        let deserialized: BuildRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.image_name, "test:latest");
        assert_eq!(deserialized.context_path, PathBuf::from("/tmp/build"));
    }

    #[test]
    fn test_resource_limits_custom() {
        let limits = ResourceLimits {
            cpu_limit: Some(2.5),
            memory_limit_mb: Some(1024),
            disk_limit_mb: Some(2048),
        };

        assert_eq!(limits.cpu_limit, Some(2.5));
        assert_eq!(limits.memory_limit_mb, Some(1024));
        assert_eq!(limits.disk_limit_mb, Some(2048));

        // Test with None values
        let no_limits = ResourceLimits {
            cpu_limit: None,
            memory_limit_mb: None,
            disk_limit_mb: None,
        };

        assert!(no_limits.cpu_limit.is_none());
        assert!(no_limits.memory_limit_mb.is_none());
        assert!(no_limits.disk_limit_mb.is_none());
    }

    #[test]
    fn test_build_request_with_real_files() {
        let temp_dir = TempDir::new().unwrap();
        let dockerfile_content = r#"
FROM alpine:latest
RUN echo "Hello World"
COPY . /app
WORKDIR /app
CMD ["echo", "Hello from container"]
"#;

        // Create a real Dockerfile
        let dockerfile_path = temp_dir.path().join("Dockerfile");
        fs::write(&dockerfile_path, dockerfile_content).unwrap();

        // Create some source files
        let src_dir = temp_dir.path().join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(
            src_dir.join("main.rs"),
            "fn main() { println!(\"Hello\"); }",
        )
        .unwrap();

        let mut build_args = HashMap::new();
        build_args.insert("RUST_VERSION".to_string(), "1.70".to_string());

        let request = BuildRequest {
            image_name: "test-app:v1.0".to_string(),
            context_path: temp_dir.path().to_path_buf(),
            dockerfile_path: Some(dockerfile_path.clone()),
            build_args: build_args.clone(),
            build_args_buildkit: build_args.clone(),
            platform: Some("linux/amd64".to_string()),
            log_path: temp_dir.path().join("build.log"),
        };

        assert!(request.dockerfile_path.as_ref().unwrap().exists());
        assert!(request.context_path.join("src/main.rs").exists());
        assert_eq!(request.build_args.get("RUST_VERSION").unwrap(), "1.70");
    }

    #[test]
    fn test_multiple_port_mappings() {
        let port_mappings = [
            PortMapping {
                host_port: 8080,
                container_port: 80,
                protocol: Protocol::Tcp,
            },
            PortMapping {
                host_port: 8443,
                container_port: 443,
                protocol: Protocol::Tcp,
            },
            PortMapping {
                host_port: 9090,
                container_port: 9090,
                protocol: Protocol::Udp,
            },
        ];

        assert_eq!(port_mappings.len(), 3);
        assert_eq!(port_mappings[0].host_port, 8080);
        assert_eq!(port_mappings[1].container_port, 443);
        assert!(matches!(port_mappings[2].protocol, Protocol::Udp));
    }

    #[test]
    fn test_environment_variables() {
        let mut env_vars = HashMap::new();
        env_vars.insert("NODE_ENV".to_string(), "production".to_string());
        env_vars.insert("PORT".to_string(), "3000".to_string());
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgres://localhost/mydb".to_string(),
        );

        let temp_dir = TempDir::new().unwrap();
        let request = DeployRequest {
            image_name: "node-app:latest".to_string(),
            container_name: "web-server".to_string(),
            environment_vars: env_vars.clone(),
            port_mappings: vec![],
            network_name: None,
            resource_limits: ResourceLimits::default(),
            restart_policy: RestartPolicy::Always,
            log_path: temp_dir.path().join("deploy.log"),
            command: None, // No custom command, use default from image
        };

        assert_eq!(request.environment_vars.len(), 3);
        assert_eq!(
            request.environment_vars.get("NODE_ENV").unwrap(),
            "production"
        );
        assert_eq!(request.environment_vars.get("PORT").unwrap(), "3000");
        assert!(request.environment_vars.contains_key("DATABASE_URL"));
    }

    #[test]
    fn test_error_display_messages() {
        let build_error = BuilderError::BuildFailed("Docker build failed".to_string());
        let deploy_error = DeployerError::DeploymentFailed("Container start failed".to_string());

        assert_eq!(build_error.to_string(), "Build failed: Docker build failed");
        assert_eq!(
            deploy_error.to_string(),
            "Deployment failed: Container start failed"
        );
    }

    #[test]
    fn test_comprehensive_type_validation() {
        // Test all our public types can be created and used

        // Test ResourceLimits
        let limits = ResourceLimits {
            cpu_limit: Some(2.0),
            memory_limit_mb: Some(1024),
            disk_limit_mb: Some(2048),
        };
        assert!(limits.cpu_limit.unwrap() > 0.0);

        // Test PortMapping
        let port_mapping = PortMapping {
            host_port: 8080,
            container_port: 3000,
            protocol: Protocol::Tcp,
        };
        assert_eq!(port_mapping.host_port, 8080);

        // Test RestartPolicy variants
        let policies = [
            RestartPolicy::Never,
            RestartPolicy::Always,
            RestartPolicy::OnFailure,
            RestartPolicy::UnlessStopped,
        ];
        assert_eq!(policies.len(), 4);

        // Test ContainerStatus variants
        let statuses = [
            ContainerStatus::Created,
            ContainerStatus::Running,
            ContainerStatus::Paused,
            ContainerStatus::Exited,
            ContainerStatus::Dead,
            ContainerStatus::Stopped,
        ];
        assert_eq!(statuses.len(), 6);

        println!("✅ All type validation tests passed");
    }

    #[test]
    fn test_serde_compatibility() {
        // Test that our types can be serialized/deserialized if needed
        use serde_json;

        let limits = ResourceLimits {
            cpu_limit: Some(1.5),
            memory_limit_mb: Some(512),
            disk_limit_mb: Some(1024),
        };

        // Test serialization
        let serialized = serde_json::to_string(&limits).unwrap();
        assert!(!serialized.is_empty());

        // Test deserialization
        let deserialized: ResourceLimits = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.cpu_limit, limits.cpu_limit);
        assert_eq!(deserialized.memory_limit_mb, limits.memory_limit_mb);

        println!("✅ Serde compatibility test passed");
    }
}
