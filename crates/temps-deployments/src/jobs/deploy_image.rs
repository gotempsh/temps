// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deploy Image Job
//!
//! Deploys built container images to target environments

use super::image_source::{DeployImageSource, ExpectedImageIdentity};
use super::node_image::{copy_node_built_image, NodeImageCopy};
use crate::workflow::JobResult;
use crate::workflow::WorkflowCancellationProvider;
use crate::workflow::WorkflowContext;
use crate::workflow::WorkflowError;
use crate::workflow::WorkflowTask;
use async_trait::async_trait;
use futures::StreamExt;
use sea_orm::{sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use temps_database::DbConnection;
use temps_deployer::{
    ContainerDeployer, ContainerLogConfig, ContainerStatus as DeployerContainerStatus,
    DeployRequest, ImageBuilder, PortMapping, Protocol, ResourceLimits, RestartPolicy,
};
use temps_deployment_logs::{LogLevel, LogService};
use temps_entities::deployment_containers;
use temps_entities::environment_workload_mode::EnvironmentWorkloadMode;
use tokio::time::{sleep, Duration};

mod replica;

/// An image a node holds under a tag, resolved for a platform the node does
/// not run.
struct NodeImagePlatformMismatch {
    /// The platform the node resolves the tag to.
    resolved: String,
    /// The platform the node runs.
    node: String,
}

fn verify_worker_image_platform(
    image: &str,
    built: &str,
    target: Option<&str>,
    node: &str,
) -> Result<(), WorkflowError> {
    if target.is_some_and(|target| temps_deployer::platform::platforms_match(built, target)) {
        return Ok(());
    }
    Err(WorkflowError::JobValidationFailed(format!(
        "Worker-built image '{image}' is {built}, but node '{node}' reports {}; select compatible target nodes or rebuild for their architecture",
        target.unwrap_or("no architecture")
    )))
}

/// Prefix of the placeholder `image_id` an external-image deploy carries: it
/// names no real image, so it is never an identity to verify against.
const EXTERNAL_IMAGE_ID_PREFIX: &str = "external-";

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
    /// The worker that built this image. `None` means it is local or registry sourced.
    #[serde(default)]
    pub builder_node_id: Option<i32>,
    /// For a mixed-architecture worker build, the worker holding each
    /// platform's image, keyed like `image_tags_by_platform`. Empty when one
    /// node (`builder_node_id`) or the control plane holds every tag.
    #[serde(default)]
    pub builder_node_ids_by_platform: HashMap<String, i32>,
    /// For a mixed-architecture worker build, the id each worker reported
    /// for the image it built, keyed like `image_tags_by_platform`.
    #[serde(default)]
    pub image_ids_by_platform: HashMap<String, String>,
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
        let builder_node_id: Option<i32> = context
            .get_output(build_job_id, "builder_node_id")?
            .flatten();
        if builder_node_id.is_some() && image_tags_by_platform.is_empty() {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Worker build '{build_job_id}' has no verified platform metadata; rebuild before deployment"
            )));
        }
        let builder_node_ids_by_platform: HashMap<String, i32> = context
            .get_output(build_job_id, "builder_node_ids_by_platform")?
            .unwrap_or_default();
        let image_ids_by_platform: HashMap<String, String> = context
            .get_output(build_job_id, "image_ids_by_platform")?
            .unwrap_or_default();

        Ok(Self {
            image_tag,
            image_id,
            size_bytes,
            build_context: PathBuf::from(build_context_str),
            dockerfile_path: PathBuf::from(dockerfile_path_str),
            image_tags_by_platform,
            builder_node_id,
            builder_node_ids_by_platform,
            image_ids_by_platform,
        })
    }

    /// The image id recorded at build time for a worker-built `tag`: each
    /// architecture's id for a mixed-architecture build, and the build's own
    /// `image_id` for the primary tag of any worker build (single-architecture
    /// ones record no per-platform ids). `None` for a control-plane build or
    /// an external image, whose identity is checked elsewhere.
    pub fn expected_image_id_for_tag(&self, tag: &str) -> Option<&str> {
        self.image_tags_by_platform
            .iter()
            .find(|(_, built)| built.as_str() == tag)
            .and_then(|(platform, _)| self.image_ids_by_platform.get(platform))
            .map(String::as_str)
            .or_else(|| {
                (self.builder_node_id.is_some()
                    && tag == self.image_tag
                    && !self.image_id.trim().is_empty()
                    && !self.image_id.starts_with(EXTERNAL_IMAGE_ID_PREFIX))
                .then_some(self.image_id.as_str())
            })
    }

    /// The worker holding `tag`: the one that built that tag's platform on a
    /// mixed-architecture worker build, otherwise the single builder. Keyed
    /// by tag rather than by node platform so the source can never disagree
    /// with the tag [`Self::tag_for_platform`] chose.
    pub fn builder_node_for_tag(&self, tag: &str) -> Option<i32> {
        self.image_tags_by_platform
            .iter()
            .find(|(_, built)| built.as_str() == tag)
            .and_then(|(platform, _)| self.builder_node_ids_by_platform.get(platform))
            .copied()
            .or(self.builder_node_id)
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

/// The worker-built images a rollback or promotion reuses: what the origin
/// deployment's `BuildImageJob` recorded about where each tag lives.
///
/// Without it a reused image is a single tag the control plane would have to
/// hold, so a rollback of a mixed-architecture deployment would hand every
/// node the primary architecture's tag, and in the control-plane profile
/// (no daemon to export from) fail on any node that does not already have it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReusedWorkerBuild {
    /// Image ID of the primary tag, as built.
    pub image_id: Option<String>,
    pub image_tags_by_platform: HashMap<String, String>,
    pub builder_node_id: Option<i32>,
    pub builder_node_ids_by_platform: HashMap<String, i32>,
    /// The id of each architecture's image as built; a reused tag is only
    /// fetched from a worker still holding exactly this image.
    pub image_ids_by_platform: HashMap<String, String>,
    /// The image's configuration as the build node reported it. A rollback or
    /// promotion has no workflow context from the original build and the
    /// control plane may hold no copy to inspect, so the ports come from here.
    pub image_config: Option<super::build_image::BuiltImageConfig>,
}

impl ReusedWorkerBuild {
    /// Read a `BuildImageJob`'s saved outputs, if that build ran on a worker
    /// and produced `image_tag` as its primary tag. `None` for a control
    /// plane build, a registry image, or outputs of another tag -- the
    /// reused deployment then keeps its single-tag behaviour.
    pub fn from_build_outputs(outputs: &serde_json::Value, image_tag: &str) -> Option<Self> {
        if outputs.get("image_tag").and_then(|value| value.as_str()) != Some(image_tag) {
            return None;
        }
        let builder_node_id = outputs
            .get("builder_node_id")
            .and_then(|value| value.as_i64())
            .and_then(|value| i32::try_from(value).ok())?;
        let image_tags_by_platform: HashMap<String, String> = outputs
            .get("image_tags_by_platform")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        // A worker build always records its verified platform; without it the
        // deploy would refuse the image anyway (see `BuildImageOutput`).
        if image_tags_by_platform.is_empty() {
            return None;
        }
        let builder_node_ids_by_platform: HashMap<String, i32> = outputs
            .get("builder_node_ids_by_platform")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let image_ids_by_platform: HashMap<String, String> = outputs
            .get("image_ids_by_platform")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        // Several builders without the id of each image is a build recorded
        // before ids were: refuse to trust where its tags now live.
        if builder_node_ids_by_platform.len() > 1
            && builder_node_ids_by_platform
                .keys()
                .any(|platform| !image_ids_by_platform.contains_key(platform))
        {
            return None;
        }
        Some(Self {
            image_id: outputs
                .get("image_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            image_tags_by_platform,
            builder_node_id: Some(builder_node_id),
            builder_node_ids_by_platform,
            image_ids_by_platform,
            image_config: outputs
                .get("image_config")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok()),
        })
    }

    /// Whether this build produced one image per architecture on several
    /// workers.
    pub fn is_per_architecture(&self) -> bool {
        self.builder_node_ids_by_platform.len() > 1
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
    pub container_port: Option<u16>,
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

/// Point the deployment-owned `PORT` variable at the port the container is
/// actually routed to, returning the value it replaced.
///
/// The planner sets `PORT` before the image exists, from the configured port
/// or the 3000 fallback. When no port is configured, the deploy job routes to
/// the image's `EXPOSE` port instead, so without this an app that listens on
/// `$PORT` would bind to 3000 while traffic and health checks go elsewhere.
/// An explicitly configured port is left alone: it already drives both
/// values. Only an existing `PORT` is rewritten — deployments the planner gave
/// no `PORT` keep having none.
fn align_port_env_with_container_port(
    environment_vars: &mut HashMap<String, String>,
    configured_port: Option<u16>,
    container_port: u16,
) -> Option<String> {
    if configured_port.is_some() {
        return None;
    }
    let port = environment_vars.get_mut("PORT")?;
    let container_port = container_port.to_string();
    if *port == container_port {
        return None;
    }
    Some(std::mem::replace(port, container_port))
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
    pub entrypoint: Option<Vec<String>>,
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
    pub health: Option<temps_core::repo_config::HealthCheckConfig>,
    /// Optional list of node IDs to deploy to. When set, replicas are distributed
    /// only across these specific nodes. When None, all active nodes are eligible.
    pub target_nodes: Option<Vec<i32>>,
    /// Label selector for node-based scheduling. Nodes whose labels match
    /// the selector are eligible. Applied after `target_nodes` filtering.
    pub target_labels: Option<serde_json::Value>,
    /// Slug of the project being deployed.
    ///
    /// Carried for exactly one reason (ADR 045): it is sent to the executing
    /// host in the `DeployRequest` so that host can compare it against its own
    /// `TEMPS_DOCKER_SOCKET_PROJECTS`, and it gates placement so a granted
    /// project never lands on a host that would start it without the socket.
    /// `None` disables both, which is the correct behaviour for job configs
    /// built outside a project context (and for tests).
    pub project_slug: Option<String>,
    /// Whether **this control plane** declares that the project requires the
    /// host Docker socket (ADR 045).
    ///
    /// Computed once, from this process's own grant, when the job is built —
    /// see [`DeployImageJobBuilder::new`] — and sent to the executing host in
    /// the `DeployRequest`. It is the control plane's half of the mount
    /// decision; the host still answers from its own environment. Without it
    /// a worker would mount the socket from its local grant alone, for a slug
    /// nobody declared and which therefore passed neither the admin-only
    /// claim guard nor the placement gate.
    ///
    /// `pub(crate)`, not `pub`: it is set exactly once, inside
    /// [`DeployImageJobBuilder::new`], from this process's own grant --
    /// never from a caller-supplied value. A `pub` setter would let code
    /// outside this crate construct a config that claims control-plane
    /// authorization it was never given.
    pub(crate) control_plane_grants_socket: bool,
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
    /// [`crate::workflow::WorkflowError::CrossNodeServiceUnreachable`]. Local
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
    /// The environment's ADR-055 mode. `Legacy` (the default) is today's
    /// deploy path, unchanged; `Shadow` also asks the unified scheduler and
    /// logs whether it agrees; `Enforce` places with the unified scheduler
    /// and starts control-plane replicas through the workload plane.
    pub workload_mode: temps_entities::environment_workload_mode::EnvironmentWorkloadMode,
    /// The environment's attached volumes (ADR-055 Phase 9), sources
    /// resolved. Empty for every environment without one, which deploys
    /// exactly as before. Mounted only in `enforce`, one replica, on the
    /// control plane; anything else fails the deployment with the reason.
    pub volume_mounts: Vec<temps_workload::VolumeMount>,
    /// Where those volumes pin the replica, when any has a node yet.
    pub volume_pin: Option<temps_workload_volumes::VolumePin>,
    /// What `enforce` replicas ask the plane for (ADR-055 Phase 10, the
    /// environment's `workload_isolation`). `legacy` and `shadow` keep a
    /// container whatever it says.
    pub workload_isolation: temps_workload::Isolation,
}

/// One replica's placement, and the guarantee the scheduler attached to it.
///
/// A struct rather than two more positional arguments: `docker_socket_required`
/// (ADR 045) is not a detail of the deploy call, it is the thing the executing
/// host's self-report is checked against, and a bare `bool` at a seven-argument
/// call site is exactly how that check would end up inverted.
#[derive(Clone, Copy)]
struct ReplicaTarget<'a> {
    assignment: &'a crate::services::NodeAssignment,
    /// Whether the scheduler gated this placement on the host Docker socket.
    docker_socket_required: bool,
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
            entrypoint: None,
            secrets: HashMap::new(),
            resources: ResourceUsage::default(),
            health_check_path: Some("/".to_string()),
            health_check_path_override: None,
            ingress_enabled: false,
            ingress_host: None,
            health_check_timeout_secs: 300,
            health: None,
            target_nodes: None,
            target_labels: None,
            project_slug: None,
            control_plane_grants_socket: false,
            remote_environment_variables: None,
            cross_node_service_blockers: Vec::new(),
            anti_affinity: true,
            exclude_node_ids: Vec::new(),
            workload_mode: Default::default(),
            volume_mounts: Vec::new(),
            volume_pin: None,
            workload_isolation: temps_workload::Isolation::Process,
        }
    }
}

/// What a `shadow` or `enforce` deployment needs (ADR-055 Phase 5): the
/// unified scheduler's inputs, and the plane that runs control-plane
/// replicas in `enforce`. Absent for `legacy`, which never touches either.
#[derive(Clone)]
pub struct WorkloadPlaneDeploy {
    pub placement: Arc<crate::services::WorkloadPlacementService>,
    /// `None` when this process did not start a plane; `enforce` then fails
    /// with that reason instead of degrading to the legacy path.
    pub plane: Option<Arc<temps_workload_plane::WorkloadPlane>>,
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
    /// ADR-055: set for `shadow`/`enforce` environments only.
    workload_plane: Option<WorkloadPlaneDeploy>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    /// Container IDs stored as soon as containers are created for cleanup on failure
    container_ids: Arc<Mutex<Vec<String>>>,
    /// Per-replica deployers: maps container_id → deployer for cleanup on correct node
    replica_deployers: Arc<Mutex<HashMap<String, Arc<dyn ContainerDeployer>>>>,
    /// Audit sink for the ADR-045 "this deployment received the host Docker
    /// socket" record. `None` on installs with no audit sink wired (and in
    /// tests): the event is then logged, never silently dropped.
    audit_logger: Option<Arc<dyn temps_core::AuditLogger>>,
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
    /// Where the image lives, and so how a remote worker obtains it. Separate
    /// from `external_image_tag`, which only says "don't look up a build job":
    /// uploads, rollbacks and promotions hand over a tag directly yet may only
    /// exist on the control plane. `None` (job configs written before this
    /// field existed) is resolved by [`DeployImageSource::resolve`].
    image_source: Option<DeployImageSource>,
    /// The image this deployment is bound to, independent of its (mutable)
    /// tag. Set by rollback/promotion from the origin deployment's record;
    /// a normal deploy instead reads the image ID `PullExternalImageJob`
    /// resolved earlier in the same workflow. Only consulted before exporting
    /// the control plane's copy of a registry image to a worker.
    expected_image_identity: Option<ExpectedImageIdentity>,
    /// For a rollback/promotion of a worker-built image: where each
    /// architecture's tag lives. Only read with `external_image_tag`.
    reused_worker_build: Option<ReusedWorkerBuild>,
    /// Docker log rotation config to prevent unbounded log growth
    log_config: Option<ContainerLogConfig>,
    /// Encryption service for decrypting node tokens during remote deployments
    encryption_service: Option<Arc<temps_core::EncryptionService>>,
    /// Config service — resolves the cluster CA for mTLS to https:// nodes (WS-2.1)
    config_service: Option<Arc<temps_config::ConfigService>>,
    /// Local image builder — used to `save_image()` before transferring to remote nodes
    image_builder: Option<Arc<dyn temps_deployer::ImageBuilder>>,
    /// Registry credentials to forward to a worker's `POST /agent/images/pull`
    /// when the deployed image is registry-sourced
    /// ([`DeployImageSource::Registry`]). `None` when the registry needs no
    /// auth, or when the image is control-plane-local and this path is unused.
    registry_credentials: Option<temps_deployer::remote::RemotePullCredentials>,
    /// What each registry image's index offered, once asked: `Some` with its
    /// platforms, or `None` when it could not be read or listed none (the
    /// local store answers instead). Scheduling and every replica's node
    /// check ask for the same tag; on a daemon without a local index each
    /// lookup is a registry request, and an unreachable registry must not be
    /// waited on again for every replica.
    registry_index_platforms: Arc<Mutex<HashMap<String, Option<Vec<String>>>>>,
}

/// What the control plane's own Docker can tell a deploy job about an image.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ControlPlaneImage {
    /// The image is in the control plane's Docker and can be exported.
    Present,
    /// The control plane's Docker answered, but not with this image (pruned,
    /// never loaded, or the daemon could not be queried). Carries the reason.
    Absent(String),
    /// This process has no Docker daemon (control-plane serve profile).
    DockerUnavailable(String),
    /// No image builder is wired at all.
    NoImageBuilder,
}

#[derive(Debug, Clone)]
struct FailedContainerCandidate {
    container_id: String,
    container_name: String,
    container_port: Option<u16>,
    host_port: u16,
    image_name: String,
    node_id: Option<i32>,
}

/// Upper bound on any metadata file read from a `docker save` archive
/// (`manifest.json`, `index.json`). Real ones are a few KiB; this only stops
/// a malformed archive from being buffered whole.
const MAX_SAVED_ARCHIVE_METADATA_BYTES: u64 = 4 * 1024 * 1024;

/// Check that, in a `docker save` archive, the exported tag `image_tag`
/// names the image `expected_id` — the ID the control plane's Docker reported
/// for that tag just before export (`image_identity`).
///
/// The worker's import binds `image_tag` to whatever the archive says the tag
/// names, so that is the only thing checked: an ID merely *present* in the
/// archive (another tag's entry, a nested index) proves nothing about the tag.
///
/// An archive names a tag in up to two places, each recording a different
/// kind of digest:
///
/// - `manifest.json` entries whose `RepoTags` contain the tag → their
///   `Config` digest (`<hex>.json` or `blobs/sha256/<hex>`). This is the image
///   ID on Docker's classic image store.
/// - OCI `index.json` descriptors annotated with the tag
///   (`io.containerd.image.name`, or `org.opencontainers.image.ref.name` as a
///   full reference or bare tag) → their `digest`. This is the image ID on the
///   containerd image store — the index digest for a multi-platform image.
///
/// Which kind `inspect` reported depends on the daemon's store, so the
/// archive is accepted when, in one of those places, the tag is named and
/// every entry naming it records exactly `expected_id`. It is rejected when
/// the tag is not named at all, or names anything else (the tag was
/// re-pointed between verification and export).
async fn verify_saved_image_id(
    tar_path: &std::path::Path,
    image_tag: &str,
    expected_id: &str,
) -> Result<(), String> {
    let tar_path = tar_path.to_path_buf();
    let image_tag = image_tag.to_string();
    let expected = digest_hex(expected_id).to_string();
    tokio::task::spawn_blocking(move || {
        let metadata = read_saved_archive_metadata(&tar_path)?;
        saved_archive_tag_matches(&metadata, &image_tag, &expected)
    })
    .await
    .map_err(|e| format!("archive inspection task failed: {e}"))?
}

/// The hex part of `sha256:<hex>` (or of a bare `<hex>`).
fn digest_hex(value: &str) -> &str {
    let value = value.trim();
    value.rsplit(':').next().unwrap_or(value)
}

/// Canonical `registry/repository:tag` form of an image reference, so that
/// Docker's familiar names compare equal to fully qualified ones:
/// `nginx:1.27` ≡ `docker.io/library/nginx:1.27`, `app` ≡
/// `docker.io/library/app:latest`. Digest references keep their digest.
fn normalize_image_ref(reference: &str) -> String {
    let reference = reference.trim();
    let (name, suffix) = match reference.split_once('@') {
        Some((name, digest)) => (name, format!("@{digest}")),
        None => {
            let last_segment_start = reference.rfind('/').map_or(0, |i| i + 1);
            match reference[last_segment_start..].rfind(':') {
                Some(i) => {
                    let split = last_segment_start + i;
                    (&reference[..split], format!(":{}", &reference[split + 1..]))
                }
                None => (reference, ":latest".to_string()),
            }
        }
    };
    let (registry, repository) = match name.split_once('/') {
        Some((first, rest))
            if first.contains('.') || first.contains(':') || first == "localhost" =>
        {
            (first.to_ascii_lowercase(), rest.to_string())
        }
        _ => ("docker.io".to_string(), name.to_string()),
    };
    let registry = if registry == "index.docker.io" {
        "docker.io".to_string()
    } else {
        registry
    };
    let repository = if registry == "docker.io" && !repository.contains('/') {
        format!("library/{repository}")
    } else {
        repository
    };
    format!("{registry}/{repository}{suffix}")
}

/// The tag part of a normalized reference (`latest` in `…/app:latest`).
fn normalized_ref_tag(normalized: &str) -> Option<&str> {
    let last_segment = normalized.rsplit('/').next()?;
    last_segment.split_once(':').map(|(_, tag)| tag)
}

#[derive(Deserialize)]
struct SavedManifestEntry {
    #[serde(rename = "Config")]
    config: String,
    #[serde(rename = "RepoTags", default)]
    repo_tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SavedOciIndex {
    #[serde(default)]
    manifests: Vec<SavedOciDescriptor>,
}

#[derive(Deserialize)]
struct SavedOciDescriptor {
    digest: String,
    #[serde(default)]
    annotations: HashMap<String, String>,
}

const CONTAINERD_IMAGE_NAME_ANNOTATION: &str = "io.containerd.image.name";
const OCI_REF_NAME_ANNOTATION: &str = "org.opencontainers.image.ref.name";

impl SavedOciDescriptor {
    /// Whether this descriptor's annotations name `normalized_tag`.
    ///
    /// `io.containerd.image.name` is the full reference. The OCI
    /// `ref.name` may be a full reference or — as Docker writes it — just the
    /// tag; a bare tag is only trusted when there is no containerd name to
    /// contradict it.
    fn names(&self, normalized_tag: &str) -> bool {
        if let Some(name) = self.annotations.get(CONTAINERD_IMAGE_NAME_ANNOTATION) {
            return normalize_image_ref(name) == normalized_tag;
        }
        match self.annotations.get(OCI_REF_NAME_ANNOTATION) {
            Some(ref_name) if ref_name.contains('/') || ref_name.contains(':') => {
                normalize_image_ref(ref_name) == normalized_tag
            }
            Some(bare_tag) => normalized_ref_tag(normalized_tag) == Some(bare_tag.trim()),
            None => false,
        }
    }
}

/// Top-level metadata of a `docker save` archive.
#[derive(Default)]
struct SavedArchiveMetadata {
    has_metadata: bool,
    manifest_entries: Vec<SavedManifestEntry>,
    index_descriptors: Vec<SavedOciDescriptor>,
}

fn saved_archive_tag_matches(
    metadata: &SavedArchiveMetadata,
    image_tag: &str,
    expected: &str,
) -> Result<(), String> {
    if !metadata.has_metadata {
        return Err("archive has neither manifest.json nor index.json".to_string());
    }
    let tag = normalize_image_ref(image_tag);

    // `manifest.json`: config digests of the entries tagged `image_tag`.
    let manifest_ids: Vec<String> = metadata
        .manifest_entries
        .iter()
        .filter(|entry| {
            entry
                .repo_tags
                .iter()
                .flatten()
                .any(|repo_tag| normalize_image_ref(repo_tag) == tag)
        })
        .map(|entry| {
            let file_name = entry.config.rsplit('/').next().unwrap_or(&entry.config);
            digest_hex(file_name.trim_end_matches(".json")).to_string()
        })
        .collect();
    // `index.json`: digests of the descriptors annotated with `image_tag`.
    let index_ids: Vec<String> = metadata
        .index_descriptors
        .iter()
        .filter(|descriptor| descriptor.names(&tag))
        .map(|descriptor| digest_hex(&descriptor.digest).to_string())
        .collect();

    if manifest_ids.is_empty() && index_ids.is_empty() {
        return Err(format!("archive does not name {image_tag}"));
    }
    let all_expected = |ids: &[String]| !ids.is_empty() && ids.iter().all(|id| id == expected);
    if all_expected(&manifest_ids) || all_expected(&index_ids) {
        return Ok(());
    }

    let mut named: Vec<String> = manifest_ids
        .into_iter()
        .chain(index_ids)
        .map(|id| format!("sha256:{id}"))
        .collect();
    named.sort();
    named.dedup();
    Err(format!(
        "in the archive {image_tag} names {} instead",
        named.join(", ")
    ))
}

/// Read a metadata entry, refusing anything implausibly large.
fn read_saved_archive_entry<R: std::io::Read>(
    entry: tar::Entry<'_, R>,
    name: &str,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let size = entry.header().size().unwrap_or(0);
    if size > MAX_SAVED_ARCHIVE_METADATA_BYTES {
        return Err(format!("{name} in archive is too large ({size} bytes)"));
    }
    let mut buf = Vec::with_capacity(size as usize);
    entry
        .take(MAX_SAVED_ARCHIVE_METADATA_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {name} from archive: {e}"))?;
    Ok(buf)
}

/// One pass over the archive for `manifest.json` and `index.json`.
fn read_saved_archive_metadata(tar_path: &std::path::Path) -> Result<SavedArchiveMetadata, String> {
    let file = std::fs::File::open(tar_path)
        .map_err(|e| format!("cannot open archive {}: {e}", tar_path.display()))?;
    let mut archive = tar::Archive::new(file);
    let entries = archive
        .entries()
        .map_err(|e| format!("cannot read archive {}: {e}", tar_path.display()))?;
    let mut metadata = SavedArchiveMetadata::default();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read archive entry: {e}"))?;
        let path = entry
            .path()
            .ok()
            .map(|path| path.to_string_lossy().trim_start_matches("./").to_string());
        match path.as_deref() {
            Some("manifest.json") => {
                let bytes = read_saved_archive_entry(entry, "manifest.json")?;
                let manifest: Vec<SavedManifestEntry> = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid manifest.json in archive: {e}"))?;
                metadata.manifest_entries.extend(manifest);
                metadata.has_metadata = true;
            }
            Some("index.json") => {
                let bytes = read_saved_archive_entry(entry, "index.json")?;
                let index: SavedOciIndex = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid index.json in archive: {e}"))?;
                metadata.index_descriptors.extend(index.manifests);
                metadata.has_metadata = true;
            }
            _ => {}
        }
    }
    Ok(metadata)
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

/// Restarts after which a container that has not yet passed its readiness
/// check is treated as crash-looping. Deployments run with
/// `RestartPolicy::Always`, and Docker reports a restarting container as
/// running, so without this an app that exits immediately would be retried
/// until the whole readiness budget ran out and fail as a generic timeout.
/// Docker backs restarts off exponentially (100 ms doubling), so five
/// restarts take a few seconds plus the app's own runtime.
const CRASH_LOOP_RESTART_THRESHOLD: i64 = 5;

/// Bound on the container-log excerpt written only to the job log. The full
/// logs stay in the deployment log; this only needs the last few lines that
/// usually name the crash (missing module, exec format error, bad config).
const FAILURE_LOG_EXCERPT_LINES: usize = 5;
const FAILURE_LOG_EXCERPT_CHARS: usize = 600;

/// Last few non-empty lines of a container log, joined on one line and
/// bounded, for the job log.
fn log_excerpt(logs: &str) -> Option<String> {
    let lines: Vec<&str> = logs
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(FAILURE_LOG_EXCERPT_LINES);
    let joined = lines[start..].join(" | ");
    let char_count = joined.chars().count();
    if char_count <= FAILURE_LOG_EXCERPT_CHARS {
        return Some(joined);
    }
    // Keep the tail: the crash is at the end.
    let tail: String = joined
        .chars()
        .skip(char_count - FAILURE_LOG_EXCERPT_CHARS)
        .collect();
    Some(format!("…{tail}"))
}

/// How Docker says the container stopped: OOM kill, exit reason or code.
fn describe_container_exit(info: &temps_deployer::ContainerInfo) -> String {
    if info.oom_killed == Some(true) {
        return "OOMKilled (exit code 137)".to_string();
    }
    match (info.exit_reason.as_deref(), info.exit_code) {
        (Some(reason), Some(code)) if !reason.trim().is_empty() => {
            let reason = reason.trim();
            if reason.eq_ignore_ascii_case(&format!("exit code {code}")) {
                reason.to_string()
            } else {
                format!("exit code {code}: {reason}")
            }
        }
        (Some(reason), None) if !reason.trim().is_empty() => reason.trim().to_string(),
        (_, Some(code)) => format!("exit code {code}"),
        _ => "no exit code reported".to_string(),
    }
}

/// Failure reason for a container that stopped before passing readiness.
fn container_exit_failure_message(
    info: &temps_deployer::ContainerInfo,
    elapsed_secs: u64,
) -> String {
    let message = format!(
        "Container exited during startup ({}) after {}s",
        describe_container_exit(info),
        elapsed_secs
    );
    message
}

/// Failure reason for a container Docker keeps restarting during startup.
fn container_crash_loop_message(info: &temps_deployer::ContainerInfo, restarts: i64) -> String {
    let message = format!(
        "Container keeps restarting during startup (restarted {} times; last exit: {})",
        restarts,
        describe_container_exit(info)
    );
    message
}

/// What the readiness probe has observed so far.
#[derive(Debug, Default)]
struct ReadinessProbeState {
    /// The app returned at least one HTTP response.
    responded: bool,
    /// Description of the most recent failed probe.
    last_failure: Option<String>,
    /// The most recent failed probe could not open a connection at all.
    last_was_connect_error: bool,
    /// The connection closed or failed before completing an HTTP response.
    last_was_request_error: bool,
}

fn http_probe_status_is_healthy(status: reqwest::StatusCode) -> bool {
    status.is_success() || status.is_redirection() || matches!(status.as_u16(), 404 | 405)
}

/// A published-port proxy can complete the handshake before discovering that
/// the application refuses connections. Reject that immediate EOF/reset rather
/// than promoting a deployment whose only listener is the forwarding proxy.
async fn confirm_tcp_probe_connection(
    stream: &tokio::net::TcpStream,
    deadline: tokio::time::Instant,
) -> std::io::Result<()> {
    let confirmation_deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    let mut byte = [0_u8; 1];
    match tokio::time::timeout_at(deadline.min(confirmation_deadline), stream.peek(&mut byte))
        .await
    {
        Ok(Ok(0)) => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "TCP connection closed before readiness was confirmed; check the application listener behind the published port",
        )),
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_) if confirmation_deadline <= deadline => Ok(()),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "TCP probe deadline expired before the connection could be confirmed",
        )),
    }
}

impl ReadinessProbeState {
    fn record_transport_error(&mut self, error: &reqwest::Error) {
        self.last_was_connect_error = error.is_connect();
        self.last_was_request_error = error.is_request() && !error.is_timeout();
        self.last_failure = Some(if error.is_connect() {
            "connection refused or unreachable".to_string()
        } else if error.is_timeout() {
            "no response within 5s".to_string()
        } else {
            format!("request failed: {error}")
        });
    }

    /// Failure reason when the readiness budget runs out. The wording is
    /// load-bearing for failure classification: "never accepted connections"
    /// means nothing listened on the probed port (usually a wrong port or a
    /// 127.0.0.1 bind); "readiness timed out" means the app answered but was
    /// too slow or kept failing.
    fn timeout_message(&self, port: u16, path: &str, limit_secs: u64, waited_secs: u64) -> String {
        let last = self.last_failure.as_deref().unwrap_or("no check completed");
        if !self.responded && (self.last_was_connect_error || self.last_failure.is_none()) {
            format!(
                "Application never accepted connections on port {port} within the readiness \
                 limit of {limit_secs}s (waited {waited_secs}s; last check: {last} for {path}). This container did not open port {port}; if it is a background worker, set health.type: process"
            )
        } else if !self.responded && self.last_was_request_error {
            format!(
                "Application never returned an HTTP response on port {port} within the readiness \
                 limit of {limit_secs}s (waited {waited_secs}s; last check: {last} for {path}). If it is a background worker, set health.type: process"
            )
        } else {
            format!(
                "Application readiness timed out on port {port}: health checks did not pass \
                 within the readiness limit of {limit_secs}s (waited {waited_secs}s; last check: \
                 {last} for {path})"
            )
        }
    }
}

/// Control-plane polling only: one in-flight probe and at most 4 KiB of output
/// per candidate. The deployment startup deadline bounds every external call.
async fn wait_for_non_http_probe(
    deployer: &dyn ContainerDeployer,
    container_id: &str,
    health: &temps_core::repo_config::HealthCheckConfig,
    address: Option<&str>,
    budget: Duration,
    job_log: Option<(&DeployImageJob, &WorkflowContext)>,
) -> Result<(), WorkflowError> {
    health.validate().map_err(|reason| {
        WorkflowError::JobValidationFailed(format!(
            "Invalid probe for container {container_id}: {reason}"
        ))
    })?;
    let kind = health.kind(address.is_some());
    if !matches!(kind, "process" | "tcp" | "exec") {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Unsupported health.type '{kind}' for container {container_id}"
        )));
    }
    let deadline = tokio::time::Instant::now() + budget;
    let mut stable_since = tokio::time::Instant::now();
    let mut last_restarts = None;
    let mut was_running = false;
    let mut last_failure = "no probe completed".to_string();
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(WorkflowError::JobExecutionFailed(format!("Container {container_id} health.type: {kind} exceeded startup timeout: {last_failure}")));
        }
        let info = tokio::time::timeout_at(deadline, deployer.get_container_info(container_id))
            .await
            .map_err(|_| {
                WorkflowError::JobExecutionFailed(format!(
                    "Container {container_id} inspection exceeded startup timeout"
                ))
            })?
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Cannot evaluate {kind} probe for container {container_id}: {error}"
                ))
            })?;
        let restarts = info.restart_count.ok_or_else(|| WorkflowError::JobExecutionFailed(format!("Cannot evaluate {kind} probe for container {container_id}: RestartCount is unknown")))?;
        if matches!(
            info.status,
            DeployerContainerStatus::Exited | DeployerContainerStatus::Dead
        ) {
            return Err(WorkflowError::JobExecutionFailed(
                container_exit_failure_message(
                    &info,
                    budget
                        .saturating_sub(
                            deadline.saturating_duration_since(tokio::time::Instant::now()),
                        )
                        .as_secs(),
                ),
            ));
        }
        if restarts >= CRASH_LOOP_RESTART_THRESHOLD {
            return Err(WorkflowError::JobExecutionFailed(
                container_crash_loop_message(&info, restarts),
            ));
        }
        if !was_running
            || last_restarts.is_some_and(|previous| previous != restarts)
            || info.status != DeployerContainerStatus::Running
        {
            stable_since = tokio::time::Instant::now();
        }
        last_restarts = Some(restarts);
        was_running = info.status == DeployerContainerStatus::Running;
        if info.status == DeployerContainerStatus::Running {
            match kind {
                "process" => {
                    if stable_since.elapsed() >= Duration::from_secs(health.stable_for) {
                        return Ok(());
                    }
                    last_failure = format!(
                        "waiting for {}s continuously Running without restarts",
                        health.stable_for
                    );
                }
                "exec" => {
                    let timeout = Duration::from_secs(health.timeout)
                        .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
                    // ExecTimeout fails immediately: Docker cannot kill only an exec;
                    // the runtime stops the candidate to contain the command.
                    let execution = tokio::time::timeout_at(
                        deadline,
                        deployer.exec_in_container(container_id, health.command.clone(), timeout),
                    )
                    .await
                    .map_err(|_| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Container {container_id} exec probe exceeded startup timeout"
                        ))
                    })?;
                    let result = match execution {
                        Ok(result) => result,
                        Err(error) => {
                            if let Some((job, context)) = job_log {
                                let _ = tokio::time::timeout_at(deadline, job.log(context,
                                    format!("Exec probe runtime error for container {container_id}: {error}"))).await;
                            }
                            // Runtime errors can contain command diagnostics too. Keep
                            // those in the authorized log, never in a persisted reason.
                            let reason = if matches!(
                                error,
                                temps_deployer::DeployerError::ExecTimeout { .. }
                            ) {
                                "timeout; candidate requires stop-only cleanup"
                            } else {
                                "runtime could not complete the command; inspect authorized job logs"
                            };
                            return Err(WorkflowError::JobExecutionFailed(format!(
                                "Container {container_id} exec probe failed: {reason}"
                            )));
                        }
                    };
                    if let Some((job, context)) = job_log {
                        // Captured output may contain credentials. Only authorized job logs
                        // receive it; persisted health details contain an exit code only.
                        tokio::time::timeout_at(deadline, job.log(
                            context,
                            format!("Exec probe exited {}: {}", result.exit_code, result.output),
                        )).await.map_err(|_| WorkflowError::JobExecutionFailed(format!("Container {container_id} exec probe logging exceeded startup timeout")))?
                            .map_err(|_| WorkflowError::JobExecutionFailed(format!("Container {container_id} exec probe log could not be written")))?;
                    }
                    if result.exit_code == 0 {
                        let after = tokio::time::timeout_at(
                            deadline,
                            deployer.get_container_info(container_id),
                        )
                        .await
                        .map_err(|_| {
                            WorkflowError::JobExecutionFailed(format!(
                                "Container {container_id} post-exec inspection timed out"
                            ))
                        })?
                        .map_err(|e| {
                            WorkflowError::JobExecutionFailed(format!(
                                "Container {container_id} post-exec inspection failed: {e}"
                            ))
                        })?;
                        if after.status == DeployerContainerStatus::Running
                            && after.restart_count == Some(restarts)
                        {
                            return Ok(());
                        }
                        return Err(WorkflowError::JobExecutionFailed(format!(
                            "Container {container_id} exited or restarted during its exec probe"
                        )));
                    }
                    // Exit 126/127 means the configured executable cannot be run.
                    if matches!(result.exit_code, 126 | 127) {
                        return Err(WorkflowError::JobValidationFailed(format!(
                            "Container {container_id} health.command could not execute (exit {})",
                            result.exit_code
                        )));
                    }
                    last_failure = format!("exec exited {}", result.exit_code);
                }
                "tcp" => {
                    let address = address.ok_or_else(|| {
                        WorkflowError::JobValidationFailed(format!(
                            "Container {container_id} health.type: tcp requires a port"
                        ))
                    })?;
                    let attempt_deadline = deadline
                        .min(tokio::time::Instant::now() + Duration::from_secs(health.timeout));
                    match tokio::time::timeout_at(
                        attempt_deadline,
                        tokio::net::TcpStream::connect(address),
                    )
                    .await
                    {
                        Ok(Ok(stream)) => {
                            match confirm_tcp_probe_connection(&stream, attempt_deadline).await {
                                Ok(()) => return Ok(()),
                                Err(error) => last_failure = error.to_string(),
                            }
                        }
                        Ok(Err(error)) => last_failure = error.to_string(),
                        Err(_) => last_failure = "TCP connection timed out".to_string(),
                    }
                }
                _ => {
                    return Err(WorkflowError::JobValidationFailed(format!(
                        "Unsupported health.type '{kind}' for container {container_id}"
                    )))
                }
            }
        }
        let interval = if kind == "process" {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(health.interval)
        };
        tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + interval)).await;
    }
}

impl DeployImageJob {
    /// Runtime output belongs only in authorized job logs, never in the
    /// persisted failure reason forwarded to notifications and webhooks.
    async fn write_startup_log_excerpt(
        &self,
        context: &WorkflowContext,
        deployer: &dyn ContainerDeployer,
        container_id: &str,
    ) {
        if let Ok(logs) = deployer.get_container_logs(container_id).await {
            if let Some(excerpt) = log_excerpt(&logs) {
                let _ = self
                    .log(context, format!("Container startup log excerpt: {excerpt}"))
                    .await;
            }
        }
    }

    /// `pub(crate)`, not `pub`: the only production constructor is
    /// [`DeployImageJobBuilder::build`], which runs
    /// `refuse_granted_project_deploy` as its first statement. A `pub`
    /// constructor here would let code outside this crate assemble an
    /// executable job that never passed through that check.
    pub(crate) fn new(
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
            workload_plane: None,
            log_id: None,
            log_service: None,
            container_ids: Arc::new(Mutex::new(Vec::new())),
            replica_deployers: Arc::new(Mutex::new(HashMap::new())),
            audit_logger: None,
            failed_candidates: Arc::new(Mutex::new(Vec::new())),
            retained_failure: Arc::new(AtomicBool::new(false)),
            retention_forbidden: Arc::new(AtomicBool::new(false)),
            failed_container_db: None,
            deployment_id: None,
            log_stream_task: Arc::new(Mutex::new(None)),
            external_image_tag: None,
            image_source: None,
            expected_image_identity: None,
            reused_worker_build: None,
            log_config: None,
            encryption_service: None,
            config_service: None,
            image_builder: None,
            registry_credentials: None,
            registry_index_platforms: Arc::new(Mutex::new(HashMap::new())),
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

    pub fn with_workload_plane(mut self, workload_plane: WorkloadPlaneDeploy) -> Self {
        self.workload_plane = Some(workload_plane);
        self
    }

    /// Deploy a reused worker-built image from the workers that hold it. See
    /// [`ReusedWorkerBuild`].
    pub fn with_reused_worker_build(mut self, reused: ReusedWorkerBuild) -> Self {
        self.reused_worker_build = Some(reused);
        self
    }

    pub fn with_external_image_tag(mut self, image_tag: String) -> Self {
        self.external_image_tag = Some(image_tag);
        self
    }

    /// Declare where the deployed image lives. See [`DeployImageSource`].
    pub fn with_image_source(mut self, image_source: DeployImageSource) -> Self {
        self.image_source = Some(image_source);
        self
    }

    /// Bind this deployment to a specific image (see
    /// [`ExpectedImageIdentity`]).
    pub fn with_expected_image_identity(mut self, expected: ExpectedImageIdentity) -> Self {
        self.expected_image_identity = Some(expected);
        self
    }

    /// The identity the deployed image must have: the explicitly recorded one,
    /// else the image ID the dependency job (`PullExternalImageJob`) resolved
    /// in this workflow. `None` when neither exists — e.g. the pull was
    /// deferred to the worker and nothing was resolved locally.
    fn expected_image_identity(&self, context: &WorkflowContext) -> Option<ExpectedImageIdentity> {
        if let Some(expected) = &self.expected_image_identity {
            return Some(expected.clone());
        }
        context
            .get_output::<String>(&self.build_job_id, "image_id")
            .ok()
            .flatten()
            .and_then(|image_id| ExpectedImageIdentity::image_id(&image_id))
    }

    /// The control plane's copy of a registry image, if it is provably the
    /// image this deployment is bound to: `Ok(image_id)`, or `Err(reason)`.
    ///
    /// A tag match alone is never accepted — the tag may have been re-pointed
    /// on the control plane since this deployment resolved it, and exporting
    /// it would run a different image on the worker instead of failing.
    ///
    /// Every ID compared here comes from the same daemon's `inspect`, so it is
    /// the same kind of digest on both sides whatever the image store: the
    /// config digest on the classic store, the manifest/index digest on the
    /// containerd store (`PullExternalImageJob` records the same field). The
    /// returned ID is what [`verify_saved_image_id`] then requires the
    /// exported tag to name in the archive, which records both kinds.
    async fn verified_control_plane_copy(
        &self,
        image_tag: &str,
        context: &WorkflowContext,
    ) -> Result<String, String> {
        let Some(image_builder) = self.image_builder.as_ref() else {
            return Err(
                "this control plane has no image builder to export a copy from".to_string(),
            );
        };
        let local = match image_builder.image_identity(image_tag).await {
            Ok(local) => local,
            Err(temps_deployer::BuilderError::DockerUnavailable(e)) => {
                return Err(format!(
                    "this control plane has no Docker daemon to export a copy from ({e})"
                ));
            }
            Err(e) => return Err(format!("the control plane does not hold a copy ({e})")),
        };
        let Some(expected) = self.expected_image_identity(context) else {
            return Err(format!(
                "no recorded image identity to verify the control plane's copy ({}) against, \
                 and a tag alone is not trusted",
                local.id
            ));
        };
        if !expected.matches(&local) {
            return Err(format!(
                "the control plane holds a different image under this tag ({}; this \
                 deployment expects {})",
                local.id, expected
            ));
        }
        Ok(local.id)
    }

    /// The source this job acts on for `image_tag`: the declared one, with a
    /// reserved `temps.internal/` ref always treated as control-plane-local.
    fn resolved_image_source(&self, image_tag: &str) -> DeployImageSource {
        DeployImageSource::resolve(
            image_tag,
            self.image_source,
            self.external_image_tag.is_some(),
        )
    }

    /// Whether this process runs workloads on its own Docker (full serve
    /// profile). `false` in the control-plane profile.
    fn local_workloads_enabled(&self) -> bool {
        self.node_scheduler
            .as_ref()
            .map(|scheduler| scheduler.local_workloads_enabled())
            .unwrap_or(true)
    }

    /// What the control plane's own Docker can tell us about `image_tag`.
    async fn control_plane_image(&self, image_tag: &str) -> ControlPlaneImage {
        let Some(image_builder) = self.image_builder.as_ref() else {
            return ControlPlaneImage::NoImageBuilder;
        };
        match image_builder.inspect_image(image_tag).await {
            Ok(_) => ControlPlaneImage::Present,
            Err(temps_deployer::BuilderError::DockerUnavailable(e)) => {
                ControlPlaneImage::DockerUnavailable(e.to_string())
            }
            Err(e) => {
                tracing::debug!(
                    image = %image_tag,
                    "Image not available in the control plane's Docker: {}",
                    e
                );
                ControlPlaneImage::Absent(e.to_string())
            }
        }
    }

    /// Why a control-plane-local image cannot be exported to a worker, or
    /// `None` when it can. Each reason names its own remedy: a control plane
    /// with Docker disabled needs a registry (or the full profile), while one
    /// whose Docker simply lost the image (pruned) needs it re-uploaded.
    async fn control_plane_export_blocker(&self, image_tag: &str) -> Option<String> {
        const USE_A_REGISTRY: &str = "Push the image to a registry and deploy it by reference \
             (`temps deploy:image`), or run the control plane with the full profile.";
        match self.control_plane_image(image_tag).await {
            ControlPlaneImage::Present => None,
            ControlPlaneImage::NoImageBuilder => Some(format!(
                "this control plane has no image builder (Docker is disabled here), so \
                 nothing can export it. {USE_A_REGISTRY}"
            )),
            ControlPlaneImage::DockerUnavailable(e) => Some(format!(
                "this control plane has no Docker daemon to export it from ({e}). \
                 {USE_A_REGISTRY}"
            )),
            ControlPlaneImage::Absent(e) if !self.local_workloads_enabled() => Some(format!(
                "this control plane runs no local workloads (control-plane profile), so it \
                 has no Docker daemon to export it from ({e}). {USE_A_REGISTRY}"
            )),
            ControlPlaneImage::Absent(e) => Some(format!(
                "the control plane's Docker no longer holds it — it may have been pruned \
                 ({e}). Re-upload the image (`temps deploy:local-image`) or redeploy to \
                 rebuild it, or push it to a registry and deploy it by reference \
                 (`temps deploy:image`)."
            )),
        }
    }

    /// Pull a registry image again when the control plane no longer holds it,
    /// before a replica is started from it here.
    ///
    /// The nightly image retention pass removes expired registry-pulled
    /// images from the control plane, because unlike an uploaded tarball they
    /// can be pulled again. This is where "again" happens: a rollback,
    /// promotion or failover reuses the tag without a `PullExternalImageJob`,
    /// and Docker's `create_container` never pulls, so without this a pruned
    /// image would fail as a bare "No such image".
    ///
    /// Only registry-sourced images are pulled; a control-plane-local image
    /// (an upload or a local build) has no registry to come from and keeps the
    /// existing behaviour. When this deployment is bound to a recorded image
    /// identity, the pulled image must match it: a registry tag is mutable,
    /// and rolling back onto whatever the tag points at today would run a
    /// different image than the one being restored.
    ///
    /// An image that is already here is not pulled, but a rollback or
    /// promotion bound to a recorded identity still checks it: a refused
    /// re-pull leaves the moved tag on disk, and a retry must refuse it again
    /// rather than start it because the tag now resolves locally.
    async fn ensure_registry_image_local(
        &self,
        image_tag: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        if self.resolved_image_source(image_tag) != DeployImageSource::Registry {
            return Ok(());
        }
        let Some(image_builder) = self.image_builder.as_ref() else {
            return Ok(());
        };
        let absent_reason = match self.control_plane_image(image_tag).await {
            ControlPlaneImage::Absent(reason) => reason,
            ControlPlaneImage::Present => {
                // Only a bound (rollback/promotion) identity: a fresh deploy's
                // image was resolved by this workflow's own pull.
                if let Some(expected) = self.expected_image_identity.clone() {
                    self.verify_local_image_identity(
                        image_builder.as_ref(),
                        image_tag,
                        &expected,
                        "image",
                        context,
                    )
                    .await?;
                }
                return Ok(());
            }
            // No Docker or no builder: the deploy itself reports that, with
            // its own remedy.
            ControlPlaneImage::DockerUnavailable(_) | ControlPlaneImage::NoImageBuilder => {
                return Ok(())
            }
        };

        self.log(
            context,
            format!(
                "Image '{}' is no longer on this server ({}); it may have been removed by \
                 image retention. Pulling it again from its registry...",
                image_tag, absent_reason
            ),
        )
        .await?;

        if let Err(pull_error) = image_builder
            .fetch_registry_image(image_tag, image_tag, self.registry_credentials.clone())
            .await
        {
            // A digest reference (`repo@sha256:...`) pulls but cannot be
            // re-tagged as itself; what matters is whether it is here now.
            if !matches!(
                self.control_plane_image(image_tag).await,
                ControlPlaneImage::Present
            ) {
                let msg = format!(
                    "Image '{}' for deployment {} is no longer on this server and pulling it \
                     again from its registry failed: {}. The registry may no longer serve this \
                     tag, or its credentials may have changed. Deploy a tag the registry still \
                     serves.",
                    image_tag, context.deployment_id, pull_error
                );
                self.log_at(context, LogLevel::Error, format!("ERROR: {}", msg))
                    .await?;
                return Err(WorkflowError::JobExecutionFailed(msg));
            }
        }

        if let Some(expected) = self.expected_image_identity(context) {
            self.verify_local_image_identity(
                image_builder.as_ref(),
                image_tag,
                &expected,
                "re-pulled image",
                context,
            )
            .await?;
        }

        self.log(
            context,
            format!("Image '{}' pulled again from its registry", image_tag),
        )
        .await
    }

    /// Refuse to start `image_tag` unless this server's copy is `expected`.
    /// `what` names the image in the error ("image", "re-pulled image").
    async fn verify_local_image_identity(
        &self,
        image_builder: &dyn ImageBuilder,
        image_tag: &str,
        expected: &ExpectedImageIdentity,
        what: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let reason = match image_builder.image_identity(image_tag).await {
            Ok(local) if expected.matches(&local) => return Ok(()),
            Ok(local) => format!(
                "the tag now names a different image on this server ({}; this deployment is \
                 bound to {}), most likely because the registry moved it",
                local.id, expected
            ),
            Err(error) => format!(
                "the image could not be inspected to verify it is {} ({})",
                expected, error
            ),
        };
        let msg = format!(
            "Refusing to start deployment {} from {} '{}': {}.",
            context.deployment_id, what, image_tag, reason
        );
        self.log_at(context, LogLevel::Error, format!("ERROR: {}", msg))
            .await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

    pub fn with_config_service(mut self, service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(service);
        self
    }

    /// Set registry credentials to use when a worker node pulls the deployed
    /// image directly from its registry (registry-sourced deploys only —
    /// see `ensure_image_on_remote`).
    pub fn with_registry_credentials(
        mut self,
        credentials: temps_deployer::remote::RemotePullCredentials,
    ) -> Self {
        self.registry_credentials = Some(credentials);
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

        match self
            .deliverable_image_platforms(image_builder, &image_output.image_tag)
            .await
        {
            Ok(platforms) => platforms,
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

    /// Every platform a node can obtain `image_tag` for, and so run it on.
    ///
    /// A registry image is pulled by each worker from its registry, which
    /// hands it that worker's own architecture: the registry index decides
    /// what is schedulable. The control plane's local store does not — its
    /// pull downloaded only the control plane's architecture, so a
    /// multi-platform tag would look single-platform and every other node
    /// would be excluded (#1892). When the index cannot be read, the local
    /// store is the fallback, as before.
    ///
    /// Any other image reaches a worker from a local store, so what that
    /// store holds is the answer — every architecture of a multi-platform
    /// image present there (#1789).
    async fn deliverable_image_platforms(
        &self,
        image_builder: &Arc<dyn temps_deployer::ImageBuilder>,
        image_tag: &str,
    ) -> Result<Vec<String>, temps_deployer::BuilderError> {
        if self.resolved_image_source(image_tag) == DeployImageSource::Registry {
            let cached =
                lock_deployment_state(&self.registry_index_platforms, "registry_index_platforms")
                    .get(image_tag)
                    .cloned();
            let index = match cached {
                Some(index) => index,
                None => {
                    let index = self.read_registry_index(image_builder, image_tag).await;
                    lock_deployment_state(
                        &self.registry_index_platforms,
                        "registry_index_platforms",
                    )
                    .insert(image_tag.to_string(), index.clone());
                    index
                }
            };
            if let Some(platforms) = index {
                return Ok(platforms);
            }
        }
        image_builder.image_platforms(image_tag).await
    }

    /// The platforms `image_tag`'s registry index offers, or `None` — logged
    /// once, with the reason — when it cannot be read or lists none.
    async fn read_registry_index(
        &self,
        image_builder: &Arc<dyn temps_deployer::ImageBuilder>,
        image_tag: &str,
    ) -> Option<Vec<String>> {
        match image_builder
            .registry_image_platforms(image_tag, self.registry_credentials.clone())
            .await
        {
            Ok(platforms) if !platforms.is_empty() => Some(platforms),
            Ok(_) => {
                tracing::warn!(
                    image = %image_tag,
                    "The registry index of this image lists no platforms; scheduling \
                     on the platforms the control plane holds locally"
                );
                None
            }
            Err(error) => {
                tracing::warn!(
                    image = %image_tag,
                    %error,
                    "Could not read the registry index of this image; scheduling on \
                     the platforms the control plane holds locally"
                );
                None
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

        let image_platforms = match image_builder.image_platforms(image_tag).await {
            Ok(platforms) => platforms,
            Err(e) => {
                tracing::debug!(
                    image = %image_tag,
                    "Could not inspect image to verify it runs on the control plane: {}",
                    e
                );
                return Ok(());
            }
        };

        if image_platforms
            .iter()
            .any(|platform| temps_deployer::platform::platforms_match(platform, local_platform))
        {
            return Ok(());
        }
        let image_platform = image_platforms.join(", ");

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

        let image_platforms = match self
            .deliverable_image_platforms(image_builder, image_tag)
            .await
        {
            Ok(platforms) => platforms,
            Err(e) => {
                tracing::debug!(
                    image = %image_tag,
                    "Could not inspect image to verify its platform: {}",
                    e
                );
                return Ok(());
            }
        };
        let image_platform = image_platforms.join(", ");

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

        if image_platforms
            .iter()
            .any(|platform| temps_deployer::platform::platforms_match(platform, &node_platform))
        {
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

    /// Stream an image from the node that built it into `target`.
    async fn transfer_node_built_image(
        &self,
        image_tag: &str,
        builder_node_id: i32,
        target: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        target_name: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let builder = self.remote_deployer_for_node_id(builder_node_id).await?;
        let builder_name = builder.node_name().to_string();
        self.log(
            context,
            format!(
                "Transferring '{}' from build node '{}' to node '{}'...",
                image_tag, builder_name, target_name
            ),
        )
        .await?;
        let started = std::time::Instant::now();
        let result = match builder.export_image_stream(image_tag).await {
            Ok(stream) => target
                .import_image_stream(stream, image_tag)
                .await
                .map(|_| ()),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                self.log(
                    context,
                    format!(
                        "Image '{}' transferred from '{}' to '{}' in {:.1}s",
                        image_tag,
                        builder_name,
                        target_name,
                        started.elapsed().as_secs_f64()
                    ),
                )
                .await?;
                Ok(())
            }
            Err(error) => {
                let msg = format!(
                    "Failed to transfer image '{}' from build node '{}' to node '{}': {}",
                    image_tag, builder_name, target_name, error
                );
                self.log(context, format!("ERROR: {}", msg)).await?;
                Err(WorkflowError::JobExecutionFailed(msg))
            }
        }
    }

    /// Make an image built on worker `builder_node_id` available to the
    /// control plane's own Docker, for a replica placed on the control plane.
    async fn ensure_node_built_image_local(
        &self,
        image_tag: &str,
        expected_image_id: Option<&str>,
        builder_node_id: i32,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let local = self.image_builder.clone().ok_or_else(|| {
            WorkflowError::JobExecutionFailed(format!(
                "Cannot run worker-built image '{image_tag}' on the control plane: this \
                 control plane has no image builder to import it into"
            ))
        })?;
        // The build normally copied the image here already. Accept it by id
        // before resolving the worker at all, so a worker that has since
        // restarted, left the cluster, or pruned the image cannot fail a
        // replica that already has what it needs.
        if let (Some(expected), Ok(cached)) =
            (expected_image_id, local.inspect_image(image_tag).await)
        {
            if super::node_image::same_image_id(&cached.id, expected) {
                return Ok(());
            }
        }
        let owner = self.remote_deployer_for_node_id(builder_node_id).await?;
        let owner_name = owner.node_name().to_string();
        let started = std::time::Instant::now();
        let copied = copy_node_built_image(
            owner.as_ref(),
            &owner_name,
            local.as_ref(),
            image_tag,
            expected_image_id,
        )
        .await;
        match copied {
            Ok(NodeImageCopy::AlreadyPresent) => Ok(()),
            Ok(NodeImageCopy::Transferred) => {
                self.log(
                    context,
                    format!(
                        "Image '{}' transferred from build node '{}' to the control plane in {:.1}s",
                        image_tag,
                        owner_name,
                        started.elapsed().as_secs_f64()
                    ),
                )
                .await
            }
            Err(error) => {
                self.log(context, format!("ERROR: {}", error)).await?;
                Err(error)
            }
        }
    }

    /// Agent client for a node known only by id (e.g. the node that built
    /// this deployment's image).
    async fn remote_deployer_for_node_id(
        &self,
        node_id: i32,
    ) -> Result<Arc<temps_deployer::remote::RemoteNodeDeployer>, WorkflowError> {
        let scheduler = self.node_scheduler.as_ref().ok_or_else(|| {
            WorkflowError::JobExecutionFailed(format!(
                "Node scheduler not available to reach build node {node_id}"
            ))
        })?;
        let node = scheduler
            .node_service()
            .get_by_id(node_id)
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to load build node {node_id}: {error}"
                ))
            })?;
        let assignment = crate::services::NodeAssignment::Remote {
            node_id: node.id,
            node_name: node.name.clone(),
            address: node.address.clone(),
            private_address: node.data_address().to_string(),
            platform: node.architecture.clone(),
        };
        let token = self.get_node_token(&assignment).await?;
        let remote = match (
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
        ) {
            (Some(config), Some(encryption)) => {
                crate::cluster_ca::build_node_deployer(
                    &node,
                    token,
                    config.as_ref(),
                    encryption.as_ref(),
                )
                .await
            }
            _ => temps_deployer::remote::RemoteNodeDeployer::new(
                node.address.clone(),
                token,
                node.name.clone(),
            )
            .map(|remote| remote.with_sealing(node.request_sealing())),
        }
        .map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to create agent client for build node '{}' (id={}): {}",
                node.name, node.id, error
            ))
        })?;
        Ok(Arc::new(remote.with_platform(node.architecture)))
    }

    /// [`Self::ensure_worker_image_on_remote`] with no recorded image id.
    #[cfg(test)]
    async fn ensure_image_on_remote(
        &self,
        image_tag: &str,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        node_name: &str,
        context: &WorkflowContext,
        builder_node_id: Option<i32>,
        target_node_id: Option<i32>,
    ) -> Result<(), WorkflowError> {
        self.ensure_worker_image_on_remote(
            image_tag,
            None,
            remote,
            node_name,
            context,
            builder_node_id,
            target_node_id,
        )
        .await
    }

    /// Ensure the image exists on a remote node, transferring it if needed.
    ///
    /// 1. Checks if the image already exists on the remote node (via agent API).
    /// 2. A node-built image stays on its builder or streams directly from
    ///    that node to the target's agent, never through local Docker.
    /// 3. Registry images are pulled by the worker, with a verified local-copy
    ///    fallback when the control plane actually holds the image.
    /// 4. Control-plane-local images are exported from its Docker daemon and
    ///    imported by the worker; the no-daemon profile fails with a remedy.
    ///
    /// With `expected_image_id` -- the id recorded when a mixed-architecture
    /// worker build produced `image_tag` -- the image only runs if it is that
    /// exact image: a target that already holds it needs nothing from its
    /// builder, and a builder that now serves something else under the tag
    /// (a node re-registered by another machine, a tag rebuilt since) is
    /// refused rather than trusted.
    #[allow(clippy::too_many_arguments)]
    async fn ensure_worker_image_on_remote(
        &self,
        image_tag: &str,
        expected_image_id: Option<&str>,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        node_name: &str,
        context: &WorkflowContext,
        builder_node_id: Option<i32>,
        target_node_id: Option<i32>,
    ) -> Result<(), WorkflowError> {
        if let (Some(_), Some(expected)) = (builder_node_id, expected_image_id) {
            if let Ok(cached) = remote.inspect_image(image_tag).await {
                if super::node_image::same_image_id(&cached.id, expected) {
                    let platform = match remote.platform() {
                        Some(platform) => Some(platform),
                        None => remote.refresh_platform().await,
                    };
                    return verify_worker_image_platform(
                        image_tag,
                        &cached.platform,
                        platform.as_deref(),
                        node_name,
                    );
                }
            }
        }
        if let Some(builder_id) = builder_node_id {
            let owner = if Some(builder_id) == target_node_id {
                remote.clone()
            } else {
                self.remote_deployer_for_node_id(builder_id).await?
            };
            let info = owner.inspect_image(image_tag).await.map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Cannot inspect worker-built image '{image_tag}' on '{}': {error}",
                    owner.node_name()
                ))
            })?;
            if let Some(expected) = expected_image_id {
                if !super::node_image::same_image_id(&info.id, expected) {
                    let msg = format!(
                        "Image '{image_tag}' on build node '{}' (id={builder_id}) is {}, not the \
                         {expected} built for this deployment; refusing to run it on '{node_name}'. \
                         Redeploy to rebuild the image",
                        owner.node_name(),
                        info.id
                    );
                    self.log(context, format!("ERROR: {msg}")).await?;
                    return Err(WorkflowError::JobValidationFailed(msg));
                }
            }
            let platform = match remote.platform() {
                Some(platform) => Some(platform),
                None => remote.refresh_platform().await,
            };
            verify_worker_image_platform(
                image_tag,
                &info.platform,
                platform.as_deref(),
                node_name,
            )?;
            if Some(builder_id) == target_node_id {
                return Ok(());
            }
            // Existence is insufficient: a cached tag can refer to a stale
            // image or another architecture. Only reuse the inspected identity.
            if let Ok(cached) = remote.inspect_image(image_tag).await {
                if cached.id == info.id {
                    return verify_worker_image_platform(
                        image_tag,
                        &cached.platform,
                        platform.as_deref(),
                        node_name,
                    );
                }
            }
            self.transfer_node_built_image(image_tag, builder_id, remote, node_name, context)
                .await?;
            let imported = remote.inspect_image(image_tag).await.map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Cannot verify imported image '{image_tag}' on '{node_name}': {error}"
                ))
            })?;
            if imported.id != info.id {
                return Err(WorkflowError::JobValidationFailed(format!(
                    "Imported worker image '{image_tag}' on '{node_name}' does not match build node {builder_id}"
                )));
            }
            return verify_worker_image_platform(
                image_tag,
                &imported.platform,
                platform.as_deref(),
                node_name,
            );
        }
        // Refuse to ship an image the node cannot execute. Without this the
        // tar transfers fine, `docker load` succeeds, and the container dies
        // at start with `exec format error` — a failure mode with no trace
        // back to the architecture mismatch that caused it.
        self.verify_image_platform_for_node(image_tag, remote, node_name, context)
            .await?;

        // Set when the node holds the tag for another architecture: an
        // ordinary pull would hand that cached copy straight back.
        let mut replacing_cached = false;

        // Check if image already exists on the remote node
        match remote.image_exists(image_tag).await {
            Ok(true) => {
                // The tag alone says nothing about the architecture: a node
                // that cached it for another platform (an earlier single-arch
                // push, a tag re-pointed since) would start the container
                // only to fail with `exec format error`. Replace that copy.
                match self.node_image_platform_mismatch(image_tag, remote).await {
                    None => {
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
                    Some(mismatch) => {
                        self.log_at(
                            context,
                            LogLevel::Warning,
                            format!(
                                "WARNING: Image '{}' already exists on node '{}' as {}, but the \
                                 node runs {}; replacing it",
                                image_tag, node_name, mismatch.resolved, mismatch.node
                            ),
                        )
                        .await?;
                        replacing_cached = true;
                    }
                }
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

        match self.resolved_image_source(image_tag) {
            DeployImageSource::Registry => {
                // The worker pulls it itself. This is the only path available
                // in the control-plane profile (no CP Docker daemon), and is
                // strictly cheaper than save+stream in the full profile too —
                // no local disk tar, no double transfer through the control
                // plane's network link.
                self.log(
                    context,
                    format!(
                        "Pulling registry image '{}' on node '{}'...",
                        image_tag, node_name
                    ),
                )
                .await?;

                let credentials = self.registry_credentials.clone();
                let pulled = if replacing_cached {
                    remote
                        .pull_image_replacing_cached(image_tag, credentials)
                        .await
                } else {
                    remote
                        .pull_image_from_registry(image_tag, credentials)
                        .await
                };
                let pull_error = match pulled {
                    Ok(_image_id) => {
                        self.log(
                            context,
                            format!(
                                "Image '{}' pulled on node '{}' successfully",
                                image_tag, node_name
                            ),
                        )
                        .await?;
                        if replacing_cached {
                            return self
                                .verify_replaced_image_platform(
                                    image_tag, remote, node_name, context,
                                )
                                .await;
                        }
                        return Ok(());
                    }
                    Err(e) => e,
                };

                let verified_image_id =
                    match self.verified_control_plane_copy(image_tag, context).await {
                        Ok(image_id) => image_id,
                        Err(reason) => {
                            let msg = format!(
                                "Failed to pull image '{}' from registry on node '{}': {}. \
                                 Not falling back to the control plane's copy: {}",
                                image_tag, node_name, pull_error, reason
                            );
                            self.log_at(context, LogLevel::Error, format!("ERROR: {}", msg))
                                .await?;
                            return Err(WorkflowError::JobExecutionFailed(msg));
                        }
                    };

                self.log_at(
                    context,
                    LogLevel::Warning,
                    format!(
                        "WARNING: Registry pull of '{}' failed on node '{}' ({}); \
                         falling back to transfer from control plane (verified {})",
                        image_tag, node_name, pull_error, verified_image_id
                    ),
                )
                .await?;
                return self
                    .transfer_image_via_import(
                        image_tag,
                        Some(&verified_image_id),
                        remote,
                        node_name,
                        context,
                    )
                    .await;
            }
            DeployImageSource::ControlPlaneLocal => {
                if let Some(reason) = self.control_plane_export_blocker(image_tag).await {
                    let msg = format!(
                        "Failed to deliver image '{}' to node '{}': it exists only in the \
                         control plane's local image store (an uploaded image or one built \
                         on the control plane), which no registry can serve, and {}",
                        image_tag, node_name, reason
                    );
                    self.log_at(context, LogLevel::Error, format!("ERROR: {}", msg))
                        .await?;
                    return Err(WorkflowError::JobExecutionFailed(msg));
                }

                self.log(
                    context,
                    format!(
                        "Transferring control-plane-local image '{}' to node '{}' via import...",
                        image_tag, node_name
                    ),
                )
                .await?;
            }
        }

        self.transfer_image_via_import(image_tag, None, remote, node_name, context)
            .await
    }

    /// After a control-plane image is imported on a worker, the image the
    /// worker resolves for the tag must be one it can run.
    ///
    /// The check before the transfer accepts a multi-platform image that has
    /// the node's architecture; this one catches an archive that did not carry
    /// it (a daemon that exported only its own platform, a worker on the
    /// classic image store), which would otherwise only show as `exec format
    /// error` when the container starts. A worker that cannot say is let
    /// through, as before.
    async fn verify_imported_image_platform(
        &self,
        image_tag: &str,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        node_name: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let Some(NodeImagePlatformMismatch { resolved, node }) =
            self.node_image_platform_mismatch(image_tag, remote).await
        else {
            return Ok(());
        };
        let msg = format!(
            "Image '{image_tag}' was transferred to node '{node_name}', which runs \
             {node}, but the node resolves it as {resolved}. The image's {node} \
             variant did not arrive with it; push the image to a registry the node can pull \
             from, or restrict this environment to {resolved} nodes with target nodes/labels."
        );
        self.log_at(context, LogLevel::Error, format!("ERROR: {msg}"))
            .await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

    /// After pulling over a cached image for another architecture, the node
    /// must now resolve the tag for its own. An agent that predates replacing
    /// a cached image hands the old copy back, which is refused here rather
    /// than started.
    async fn verify_replaced_image_platform(
        &self,
        image_tag: &str,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        node_name: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let Some(NodeImagePlatformMismatch { resolved, node }) =
            self.node_image_platform_mismatch(image_tag, remote).await
        else {
            return Ok(());
        };
        let msg = format!(
            "Node '{node_name}' runs {node} but still resolves image '{image_tag}' as \
             {resolved} after pulling it again. Its agent may be too old to replace a \
             cached image: upgrade the node agent, or remove the image on the node, then \
             redeploy."
        );
        self.log_at(context, LogLevel::Error, format!("ERROR: {msg}"))
            .await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

    /// The platform the node resolves `image_tag` to, when it is not one the
    /// node runs. `None` when they match, or when the node cannot report its
    /// platform or inspect the image (an older agent), which is let through.
    async fn node_image_platform_mismatch(
        &self,
        image_tag: &str,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
    ) -> Option<NodeImagePlatformMismatch> {
        let node = match remote.platform() {
            Some(platform) => platform,
            None => remote.refresh_platform().await?,
        };
        let resolved = match remote.inspect_image(image_tag).await {
            Ok(image) => image.platform,
            Err(error) => {
                tracing::debug!(
                    image = %image_tag,
                    node = %remote.node_name(),
                    %error,
                    "Could not inspect the image on the node to verify its platform"
                );
                return None;
            }
        };
        (!temps_deployer::platform::platforms_match(&resolved, &node))
            .then_some(NodeImagePlatformMismatch { resolved, node })
    }

    /// Save `image_tag` from the control plane's Docker (`docker save`) and
    /// stream it to the remote agent (`POST /agent/images/import`), cleaning up
    /// the local tar afterward.
    ///
    /// With `expected_image_id`, the exported archive itself must contain that
    /// image: the tag is re-resolved by `docker save`, so a re-point between
    /// verification and export would otherwise ship a different image.
    ///
    /// Every import ends with [`Self::verify_imported_image_platform`], so no
    /// caller can deliver an image the node cannot run.
    async fn transfer_image_via_import(
        &self,
        image_tag: &str,
        expected_image_id: Option<&str>,
        remote: &Arc<temps_deployer::remote::RemoteNodeDeployer>,
        node_name: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
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

        if let Some(expected_image_id) = expected_image_id {
            if let Err(reason) =
                verify_saved_image_id(&tar_path, image_tag, expected_image_id).await
            {
                if let Err(e) = tokio::fs::remove_file(&tar_path).await {
                    tracing::warn!("Failed to clean up image tar {:?}: {}", tar_path, e);
                }
                let msg = format!(
                    "Failed to transfer image '{}' to node '{}': the exported archive is not \
                     the verified image {} ({})",
                    image_tag, node_name, expected_image_id, reason
                );
                self.log_at(context, LogLevel::Error, format!("ERROR: {}", msg))
                    .await?;
                return Err(WorkflowError::JobExecutionFailed(msg));
            }
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

        self.verify_imported_image_platform(image_tag, remote, node_name, context)
            .await
    }

    /// Write log message to both job-specific log file and context log writer
    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content/emojis
        let level = Self::detect_log_level(&message);
        self.log_at(context, level, message).await
    }

    /// [`Self::log`] with an explicit level, for messages whose wording the
    /// keyword-based [`Self::detect_log_level`] would misclassify.
    async fn log_at(
        &self,
        context: &WorkflowContext,
        level: LogLevel,
        message: String,
    ) -> Result<(), WorkflowError> {
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
    async fn resolve_container_port(
        &self,
        image_tag: &str,
        context: &WorkflowContext,
    ) -> Option<u16> {
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
            return Some(configured);
        }

        // A worker build records the image's configuration as read from the
        // node that owns it. It is authoritative: the control plane may hold
        // no copy of the image to inspect, and a failed local inspection would
        // otherwise silently fall back to a port the image does not listen on.
        if let Some(config) = self.recorded_image_config(context) {
            return match config.exposed_ports.first() {
                Some(port) => {
                    let _ = self
                        .log(
                            context,
                            format!(
                                "Detected EXPOSE directive in image (read from its build \
                                 node): port {port}"
                            ),
                        )
                        .await;
                    Some(*port)
                }
                None => None,
            };
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
                        return Some(port);
                    }
                    Ok(None) => return None,
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
        Some(self.config.port as u16)
    }

    /// The image configuration the build job recorded from its build node.
    /// `None` for an image built on the control plane, one deployed by
    /// reference with no worker build behind it, and a node that did not
    /// report it.
    fn recorded_image_config(
        &self,
        context: &WorkflowContext,
    ) -> Option<super::build_image::BuiltImageConfig> {
        if self.external_image_tag.is_some() {
            // A rollback or promotion: what the original build's node
            // reported travels with the reused worker build.
            return self
                .reused_worker_build
                .as_ref()
                .and_then(|reused| reused.image_config.clone());
        }
        if self.build_job_id.is_empty() {
            return None;
        }
        context
            .get_output::<super::build_image::BuiltImageConfig>(&self.build_job_id, "image_config")
            .ok()
            .flatten()
    }

    /// Public getter for config to allow test access
    pub fn config(&self) -> &DeploymentJobConfig {
        &self.config
    }

    /// Public getter for target to allow test access
    pub fn target(&self) -> &DeploymentTarget {
        &self.target
    }

    /// Record a newly-created container and the deployer that owns it before
    /// any subsequent fallible operation. Cleanup must never guess which
    /// Docker daemon owns a container created on a worker node.
    /// Record that this deployment received the host Docker socket (ADR 045).
    ///
    /// Never fails the deployment: auditing is a graceful-degradation
    /// dependency, and the container is already running by the time we get
    /// here. A missing sink, or a sink that errors, still leaves the event in
    /// the process log and in the deploy log the user reads.
    async fn audit_docker_socket_mount(&self, context: &WorkflowContext, node: &str) {
        tracing::warn!(
            project_id = context.project_id,
            deployment_id = context.deployment_id,
            node,
            "Deployment received the host Docker socket; it is root-equivalent on that host"
        );

        let Some(audit_logger) = self.audit_logger.as_ref() else {
            tracing::warn!(
                project_id = context.project_id,
                deployment_id = context.deployment_id,
                "No audit sink is wired; the host-Docker-socket mount was logged but not \
                 recorded in the audit trail"
            );
            return;
        };

        let audit = crate::handlers::audit::DeploymentDockerSocketMountedAudit {
            context: temps_core::AuditContext {
                // Not a user action: the grant is host policy, set by whoever
                // has a shell on the machine, and this event is produced by
                // the deploy pipeline. `0` is the codebase's convention for
                // an actor that is not a user.
                user_id: 0,
                ip_address: None,
                user_agent: format!("temps-deployer/node-{node}"),
            },
            project_id: context.project_id,
            deployment_id: context.deployment_id,
            node: node.to_string(),
        };
        if let Err(e) = audit_logger.create_audit_log(&audit).await {
            tracing::error!(
                project_id = context.project_id,
                deployment_id = context.deployment_id,
                error = %e,
                "Failed to create the host-Docker-socket audit log"
            );
        }
    }

    /// Persist that this deployment received the host Docker socket, so
    /// exec/terminal authorization can check the *deployment's* recorded
    /// status instead of the project's current slug (ADR 045). A project
    /// renamed away from a granted slug keeps working through this ADR's
    /// admin-only rename guard, but its already-running containers are not
    /// stopped or recreated by the rename -- checking the current slug alone
    /// would silently downgrade exec authorization on a still-root-equivalent
    /// container from admin-only to anyone holding `ContainersExec`.
    ///
    /// Uses `failed_container_db`/`deployment_id` -- the same pair
    /// `record_failed_candidate` already relies on being set for every
    /// production deploy (`failed_container_retention` is unconditional on
    /// every builder chain that reaches this job). Best-effort: a write
    /// failure is logged, not propagated, since refusing to complete an
    /// already-running deployment over a follow-up bookkeeping write would
    /// be worse than the (still-audited, still-logged) gap it would leave.
    ///
    /// `pub(crate)` rather than private so `services.rs`'s test module can
    /// prove the write is visible through
    /// `DeploymentService::deployment_docker_socket_mounted` -- the exact
    /// read path exec authorization uses -- with a real database, not just
    /// that the two methods independently look correct in isolation.
    pub(crate) async fn persist_docker_socket_mounted(&self, context: &WorkflowContext) {
        let (Some(db), Some(deployment_id)) =
            (self.failed_container_db.as_ref(), self.deployment_id)
        else {
            tracing::warn!(
                project_id = context.project_id,
                deployment_id = context.deployment_id,
                "No database handle wired into this job; the host-Docker-socket mount was \
                 logged and audited but not persisted onto the deployment row"
            );
            return;
        };
        if let Err(error) = temps_entities::deployments::Entity::update_many()
            .col_expr(
                temps_entities::deployments::Column::DockerSocketMounted,
                Expr::value(true),
            )
            .filter(temps_entities::deployments::Column::Id.eq(deployment_id))
            .exec(db.as_ref())
            .await
        {
            tracing::error!(
                project_id = context.project_id,
                deployment_id,
                error = %error,
                "Failed to persist docker_socket_mounted on the deployment row"
            );
        }
    }

    fn track_container(&self, container_id: String, deployer: Arc<dyn ContainerDeployer>) {
        // Insert ownership first. This prevents cleanup from observing a
        // container ID without the node-aware deployer needed to remove it.
        lock_deployment_state(&self.replica_deployers, "replica_deployers")
            .insert(container_id.clone(), deployer);
        lock_deployment_state(&self.container_ids, "container_ids").push(container_id);
    }

    /// Write a progress line on a cleanup path. Cleanup has to finish whether or
    /// not log storage works (a full disk fails every write), so a failed write
    /// is reported here and never returned to stop the cleanup.
    async fn log_best_effort(&self, context: &WorkflowContext, message: String) {
        if let Err(error) = self.log(context, message).await {
            tracing::warn!(
                job_id = %self.job_id,
                deployment_id = ?self.deployment_id,
                error = %error,
                "Failed to write a deployment cleanup log line; continuing cleanup"
            );
        }
    }

    async fn stop_background_log_stream(&self, context: &WorkflowContext) {
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
            self.log_best_effort(context, "🧹 Stopped background log streaming".to_string())
                .await;
        }
    }

    /// Remove all containers if they exist (called on timeout/failure/cancellation)
    async fn cleanup_container(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        self.stop_background_log_stream(context).await;

        // Then clean up all containers
        let container_ids = {
            let guard = lock_deployment_state(&self.container_ids, "container_ids");
            guard.clone()
        };

        let mut cleanup_errors = Vec::new();
        if !container_ids.is_empty() {
            self.log_best_effort(
                context,
                format!("🧹 Cleaning up {} container(s)", container_ids.len()),
            )
            .await;

            for container_id in &container_ids {
                self.log_best_effort(context, format!("🧹 Removing container: {}", container_id))
                    .await;

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
                        lock_deployment_state(&self.replica_deployers, "replica_deployers")
                            .remove(container_id);
                        self.log_best_effort(
                            context,
                            format!("✅ Container {} is absent", container_id),
                        )
                        .await;
                    }
                    Err(error) => {
                        cleanup_errors.push(format!("container {container_id}: {error}"));
                        self.log_best_effort(
                            context,
                            format!(
                                "⚠️  Warning: Failed to remove container {}: {}",
                                container_id, error
                            ),
                        )
                        .await;
                    }
                }
            }

            // Snapshot ownership before locking the ID list. `track_container`
            // acquires these locks in the opposite phase (ownership, then IDs),
            // so nesting them here could deadlock a concurrent cancellation.
            let remaining_container_ids =
                lock_deployment_state(&self.replica_deployers, "replica_deployers")
                    .keys()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>();
            lock_deployment_state(&self.container_ids, "container_ids")
                .retain(|container_id| remaining_container_ids.contains(container_id));
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

        self.stop_background_log_stream(context).await;
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
                container_port: Set(candidate.container_port.map(i32::from)),
                health_state: Set("unhealthy".to_string()),
                health_checked_at: Set(Some(now)),
                health_detail: Set(context.get_output::<String>(&self.job_id, "health_detail")?.map(|s| s.chars().take(4096).collect())),
                host_port: Set(candidate.container_port.map(|_| i32::from(candidate.host_port))),
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

        // Schedule replicas across nodes (or deploy locally if no scheduler/no nodes).
        //
        // The pass also reports whether the ADR-045 socket gate applied. That
        // travels with the assignments so the deploy step can verify the host
        // actually mounted the socket: a gated deployment the executing host
        // silently started *without* it is a service running with none of the
        // access it exists for, reported as healthy.
        // ADR-055 `enforce`: the unified scheduler places (strict pins, the
        // node reserve, enforced capacity, placements in flight) and the
        // reservation is held until the replicas are started.
        let enforce = self.config.workload_mode == EnvironmentWorkloadMode::Enforce;
        // ADR-055 Phase 9: attached volumes are mounted only through the
        // plane, by one replica, on the control plane. Anything else fails
        // here with the fix, never deploys a replica without its data.
        if !self.config.volume_mounts.is_empty() {
            if let Some(message) = crate::services::workload_volumes::volumes_blocker(
                context.environment_id,
                self.config.workload_mode,
                i32::try_from(self.config.replicas).unwrap_or(i32::MAX),
                self.config.volume_pin.as_ref().map(|pin| pin.node_id),
            ) {
                self.log(context, format!("ERROR: {message}")).await?;
                return Err(WorkflowError::JobExecutionFailed(message));
            }
        }
        let mut _in_flight: Option<temps_workload_plane::inventory::InFlightGuard> = None;
        let (node_assignments, docker_socket_required) = if enforce {
            let (assignments, socket_required, guard) =
                self.enforced_placement(image_output, context).await?;
            _in_flight = Some(guard);
            (assignments, socket_required)
        } else if let Some(ref scheduler) = self.node_scheduler {
            // A node-built image is handed to each replica's node from its
            // builder, so the build location does not constrain placement;
            // the platform filter below does.
            let target_ids = self.config.target_nodes.as_deref();
            let target_labels = self.config.target_labels.as_ref();
            let has_explicit_constraints =
                has_explicit_placement_constraints(target_ids, target_labels);
            // Which architectures do we actually have an image for? Nodes that
            // match none of them are excluded from the pool instead of being
            // handed a container that cannot start.
            let image_platforms = self.available_image_platforms(image_output).await;
            let placement_result = scheduler
                .schedule_placement(crate::services::node_scheduler::ReplicaPlacementRequest {
                    replica_count: self.config.replicas,
                    labels: target_labels,
                    target_node_ids: target_ids,
                    anti_affinity: self.config.anti_affinity,
                    exclude_node_ids: &self.config.exclude_node_ids,
                    image_platforms: &image_platforms,
                    // ADR 045: lets the scheduler refuse, up front, to
                    // place a socket-granted project on a host that does
                    // not grant it.
                    project_slug: self.config.project_slug.as_deref(),
                    exclude_control_plane: false,
                })
                .await;
            // ADR-055 `shadow`: NodeScheduler still decides; the unified
            // scheduler's answer for the same request is only logged.
            if self.config.workload_mode == EnvironmentWorkloadMode::Shadow {
                self.log_shadow_placement(scheduler, &placement_result, &image_platforms, context)
                    .await;
            }
            match placement_result {
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
                    for warning in &outcome.warnings {
                        self.log(context, format!("WARNING: {warning}")).await?;
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
                    (assignments, outcome.docker_socket_required)
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
                    // Only dedicated nodes could take it and none is pinned.
                    // Degrading to Local would either start an image the
                    // control plane cannot run or ignore a label selector.
                    | crate::services::node_service::NodeError::DedicatedNodesNotPinned {
                        ..
                    }
                    | crate::services::node_service::NodeError::Validation {
                        ..
                    }
                    // This process runs no workloads and no worker node could
                    // take the replicas. Degrading to Local would produce a
                    // "successful" deployment whose containers never start.
                    | crate::services::node_service::NodeError::LocalWorkloadsDisabled {
                        ..
                    }
                    // ADR 045: no host that grants this project the Docker
                    // socket can take it. Unreachable today — a gate only
                    // exists when this control plane declares the project,
                    // and declaring it also grants it here, so `Local` is
                    // always a candidate — but that is a two-file invariant
                    // (`declares()` delegating to `allows()`), not a property
                    // of this match. Listing it means a future split of
                    // "declared" from "granted" cannot silently reopen the
                    // silent-mount-miss this ADR exists to prevent: falling
                    // back to Local would deploy without the socket.
                    | crate::services::node_service::NodeError::DockerSocketNotSchedulable {
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
                // Same guard for the transient-failure path: the historical
                // degrade-to-local fallback is only safe where local
                // deployment is possible at all.
                Err(e) if !scheduler.local_workloads_enabled() => {
                    let msg = format!(
                        "Cannot schedule this deployment: {}. This control plane does not run \
                         application containers, so there is no local fallback — join a worker \
                         node with `temps join`",
                        e
                    );
                    self.log(context, format!("ERROR: {}", msg)).await?;
                    return Err(WorkflowError::JobExecutionFailed(msg));
                }
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
                    // No placement pass completed, so no gate was established
                    // for this deployment — the executing host still answers
                    // from its own environment, it is simply not verified.
                    (
                        vec![crate::services::NodeAssignment::Local; self.config.replicas as usize],
                        false,
                    )
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
            (
                vec![crate::services::NodeAssignment::Local; self.config.replicas as usize],
                false,
            )
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

        // `enforce`: every replica is created through the plane, pinned to
        // the node chosen here; the product keeps the health check and
        // blue/green around them.
        let plane_deployer: Option<Arc<dyn ContainerDeployer>> = if enforce {
            Some(self.plane_deployer(context, self.container_deployer.clone(), None)?)
        } else {
            None
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
                    // A build moved off the control plane (build location
                    // `node`) copies its image here when it finishes; if that
                    // copy failed, retry it before starting the replica.
                    if let Some(builder_id) = image_output.builder_node_for_tag(&replica_image_tag)
                    {
                        let expected_image_id =
                            image_output.expected_image_id_for_tag(&replica_image_tag);
                        self.ensure_node_built_image_local(
                            &replica_image_tag,
                            expected_image_id,
                            builder_id,
                            context,
                        )
                        .await?;
                    }
                    // A rollback/promotion of a registry image that image
                    // retention removed from this server pulls it again.
                    self.ensure_registry_image_local(&replica_image_tag, context)
                        .await?;
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
                    match plane_deployer.as_ref() {
                        Some(plane_deployer) => plane_deployer.clone(),
                        None => self.container_deployer.clone(),
                    }
                }
                crate::services::NodeAssignment::Remote {
                    address,
                    node_name,
                    platform,
                    ..
                } => {
                    // The agent's deploy call carries no volumes: a replica
                    // with volumes never starts there without its data.
                    if !self.config.volume_mounts.is_empty() {
                        let message = format!(
                            "Replica {} was placed on node '{node_name}', but this environment \
                             mounts volumes, which only control-plane replicas receive",
                            replica_index + 1
                        );
                        self.log(context, format!("ERROR: {message}")).await?;
                        return Err(WorkflowError::JobExecutionFailed(message));
                    }
                    // Look up the node and its token from the node service
                    let (node, token) = self.get_remote_node(assignment).await?;
                    // Build the deployer via the shared factory so https:// nodes
                    // get mutual TLS (ADR-020 WS-2.1); falls back to plain HTTP
                    // when the cluster CA deps aren't wired in.
                    let build_result = match (
                        self.config_service.as_ref(),
                        self.encryption_service.as_ref(),
                    ) {
                        (Some(cs), Some(es)) => {
                            crate::cluster_ca::build_node_deployer(
                                &node,
                                token,
                                cs.as_ref(),
                                es.as_ref(),
                            )
                            .await
                        }
                        _ => temps_deployer::remote::RemoteNodeDeployer::new(
                            address.clone(),
                            token,
                            node_name.clone(),
                        )
                        .map(|remote| remote.with_sealing(node.request_sealing())),
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
                    self.ensure_worker_image_on_remote(
                        &replica_image_tag,
                        image_output.expected_image_id_for_tag(&replica_image_tag),
                        &remote,
                        node_name,
                        context,
                        image_output.builder_node_for_tag(&replica_image_tag),
                        assignment.node_id(),
                    )
                    .await?;

                    if enforce {
                        // The worker's agent stays the deployer of every
                        // operation after the create.
                        self.plane_deployer(context, remote, assignment.node_id())?
                    } else {
                        remote
                    }
                }
            };

            match self
                .deploy_single_replica(
                    &replica_image_tag,
                    context,
                    replica_index as u32,
                    health_check_override.as_deref(),
                    &deployer,
                    ReplicaTarget {
                        assignment,
                        docker_socket_required,
                    },
                )
                .await
            {
                Ok((container_id, host_port, container_port)) => {
                    all_container_ids.push(container_id);
                    all_host_ports.push(host_port);
                    all_node_ids.push(assignment.node_id());
                    all_image_names.push(replica_image_tag.clone());
                    // All replicas share the same container port
                    resolved_container_port = container_port;
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
            container_port: resolved_container_port,
            node_ids: all_node_ids,
            image_names: all_image_names,
        })
    }

    /// Get the token for a remote node by decrypting the stored encrypted token.
    async fn get_node_token(
        &self,
        assignment: &crate::services::NodeAssignment,
    ) -> Result<String, WorkflowError> {
        self.get_remote_node(assignment)
            .await
            .map(|(_node, token)| token)
    }

    /// Load a remote assignment's node row and decrypt its agent token. The
    /// row decides where its agent is dialed (`cluster_ca::node_agent_endpoint`).
    async fn get_remote_node(
        &self,
        assignment: &crate::services::NodeAssignment,
    ) -> Result<(temps_entities::nodes::Model, String), WorkflowError> {
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

                let encrypted_token = node.token_encrypted.clone().ok_or_else(|| {
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

                let token = String::from_utf8(decrypted_bytes).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Decrypted token for node '{}' (id={}) is not valid UTF-8: {}",
                        node_name, node_id, e
                    ))
                })?;
                Ok((node, token))
            }
        }
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

        // Saves refuse this now, but a row saved before that check existed
        // still reaches here, and every later step allocates per replica.
        let max_replicas = temps_entities::deployment_config::MAX_REPLICAS.unsigned_abs();
        if self.config.replicas > max_replicas {
            return Err(WorkflowError::JobValidationFailed(format!(
                "replicas is {} for service '{}', above the maximum of {max_replicas}; \
                 lower it in the environment settings and deploy again",
                self.config.replicas, self.config.service_name
            )));
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
            let synthetic = BuildImageOutput {
                image_tag: external_tag.clone(),
                image_id: format!(
                    "{EXTERNAL_IMAGE_ID_PREFIX}{}",
                    external_tag.replace(":", "-")
                ),
                size_bytes: 0, // Not applicable for external images
                build_context: std::path::PathBuf::from("."),
                dockerfile_path: std::path::PathBuf::from("."),
                // External images come as a single tag; the platform check
                // reads the real architecture from the image itself.
                image_tags_by_platform: HashMap::new(),
                builder_node_id: None,
                builder_node_ids_by_platform: HashMap::new(),
                image_ids_by_platform: HashMap::new(),
            };
            match &self.reused_worker_build {
                // A rollback/promotion of a worker build: each node gets the
                // tag of its own architecture, from the worker holding it.
                Some(reused) => {
                    if reused.is_per_architecture() {
                        let mut platforms: Vec<String> =
                            reused.image_tags_by_platform.keys().cloned().collect();
                        platforms.sort();
                        self.log(
                            &context,
                            format!(
                                "Reusing the per-architecture images of the original build ({}); \
                                 each replica runs the image built for its node's architecture",
                                platforms.join(", ")
                            ),
                        )
                        .await?;
                    }
                    BuildImageOutput {
                        image_id: reused.image_id.clone().unwrap_or(synthetic.image_id),
                        image_tags_by_platform: reused.image_tags_by_platform.clone(),
                        builder_node_id: reused.builder_node_id,
                        builder_node_ids_by_platform: reused.builder_node_ids_by_platform.clone(),
                        image_ids_by_platform: reused.image_ids_by_platform.clone(),
                        ..synthetic
                    }
                }
                None => synthetic,
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
            .or_else(|| {
                self.config
                    .health
                    .as_ref()
                    .map(|health| health.path.clone())
            })
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
                context.set_output(&self.job_id, "health_detail", deploy_error.to_string())?;
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

        let repository_probe = context.get_output::<temps_core::repo_config::HealthCheckConfig>(
            &self.build_job_id,
            "health",
        )?;
        let probe_source = if repository_probe.is_some() {
            "repository"
        } else if self.config.health.is_some() {
            "deployment_settings"
        } else {
            "automatic"
        };
        let effective_probe = repository_probe
            .or_else(|| self.config.health.clone())
            .unwrap_or_default();
        let runtime_revision = crate::services::runtime_health::EffectiveProbeRevision::resolve(
            &effective_probe,
            deployment_output.container_port,
            effective_health_path.as_deref(),
            probe_source,
            self.config.replicas,
        );
        context.set_output(&self.job_id, "runtime_health_revision", runtime_revision)?;

        // Set typed job outputs
        context.set_output(
            &self.job_id,
            "portless",
            deployment_output.container_port.is_none(),
        )?;
        context.set_output(
            &self.job_id,
            "health_state",
            if deployment_output.container_port.is_some() && effective_health_path.is_none() {
                "unknown"
            } else {
                "healthy"
            },
        )?;
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
                deployment_output.container_port.map(i32::from),
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
            self.stop_background_log_stream(context).await;
            return Ok(());
        }
        // Use the stored container_id (set immediately after container creation)
        // This ensures cleanup works even if deployment fails before setting outputs
        self.cleanup_container(context).await
    }
}

/// The job's resource settings as the plane's [`temps_workload::Resources`]:
/// limits as limits, requests as requests, `0` MB memory as uncapped.
fn workload_resources(usage: &ResourceUsage) -> temps_workload::Resources {
    let cpu = |value: &Option<String>| {
        value
            .as_deref()
            .and_then(parse_cpu_cores)
            .filter(|cores| *cores > 0.0)
            .map(|cores| (cores * 1000.0).ceil() as u32)
    };
    let memory = |value: &Option<String>| {
        value
            .as_deref()
            .and_then(parse_memory_mb)
            .filter(|mb| *mb > 0)
            .map(|mb| u32::try_from(mb).unwrap_or(u32::MAX))
    };
    let amounts = |cpu_millis, memory_mb| {
        let amounts = temps_workload::ResourceAmounts {
            cpu_millis,
            memory_mb,
            disk_mb: None,
            pids: None,
            disk_io_mbps: None,
            network_mbps: None,
        };
        (!amounts.is_empty()).then_some(amounts)
    };
    temps_workload::Resources {
        requests: amounts(cpu(&usage.cpu_request), memory(&usage.memory_request)),
        limits: amounts(cpu(&usage.cpu_limit), memory(&usage.memory_limit)),
    }
}

impl DeployImageJob {
    fn workload_placement_spec(
        &self,
    ) -> Result<temps_workload::PlacementSpec, crate::services::WorkloadPlacementError> {
        crate::services::workload_placement::placement_spec_from_job(
            self.config.target_nodes.as_deref(),
            self.config.target_labels.as_ref(),
            self.config.anti_affinity,
            self.config.replicas,
        )
    }

    /// `shadow`: ask the unified scheduler what it would do with the request
    /// NodeScheduler just answered, and log the comparison. Never fails the
    /// deployment: the legacy answer is the one that is used.
    async fn log_shadow_placement(
        &self,
        scheduler: &crate::services::NodeScheduler,
        legacy: &Result<
            crate::services::node_scheduler::SchedulingOutcome,
            crate::services::node_service::NodeError,
        >,
        image_platforms: &[String],
        context: &WorkflowContext,
    ) {
        use crate::services::workload_placement::{describe_shadow, ShadowOutcome, ShadowRequest};
        let Some(workload_plane) = self.workload_plane.as_ref() else {
            tracing::warn!(
                deployment_id = context.deployment_id,
                "workload_mode is shadow but the placement service is not wired; comparison skipped"
            );
            return;
        };
        let placement = match self.workload_placement_spec() {
            Ok(placement) => placement,
            Err(e) => {
                tracing::warn!(
                    deployment_id = context.deployment_id,
                    "shadow placement skipped: {e}"
                );
                let _ = self
                    .log(
                        context,
                        format!("WARNING: workload plane shadow run skipped: {e}"),
                    )
                    .await;
                return;
            }
        };
        let resources = workload_resources(&self.config.resources);
        let request = ShadowRequest {
            placement: &placement,
            target_labels: self.config.target_labels.as_ref(),
            resources: &resources,
            exclude_node_ids: &self.config.exclude_node_ids,
            image_platforms,
            project_slug: self.config.project_slug.as_deref(),
        };
        match workload_plane
            .placement
            .shadow_run(
                scheduler,
                Some(ShadowOutcome::from_legacy(legacy)),
                &request,
            )
            .await
        {
            Ok(comparison) => {
                if comparison.is_unexpected() {
                    tracing::warn!(
                        deployment_id = context.deployment_id,
                        legacy = ?comparison.legacy,
                        unified = ?comparison.unified,
                        "workload plane shadow run: unexpected disagreement"
                    );
                } else {
                    tracing::info!(
                        deployment_id = context.deployment_id,
                        agrees = comparison.agrees(),
                        disagreement = ?comparison.disagreement,
                        "workload plane shadow run"
                    );
                }
                let _ = self.log(context, describe_shadow(&comparison)).await;
            }
            Err(e) => {
                tracing::warn!(
                    deployment_id = context.deployment_id,
                    "workload plane shadow run failed: {e}"
                );
                let _ = self
                    .log(
                        context,
                        format!("WARNING: workload plane shadow run failed: {e}"),
                    )
                    .await;
            }
        }
    }

    fn enforce_unavailable(&self) -> Option<String> {
        match self.workload_plane.as_ref() {
            None => Some(
                "This environment's workload_mode is enforce, but this process did not wire the \
                 workload plane's placement service into the deploy job. Set workload_mode back \
                 to legacy to deploy it"
                    .to_string(),
            ),
            Some(WorkloadPlaneDeploy { plane: None, .. }) => Some(
                "This environment's workload_mode is enforce, but the workload plane is not \
                 running in this process (it needs an audit logger, and starts at boot). Set \
                 workload_mode back to legacy to deploy it"
                    .to_string(),
            ),
            Some(_) => None,
        }
    }

    /// `enforce`: place every replica with the unified scheduler, strictly
    /// (a pin that cannot be met fails the deployment, it is never moved),
    /// and hold the placement's reservation until the replicas started.
    async fn enforced_placement(
        &self,
        image_output: &BuildImageOutput,
        context: &WorkflowContext,
    ) -> Result<
        (
            Vec<crate::services::NodeAssignment>,
            bool,
            temps_workload_plane::inventory::InFlightGuard,
        ),
        WorkflowError,
    > {
        if let Some(message) = self.enforce_unavailable() {
            self.log(context, format!("ERROR: {message}")).await?;
            return Err(WorkflowError::JobExecutionFailed(message));
        }
        let (Some(workload_plane), Some(plane)) = (
            self.workload_plane.as_ref(),
            self.workload_plane.as_ref().and_then(|w| w.plane.clone()),
        ) else {
            return Err(WorkflowError::JobExecutionFailed(
                "workload plane unavailable".to_string(),
            ));
        };
        let mut placement = self.workload_placement_spec().map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Cannot schedule this deployment: {e}"))
        })?;
        // A node-local volume pins the replica (§8, strict): to the
        // volume's node, or to the control plane where an uncreated volume
        // will be created.
        if !self.config.volume_mounts.is_empty() {
            let pin = self.config.volume_pin.clone().unwrap_or_else(|| {
                temps_workload_volumes::VolumePin {
                    node_id: temps_workload_plane::scheduler::CONTROL_PLANE_NODE_ID,
                    volumes: self
                        .config
                        .volume_mounts
                        .iter()
                        .map(|m| m.volume.clone())
                        .collect(),
                }
            });
            placement.nodes = Some(vec![pin.node_id]);
            self.log(context, format!("Replica {pin}")).await?;
        }
        let image_platforms = self.available_image_platforms(image_output).await;
        let spec = temps_workload::WorkloadSpec {
            owner: temps_workload::WorkloadRef::new(
                temps_workload::WorkloadKind::Deployment,
                i64::from(context.deployment_id),
            ),
            artifact: temps_workload::Artifact::oci(image_output.image_tag.clone()),
            isolation: self.config.workload_isolation,
            process: Default::default(),
            env: Default::default(),
            resources: workload_resources(&self.config.resources),
            volumes: Vec::new(),
            ports: Vec::new(),
            network: Default::default(),
            probes: Default::default(),
            placement,
            lifetime: temps_workload::Lifetime::LongRunning {
                restart: temps_workload::RestartPolicy::UnlessStopped,
            },
            security: None,
            labels: Default::default(),
        };
        let hints = temps_workload_plane::inventory::PlacementHints {
            project_slug: self.config.project_slug.clone(),
            image_platforms,
            exclude_node_ids: self.config.exclude_node_ids.clone(),
            affinity_nodes: Vec::new(),
            exclude_control_plane: false,
        };
        self.log(
            context,
            format!(
                "Placing replicas with the unified scheduler (workload_mode: enforce, \
                 isolation: {})",
                temps_workload_plane::scheduler::isolation_name(self.config.workload_isolation)
            ),
        )
        .await?;
        let name = format!("deployment-{}", context.deployment_id);
        let (placed, guard) = match plane.schedule(&name, &spec, &hints).await {
            Ok(placed) => placed,
            Err(e) => {
                let message = format!("Cannot schedule this deployment: {e}");
                self.log(context, format!("ERROR: {message}")).await?;
                return Err(WorkflowError::JobExecutionFailed(message));
            }
        };
        for exclusion in &placed.exclusions {
            let line = if exclusion.excluded {
                format!("Skipping {exclusion}")
            } else {
                format!("WARNING: {exclusion}")
            };
            self.log(context, line).await?;
        }
        for warning in &placed.warnings {
            self.log(context, format!("WARNING: {warning}")).await?;
        }
        let assignments = workload_plane
            .placement
            .assignments_for(&placed.nodes)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Cannot map the unified placement {:?} onto nodes: {e}",
                    placed.nodes
                ))
            })?;
        for (i, assignment) in assignments.iter().enumerate() {
            let line = match assignment {
                crate::services::NodeAssignment::Local => {
                    format!("Replica {} scheduled on local node", i + 1)
                }
                crate::services::NodeAssignment::Remote {
                    node_name, node_id, ..
                } => format!(
                    "Replica {} scheduled on node '{}' (id={})",
                    i + 1,
                    node_name,
                    node_id
                ),
            };
            self.log(context, line).await?;
        }
        Ok((assignments, placed.socket_required, guard))
    }

    /// The deployer `enforce` uses for replicas on `node_id` (`None`: the
    /// control plane), with `node_deployer` for every operation after the
    /// create.
    fn plane_deployer(
        &self,
        context: &WorkflowContext,
        node_deployer: Arc<dyn ContainerDeployer>,
        node_id: Option<i32>,
    ) -> Result<Arc<dyn ContainerDeployer>, WorkflowError> {
        let plane = self
            .workload_plane
            .as_ref()
            .and_then(|w| w.plane.clone())
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(
                    self.enforce_unavailable()
                        .unwrap_or_else(|| "workload plane unavailable".to_string()),
                )
            })?;
        let deployer = crate::services::plane_deployer::PlaneContainerDeployer::new(
            plane,
            node_deployer,
            context.deployment_id,
            Duration::from_secs(self.config.health_check_timeout_secs.max(30)),
        )
        .with_volumes(
            self.config.volume_mounts.clone(),
            crate::services::workload_volumes::environment_volume_owner(context.environment_id),
        )
        .with_isolation(self.config.workload_isolation);
        Ok(Arc::new(match node_id {
            Some(node_id) => deployer.on_node(node_id),
            None => deployer,
        }))
    }
}

/// Builder for DeployImageJob
pub struct DeployImageJobBuilder {
    job_id: Option<String>,
    build_job_id: Option<String>,
    target: Option<DeploymentTarget>,
    config: DeploymentJobConfig,
    node_scheduler: Option<Arc<crate::services::NodeScheduler>>,
    workload_plane: Option<WorkloadPlaneDeploy>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    external_image_tag: Option<String>,
    image_source: Option<DeployImageSource>,
    expected_image_identity: Option<ExpectedImageIdentity>,
    log_config: Option<ContainerLogConfig>,
    encryption_service: Option<Arc<temps_core::EncryptionService>>,
    config_service: Option<Arc<temps_config::ConfigService>>,
    image_builder: Option<Arc<dyn temps_deployer::ImageBuilder>>,
    registry_credentials: Option<temps_deployer::remote::RemotePullCredentials>,
    failed_container_db: Option<Arc<DbConnection>>,
    deployment_id: Option<i32>,
    audit_logger: Option<Arc<dyn temps_core::AuditLogger>>,
    caller: temps_core::docker_socket_grant::DeployCaller,
}

/// The ADR-045 deploy rule, with the grant injected.
///
/// Split out of [`DeployImageJobBuilder::build`] and pure, mirroring
/// `ProjectService::guard_reserved_slug_against`: the process grant is a
/// `OnceLock` frozen at first use, so the rule would otherwise be untestable
/// without mutating process-global environment state from parallel tests.
///
/// `project_slug` is `Option` only because [`DeploymentJobConfig`] stores it
/// that way; the builder's constructor always sets it. `None` is nothing to
/// match against, and a slug no host declares is unaffected — which is every
/// project on every install that never set the variable.
fn refuse_granted_project_deploy(
    grant: &temps_core::docker_socket_grant::DockerSocketGrant,
    project_slug: Option<&str>,
    caller: temps_core::docker_socket_grant::DeployCaller,
) -> Result<(), WorkflowError> {
    let Some(project_slug) = project_slug else {
        return Ok(());
    };
    if !temps_core::docker_socket_grant::deploy_requires_instance_admin(grant, project_slug, caller)
    {
        return Ok(());
    }
    tracing::warn!(
        project_slug = %project_slug,
        caller = ?caller,
        env = temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV,
        "Refused to build a deploy job for a project that holds host Docker access (ADR 045)"
    );
    Err(WorkflowError::DockerSocketDeployRequiresAdmin {
        project_slug: project_slug.to_string(),
    })
}

impl DeployImageJobBuilder {
    /// Start a deploy-image job for `project_slug`.
    ///
    /// The slug is a constructor argument rather than an optional setter
    /// because omitting it is not a smaller deployment — it is a *different*
    /// one (ADR 045): the executing host compares it against its own
    /// `TEMPS_DOCKER_SOCKET_PROJECTS`, and the scheduler gates placement on
    /// it. Two call sites (rollback and promotion) silently omitted it when it
    /// was a setter, which meant a granted project redeployed by either path
    /// would be placed anywhere and started without its socket. A required
    /// argument makes that omission impossible to reintroduce.
    ///
    /// `caller` is required for the same reason, and is the actual enforcement
    /// point for the ADR-045 deploy rule. Gating the *service* methods was not
    /// enough: three remote-deployment handlers build the deployment row and
    /// its jobs themselves and never call any of them. This builder is the one
    /// place every image deployment — present and future — is structurally
    /// forced through, so the check lives here, and a new deploy route cannot
    /// reach a container without having said, in a required argument, on whose
    /// authority it is running.
    pub fn new(
        project_slug: impl Into<String>,
        caller: temps_core::docker_socket_grant::DeployCaller,
    ) -> Self {
        Self {
            caller,
            job_id: None,
            build_job_id: None,
            target: None,
            config: {
                let project_slug = project_slug.into();
                DeploymentJobConfig {
                    // ADR 045: the control plane's own declaration, resolved
                    // here rather than at each call site so no deploy path can
                    // send a slug without the authorization that goes with it.
                    // This code only ever runs on the control plane, so its
                    // process grant *is* the control plane's grant.
                    control_plane_grants_socket: temps_core::docker_socket_grant::process_grant()
                        .declares(&project_slug),
                    project_slug: Some(project_slug),
                    ..DeploymentJobConfig::default()
                }
            },
            node_scheduler: None,
            workload_plane: None,
            log_id: None,
            log_service: None,
            external_image_tag: None,
            image_source: None,
            expected_image_identity: None,
            log_config: None,
            encryption_service: None,
            config_service: None,
            image_builder: None,
            registry_credentials: None,
            failed_container_db: None,
            deployment_id: None,
            audit_logger: None,
        }
    }

    /// Audit sink for the ADR-045 host-Docker-socket record. Optional: an
    /// install with no sink logs the event instead of persisting it.
    pub fn audit_logger(mut self, logger: Option<Arc<dyn temps_core::AuditLogger>>) -> Self {
        self.audit_logger = logger;
        self
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

    pub fn entrypoint(mut self, entrypoint: Option<Vec<String>>) -> Self {
        self.config.entrypoint = entrypoint;
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
    /// entirely after the container reaches running state, so a container that
    /// answers every request with an error still goes live. A service without
    /// an HTTP endpoint does not need this: it has no port, or declares a
    /// non-HTTP `health.type`, and is never HTTP-probed.
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
    pub fn health(mut self, health: Option<temps_core::repo_config::HealthCheckConfig>) -> Self {
        self.config.health = health;
        self
    }

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

    /// Declare where the deployed image lives, which decides how a remote
    /// worker obtains it. See [`DeployImageSource`].
    pub fn image_source(mut self, image_source: DeployImageSource) -> Self {
        self.image_source = Some(image_source);
        self
    }

    /// Bind the deployment to a specific image. See
    /// [`DeployImageJob::with_expected_image_identity`].
    pub fn expected_image_identity(mut self, expected: ExpectedImageIdentity) -> Self {
        self.expected_image_identity = Some(expected);
        self
    }

    /// Apply the image source the planner recorded in a `DeployImageJob` job
    /// config. A config written before the field existed leaves it unset, and
    /// the job derives it (see [`DeployImageSource::resolve`]).
    pub fn image_source_from_job_config(self, config: &serde_json::Value) -> Self {
        match DeployImageSource::from_job_config(config) {
            Some(image_source) => self.image_source(image_source),
            None => self,
        }
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

    /// The environment's ADR-055 workload mode, and what `shadow` and
    /// `enforce` need. `legacy` ignores `workload_plane`.
    pub fn workload_mode(
        mut self,
        mode: temps_entities::environment_workload_mode::EnvironmentWorkloadMode,
        workload_plane: Option<WorkloadPlaneDeploy>,
    ) -> Self {
        self.config.workload_mode = mode;
        self.workload_plane = workload_plane;
        self
    }

    /// The environment's attached volumes (ADR-055 Phase 9).
    pub fn volume_mounts(
        mut self,
        mounts: Vec<temps_workload::VolumeMount>,
        pin: Option<temps_workload_volumes::VolumePin>,
    ) -> Self {
        self.config.volume_mounts = mounts;
        self.config.volume_pin = pin;
        self
    }

    /// The isolation `enforce` replicas request (ADR-055 Phase 10).
    pub fn workload_isolation(mut self, isolation: temps_workload::Isolation) -> Self {
        self.config.workload_isolation = isolation;
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

    /// Set registry credentials for a worker's direct registry pull, when the
    /// deployed image is registry-sourced ([`DeployImageSource::Registry`]) and needs
    /// authentication. See [`DeployImageJob::with_registry_credentials`].
    pub fn registry_credentials(
        mut self,
        credentials: temps_deployer::remote::RemotePullCredentials,
    ) -> Self {
        self.registry_credentials = Some(credentials);
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
        // ADR 045, before anything else is validated: a project this control
        // plane declares runs its image and command as host root, so the
        // deployment is admin-only however it was started. Pure and sync — the
        // process grant is already in memory and the slug is already here — so
        // it costs nothing to make it the first thing the builder does.
        refuse_granted_project_deploy(
            temps_core::docker_socket_grant::process_grant(),
            self.config.project_slug.as_deref(),
            self.caller,
        )?;
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
        if let Some(workload_plane) = self.workload_plane {
            job = job.with_workload_plane(workload_plane);
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
        if let Some(image_source) = self.image_source {
            job = job.with_image_source(image_source);
        }
        if let Some(expected) = self.expected_image_identity {
            job = job.with_expected_image_identity(expected);
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
        if let Some(registry_credentials) = self.registry_credentials {
            job = job.with_registry_credentials(registry_credentials);
        }
        if let (Some(db), Some(deployment_id)) = (self.failed_container_db, self.deployment_id) {
            job = job.with_failed_container_retention(db, deployment_id);
        }
        job.audit_logger = self.audit_logger;

        Ok(job)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod health_probe_tests;
