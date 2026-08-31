// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// Backpressure-aware byte stream used when importing an OCI image.
///
/// Keeping this at the trait boundary lets remote agents forward an upload
/// directly to Docker instead of first materializing the complete archive in
/// a temporary file (and charging that file's page cache to the agent cgroup).
pub type ImageImportStream =
    Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>>;

pub mod compose;

/// Callback function type for processing build logs in real-time
pub type LogCallback =
    std::sync::Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

pub mod docker;
pub mod metadata_egress;
pub mod platform;
pub mod plugin;
pub mod readiness;
pub mod remote;
pub mod static_deployer;
pub(crate) mod static_ingestion;

pub use platform::{
    canonicalize_platform, is_buildable_platform, native_platform, normalize_arch,
    normalize_platform, platform_arch, platform_tag_suffix, platforms_match, tag_for_platform,
};

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

    #[error("Platform mismatch: image is for {image_platform}, but target platform is {target_platform}")]
    PlatformMismatch {
        image_platform: String,
        target_platform: String,
    },

    #[error("Image not found: {0}")]
    ImageNotFound(String),

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

    #[error("Failed to mount secrets for container '{container_name}': {reason}")]
    SecretMountFailed {
        container_name: String,
        reason: String,
    },

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

/// Information about a Docker image, including architecture and platform details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    /// Image ID (sha256:...)
    pub id: String,
    /// Image architecture (e.g., "amd64", "arm64")
    pub architecture: String,
    /// Operating system (e.g., "linux")
    pub os: String,
    /// Full platform string (e.g., "linux/amd64")
    pub platform: String,
    /// Image size in bytes
    pub size_bytes: u64,
    /// Image tags
    pub tags: Vec<String>,
    /// Creation timestamp
    pub created: Option<String>,
    /// Working directory (WORKDIR from Dockerfile)
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DeployRequest {
    pub image_name: String,
    pub container_name: String,
    pub environment_vars: HashMap<String, String>,
    /// Secret values (plaintext, already decrypted by the caller) to mount as
    /// files under `/run/secrets/<KEY>` inside the container, and never
    /// injected as environment variables — so they do not appear in
    /// `docker inspect`. Per-secret plaintext must be <= 1 MiB (enforced
    /// upstream in `SecretService`).
    ///
    /// Delivery is a read-only bind mount of a per-container directory under
    /// `$TEMPS_DATA_DIR/secrets`, each file mode 0400 and chowned to the uid
    /// resolved from the image's `USER`. **Not** a tmpfs: `/tmp` is tmpfs on
    /// most distributions, so a tmpfs-backed mount would point at an empty
    /// directory after a host reboot and every container would come back with
    /// no secrets until someone redeployed. The tradeoff is that plaintext
    /// lives on host disk for the container's lifetime; the threat this
    /// addresses is read access to the Docker API, not host root.
    #[serde(default)]
    pub secrets: HashMap<String, String>,
    pub port_mappings: Vec<PortMapping>,
    pub network_name: Option<String>,
    /// Additional Docker networks that this container must join before it
    /// starts. Use this for host-local dependency networks, such as a
    /// self-hosted database Compose network, where a successful deploy is not
    /// useful unless service DNS is available at application boot.
    #[serde(default)]
    pub extra_networks: Vec<String>,
    pub resource_limits: ResourceLimits,
    pub restart_policy: RestartPolicy,
    #[schema(value_type = String)]
    pub log_path: PathBuf,
    pub command: Option<Vec<String>>,
    /// Docker log rotation config (max-size, max-file). If None, uses Docker daemon defaults.
    pub log_config: Option<ContainerLogConfig>,
    /// Docker labels to apply to the container.
    /// The log aggregator expects `sh.temps.project_id`, `sh.temps.environment`,
    /// `sh.temps.service`, and optionally `sh.temps.deploy_id`.
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

/// Docker container log rotation configuration
/// Applied via Docker's `--log-opt` to prevent unbounded log growth
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContainerLogConfig {
    /// Maximum size of each log file (e.g., "50m", "100m", "1g")
    pub max_size: String,
    /// Maximum number of rotated log files to keep
    pub max_file: u32,
}

impl ContainerLogConfig {
    /// Default log config for application containers (50MB x 3 = 150MB max)
    pub fn app_default() -> Self {
        Self {
            max_size: "50m".to_string(),
            max_file: 3,
        }
    }

    /// Default log config for external service containers (20MB x 3 = 60MB max)
    pub fn service_default() -> Self {
        Self {
            max_size: "20m".to_string(),
            max_file: 3,
        }
    }

    /// Create from settings values
    pub fn new(max_size: String, max_file: u32) -> Self {
        Self { max_size, max_file }
    }

    /// Convert to Bollard's HostConfigLogConfig
    pub fn to_bollard_log_config(&self) -> bollard::models::HostConfigLogConfig {
        let mut config = HashMap::new();
        config.insert("max-size".to_string(), self.max_size.clone());
        config.insert("max-file".to_string(), self.max_file.to_string());

        bollard::models::HostConfigLogConfig {
            typ: Some("json-file".to_string()),
            config: Some(config),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: Protocol,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResourceLimits {
    pub cpu_limit: Option<f64>, // CPU cores
    pub memory_limit_mb: Option<u64>,
    pub disk_limit_mb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema)]
pub enum RestartPolicy {
    Never,
    Always,
    #[default]
    OnFailure,
    UnlessStopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DeployResult {
    pub container_id: String,
    pub container_name: String,
    pub container_port: u16,
    pub host_port: u16,
    pub status: ContainerStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum ContainerStatus {
    Created,
    Running,
    Paused,
    Stopped,
    Exited,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContainerInfo {
    pub container_id: String,
    pub container_name: String,
    pub image_name: String,
    pub status: ContainerStatus,
    #[schema(value_type = String, example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: UtcDateTime,
    pub ports: Vec<PortMapping>,
    pub environment_vars: HashMap<String, String>,
    pub restart_count: Option<i64>,
    /// Docker labels set on the container (e.g., `sh.temps.managed`, `sh.temps.project_id`).
    #[serde(default)]
    pub labels: HashMap<String, String>,
    /// Process exit code reported by Docker. None while the container is still running.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exit_code: Option<i32>,
    /// Human-readable reason the container exited (e.g. "OOMKilled",
    /// "Signal SIGKILL (9)", "Exit code 137"). None while still running.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exit_reason: Option<String>,
    /// True when Docker's OOM killer terminated the container.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub oom_killed: Option<bool>,
    /// Error string captured from Docker's container state on exit.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error_message: Option<String>,
    /// When the container exited (Docker's FinishedAt). None while still running.
    #[schema(value_type = Option<String>, example = "2025-10-12T12:16:47.609192Z")]
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub finished_at: Option<UtcDateTime>,
    /// When the container's main process most recently started (Docker's
    /// StartedAt). On a restarted container this is *after* `created_at`, so
    /// uptime should be derived from this rather than `created_at`. None for
    /// containers that have never started.
    #[schema(value_type = Option<String>, example = "2025-10-12T12:15:50.000000Z")]
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub started_at: Option<UtcDateTime>,
    /// CPU limit applied to the container, in whole cores (e.g. `1.0` =
    /// 1 vCPU, `0.5` = half a vCPU). None if no limit is set.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_limit_cores: Option<f64>,
}

impl Default for ContainerInfo {
    fn default() -> Self {
        Self {
            container_id: String::new(),
            container_name: String::new(),
            image_name: String::new(),
            status: ContainerStatus::Created,
            created_at: chrono::Utc::now(),
            ports: Vec::new(),
            environment_vars: HashMap::new(),
            restart_count: None,
            labels: HashMap::new(),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        }
    }
}

/// Container performance statistics (CPU, memory, network)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerStats {
    pub container_id: String,
    pub container_name: String,
    /// Raw Docker CPU usage percentage, where **100% == one full core**.
    /// A container saturating 2 cores reads `200.0`, 4 cores `400.0`, etc.
    /// This is NOT bounded to 0-100 — to compare against a threshold you must
    /// normalise against the CPU the container is allowed to use; use
    /// [`ContainerStats::cpu_utilization_percent`].
    pub cpu_percent: f64,
    /// CPU limit applied to the container, in whole cores (e.g. `1.0`).
    /// None if no limit is set. Lets the UI render "0.5 / 1.0 cores".
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_limit_cores: Option<f64>,
    /// Number of CPU cores Docker reported as online on the host at sample
    /// time. This is the real ceiling for an *uncapped* container — without it
    /// there is no way to tell "using 1 of 1 core" (saturated) from "using 1 of
    /// 8 cores" (idle host). None when the counter is absent (e.g. stats from a
    /// worker agent predating this field).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub online_cpus: Option<u32>,
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
    /// Container restart count from Docker. Lets the UI render
    /// "Restarted 3×" so a restart loop is visible at a glance.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub restart_count: Option<i64>,
    /// When the container's main process most recently started. Drives the
    /// uptime label in the header — distinct from `created_at` because the
    /// container may have been restarted in place after a crash.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub started_at: Option<UtcDateTime>,
    /// Timestamp of metrics collection
    pub timestamp: UtcDateTime,
}

impl Default for ContainerStats {
    fn default() -> Self {
        Self {
            container_id: String::new(),
            container_name: String::new(),
            cpu_percent: 0.0,
            cpu_limit_cores: None,
            online_cpus: None,
            memory_bytes: 0,
            memory_limit_bytes: None,
            memory_percent: None,
            network_rx_bytes: 0,
            network_tx_bytes: 0,
            restart_count: None,
            started_at: None,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl ContainerStats {
    /// CPU usage as a percentage of the container's *allowed* CPU, suitable for
    /// threshold comparison (a value of `100.0` means the container is using its
    /// entire CPU allocation).
    ///
    /// `cpu_percent` is the raw Docker number where 100% == one core, so a
    /// container allowed 2 cores can legitimately reach 200% while still being
    /// only 100% utilised. Alarms must compare against *this* value, not the raw
    /// `cpu_percent`, otherwise a 2-core container fires at ~95% raw (≈47% of its
    /// limit) — well below saturation.
    ///
    /// When the container has an explicit limit (`cpu_limit_cores`), the ceiling
    /// is `limit * 100` raw percent. When it has **no** limit the container may
    /// legitimately use every core on the box, so the ceiling is the host's core
    /// count (`online_cpus`) — using one core as the ceiling would report a
    /// container quietly using 1 of 8 cores as "100% utilised" and fire a
    /// high-CPU alarm on an idle host.
    ///
    /// Only when neither is known (no limit *and* no core count, e.g. stats from
    /// an older worker agent) do we fall back to a one-core ceiling.
    pub fn cpu_utilization_percent(&self) -> f64 {
        self.cpu_percent / self.cpu_ceiling_cores()
    }

    /// Cores the container is allowed to use: its explicit limit, else every
    /// core on the host, else one core when neither is known.
    pub fn cpu_ceiling_cores(&self) -> f64 {
        match self.cpu_limit_cores {
            Some(cores) if cores > 0.0 => cores,
            // Uncapped: the whole host is fair game.
            _ => match self.online_cpus {
                Some(cpus) if cpus > 0 => f64::from(cpus),
                _ => 1.0,
            },
        }
    }
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

    /// Import an image from a backpressure-aware byte stream.
    ///
    /// Implementations that accept remote uploads should override this. The
    /// default preserves compatibility for builders that only support files.
    async fn import_image_stream(
        &self,
        mut stream: ImageImportStream,
        tag: &str,
    ) -> Result<String, BuilderError> {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        // File-only builders keep their previous behavior. DockerRuntime
        // overrides this method and forwards chunks directly to dockerd, so
        // worker image transfers do not take this compatibility path.
        let archive = tempfile::NamedTempFile::new().map_err(|source| {
            BuilderError::IoError(std::io::Error::new(
                source.kind(),
                format!("Failed to create temporary image archive for '{tag}': {source}"),
            ))
        })?;
        let archive_file = archive.reopen().map_err(|source| {
            BuilderError::IoError(std::io::Error::new(
                source.kind(),
                format!("Failed to open temporary image archive for '{tag}': {source}"),
            ))
        })?;
        let mut archive_file = tokio::fs::File::from_std(archive_file);

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| {
                BuilderError::IoError(std::io::Error::new(
                    source.kind(),
                    format!("Failed while receiving image archive for '{tag}': {source}"),
                ))
            })?;
            archive_file.write_all(&chunk).await.map_err(|source| {
                BuilderError::IoError(std::io::Error::new(
                    source.kind(),
                    format!("Failed to write temporary image archive for '{tag}': {source}"),
                ))
            })?;
        }
        archive_file.flush().await.map_err(|source| {
            BuilderError::IoError(std::io::Error::new(
                source.kind(),
                format!("Failed to flush temporary image archive for '{tag}': {source}"),
            ))
        })?;
        drop(archive_file);

        self.import_image(archive.path().to_path_buf(), tag).await
    }

    /// Export (save) an image to a tar archive file.
    /// Equivalent to `docker save <image_name> -o <output_path>`.
    async fn save_image(&self, image_name: &str, output_path: &Path) -> Result<(), BuilderError>;

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

    /// Inspect an image and return its metadata including architecture
    async fn inspect_image(&self, image_name: &str) -> Result<ImageInfo, BuilderError>;

    /// Get the native platform string for this runtime (e.g., "linux/amd64" or "linux/arm64")
    ///
    /// May be a *fallback* — the architecture this binary was compiled for —
    /// when the daemon's platform hasn't been discovered. Callers that must
    /// not act on a guess should use [`Self::discovered_platform`] instead.
    fn get_native_platform(&self) -> String;

    /// The platform this runtime **confirmed** with its Docker daemon, or
    /// `None` when discovery hasn't succeeded.
    ///
    /// The distinction matters wherever a wrong answer is worse than no
    /// answer: with a cross-architecture `DOCKER_HOST`, `get_native_platform`
    /// reports this process's architecture until discovery lands, and treating
    /// that as authoritative would pick the wrong image for the control plane.
    ///
    /// Defaults to `None` so an implementation that can't tell the difference
    /// is treated as "unknown" rather than as a confirmation.
    fn discovered_platform(&self) -> Option<String> {
        None
    }

    /// Confirm the daemon's platform, querying it if that hasn't happened yet.
    ///
    /// [`Self::discovered_platform`] only reports what is already known, which
    /// leaves callers on paths that never build — image uploads, external
    /// images — permanently unable to tell a matching image from a mismatched
    /// one. This lets them ask, at the cost of one `docker info`.
    ///
    /// Still `None` when the daemon can't be reached: unknown, never a guess.
    async fn ensure_platform_discovered(&self) -> Option<String> {
        self.discovered_platform()
    }

    /// Validate that an image's architecture matches the target platform
    /// Returns Ok(()) if compatible, or Err(BuilderError::PlatformMismatch) if not
    async fn validate_image_platform(&self, image_name: &str) -> Result<(), BuilderError> {
        let image_info = self.inspect_image(image_name).await?;
        let native_platform = self.get_native_platform();

        if image_info.platform != native_platform {
            return Err(BuilderError::PlatformMismatch {
                image_platform: image_info.platform,
                target_platform: native_platform,
            });
        }

        Ok(())
    }
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

    /// Check if a Docker image exists locally.
    /// Returns Ok(true) if the image exists, Ok(false) if it does not.
    /// Default implementation returns true (assumes image exists) for backward compatibility.
    async fn image_exists(&self, _image_name: &str) -> Result<bool, DeployerError> {
        Ok(true)
    }
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

    fn stats_with_cpu(cpu_percent: f64, cpu_limit_cores: Option<f64>) -> ContainerStats {
        ContainerStats {
            cpu_percent,
            cpu_limit_cores,
            ..Default::default()
        }
    }

    fn stats_with_cpu_on_host(
        cpu_percent: f64,
        cpu_limit_cores: Option<f64>,
        online_cpus: u32,
    ) -> ContainerStats {
        ContainerStats {
            cpu_percent,
            cpu_limit_cores,
            online_cpus: Some(online_cpus),
            ..Default::default()
        }
    }

    #[test]
    fn test_cpu_utilization_with_limit() {
        // 2-core limit, container using 1.8 cores (180% raw) -> 90% of its limit.
        let stats = stats_with_cpu(180.0, Some(2.0));
        assert!((stats.cpu_utilization_percent() - 90.0).abs() < 1e-9);

        // The whole point of the bugfix: 95% raw on a 2-core limit is only ~47%
        // utilised and must NOT cross a 90% threshold.
        let stats = stats_with_cpu(95.0, Some(2.0));
        assert!(stats.cpu_utilization_percent() < 90.0);
        assert!((stats.cpu_utilization_percent() - 47.5).abs() < 1e-9);

        // Fully saturating a 2-core limit (200% raw) reads exactly 100%.
        let stats = stats_with_cpu(200.0, Some(2.0));
        assert!((stats.cpu_utilization_percent() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_cpu_utilization_fractional_limit() {
        // 0.5-core limit, container using half a core (50% raw) -> 100% utilised.
        let stats = stats_with_cpu(50.0, Some(0.5));
        assert!((stats.cpu_utilization_percent() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_cpu_utilization_no_limit_is_relative_to_host_cores() {
        // The bug this guards: an uncapped container saturating one core on an
        // 8-core host is at 12.5% of what it's allowed, NOT 100%. Normalising
        // per-core fired a high-CPU alarm on an essentially idle host.
        let stats = stats_with_cpu_on_host(100.0, None, 8);
        assert!((stats.cpu_utilization_percent() - 12.5).abs() < 1e-9);

        // Only when it saturates every core does it read 100%.
        let stats = stats_with_cpu_on_host(800.0, None, 8);
        assert!((stats.cpu_utilization_percent() - 100.0).abs() < 1e-9);

        // Single-core host: uncapped and pinned really is 100%.
        let stats = stats_with_cpu_on_host(100.0, None, 1);
        assert!((stats.cpu_utilization_percent() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_cpu_utilization_explicit_limit_wins_over_host_cores() {
        // A 0.5-core cap on an 8-core host: the cap is the ceiling, so half a
        // core in use is full saturation.
        let stats = stats_with_cpu_on_host(50.0, Some(0.5), 8);
        assert!((stats.cpu_utilization_percent() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_cpu_utilization_unknown_host_cores_falls_back_to_one_core() {
        // No limit and no core count (older worker agent) -> one-core ceiling,
        // so raw% == utilisation%.
        let stats = stats_with_cpu(95.0, None);
        assert!((stats.cpu_utilization_percent() - 95.0).abs() < 1e-9);

        let stats = stats_with_cpu(250.0, None);
        assert!((stats.cpu_utilization_percent() - 250.0).abs() < 1e-9);
    }

    #[test]
    fn test_cpu_utilization_zero_or_invalid_limit_falls_back_to_host_cores() {
        // A zero/negative limit is treated as "no limit" (host-core ceiling)
        // rather than dividing by zero.
        let stats = stats_with_cpu_on_host(120.0, Some(0.0), 4);
        assert!((stats.cpu_utilization_percent() - 30.0).abs() < 1e-9);

        let stats = stats_with_cpu_on_host(120.0, Some(-1.0), 4);
        assert!((stats.cpu_utilization_percent() - 30.0).abs() < 1e-9);

        // ...and to one core when the host core count is unknown too.
        let stats = stats_with_cpu(120.0, Some(0.0));
        assert!((stats.cpu_utilization_percent() - 120.0).abs() < 1e-9);
    }

    #[test]
    fn test_cpu_utilization_zero_online_cpus_is_not_a_divide_by_zero() {
        let stats = ContainerStats {
            cpu_percent: 50.0,
            cpu_limit_cores: None,
            online_cpus: Some(0),
            ..Default::default()
        };
        assert!((stats.cpu_utilization_percent() - 50.0).abs() < 1e-9);
    }

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
            secrets: HashMap::new(),
            port_mappings,
            network_name: Some("test-network".to_string()),
            extra_networks: vec!["dependency-network".to_string()],
            resource_limits: ResourceLimits::default(),
            restart_policy: RestartPolicy::Always,
            log_path,
            command: Some(vec!["node".to_string(), "server.js".to_string()]),
            log_config: Some(ContainerLogConfig::app_default()),
            labels: HashMap::new(),
        };

        assert_eq!(request.image_name, "test-image:latest");
        assert_eq!(request.container_name, "test-container");
        assert_eq!(request.environment_vars.get("PORT").unwrap(), "3000");
        assert_eq!(request.port_mappings.len(), 1);
        assert_eq!(request.port_mappings[0].host_port, 8080);
        assert_eq!(request.port_mappings[0].container_port, 3000);
        assert!(matches!(request.port_mappings[0].protocol, Protocol::Tcp));
        assert_eq!(request.network_name.as_ref().unwrap(), "test-network");
        assert_eq!(request.extra_networks, vec!["dependency-network"]);
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
            restart_count: Some(0),
            labels: HashMap::new(),
            ..Default::default()
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
            secrets: HashMap::new(),
            port_mappings: vec![],
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits: ResourceLimits::default(),
            restart_policy: RestartPolicy::Always,
            log_path: temp_dir.path().join("deploy.log"),
            command: None, // No custom command, use default from image
            log_config: Some(ContainerLogConfig::app_default()),
            labels: HashMap::new(),
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
    fn test_type_validation() {
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
