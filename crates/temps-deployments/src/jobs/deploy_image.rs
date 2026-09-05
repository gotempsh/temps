// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deploy Image Job
//!
//! Deploys built container images to target environments

use async_trait::async_trait;
use futures::StreamExt;
use sea_orm::{sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use temps_core::{
    JobResult, WorkflowCancellationProvider, WorkflowContext, WorkflowError, WorkflowTask,
};
use temps_database::DbConnection;
use temps_deployer::{
    ContainerDeployer, ContainerLogConfig, ContainerStatus as DeployerContainerStatus,
    DeployRequest, ImageBuilder, PortMapping, Protocol, ResourceLimits, RestartPolicy,
};
use temps_entities::deployment_containers;
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
    /// Per-platform image tags produced by a multi-arch build, keyed by
    /// canonical platform (`linux/arm64` → `myapp:latest-arm64`).
    ///
    /// Empty for a single-architecture build — the overwhelmingly common case
    /// — where `image_tag` alone covers the cluster. Also empty when reading a
    /// workflow context written before multi-arch support, hence the
    /// `#[serde(default)]`.
    #[serde(default)]
    pub image_tags_by_platform: HashMap<String, String>,
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

        // Absent for single-arch builds and for contexts written by an older
        // version — both mean "just the one tag".
        let image_tags_by_platform: HashMap<String, String> = context
            .get_output(build_job_id, "image_tags_by_platform")?
            .unwrap_or_default();

        Ok(Self {
            image_tag,
            image_id,
            size_bytes,
            build_context: PathBuf::from(build_context_str),
            dockerfile_path: PathBuf::from(dockerfile_path_str),
            image_tags_by_platform,
        })
    }

    /// The tag to deploy on a node running `platform`.
    ///
    /// Falls back to the primary tag when the platform is unknown or the build
    /// produced a single image, which is exactly the pre-multi-arch behaviour.
    pub fn tag_for_platform(&self, platform: Option<&str>) -> &str {
        let Some(platform) = platform else {
            return &self.image_tag;
        };
        if self.image_tags_by_platform.is_empty() {
            return &self.image_tag;
        }
        self.image_tags_by_platform
            .iter()
            .find(|(built, _)| temps_deployer::platform::platforms_match(built, platform))
            .map(|(_, tag)| tag.as_str())
            .unwrap_or(&self.image_tag)
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
    /// Image tag each replica actually runs. Parallel to `container_ids`.
    ///
    /// On a mixed-architecture deployment these differ per replica
    /// (`app:latest` on amd64 nodes, `app:latest-arm64` on arm64 ones), and
    /// `MarkDeploymentCompleteJob` records them per container — otherwise every
    /// row would claim the primary tag and the node/deployment APIs would
    /// report ARM replicas as running the amd64 image.
    #[serde(default)]
    pub image_names: Vec<String>,
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

/// Turn the planner's cross-node blockers into the failure a remotely
/// scheduled replica must report, or `None` when there is nothing blocking.
///
/// Pure so the exact operator-facing message — which is the entire point of
/// this code path — can be asserted in tests. Only ever called for a
/// non-local assignment: a replica staying on the control plane reaches its
/// linked services by container name and is unaffected.
fn cross_node_unreachable_error(
    node_name: &str,
    blockers: &[crate::services::CrossNodeServiceBlocker],
) -> Option<WorkflowError> {
    if blockers.is_empty() {
        return None;
    }
    Some(WorkflowError::CrossNodeServiceUnreachable {
        node_name: node_name.to_string(),
        blocker_count: blockers.len(),
        details: blockers
            .iter()
            .map(|blocker| blocker.describe())
            .collect::<Vec<_>>()
            .join("; "),
    })
}

fn private_remote_bind_address(address: &str) -> Result<String, WorkflowError> {
    let ip = address.parse::<std::net::IpAddr>().map_err(|error| {
        WorkflowError::JobExecutionFailed(format!(
            "Worker private address '{address}' is not a valid IP address: {error}"
        ))
    })?;
    let is_private = match ip {
        std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        std::net::IpAddr::V6(ip) => {
            ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local()
        }
    };
    if !is_private {
        return Err(WorkflowError::JobExecutionFailed(format!(
            "Worker address '{address}' is public; refusing to publish an app container outside the Temps proxy"
        )));
    }
    Ok(address.to_string())
}

fn confirms_private_port_binding(
    ports: &[PortMapping],
    container_port: u16,
    host_port: u16,
    expected_host_ip: &str,
) -> bool {
    ports.iter().any(|port| {
        port.container_port == container_port
            && port.host_port == host_port
            && port.host_ip.as_deref() == Some(expected_host_ip)
    })
}

/// Configuration for deployment job execution
/// This is built from the entity's DeploymentConfig + runtime values
#[derive(Debug, Clone)]
pub struct DeploymentJobConfig {
    pub namespace: String,
    pub service_name: String,
    pub replicas: u32,
    /// Fallback container port, used when `configured_port` is `None` and
    /// image `EXPOSE` auto-detection finds nothing either. Every caller that
    /// builds a real deployment job resolves and sets this explicitly (3000
    /// when neither environment nor project configures a port); the `8080`
    /// in [`Default::default`] below is only a placeholder for tests and is
    /// never meant to reach `resolve_container_port()` unmodified.
    pub port: u32,
    /// Explicit port override from the environment or project scope (in that
    /// priority order), as resolved by the job planner. When `Some`, this
    /// wins over image `EXPOSE` auto-detection in `resolve_container_port()`
    /// — an operator's explicit configuration must never be overridden by a
    /// heuristic guess at the image's listening port. `None` means neither
    /// scope configured a port, so auto-detection is allowed to run.
    pub configured_port: Option<u16>,
    pub environment_variables: HashMap<String, String>,
    /// Optional command passed to the container image entrypoint.
    pub command: Option<Vec<String>>,
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
    /// (linked-service container names are replaced with their internal
    /// `*.temps.local` DNS names, which resolve to overlay IPs on every node).
    pub remote_environment_variables: Option<HashMap<String, String>>,
    /// Linked external services that have no working address from any node
    /// other than their own. Empty in the normal case.
    ///
    /// A replica scheduled remotely with a non-empty list is failed instead
    /// of being handed a connection string that can never connect — see
    /// [`temps_core::WorkflowError::CrossNodeServiceUnreachable`]. Local
    /// replicas ignore it entirely: they reach the service by container name
    /// over the shared bridge network exactly as before.
    pub cross_node_service_blockers: Vec<crate::services::CrossNodeServiceBlocker>,
    /// Anti-affinity: avoid placing two replicas on the same node.
    /// When true, the scheduler spreads replicas across different nodes.
    pub anti_affinity: bool,
    /// Node IDs that already host containers for the current environment.
    /// During rolling updates, the outgoing containers haven't been removed yet.
    /// When anti-affinity is enabled, these nodes are excluded from scheduling
    /// to prevent new replicas from landing on the same nodes as old ones.
    pub exclude_node_ids: Vec<i32>,
}

fn has_explicit_placement_constraints(
    target_node_ids: Option<&[i32]>,
    target_labels: Option<&serde_json::Value>,
) -> bool {
    crate::services::node_scheduler::placement_node_ids(target_node_ids).is_some()
        || target_labels
            .is_some_and(|labels| !labels.as_object().is_some_and(serde_json::Map::is_empty))
}

impl Default for DeploymentJobConfig {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            service_name: "app".to_string(),
            replicas: 1,
            port: 8080,
            configured_port: None,
            environment_variables: HashMap::new(),
            command: None,
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
            cross_node_service_blockers: Vec::new(),
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
    /// Candidate metadata is persisted on a failed readiness check so the
    /// authenticated container-log endpoints can still resolve the container.
    failed_candidates: Arc<Mutex<Vec<FailedContainerCandidate>>>,
    /// Set only after stopped retained-container rows commit successfully.
    /// Workflow cleanup leaves those registered candidates available for
    /// authenticated inspection without consuming runtime resources.
    retained_failure: Arc<AtomicBool>,
    /// A remote agent that cannot prove private-only port binding must never
    /// be retained, even if another replica was otherwise safe to keep.
    retention_forbidden: Arc<AtomicBool>,
    failed_container_db: Option<Arc<DbConnection>>,
    deployment_id: Option<i32>,
    /// Background task handle for log streaming (aborted on cleanup)
    log_stream_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Optional: directly provided image tag (for external/pre-built images, bypasses BuildImageJob lookup)
    external_image_tag: Option<String>,
    /// Docker log rotation config to prevent unbounded log growth
    log_config: Option<ContainerLogConfig>,
    /// Encryption service for decrypting node tokens during remote deployments
    encryption_service: Option<Arc<temps_core::EncryptionService>>,
    /// Config service — resolves the cluster CA for mTLS to https:// nodes (WS-2.1)
    config_service: Option<Arc<temps_config::ConfigService>>,
    /// Local image builder — used to `save_image()` before transferring to remote nodes
    image_builder: Option<Arc<dyn temps_deployer::ImageBuilder>>,
}

#[derive(Debug, Clone)]
struct FailedContainerCandidate {
    container_id: String,
    container_name: String,
    container_port: u16,
    host_port: u16,
    image_name: String,
    node_id: Option<i32>,
}

fn lock_deployment_state<'a, T>(
    mutex: &'a Mutex<T>,
    state_name: &'static str,
) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                state_name,
                "Recovering poisoned deployment state lock to preserve container cleanup"
            );
            poisoned.into_inner()
        }
    }
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
            failed_candidates: Arc::new(Mutex::new(Vec::new())),
            retained_failure: Arc::new(AtomicBool::new(false)),
            retention_forbidden: Arc::new(AtomicBool::new(false)),
            failed_container_db: None,
            deployment_id: None,
            log_stream_task: Arc::new(Mutex::new(None)),
            external_image_tag: None,
            log_config: None,
            encryption_service: None,
            config_service: None,
            image_builder: None,
        }
    }

    fn with_failed_container_retention(
        mut self,
        db: Arc<DbConnection>,
        deployment_id: i32,
    ) -> Self {
        self.failed_container_db = Some(db);
        self.deployment_id = Some(deployment_id);
        self
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

    pub fn with_config_service(mut self, service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(service);
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
    /// The container platforms this deployment has an image for.
    ///
    /// An empty result means "unknown" and disables architecture filtering —
    /// that's the honest answer when the builder can't inspect the image, and
    /// it preserves the pre-multi-arch behaviour rather than guessing amd64.
    async fn available_image_platforms(&self, image_output: &BuildImageOutput) -> Vec<String> {
        // A multi-arch build records one tag per platform; those keys are the
        // authoritative answer and need no Docker round-trip.
        if !image_output.image_tags_by_platform.is_empty() {
            return image_output
                .image_tags_by_platform
                .keys()
                .cloned()
                .collect();
        }

        let Some(image_builder) = self.image_builder.as_ref() else {
            return Vec::new();
        };

        match image_builder.inspect_image(&image_output.image_tag).await {
            Ok(info) => vec![info.platform],
            Err(e) => {
                tracing::debug!(
                    image = %image_output.image_tag,
                    "Could not determine image platform for scheduling: {}",
                    e
                );
                Vec::new()
            }
        }
    }

    /// Guard the "just deploy it locally" fallbacks.
    ///
    /// Those paths exist so a scheduling hiccup degrades instead of failing —
    /// which is right as long as the control plane can run the image. It can't
    /// always: an uploaded image is now accepted when *any* node in the cluster
    /// matches its architecture, so a remote-only image reaching this fallback
    /// would be started on an incompatible control plane and die with
    /// `exec format error`.
    ///
    /// Unknown platforms (`image_platforms` empty, or no image builder to ask)
    /// keep the historical behaviour: proceed.
    async fn ensure_local_can_run(
        &self,
        image_platforms: &[String],
        context: &WorkflowContext,
        reason: &str,
    ) -> Result<(), WorkflowError> {
        if image_platforms.is_empty() {
            return Ok(());
        }
        let Some(image_builder) = self.image_builder.as_ref() else {
            return Ok(());
        };

        // The *confirmed* daemon platform, asked for if it isn't known yet.
        // `get_native_platform()` would answer with this process's
        // architecture, and on a cross-architecture `DOCKER_HOST` approving a
        // local fallback on that basis lets the container through — the local
        // verification below deliberately stays quiet while the platform is
        // unknown, so nothing else would catch it.
        let Some(local_platform) = image_builder.ensure_platform_discovered().await else {
            tracing::warn!(
                image_platforms = ?image_platforms,
                "Control-plane platform unknown; deploying locally without an \
                 architecture check"
            );
            return Ok(());
        };
        if image_platforms
            .iter()
            .any(|p| temps_deployer::platform::platforms_match(p, &local_platform))
        {
            return Ok(());
        }

        let msg = format!(
            "Cannot deploy locally after falling back ({}): this image is built for [{}] \
             and the control plane runs {}. It would fail to start with 'exec format error'. \
             Retry once the worker nodes for [{}] are reachable.",
            reason,
            image_platforms.join(", "),
            local_platform,
            image_platforms.join(", ")
        );
        self.log(context, format!("ERROR: {}", msg)).await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

    /// Verify an image can run on the control plane before deploying it here.
    ///
    /// Only acts on a **confirmed** local platform: while the daemon's
    /// architecture is unknown, comparing against the compiled-in fallback
    /// could reject a perfectly good image, so we let the deploy proceed as it
    /// did before multi-arch support.
    async fn verify_image_platform_for_local(
        &self,
        image_tag: &str,
        local_platform: Option<&str>,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let (Some(local_platform), Some(image_builder)) =
            (local_platform, self.image_builder.as_ref())
        else {
            return Ok(());
        };

        let image_platform = match image_builder.inspect_image(image_tag).await {
            Ok(info) => info.platform,
            Err(e) => {
                tracing::debug!(
                    image = %image_tag,
                    "Could not inspect image to verify it runs on the control plane: {}",
                    e
                );
                return Ok(());
            }
        };

        if temps_deployer::platform::platforms_match(&image_platform, local_platform) {
            return Ok(());
        }

        let msg = format!(
            "Image '{}' is built for {} but the control plane runs {}. \
             The container would fail to start with 'exec format error'. \
             Build for {} (multi-arch build), or restrict this environment to \
             {} nodes with target nodes/labels.",
            image_tag, image_platform, local_platform, local_platform, image_platform
        );
        self.log(context, format!("ERROR: {}", msg)).await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

    /// Verify that `image_tag` can actually run on the target node.
    ///
    /// Compares the image's architecture (read from the control plane's own
    /// Docker, which built or pulled it) against the node's. Both sides can be
    /// unknown, and neither unknown is treated as a failure:
    ///
    /// - **Image platform unknown** — the local builder can't inspect it (some
    ///   `ImageBuilder` impls don't support inspection). Nothing to compare.
    /// - **Node platform unknown** — a pre-multi-arch agent. We ask its health
    ///   endpoint once; if that also comes back empty we log and proceed,
    ///   preserving the behaviour those nodes have today.
    ///
    /// Only a *known* mismatch aborts the deploy.
    async fn verify_image_platform_for_node(
        &self,
        image_tag: &str,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        node_name: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let Some(image_builder) = self.image_builder.as_ref() else {
            return Ok(());
        };

        let image_platform = match image_builder.inspect_image(image_tag).await {
            Ok(info) => info.platform,
            Err(e) => {
                tracing::debug!(
                    image = %image_tag,
                    "Could not inspect image to verify its platform: {}",
                    e
                );
                return Ok(());
            }
        };

        let node_platform = match remote.platform() {
            Some(platform) => platform,
            None => match remote.refresh_platform().await {
                Some(platform) => platform,
                None => {
                    self.log(
                        context,
                        format!(
                            "WARNING: node '{}' did not report its architecture; \
                             deploying '{}' ({}) without an architecture check. \
                             Upgrade the node agent to enable it.",
                            node_name, image_tag, image_platform
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            },
        };

        if temps_deployer::platform::platforms_match(&image_platform, &node_platform) {
            return Ok(());
        }

        let msg = format!(
            "Image '{}' is built for {} but node '{}' runs {}. \
             The container would fail to start with 'exec format error'. \
             Build for {} (multi-arch build), or restrict this environment to \
             {} nodes with target nodes/labels.",
            image_tag, image_platform, node_name, node_platform, node_platform, image_platform
        );
        self.log(context, format!("ERROR: {}", msg)).await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

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
        // Refuse to ship an image the node cannot execute. Without this the
        // tar transfers fine, `docker load` succeeds, and the container dies
        // at start with `exec format error` — a failure mode with no trace
        // back to the architecture mismatch that caused it.
        self.verify_image_platform_for_node(image_tag, remote, node_name, context)
            .await?;

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
    /// 1. Explicit environment-level or project-level port override
    ///    (`self.config.configured_port`) — an operator's explicit
    ///    configuration always wins over a heuristic guess.
    /// 2. Auto-detected from the Docker image's EXPOSE directive
    /// 3. Configured/default port (`self.config.port`, e.g. 3000)
    ///
    /// This method inspects the built image and extracts exposed ports, but
    /// only when neither scope explicitly configures a port.
    async fn resolve_container_port(&self, image_tag: &str, context: &WorkflowContext) -> u16 {
        if let Some(configured) = self.config.configured_port {
            let _ = self
                .log(
                    context,
                    format!(
                        "Using explicitly configured port: {} (environment/project override)",
                        configured
                    ),
                )
                .await;
            return configured;
        }

        // No explicit override — try to inspect the image and get exposed ports
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

        // Fallback to configured/default port
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

    async fn stop_background_log_stream(
        &self,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let should_log = {
            let mut task_handle = lock_deployment_state(&self.log_stream_task, "log_stream_task");
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

        Ok(())
    }

    /// Remove all containers if they exist (called on timeout/failure/cancellation)
    async fn cleanup_container(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        self.stop_background_log_stream(context).await?;

        // Then clean up all containers
        let container_ids = {
            let guard = lock_deployment_state(&self.container_ids, "container_ids");
            guard.clone()
        };

        let mut cleanup_errors = Vec::new();
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
                    let deployers =
                        lock_deployment_state(&self.replica_deployers, "replica_deployers");
                    deployers
                        .get(container_id)
                        .cloned()
                        .unwrap_or_else(|| self.container_deployer.clone())
                };

                match deployer.remove_container(container_id).await {
                    Ok(()) | Err(temps_deployer::DeployerError::ContainerNotFound(_)) => {
                        self.log(context, format!("✅ Container {} is absent", container_id))
                            .await?;
                    }
                    Err(error) => {
                        cleanup_errors.push(format!("container {container_id}: {error}"));
                        self.log(
                            context,
                            format!(
                                "⚠️  Warning: Failed to remove container {}: {}",
                                container_id, error
                            ),
                        )
                        .await?;
                    }
                }
            }
        }

        if !cleanup_errors.is_empty() {
            return Err(WorkflowError::JobExecutionFailed(format!(
                "Failed to remove {} deployment container(s): {}",
                cleanup_errors.len(),
                cleanup_errors.join("; ")
            )));
        }

        Ok(())
    }

    /// Persist failed app candidates without promoting routes. This mirrors
    /// failed Compose retention: only a successful MarkDeploymentCompleteJob
    /// makes a container public, while the ordinary authenticated log endpoint
    /// can resolve these rows for debugging.
    async fn retain_failed_containers(
        &self,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        if self.retention_forbidden.load(Ordering::Acquire) {
            return self.cleanup_container(context).await;
        }
        let candidates =
            lock_deployment_state(&self.failed_candidates, "failed_candidates").clone();
        if candidates.is_empty() {
            return self.cleanup_container(context).await;
        }

        let (Some(db), Some(deployment_id)) =
            (self.failed_container_db.as_ref(), self.deployment_id)
        else {
            // Jobs created by isolated tests or legacy callers do not have a
            // durable ownership record, so keeping their containers would leak
            // an inaccessible Docker resource.
            return self.cleanup_container(context).await;
        };

        self.stop_background_log_stream(context).await?;
        let replica_deployers =
            lock_deployment_state(&self.replica_deployers, "replica_deployers").clone();
        for candidate in &candidates {
            let deployer = replica_deployers
                .get(&candidate.container_id)
                .cloned()
                .unwrap_or_else(|| self.container_deployer.clone());
            deployer
                .stop_container(&candidate.container_id)
                .await
                .map_err(|error| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to stop app container '{}' before retaining its logs for deployment {deployment_id}: {error}",
                        candidate.container_id
                    ))
                })?;
        }
        let transaction = db.begin().await.map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to begin retained app-container registration for deployment {deployment_id}: {error}"
            ))
        })?;
        let now = chrono::Utc::now();

        for candidate in &candidates {
            deployment_containers::Entity::insert(deployment_containers::ActiveModel {
                deployment_id: Set(deployment_id),
                container_id: Set(candidate.container_id.clone()),
                container_name: Set(candidate.container_name.clone()),
                container_port: Set(i32::from(candidate.container_port)),
                host_port: Set(Some(i32::from(candidate.host_port))),
                image_name: Set(Some(candidate.image_name.clone())),
                status: Set(Some("retained:stopped-after-failed-readiness".to_string())),
                service_name: Set(Some(self.config.service_name.clone())),
                created_at: Set(now),
                deployed_at: Set(now),
                ready_at: Set(None),
                deleted_at: Set(None),
                node_id: Set(candidate.node_id),
                ..Default::default()
            })
            .exec_without_returning(&transaction)
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to register retained app container '{}' for deployment {deployment_id}: {error}",
                    candidate.container_id
                ))
            })?;
        }

        transaction.commit().await.map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to commit {} retained app container record(s) for deployment {deployment_id}: {error}",
                candidates.len()
            ))
        })?;
        self.retained_failure.store(true, Ordering::Release);
        // The ownership row is already committed. A transient stage-log write
        // failure must not make the workflow tear down the now-discoverable
        // candidate while leaving its database row live.
        let _ = self
            .log(
            context,
            format!(
                "Stopped and retained {} failed app container(s) for authenticated log inspection. They are not routed publicly and the next successful deployment removes them.",
                candidates.len()
            ),
        )
        .await;
        Ok(())
    }

    /// Cancellation is an explicit request to stop, not a failed deployment
    /// to diagnose. Remove any candidate that raced with cancellation and
    /// retire a registration that may already have committed.
    async fn discard_failed_candidates(
        &self,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        self.retained_failure.store(false, Ordering::Release);
        self.cleanup_container(context).await?;

        let candidate_ids = lock_deployment_state(&self.failed_candidates, "failed_candidates")
            .iter()
            .map(|candidate| candidate.container_id.clone())
            .collect::<Vec<_>>();
        let (Some(db), Some(deployment_id)) =
            (self.failed_container_db.as_ref(), self.deployment_id)
        else {
            return Ok(());
        };
        if candidate_ids.is_empty() {
            return Ok(());
        }

        deployment_containers::Entity::update_many()
            .col_expr(
                deployment_containers::Column::DeletedAt,
                Expr::value(Some(chrono::Utc::now())),
            )
            .col_expr(
                deployment_containers::Column::Status,
                Expr::value(Some("cancelled".to_string())),
            )
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::ContainerId.is_in(candidate_ids))
            .exec(db.as_ref())
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Cancelled deployment {deployment_id} removed its app containers but failed to retire their diagnostic rows: {error}"
                ))
            })?;

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
            let has_explicit_constraints =
                has_explicit_placement_constraints(target_ids, target_labels);
            // Which architectures do we actually have an image for? Nodes that
            // match none of them are excluded from the pool instead of being
            // handed a container that cannot start.
            let image_platforms = self.available_image_platforms(image_output).await;
            match scheduler
                .schedule_replicas_excluding(
                    self.config.replicas,
                    target_labels,
                    target_ids,
                    self.config.anti_affinity,
                    &self.config.exclude_node_ids,
                    &image_platforms,
                )
                .await
            {
                Ok(outcome) => {
                    // Say which nodes were passed over and why. The scheduler
                    // only had `tracing` for this, which the user never sees —
                    // a node they are paying for would silently not be used,
                    // with nothing in the deploy log to explain it.
                    for exclusion in &outcome.exclusions {
                        let line = if exclusion.excluded {
                            format!("Skipping node {}", exclusion)
                        } else {
                            format!("WARNING: node {}", exclusion)
                        };
                        self.log(context, line).await?;
                    }

                    let assignments = outcome.assignments;
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
                // A cluster with no node able to run this image is a hard
                // error: falling back to Local would deploy the very container
                // the scheduler just established cannot start here.
                Err(e @ crate::services::node_service::NodeError::NoCompatibleNode { .. }) => {
                    let msg = format!("Cannot schedule this deployment: {}", e);
                    self.log(context, format!("ERROR: {}", msg)).await?;
                    return Err(WorkflowError::JobExecutionFailed(msg));
                }
                // Anti-affinity can't be honoured because nodes were excluded.
                // Also hard: degrading to Local here would stack every replica
                // on one machine, which is the opposite of what was asked for,
                // and reporting it as success would hide that.
                Err(
                    e @ (crate::services::node_service::NodeError::InsufficientCompatibleNodes {
                        ..
                    }
                    | crate::services::node_service::NodeError::PlacementConstraintsUnsatisfied {
                        ..
                    }
                    | crate::services::node_service::NodeError::Validation {
                        ..
                    }),
                ) => {
                    let msg = format!("Cannot schedule this deployment: {}", e);
                    self.log(context, format!("ERROR: {}", msg)).await?;
                    return Err(WorkflowError::JobExecutionFailed(msg));
                }
                Err(e) if has_explicit_constraints => {
                    let msg = format!(
                        "Cannot enforce this deployment's placement constraints: {}",
                        e
                    );
                    self.log(context, format!("ERROR: {}", msg)).await?;
                    return Err(WorkflowError::JobExecutionFailed(msg));
                }
                // Any other scheduling error (a transient database failure,
                // say) historically degrades to a local deployment. That is
                // still the right call — but only when the control plane can
                // actually run this image. An uploaded image accepted because
                // some worker matches it would otherwise be started here and
                // die with the very `exec format error` this feature exists to
                // prevent.
                Err(e) => {
                    self.ensure_local_can_run(&image_platforms, context, &e.to_string())
                        .await?;
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
            // A worker-only placement policy cannot be honoured without a
            // scheduler. Failing closed also protects tests/custom embeddings
            // that omit the scheduler even though production normally injects it.
            if has_explicit_placement_constraints(
                self.config.target_nodes.as_deref(),
                self.config.target_labels.as_ref(),
            ) {
                let msg = "Cannot enforce placement constraints: no node scheduler is configured"
                    .to_string();
                self.log(context, format!("ERROR: {}", msg)).await?;
                return Err(WorkflowError::JobExecutionFailed(msg));
            }
            // Pure single-node mode. Same architecture guard: an image built
            // for another architecture cannot run here either.
            let image_platforms = self.available_image_platforms(image_output).await;
            self.ensure_local_can_run(&image_platforms, context, "no node scheduler is configured")
                .await?;
            vec![crate::services::NodeAssignment::Local; self.config.replicas as usize]
        };

        // Deploy multiple replicas
        let mut all_container_ids = Vec::new();
        let mut all_host_ports = Vec::new();
        let mut all_node_ids: Vec<Option<i32>> = Vec::new();
        let mut all_image_names: Vec<String> = Vec::new();
        let mut resolved_container_port: Option<u16> = None;
        let mut deployment_error: Option<WorkflowError> = None;

        // The control plane's own platform, when its daemon confirmed one.
        // `NodeAssignment::Local` carries no platform of its own, so without
        // this a local replica always takes the primary tag — which is the
        // wrong image whenever the primary was built for another architecture.
        let local_platform = match self.image_builder.as_ref() {
            Some(builder) => builder.ensure_platform_discovered().await,
            None => None,
        };

        for (replica_index, assignment) in node_assignments.iter().enumerate() {
            // The tag this replica deploys. Reassigned below for remote nodes
            // whose architecture only becomes known after querying the agent.
            let mut replica_image_tag = image_output
                .tag_for_platform(assignment.platform().or(local_platform.as_deref()))
                .to_string();
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
                crate::services::NodeAssignment::Local => {
                    // Remote replicas are checked before the image is
                    // transferred; local ones had no equivalent guard, so a
                    // mismatch here surfaced only as a container that won't
                    // start.
                    self.verify_image_platform_for_local(
                        &replica_image_tag,
                        local_platform.as_deref(),
                        context,
                    )
                    .await?;
                    self.container_deployer.clone()
                }
                crate::services::NodeAssignment::Remote {
                    address,
                    node_name,
                    platform,
                    ..
                } => {
                    // Look up the node's token from the node service
                    let token = self.get_node_token(assignment).await?;
                    // Build the deployer via the shared factory so https:// nodes
                    // get mutual TLS (ADR-020 WS-2.1); falls back to plain HTTP
                    // when the cluster CA deps aren't wired in.
                    let build_result = match (
                        self.config_service.as_ref(),
                        self.encryption_service.as_ref(),
                    ) {
                        (Some(cs), Some(es)) => {
                            crate::cluster_ca::build_node_deployer(
                                address,
                                token,
                                node_name.clone(),
                                cs.as_ref(),
                                es.as_ref(),
                            )
                            .await
                        }
                        _ => temps_deployer::remote::RemoteNodeDeployer::new(
                            address.clone(),
                            token,
                            node_name.clone(),
                        ),
                    };
                    let remote = match build_result {
                        // Teach the deployer which architecture this node runs
                        // so `get_native_platform()` reports the truth and the
                        // pre-transfer platform check below is meaningful.
                        Ok(remote) => Arc::new(remote.with_platform(platform.clone())),
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

                    // Pick the image built for THIS node's architecture. When
                    // the node row carries no platform (an agent that predates
                    // multi-arch, or one upgraded but not yet heartbeated) ask
                    // the agent directly rather than defaulting to the primary
                    // tag — on a multi-arch build the right image may well
                    // exist, and shipping the wrong one would fail the deploy
                    // for no reason.
                    let node_platform = match platform.clone() {
                        Some(platform) => Some(platform),
                        None => remote.refresh_platform().await,
                    };
                    replica_image_tag = image_output
                        .tag_for_platform(node_platform.as_deref())
                        .to_string();

                    // Transfer image to remote node if it doesn't already exist there
                    self.ensure_image_on_remote(&replica_image_tag, &remote, node_name, context)
                        .await?;

                    remote
                }
            };

            match self
                .deploy_single_replica(
                    &replica_image_tag,
                    context,
                    replica_index as u32,
                    health_check_override.as_deref(),
                    &deployer,
                    assignment,
                )
                .await
            {
                Ok((container_id, host_port, container_port)) => {
                    all_container_ids.push(container_id);
                    all_host_ports.push(host_port);
                    all_node_ids.push(assignment.node_id());
                    all_image_names.push(replica_image_tag.clone());
                    // All replicas share the same container port
                    resolved_container_port = Some(container_port);
                }
                Err(e) => {
                    self.log(
                        context,
                        format!("❌ Failed to deploy replica {}: {}", replica_index + 1, e),
                    )
                    .await?;

                    self.log(
                        context,
                        format!(
                            "Retaining {} created container(s) while the failed deployment is recorded",
                            all_container_ids.len()
                        ),
                    )
                    .await?;

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

        // No fraction here: the failure path above rolls back and returns, so
        // this line can never report a partial deployment — printing "2/3"
        // would only ever be a lie.
        self.log(
            context,
            format!(
                "✅ Successfully deployed {} replica(s)",
                all_container_ids.len()
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
            image_names: all_image_names,
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
        // Tag resolved for the target node's architecture by the caller — on a
        // multi-arch build this is the per-node tag, not the primary one.
        image_tag: &str,
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
        // Priority: explicit environment/project override > Image EXPOSE directive > default
        let container_port = self.resolve_container_port(image_tag, context).await;

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

        let host_ip = assignment
            .private_address()
            .map(private_remote_bind_address)
            .transpose()?;
        let port_mappings = vec![PortMapping {
            host_port,
            container_port,
            protocol: Protocol::Tcp,
            host_ip,
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

        // Use remote environment variables for remote deployments (linked-service
        // container names rewritten to their internal `*.temps.local` DNS names),
        // fall back to local env vars.
        let mut environment_vars = if !assignment.is_local() {
            // Refuse to start a container that has been handed a connection
            // string which cannot possibly connect. Managed service ports
            // bind to 127.0.0.1 on their own host, so when the planner could
            // not produce a resolvable name there is no working address at
            // all — and a container that boots and then fails to reach its
            // database forever is the exact silent failure this guards.
            let node_name = match assignment {
                crate::services::NodeAssignment::Remote { node_name, .. } => node_name.as_str(),
                crate::services::NodeAssignment::Local => "control-plane",
            };
            if let Some(error) =
                cross_node_unreachable_error(node_name, &self.config.cross_node_service_blockers)
            {
                tracing::error!("{}", error);
                self.log(context, format!("❌ {}", error)).await?;
                return Err(error);
            }

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

        // Inject this replica's node identity so the workload can report which
        // node (and replica) is serving a given request — useful for debugging
        // multi-node scheduling and load distribution. Set on every node, local
        // or remote.
        let (assigned_node_name, assigned_node_id) = match assignment {
            crate::services::NodeAssignment::Remote {
                node_name, node_id, ..
            } => (node_name.clone(), node_id.to_string()),
            crate::services::NodeAssignment::Local => {
                ("control-plane".to_string(), "0".to_string())
            }
        };
        environment_vars.insert("TEMPS_NODE_NAME".to_string(), assigned_node_name);
        environment_vars.insert("TEMPS_NODE_ID".to_string(), assigned_node_id);
        environment_vars.insert("TEMPS_REPLICA".to_string(), (replica_index + 1).to_string());

        tracing::info!(
            "Deploying container with {} env vars (Postgres host configured: {}, URL configured: {})",
            environment_vars.len(),
            environment_vars.contains_key("POSTGRES_HOST"),
            environment_vars.contains_key("POSTGRES_URL")
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
            image_name: image_tag.to_string(),
            container_name,
            environment_vars,
            secrets: self.config.secrets.clone(),
            port_mappings,
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits,
            restart_policy: RestartPolicy::Always,
            log_path,
            command: self.config.command.clone(),
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
            let mut container_ids = lock_deployment_state(&self.container_ids, "container_ids");
            container_ids.push(deploy_result.container_id.clone());
        }
        {
            let mut deployers = lock_deployment_state(&self.replica_deployers, "replica_deployers");
            deployers.insert(deploy_result.container_id.clone(), deployer.clone());
        }

        // A rolling-upgrade cluster may still have an older agent that ignores
        // PortMapping.host_ip. Inspect what Docker actually published before
        // this container becomes eligible for retention; fail closed if the
        // worker cannot prove a private-only bind.
        if let Some(expected_host_ip) = assignment.private_address() {
            let private_host_ip = private_remote_bind_address(expected_host_ip)?;
            let binding_is_private = deployer
                .get_container_info(&deploy_result.container_id)
                .await
                .map(|info| {
                    confirms_private_port_binding(
                        &info.ports,
                        deploy_result.container_port,
                        deploy_result.host_port,
                        &private_host_ip,
                    )
                })
                .unwrap_or(false);
            if !binding_is_private {
                self.retention_forbidden.store(true, Ordering::Release);
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Worker did not confirm that container {} is bound only to private address {}; refusing to retain or route it. Upgrade the Temps agent on this node.",
                    deploy_result.container_id, private_host_ip
                )));
            }
        }
        {
            let mut candidates =
                lock_deployment_state(&self.failed_candidates, "failed_candidates");
            candidates.push(FailedContainerCandidate {
                container_id: deploy_result.container_id.clone(),
                container_name: deploy_result.container_name.clone(),
                container_port: deploy_result.container_port,
                host_port: deploy_result.host_port,
                image_name: image_tag.to_string(),
                node_id: assignment.node_id(),
            });
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
                    return Err(WorkflowError::JobExecutionFailed(
                        "Container failed to start".to_string(),
                    ));
                }
                DeployerContainerStatus::Created => {
                    if start_time.elapsed() > max_wait_time {
                        self.log(context, "⏱️  Container start timeout".to_string())
                            .await?;
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
                "📋 Streaming container logs for 15s...".to_string()
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
            let mut task_handle = lock_deployment_state(&self.log_stream_task, "log_stream_task");
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
                // External images come as a single tag; the platform check
                // reads the real architecture from the image itself.
                image_tags_by_platform: HashMap::new(),
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
        let deployment_output = match self
            .deploy_image(&image_output, &context, health_override)
            .await
        {
            Ok(output) => output,
            Err(deploy_error) => {
                if let Err(retention_error) = self.retain_failed_containers(&context).await {
                    // A container without a committed ownership row would be
                    // unreachable through the authenticated API. Tear it down
                    // rather than leaking an untracked runtime candidate.
                    let cleanup_error = self.cleanup_container(&context).await.err();
                    return Err(WorkflowError::JobExecutionFailed(format!(
                        "{deploy_error}; additionally failed to retain candidate containers for log inspection: {retention_error}; cleanup: {}",
                        cleanup_error
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "completed".to_string())
                    )));
                }
                return Err(deploy_error);
            }
        };

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
        // Consumed by MarkDeploymentCompleteJob to record what each container
        // actually runs; without it every replica of a mixed-architecture
        // deployment is stored under the primary tag.
        context.set_output(&self.job_id, "image_names", &deployment_output.image_names)?;

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

                if let Err(error) = self.discard_failed_candidates(&context).await {
                    tracing::error!(
                        deployment_id = context.deployment_id,
                        error = %error,
                        "Failed to fully discard app containers after deployment cancellation"
                    );
                }

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
        if self.retained_failure.load(Ordering::Acquire) {
            return self.stop_background_log_stream(context).await;
        }
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
    config_service: Option<Arc<temps_config::ConfigService>>,
    image_builder: Option<Arc<dyn temps_deployer::ImageBuilder>>,
    failed_container_db: Option<Arc<DbConnection>>,
    deployment_id: Option<i32>,
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
            config_service: None,
            image_builder: None,
            failed_container_db: None,
            deployment_id: None,
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

    /// Explicit port override from the environment/project scope. When
    /// `Some`, `resolve_container_port()` uses it directly and skips image
    /// `EXPOSE` auto-detection entirely.
    pub fn configured_port(mut self, configured_port: Option<u16>) -> Self {
        self.config.configured_port = configured_port;
        self
    }

    pub fn environment_variables(mut self, env_vars: HashMap<String, String>) -> Self {
        self.config.environment_variables = env_vars;
        self
    }

    pub fn command(mut self, command: Option<Vec<String>>) -> Self {
        self.config.command = command;
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

    /// Set the linked services that cannot be reached from another node.
    /// A remotely-scheduled replica fails with these rather than silently
    /// receiving an unusable connection string.
    pub fn cross_node_service_blockers(
        mut self,
        blockers: Vec<crate::services::CrossNodeServiceBlocker>,
    ) -> Self {
        self.config.cross_node_service_blockers = blockers;
        self
    }

    /// Set the encryption service for decrypting node tokens during remote deployments
    pub fn encryption_service(mut self, service: Arc<temps_core::EncryptionService>) -> Self {
        self.encryption_service = Some(service);
        self
    }

    /// Set the config service (resolves the cluster CA for mTLS to https:// nodes)
    pub fn config_service(mut self, service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(service);
        self
    }

    /// Set the local image builder for transferring images to remote nodes
    pub fn image_builder(mut self, builder: Arc<dyn temps_deployer::ImageBuilder>) -> Self {
        self.image_builder = Some(builder);
        self
    }

    /// Enable durable retention of failed app candidates for authenticated
    /// runtime-log inspection.
    pub fn failed_container_retention(mut self, db: Arc<DbConnection>, deployment_id: i32) -> Self {
        self.failed_container_db = Some(db);
        self.deployment_id = Some(deployment_id);
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
        if let Some(config_service) = self.config_service {
            job = job.with_config_service(config_service);
        }
        if let Some(image_builder) = self.image_builder {
            job = job.with_image_builder(image_builder);
        }
        if let (Some(db), Some(deployment_id)) = (self.failed_container_db, self.deployment_id) {
            job = job.with_failed_container_retention(db, deployment_id);
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

    struct TestLogWriter;

    #[async_trait]
    impl temps_core::LogWriter for TestLogWriter {
        async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            1
        }
    }

    #[test]
    fn poisoned_deployment_state_is_recovered_for_cleanup() {
        let state = Arc::new(Mutex::new(vec!["candidate".to_string()]));
        let poison_target = Arc::clone(&state);
        let _ = std::panic::catch_unwind(move || {
            let _guard = poison_target.lock().expect("initial lock");
            panic!("poison deployment state for regression coverage");
        });

        let mut recovered = lock_deployment_state(&state, "test_state");
        recovered.push("cleanup".to_string());

        assert_eq!(recovered.as_slice(), ["candidate", "cleanup"]);
    }

    fn build_output_with_tags(tags: &[(&str, &str)]) -> BuildImageOutput {
        BuildImageOutput {
            image_tag: "myapp:latest".to_string(),
            image_id: "sha256:abc".to_string(),
            size_bytes: 1,
            build_context: PathBuf::from("/tmp"),
            dockerfile_path: PathBuf::from("/tmp/Dockerfile"),
            image_tags_by_platform: tags
                .iter()
                .map(|(p, t)| (p.to_string(), t.to_string()))
                .collect(),
        }
    }

    fn blocker(
        reason: temps_providers::CrossNodeBlockReason,
    ) -> crate::services::CrossNodeServiceBlocker {
        crate::services::CrossNodeServiceBlocker {
            service_id: 7,
            service_name: "orders-db".to_string(),
            fqdn: Some("orders-db.temps.local".to_string()),
            detail: reason.detail("orders-db"),
            remedy: reason.remedy().to_string(),
            setup_path: reason.setup_path(7),
        }
    }

    /// Default (and overwhelmingly common) case: nothing blocks the replica,
    /// so a remote deployment proceeds exactly as before.
    #[test]
    fn no_blockers_never_fails_a_remote_replica() {
        assert!(cross_node_unreachable_error("worker-1", &[]).is_none());
        assert!(DeploymentJobConfig::default()
            .cross_node_service_blockers
            .is_empty());
    }

    /// The whole point of the guard: the operator gets the node, the
    /// service, the reason, and the fix — not a container that silently
    /// cannot reach its database.
    #[test]
    fn blocked_remote_replica_fails_with_an_actionable_message() {
        let error = cross_node_unreachable_error(
            "worker-1",
            &[blocker(
                temps_providers::CrossNodeBlockReason::ClusterDnsDisabled,
            )],
        )
        .expect("a blocker must produce an error");

        assert!(matches!(
            error,
            WorkflowError::CrossNodeServiceUnreachable {
                blocker_count: 1,
                ..
            }
        ));

        let message = error.to_string();
        assert!(message.contains("worker-1"), "{message}");
        assert!(message.contains("orders-db"), "{message}");
        assert!(message.contains("Cluster DNS is disabled"), "{message}");
        assert!(message.contains("Enable cluster DNS"), "{message}");
    }

    #[test]
    fn every_blocked_service_is_named_in_the_failure() {
        let error = cross_node_unreachable_error(
            "worker-2",
            &[
                blocker(temps_providers::CrossNodeBlockReason::ClusterDnsDisabled),
                crate::services::CrossNodeServiceBlocker {
                    service_id: 9,
                    service_name: "cache".to_string(),
                    ..blocker(temps_providers::CrossNodeBlockReason::DnsRecordMissing)
                },
            ],
        )
        .expect("blockers must produce an error");

        let message = error.to_string();
        assert!(message.contains("2 linked service(s)"), "{message}");
        assert!(message.contains("orders-db"), "{message}");
        assert!(message.contains("cache"), "{message}");
    }

    #[test]
    fn remote_app_ports_only_bind_private_interfaces() {
        for address in ["10.0.0.8", "172.20.0.5", "192.168.1.10", "fd00::5"] {
            assert_eq!(
                private_remote_bind_address(address).expect("private address should be accepted"),
                address
            );
        }

        let public = private_remote_bind_address("203.0.113.10")
            .expect_err("public worker address must not expose an app port");
        assert!(public.to_string().contains("outside the Temps proxy"));
        assert!(private_remote_bind_address("worker.example.com").is_err());

        let expected = PortMapping {
            host_port: 18080,
            container_port: 3000,
            protocol: Protocol::Tcp,
            host_ip: Some("10.0.0.8".to_string()),
        };
        assert!(confirms_private_port_binding(
            std::slice::from_ref(&expected),
            3000,
            18080,
            "10.0.0.8"
        ));

        let legacy_agent = PortMapping {
            host_ip: None,
            ..expected.clone()
        };
        assert!(!confirms_private_port_binding(
            &[legacy_agent],
            3000,
            18080,
            "10.0.0.8"
        ));
        let public_binding = PortMapping {
            host_ip: Some("0.0.0.0".to_string()),
            ..expected
        };
        assert!(!confirms_private_port_binding(
            &[public_binding],
            3000,
            18080,
            "10.0.0.8"
        ));
    }

    /// Single-arch build: every node gets the one tag, whatever it reports.
    #[test]
    fn test_tag_for_platform_without_multi_arch_build() {
        let output = build_output_with_tags(&[]);
        assert_eq!(output.tag_for_platform(None), "myapp:latest");
        assert_eq!(output.tag_for_platform(Some("linux/arm64")), "myapp:latest");
    }

    /// Multi-arch build: each node must receive the image built for it.
    #[test]
    fn test_tag_for_platform_selects_the_matching_image() {
        let output = build_output_with_tags(&[
            ("linux/amd64", "myapp:latest"),
            ("linux/arm64", "myapp:latest-arm64"),
        ]);

        assert_eq!(output.tag_for_platform(Some("linux/amd64")), "myapp:latest");
        assert_eq!(
            output.tag_for_platform(Some("linux/arm64")),
            "myapp:latest-arm64"
        );
        // Equivalent spellings resolve to the same image.
        assert_eq!(
            output.tag_for_platform(Some("linux/aarch64")),
            "myapp:latest-arm64"
        );
    }

    /// A node whose platform we don't know, or one we didn't build for, falls
    /// back to the primary tag — the deploy path then runs the explicit
    /// architecture check before transferring anything.
    #[test]
    fn test_tag_for_platform_falls_back_to_primary_tag() {
        let output = build_output_with_tags(&[
            ("linux/amd64", "myapp:latest"),
            ("linux/arm64", "myapp:latest-arm64"),
        ]);

        assert_eq!(output.tag_for_platform(None), "myapp:latest");
        assert_eq!(
            output.tag_for_platform(Some("linux/riscv64")),
            "myapp:latest"
        );
    }

    /// Minimal `ImageBuilder` that only answers "what platform do I run".
    struct PlatformOnlyImageBuilder {
        platform: String,
        /// Platform the daemon confirmed, if any. `None` models a control
        /// plane whose `docker info` hasn't answered yet.
        discovered: Option<String>,
        /// Platform `inspect_image` reports for any tag.
        image_platform: Option<String>,
        /// What a discovery attempt would return. `None` models a daemon that
        /// still doesn't answer.
        discoverable: Option<String>,
    }

    impl PlatformOnlyImageBuilder {
        fn confirmed(platform: &str, image_platform: &str) -> Self {
            Self {
                platform: platform.to_string(),
                discovered: Some(platform.to_string()),
                image_platform: Some(image_platform.to_string()),
                discoverable: None,
            }
        }

        /// A daemon whose platform isn't cached yet but answers when asked —
        /// the state an upload/external-image deploy starts in, since nothing
        /// on that path runs a build.
        fn discoverable_on_demand(fallback: &str, daemon: &str, image_platform: &str) -> Self {
            Self {
                platform: fallback.to_string(),
                discovered: None,
                image_platform: Some(image_platform.to_string()),
                discoverable: Some(daemon.to_string()),
            }
        }
    }

    #[async_trait]
    impl temps_deployer::ImageBuilder for PlatformOnlyImageBuilder {
        async fn build_image(
            &self,
            _request: temps_deployer::BuildRequest,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn build_image_with_callback(
            &self,
            _request: temps_deployer::BuildRequestWithCallback,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn remove_image(
            &self,
            _image_name: &str,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("not used")
        }

        async fn inspect_image(
            &self,
            image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
            let Some(platform) = self.image_platform.clone() else {
                return Err(temps_deployer::BuilderError::ImageNotFound(
                    image_name.to_string(),
                ));
            };
            Ok(temps_deployer::ImageInfo {
                id: format!("sha256:{image_name}"),
                architecture: temps_deployer::platform::platform_arch(&platform),
                os: "linux".to_string(),
                platform,
                size_bytes: 1,
                tags: vec![image_name.to_string()],
                created: None,
                working_dir: None,
            })
        }

        fn get_native_platform(&self) -> String {
            self.platform.clone()
        }

        fn discovered_platform(&self) -> Option<String> {
            self.discovered.clone()
        }

        async fn ensure_platform_discovered(&self) -> Option<String> {
            self.discovered
                .clone()
                .or_else(|| self.discoverable.clone())
        }
    }

    fn job_with_image_builder(builder: PlatformOnlyImageBuilder) -> DeployImageJob {
        let container_deployer: Arc<dyn ContainerDeployer> =
            Arc::new(TrackingMockContainerDeployer::new());
        DeployImageJobBuilder::new()
            .job_id("deploy".to_string())
            .build_job_id("build".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("app".to_string())
            .namespace("default".to_string())
            .image_builder(Arc::new(builder))
            .build(container_deployer)
            .unwrap()
    }

    fn job_with_local_platform(platform: &str) -> DeployImageJob {
        job_with_image_builder(PlatformOnlyImageBuilder {
            platform: platform.to_string(),
            discovered: Some(platform.to_string()),
            image_platform: None,
            discoverable: None,
        })
    }

    /// Each replica's record must name the image that replica actually runs.
    /// `MarkDeploymentCompleteJob` reads the `image_names` output when writing
    /// `deployment_containers`; without it every row falls back to the
    /// deployment's primary tag, so on a mixed fleet the node and deployment
    /// APIs would report ARM replicas as running the amd64 image.
    #[test]
    fn test_deployment_output_carries_a_tag_per_replica() {
        let output = DeploymentOutput {
            status: DeploymentStatus::Running,
            replicas: 2,
            resources: ResourceUsage::default(),
            container_ids: vec!["c1".to_string(), "c2".to_string()],
            host_ports: vec![30001, 30002],
            container_port: 3000,
            node_ids: vec![None, Some(7)],
            image_names: vec!["app:latest".to_string(), "app:latest-arm64".to_string()],
        };

        // Parallel to container_ids, which is how the consumer indexes them.
        assert_eq!(output.image_names.len(), output.container_ids.len());
        assert_eq!(output.image_names[1], "app:latest-arm64");

        // And it survives the workflow context round-trip the job performs.
        let encoded = serde_json::to_value(&output.image_names).unwrap();
        let decoded: Vec<String> = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, output.image_names);
    }

    /// Older workflow contexts have no `image_names`; deserialization must not
    /// break for a deployment that was in flight across an upgrade.
    #[test]
    fn test_deployment_output_without_image_names_still_parses() {
        let legacy = serde_json::json!({
            "status": "Running",
            "replicas": 1,
            "resources": {},
            "container_ids": ["c1"],
            "host_ports": [30001],
            "container_port": 3000
        });

        let output: DeploymentOutput = serde_json::from_value(legacy).unwrap();
        assert!(output.image_names.is_empty());
        assert!(output.node_ids.is_empty());
    }

    /// Remote replicas are checked before the image is transferred; local ones
    /// had no equivalent guard, so an image built for another architecture
    /// reached the control plane's Docker and failed as a container that won't
    /// start — with nothing in the log about architecture.
    #[tokio::test]
    async fn test_local_deploy_refuses_an_image_for_another_architecture() {
        let job = job_with_image_builder(PlatformOnlyImageBuilder::confirmed(
            "linux/amd64",
            "linux/arm64",
        ));
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let err = job
            .verify_image_platform_for_local("app:latest-arm64", Some("linux/amd64"), &context)
            .await
            .expect_err("an arm64 image must not be deployed on an amd64 control plane");

        let message = err.to_string();
        assert!(message.contains("linux/arm64"), "got: {message}");
        assert!(message.contains("linux/amd64"), "got: {message}");
        assert!(message.contains("exec format error"), "got: {message}");
    }

    #[tokio::test]
    async fn test_local_deploy_accepts_a_matching_image() {
        let job = job_with_image_builder(PlatformOnlyImageBuilder::confirmed(
            "linux/amd64",
            "linux/amd64",
        ));
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        assert!(job
            .verify_image_platform_for_local("app:latest", Some("linux/amd64"), &context)
            .await
            .is_ok());
    }

    /// While the control plane's own platform is unconfirmed, comparing
    /// against the compiled-in fallback could reject a perfectly good image.
    /// Unknown means "proceed", as it did before multi-arch support.
    #[tokio::test]
    async fn test_local_deploy_skips_the_check_when_the_platform_is_unknown() {
        let job = job_with_image_builder(PlatformOnlyImageBuilder {
            platform: "linux/amd64".to_string(),
            discovered: None,
            image_platform: Some("linux/arm64".to_string()),
            discoverable: None,
        });
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        assert!(job
            .verify_image_platform_for_local("app:latest-arm64", None, &context)
            .await
            .is_ok());
    }

    /// The "scheduling failed, deploy locally" fallback is a degradation, not
    /// a licence to run an image the control plane cannot execute. An uploaded
    /// image is accepted when *any* node matches its architecture, so a
    /// remote-only image can reach this path — and starting it here would
    /// reproduce the `exec format error` this feature exists to prevent.
    #[tokio::test]
    async fn test_local_fallback_refuses_an_image_the_control_plane_cannot_run() {
        let job = job_with_local_platform("linux/amd64");
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let err = job
            .ensure_local_can_run(&["linux/arm64".to_string()], &context, "database timeout")
            .await
            .expect_err("an arm64-only image must not fall back onto an amd64 control plane");

        let message = err.to_string();
        assert!(message.contains("linux/arm64"), "got: {message}");
        assert!(message.contains("linux/amd64"), "got: {message}");
        // The operator needs to know why the fallback happened at all.
        assert!(message.contains("database timeout"), "got: {message}");
    }

    #[tokio::test]
    async fn test_local_fallback_allowed_when_the_image_matches() {
        let job = job_with_local_platform("linux/amd64");
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        // Same architecture, and the multi-arch case where one of the built
        // platforms is the control plane's.
        assert!(job
            .ensure_local_can_run(&["linux/amd64".to_string()], &context, "whatever")
            .await
            .is_ok());
        assert!(job
            .ensure_local_can_run(
                &["linux/arm64".to_string(), "linux/x86_64".to_string()],
                &context,
                "whatever"
            )
            .await
            .is_ok());
    }

    /// An upload or external-image deploy never runs a build, so the daemon's
    /// platform may still be undiscovered when the local fallback is
    /// considered. Judging that on the binary's architecture would approve the
    /// fallback — and the local verification stays quiet while the platform is
    /// unknown, so the container would reach `exec format error` unchallenged.
    /// Discovery has to happen here.
    #[tokio::test]
    async fn test_local_fallback_discovers_the_platform_before_authorising() {
        // Binary says amd64; the daemon behind DOCKER_HOST is arm64 and will
        // say so when asked. The image is amd64-only.
        let job = job_with_image_builder(PlatformOnlyImageBuilder::discoverable_on_demand(
            "linux/amd64",
            "linux/arm64",
            "linux/amd64",
        ));
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let err = job
            .ensure_local_can_run(&["linux/amd64".to_string()], &context, "database timeout")
            .await
            .expect_err("the daemon is arm64, so an amd64-only image cannot run here");

        let message = err.to_string();
        assert!(message.contains("linux/arm64"), "got: {message}");
    }

    /// Unknown platforms must not start blocking deployments that worked
    /// before this feature existed.
    #[tokio::test]
    async fn test_local_fallback_allowed_when_platforms_are_unknown() {
        let job = job_with_local_platform("linux/amd64");
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        assert!(job
            .ensure_local_can_run(&[], &context, "whatever")
            .await
            .is_ok());
    }

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
        stopped_containers: Arc<StdMutex<Vec<String>>>,
    }

    impl TrackingMockContainerDeployer {
        fn new() -> Self {
            Self {
                deployed_containers: Arc::new(StdMutex::new(Vec::new())),
                stopped_containers: Arc::new(StdMutex::new(Vec::new())),
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

        async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
            self.stopped_containers
                .lock()
                .unwrap()
                .push(container_id.to_string());
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
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );

        let job = DeployImageJobBuilder::new()
            .job_id("test_deploy".to_string())
            .build_job_id("build_image".to_string())
            .target(target)
            .service_name("myapp".to_string())
            .namespace("production".to_string())
            .replicas(3)
            .environment_variables(env_vars)
            .failed_container_retention(db, 42)
            .build(container_deployer)
            .unwrap();

        assert_eq!(job.job_id(), "test_deploy");
        assert_eq!(job.build_job_id, "build_image");
        assert_eq!(job.config.service_name, "myapp");
        assert_eq!(job.config.namespace, "production");
        assert_eq!(job.deployment_id, Some(42));
        assert!(job.failed_container_db.is_some());
        assert_eq!(job.config.replicas, 3);
        assert_eq!(job.depends_on(), vec!["build_image".to_string()]);
    }

    #[tokio::test]
    async fn failed_app_container_is_registered_without_becoming_ready() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let deployer = Arc::new(TrackingMockContainerDeployer::new());
        let job = DeployImageJobBuilder::new()
            .job_id("deploy".to_string())
            .build_job_id("build".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("checkout".to_string())
            .failed_container_retention(db.clone(), 42)
            .build(deployer.clone())
            .expect("valid deploy job");
        job.failed_candidates
            .lock()
            .expect("candidate lock")
            .push(FailedContainerCandidate {
                container_id: "failed-app-id".to_string(),
                container_name: "checkout-production".to_string(),
                container_port: 3000,
                host_port: 18080,
                image_name: "checkout:broken".to_string(),
                node_id: Some(7),
            });
        let context = WorkflowContext::new("run-42".to_string(), 42, 2, 3, Arc::new(TestLogWriter));

        job.retain_failed_containers(&context)
            .await
            .expect("failed candidate should remain available for logs");
        assert!(job.retained_failure.load(Ordering::Acquire));
        assert_eq!(
            deployer.stopped_containers.lock().unwrap().as_slice(),
            ["failed-app-id"],
            "retained failures must be stopped before their ownership rows are committed"
        );

        drop(job);
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still has owners"));
        let transaction_log = db.into_transaction_log();
        let rendered = transaction_log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .filter(|statement| {
                statement
                    .sql
                    .contains("INSERT INTO \"deployment_containers\"")
            })
            .map(|statement| format!("{statement:?}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("failed-app-id"));
        assert!(rendered.contains("checkout-production"));
        assert!(rendered.contains("retained:stopped-after-failed-readiness"));
        assert!(rendered.contains("checkout:broken"));
        assert!(rendered.contains("18080"));
        assert!(rendered.contains("3000"));
        assert!(
            rendered.contains("Some(7)"),
            "node ownership missing: {rendered}"
        );
        assert!(
            !rendered.contains("ready_at\", Some"),
            "failed candidates must never be routable: {rendered}"
        );
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

    /// Regression test for https://github.com/gotempsh/temps/issues/879:
    /// an explicit environment/project port override must win over image
    /// EXPOSE auto-detection, not the other way around. The image tag here
    /// doesn't exist, so any attempt to consult it would fail; the override
    /// must be returned without ever needing a successful inspection.
    #[tokio::test]
    async fn test_resolve_container_port_prefers_explicit_override_over_image_detection() {
        let job = DeployImageJobBuilder::new()
            .job_id("deploy".to_string())
            .build_job_id("build_image".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("app".to_string())
            .port(3000)
            .configured_port(Some(9090))
            .build(Arc::new(TrackingMockContainerDeployer::new()))
            .unwrap();

        let context = crate::test_utils::create_test_context("run-1".to_string(), 1, 1, 1);

        let port = job
            .resolve_container_port("temps-test-nonexistent-image:latest", &context)
            .await;

        assert_eq!(port, 9090);
    }

    /// When neither environment nor project configures a port, image
    /// inspection is attempted; if it fails (as it always will here, since
    /// the image tag doesn't exist), resolution falls back to the
    /// configured/default port.
    #[tokio::test]
    async fn test_resolve_container_port_falls_back_to_default_without_override() {
        let job = DeployImageJobBuilder::new()
            .job_id("deploy".to_string())
            .build_job_id("build_image".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name("app".to_string())
            .port(4000)
            .build(Arc::new(TrackingMockContainerDeployer::new()))
            .unwrap();

        assert_eq!(job.config.configured_port, None);

        let context = crate::test_utils::create_test_context("run-1".to_string(), 1, 1, 1);

        let port = job
            .resolve_container_port("temps-test-nonexistent-image:latest", &context)
            .await;

        assert_eq!(port, 4000);
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

    #[test]
    fn explicit_placement_constraints_are_fail_closed_for_every_scheduler_error() {
        assert!(!has_explicit_placement_constraints(None, None));
        assert!(!has_explicit_placement_constraints(
            None,
            Some(&serde_json::json!({}))
        ));
        // Empty selectors of either kind name nothing, so they constrain
        // nothing — the two must agree, and an empty label object has always
        // read that way.
        assert!(!has_explicit_placement_constraints(Some(&[]), None));
        assert!(has_explicit_placement_constraints(Some(&[1]), None));
        assert!(has_explicit_placement_constraints(
            None,
            Some(&serde_json::json!({"region": "eu"}))
        ));
        assert!(has_explicit_placement_constraints(
            None,
            Some(&serde_json::json!(["malformed"]))
        ));
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
                architecture: None,
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
                dns_resolver_running: None,
                dns_resolver_tasks_alive: None,
                dns_resolver_last_sync_at: None,
                dns_resolver_consecutive_failures: 0,
                dns_resolver_last_error: None,
                dns_resolver_record_count: None,
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
                architecture: None,
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
                dns_resolver_running: None,
                dns_resolver_tasks_alive: None,
                dns_resolver_last_sync_at: None,
                dns_resolver_consecutive_failures: 0,
                dns_resolver_last_error: None,
                dns_resolver_record_count: None,
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

        // Explicit targets restrict the pool to worker-a(1) and worker-c(3).
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
                    panic!("explicit target nodes must exclude the control plane")
                }
            }
        }
    }

    /// Explicit target constraints must never silently fall back to local.
    #[tokio::test]
    async fn test_node_scheduling_target_nodes_no_match_fails_closed() {
        use crate::services::{NodeError, NodeScheduler, NodeService};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::nodes;

        let node = nodes::Model {
            architecture: None,
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
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
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
        let error = scheduler
            .schedule_replicas(2, None, Some(&target_ids), false)
            .await
            .expect_err("an unmatched explicit target must not run on the control plane");
        match error {
            NodeError::PlacementConstraintsUnsatisfied { excluded } => {
                assert!(
                    excluded.contains("no active node matched"),
                    "unexpected placement diagnostic: {excluded}"
                );
            }
            other => {
                panic!("expected PlacementConstraintsUnsatisfied, got {other:?}");
            }
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
            platform: None,
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
                platform: None,
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
            architecture: None,
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
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
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
                platform: None,
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
            architecture: None,
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
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
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
                platform: None,
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
