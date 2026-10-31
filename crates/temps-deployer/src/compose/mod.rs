// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker Compose deployment executor.
//!
//! Manages multi-container deployments using `docker compose` CLI commands.
//! After `compose up`, discovers running containers, applies Temps labels,
//! and returns per-service results that get inserted into `deployment_containers`.
//!
//! [`ComposeExecutor`] and the public request, result and error types are
//! defined here; its behaviour is split by concern:
//!
//! - `support`: detecting a usable Compose v2 on the host
//! - `lifecycle`: prepare, pull, deploy, teardown, destroy and stop
//! - `exec`: running `docker compose` commands with bounded, redacted output
//! - `secrets`: materialising per-service secret files and their override
//! - `files`: writing the compose files and env files Temps generates
//! - `resolve`: resolving and checking the effective configuration, and
//!   snapshotting the files it includes or references
//! - `security_policy`: the deny-list applied to compose documents and overrides
//! - `fs_confinement`: keeping every host path inside the project directory
//! - `overrides`: generated env, network, security and label overrides
//! - `readiness`: waiting for services and verifying the started containers
//! - `diagnostics`: redacting values and classifying `compose config` failures

mod diagnostics;
mod exec;
mod files;
mod fs_confinement;
mod lifecycle;
mod overrides;
mod readiness;
mod resolve;
mod secrets;
mod security_policy;
mod support;
#[cfg(test)]
mod test_support;

pub use diagnostics::{redact_variable_text, SubstitutedValues};
pub use exec::{ComposeOutputCallback, ComposeOutputSink};
pub use files::{EnvFilePlan, EnvFileSource};
pub use resolve::{CheckedComposeConfiguration, ComposeCheckInput, ResolvedComposeConfiguration};
pub use support::{
    ComposeSupport, COMPOSE_CONFIG_FLAGS, COMPOSE_CONSISTENCY_FLAGS, MIN_COMPOSE_VERSION,
};

use bollard::Docker;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Weak};
use temps_core::{DockerHandle, DockerUnavailable};
use temps_entities::compose_security::ComposeSecurityCheck as PolicyCheck;
use temps_presets::compose_check::{ComposeCheckCommand, ComposeCheckErrorClass};
use thiserror::Error;

use crate::compose_interpolation::InterpolationError;
use tokio::sync::Mutex;

/// How long `deploy()` waits for every Compose service to report `running`
/// (and `healthy`, if it defines a healthcheck) before failing the
/// deployment. Mirrors the single-container deploy path's
/// `health_check_timeout_secs` default.
const COMPOSE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Interval between `docker compose ps` polls while waiting for readiness.
const COMPOSE_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Maximum time allowed for `docker compose pull` to complete. Pulls can be
/// slow on large images or slow registries, but an unbounded pull would stall a
/// deployment forever. 300 s matches [`COMPOSE_READY_TIMEOUT`] — both bound a
/// single phase of the deployment that should complete within minutes, not
/// hours, on any reasonable network.
const COMPOSE_PULL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const COMPOSE_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);
const COMPOSE_UP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Resolving the fully merged Compose model is local work and should complete
/// quickly. Keep it bounded independently from image pulls so a wedged Compose
/// plugin cannot stall a deployment after all images have already downloaded.
const COMPOSE_CONFIG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const COMPOSE_VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A merged Compose model expands anchors and interpolation, so its output can
/// be much larger than the source. Bound both memory and subsequent Docker API
/// fan-out before parsing tenant-controlled output.
const MAX_RESOLVED_COMPOSE_CONFIG_BYTES: usize = 4 * 1024 * 1024;
const MAX_COMPOSE_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_RESOLVED_COMPOSE_SERVICES: usize = 256;
// Platform-owned memory ceiling, not a reservation. CPU limits are left to
// the operator: a fixed CPU cap can exceed the Docker host's available CPUs
// and prevent container creation on smaller hosts.
const COMPOSE_SERVICE_MEMORY_LIMIT: &str = "4g";
const DOCKER_IMAGE_INSPECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_CONCURRENT_IMAGE_INSPECTIONS: usize = 8;

/// How long a single TCP connect attempt against a published port may take
/// before `port_reachable` gives up and reports the port not yet listening.
/// Short by design: this runs once per poll interval, not once total, so a
/// slow/unreachable port degrades to "still pending" rather than stalling
/// the whole readiness loop.
const PORT_PROBE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);

/// Docker label Compose attaches to every network/volume/container it
/// manages, set to the `-p <project_name>` value.
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// Temps-generated override that bind-mounts the stack's materialized secret
/// files into every service. Written last in the `-f` order so a repository
/// or user override cannot redirect the mount.
const TEMPS_SECRETS_OVERRIDE: &str = "docker-compose.temps-secrets.yml";

static COMPOSE_LIFECYCLE_LOCKS: LazyLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Mount point for project secrets inside every container. Identical to the
/// single-container deploy path (`DockerRuntime`), so an application reads its
/// secrets the same way regardless of which preset deployed it.
const CONTAINER_SECRETS_DIR: &str = "/run/secrets";

/// Maximum diagnostic text persisted into a deployment error. Full container
/// logs remain available through the authenticated logs endpoint.
const MAX_COMPOSE_DIAGNOSTIC_BYTES: usize = 32 * 1024;

/// Where the console exposes the per-service "Elevated permissions" toggle.
/// Named once here because two places quote it — the pre-deploy advisory in
/// `temps-deployments::jobs::deploy_compose` and the post-failure remediation
/// in [`ComposeExecutor::capability_denial_remediation`]. A UI reshuffle that
/// updated only one of them would send half of users to a page that no longer
/// exists.
pub const ELEVATED_PERMISSIONS_SETTINGS_PATH: &str =
    "Project Settings → Build & deploy → Build → Docker Compose → Compose services";

#[derive(Error, Debug)]
pub enum ComposeError {
    #[error("Compose command failed for project '{project}': {reason}")]
    CommandFailed { project: String, reason: String },

    #[error("Failed to write compose files to '{path}': {reason}")]
    FileWriteFailed { path: String, reason: String },

    #[error("Failed to discover containers for project '{project}': {reason}")]
    DiscoveryFailed { project: String, reason: String },

    #[error("Invalid compose override for project '{project}': {reason}")]
    InvalidOverride { project: String, reason: String },

    #[error("Docker API error: {0}")]
    Docker(String),

    #[error(
        "Docker Compose v2 is unavailable: {reason}. Install the Docker Compose CLI plugin and verify it with `docker compose version`, then restart Temps"
    )]
    ComposeUnavailable { reason: String },

    #[error("Compose security policy rejected {field} for service '{service}': {reason}. Review Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings; only instance administrators can change security checks. Invalid Compose values must be corrected in the Compose file")]
    SecurityPolicyViolation {
        service: String,
        field: String,
        reason: String,
    },

    #[error("Failed to parse compose YAML for '{compose_source}': {reason}")]
    InvalidComposeYaml {
        compose_source: String,
        reason: String,
    },

    #[error("Compose path '{path}' rejected for field '{field}': {reason}")]
    InvalidComposePath {
        field: String,
        path: String,
        reason: String,
    },

    #[error("Invalid Compose environment variable '{key}': {reason}")]
    InvalidEnvironmentVariable { key: String, reason: String },

    #[error("Compose stack '{project}' did not become ready within {timeout_secs}s: {reason}")]
    ServicesNotReady {
        project: String,
        timeout_secs: u64,
        reason: String,
    },

    /// `docker compose config` rejected the model. `cause` is safe to show
    /// to anyone who can read the deployment; `diagnostics` carries the exact
    /// command, exit code and a sanitized stderr for the deployment log.
    #[error(
        "Compose configuration resolution failed for project '{project}' (exit status {exit_status}, {class}): {cause}"
    )]
    ConfigResolutionFailed {
        project: String,
        class: ComposeConfigFailureClass,
        exit_status: String,
        cause: String,
        diagnostics: Box<ComposeCommandDiagnostics>,
    },

    /// A `${VAR}` expression could not be resolved before validation.
    #[error("{0}")]
    Interpolation(#[from] InterpolationError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// This process has no local Docker daemon -- the control-plane serve
    /// profile, which runs no local workloads. Every method that eventually
    /// touches Docker (a `docker compose` CLI invocation or a bollard call)
    /// resolves this as its very first step, before doing anything else, so
    /// a caller sees this typed error rather than a raw CLI/connection
    /// failure part-way through a deploy.
    #[error(transparent)]
    DockerUnavailable(#[from] DockerUnavailable),

    /// The address a worker asked to publish the stack's ports on cannot be
    /// used without exposing them beyond the cluster.
    #[error("Cannot publish Compose ports on host address '{address}': {reason}")]
    InvalidPublishedHostAddress { address: String, reason: String },

    /// An operation that ran on a worker node failed there. `message` is the
    /// worker's own error text, so the deployment records the same failure it
    /// would have recorded locally, and `class` is its class.
    #[error("{message} (on worker node '{node_name}')")]
    Remote {
        node_name: String,
        message: String,
        class: ComposeCheckErrorClass,
    },
}

impl ComposeError {
    /// The coarse class shared by deploy, preview and "Run compose check".
    pub fn class(&self) -> ComposeCheckErrorClass {
        match self {
            Self::SecurityPolicyViolation { .. } => ComposeCheckErrorClass::PolicyViolation,
            Self::InvalidComposePath { .. } => ComposeCheckErrorClass::InvalidPath,
            Self::InvalidComposeYaml { .. } => ComposeCheckErrorClass::InvalidYaml,
            Self::InvalidOverride { .. } => ComposeCheckErrorClass::InvalidOverride,
            Self::Interpolation(_) | Self::InvalidEnvironmentVariable { .. } => {
                ComposeCheckErrorClass::Interpolation
            }
            Self::ConfigResolutionFailed { class, .. } => match class {
                ComposeConfigFailureClass::UnsupportedComposeVersion => {
                    ComposeCheckErrorClass::UnsupportedComposeVersion
                }
                ComposeConfigFailureClass::RequiredVariable
                | ComposeConfigFailureClass::ExtendsTarget
                | ComposeConfigFailureClass::UndefinedDependency
                | ComposeConfigFailureClass::MissingEnvFile
                | ComposeConfigFailureClass::MissingFile
                | ComposeConfigFailureClass::InvalidModel
                | ComposeConfigFailureClass::InvalidYaml
                | ComposeConfigFailureClass::Unclassified => {
                    ComposeCheckErrorClass::ConfigResolution
                }
            },
            Self::CommandFailed { .. } => ComposeCheckErrorClass::ConfigResolution,
            Self::ComposeUnavailable { .. } | Self::DockerUnavailable(_) => {
                ComposeCheckErrorClass::ComposeUnavailable
            }
            Self::FileWriteFailed { .. }
            | Self::DiscoveryFailed { .. }
            | Self::Docker(_)
            | Self::ServicesNotReady { .. }
            | Self::InvalidPublishedHostAddress { .. }
            | Self::Io(_) => ComposeCheckErrorClass::Internal,
            Self::Remote { class, .. } => *class,
        }
    }
}

/// Why `docker compose config` failed, classified from its stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeConfigFailureClass {
    /// The Compose CLI does not know an option Temps passes.
    UnsupportedComposeVersion,
    RequiredVariable,
    ExtendsTarget,
    UndefinedDependency,
    MissingEnvFile,
    MissingFile,
    InvalidModel,
    InvalidYaml,
    /// Anything Temps does not recognise; the sanitized stderr is the
    /// diagnostic.
    Unclassified,
}

impl std::fmt::Display for ComposeConfigFailureClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedComposeVersion => "unsupported Compose version",
            Self::RequiredVariable => "required variable",
            Self::ExtendsTarget => "extends target",
            Self::UndefinedDependency => "undefined dependency",
            Self::MissingEnvFile => "missing env file",
            Self::MissingFile => "missing file",
            Self::InvalidModel => "invalid model",
            Self::InvalidYaml => "invalid YAML",
            Self::Unclassified => "unclassified",
        })
    }
}

/// One `docker compose` invocation, sanitized for the deployment log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeCommandDiagnostics {
    /// Program and arguments. Variable values are never passed on the command
    /// line (they travel in a temporary env file), and known values are
    /// redacted regardless.
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    /// Sanitized stderr tail: see [`sanitize_config_stderr`].
    pub stderr: String,
}

impl ComposeCommandDiagnostics {
    pub fn into_report(self) -> ComposeCheckCommand {
        ComposeCheckCommand {
            argv: self.argv,
            exit_code: self.exit_code,
            stderr: (!self.stderr.is_empty()).then_some(self.stderr),
        }
    }
}

/// A failed Compose startup together with every container Docker managed to
/// create for the candidate stack.
///
/// Readiness failures are operationally different from preparation failures:
/// the containers often contain the only useful diagnostics. Carrying them
/// alongside the typed error lets the deployment layer register them for
/// authenticated log access instead of immediately destroying the evidence.
#[derive(Debug)]
pub struct ComposeDeployFailure {
    pub error: ComposeError,
    pub containers: Vec<ComposeServiceResult>,
    /// Ownership-verified containers that are unsafe to retain but can be
    /// removed directly if `docker compose down` fails.
    pub cleanup_containers: Vec<ComposeServiceResult>,
    /// Why a partially-created stack could not be retained safely. The
    /// primary deployment error remains in `error`; this secondary reason is
    /// for operator diagnostics and forces the caller to tear the stack down.
    pub retention_error: Option<ComposeError>,
}

impl std::fmt::Display for ComposeDeployFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ComposeDeployFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl ComposeDeployFailure {
    fn without_containers(error: ComposeError) -> Self {
        Self {
            error,
            containers: Vec::new(),
            cleanup_containers: Vec::new(),
            retention_error: None,
        }
    }
}

/// Request to deploy a Docker Compose stack.
#[derive(Debug, Clone)]
pub struct ComposeDeployRequest {
    /// Compose project name (e.g., "temps-{project_id}-{env_id}")
    pub project_name: String,
    /// Compose file content (the YAML)
    pub compose_content: String,
    /// Optional .env file content
    pub env_content: Option<String>,
    /// Working directory where compose files are written
    pub work_dir: PathBuf,
    /// Path to compose file relative to work_dir (default: "docker-compose.yml")
    pub compose_path: Option<String>,
    /// Environment variables to inject (merged with .env)
    pub environment_vars: HashMap<String, String>,
    /// Decrypted project secrets, keyed by name. Materialized as files under
    /// `/run/secrets/<KEY>` via a generated override.
    ///
    /// Deliberately separate from [`Self::environment_vars`] and
    /// [`Self::build_args`]: values here must never reach a container
    /// environment, a build argument, an env file, or `docker inspect`.
    pub secrets: HashMap<String, String>,
    /// Which Compose services may read each secret, keyed by secret name.
    ///
    /// A key absent from this map (or mapped to an empty list) goes to every
    /// service -- that is the pre-scoping behaviour, and it has to stay the
    /// default so an unconfigured secret is never silently withheld from the
    /// service that needs it.
    pub secret_compose_services: HashMap<String, Vec<String>>,
    /// Platform-owned arguments passed only to `docker compose build`.
    /// These are deliberately separate from service runtime environments.
    pub build_args: HashMap<String, String>,
    /// Temps labels to apply to all containers
    pub labels: HashMap<String, String>,
    /// Source repo directory (needed for compose files with build: directives)
    pub repo_dir: Option<PathBuf>,
    /// User-provided docker-compose.temps-override.yml content
    pub compose_override: Option<String>,
    /// Compose service names granted back the minimal Linux capabilities
    /// (see [`ComposeExecutor::RELAXED_CAPABILITIES`]) needed by official
    /// database images to fix ownership on their data/socket directory at
    /// container start. Empty by default — every service keeps the full
    /// `cap_drop: ALL` sandbox unless the user has explicitly opted it in.
    pub relaxed_capability_services: Vec<String>,
    /// Compose services explicitly exempted from Temps' generated runtime
    /// sandbox. Repository security validation still applies.
    pub unsandboxed_services: Vec<String>,
    /// How long every service may take to become running (and healthy, when
    /// it defines a healthcheck) after `up`. `None` uses
    /// [`COMPOSE_READY_TIMEOUT`]; the project's startup-timeout setting
    /// supplies an override.
    pub ready_timeout: Option<std::time::Duration>,
    /// Services Temps routes public traffic to. They must stay running: a
    /// routed service that exits — even with status 0 — fails the deploy,
    /// while other services may exit 0 as completed one-shot tasks.
    pub routed_services: Vec<String>,
    /// Receives `docker compose build`, `pull`, and `up` output while each
    /// command runs, already redacted. Without it a failing build shows only
    /// the diagnostic tail attached to the error.
    pub output_sink: Option<ComposeOutputSink>,
}

/// Result for a single compose service after deployment.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComposeServiceResult {
    pub container_id: String,
    pub container_name: String,
    pub service_name: String,
    pub image_name: String,
    /// Ports published to the host (may be empty for internal services)
    pub ports: Vec<ComposePortBinding>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComposePortBinding {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

/// State produced by [`ComposeExecutor::prepare_and_pull`] and consumed by
/// [`ComposeExecutor::deploy_prepared`].
///
/// Carrying these fields out of `prepare_and_pull` avoids re-computing them in
/// `deploy_prepared` and keeps the split-phase API type-safe: a caller cannot
/// pass an arbitrary directory to `deploy_prepared` — it must come from the
/// same preparation step that wrote the files to disk.
pub struct PreparedComposeDeploy {
    /// Resolved working directory (repo checkout when available, otherwise the
    /// Temps data-dir project directory).
    pub effective_dir: PathBuf,
    /// Compose project name (e.g., `"temps-{project_id}-{env_id}"`).
    pub project_name: String,
    /// Compose file name relative to `effective_dir`.
    pub compose_file: String,
    /// Values to strip from diagnostic messages (secrets, env values, etc.).
    pub redact_values: Vec<String>,
    /// Deployment-scoped directory containing only this candidate's secrets.
    /// The previous generation remains mounted by the live stack until this
    /// candidate has started successfully.
    pub secret_generation: String,
    pub has_materialized_secrets: bool,
}

/// Docker Compose deployment executor.
#[derive(Debug)]
pub struct ComposeExecutor {
    policy: temps_entities::compose_security::ComposeSecurityPolicy,
    policy_authoritative: bool,
    /// The process-wide Docker client, which may be unavailable on a
    /// control-plane node that runs no local workloads. Every method that
    /// eventually needs the daemon calls [`Self::require_docker`] as late as
    /// possible -- immediately before the first CLI/API call it makes --
    /// so this executor can always be constructed, even with no daemon.
    docker: Arc<DockerHandle>,
    /// Base directory for compose work dirs
    data_dir: PathBuf,
    /// Host address the stack's published ports are bound to.
    ///
    /// The security policy only admits `127.0.0.1` bindings, which is right
    /// on the control plane: its proxy dials loopback. A worker node's proxy
    /// path comes from the control plane over the node's private (or mesh)
    /// address instead, so a worker sets that address here and every
    /// policy-checked loopback binding is rebound to it when the files are
    /// written (see [`Self::rebind_loopback_published_ports`]). The author
    /// still decides which ports are published; only the interface changes,
    /// and it is never a public or wildcard address.
    published_host_ip: std::net::IpAddr,
}

impl ComposeExecutor {
    /// Bind the stack's published ports to `address` instead of loopback.
    ///
    /// Only loopback and private addresses are accepted: the point of the
    /// loopback rule is that published ports never bypass the Temps proxy,
    /// and a worker's private address is reachable only from the cluster.
    pub fn with_published_host_ip(
        mut self,
        address: std::net::IpAddr,
    ) -> Result<Self, ComposeError> {
        if address.is_unspecified() || address.is_multicast() {
            return Err(ComposeError::InvalidPublishedHostAddress {
                address: address.to_string(),
                reason: "a wildcard or multicast address would publish the stack's ports on \
                         every interface"
                    .to_string(),
            });
        }
        if !temps_core::node_address::is_private_publish_address(address) {
            return Err(ComposeError::InvalidPublishedHostAddress {
                address: address.to_string(),
                reason: "it is neither loopback nor a private address, so the stack's ports \
                         would be reachable from outside the cluster without the Temps proxy"
                    .to_string(),
            });
        }
        self.published_host_ip = address;
        Ok(self)
    }

    /// The host address published ports are bound to and probed on.
    pub fn published_host_ip(&self) -> std::net::IpAddr {
        self.published_host_ip
    }

    /// Immutable deployment-scoped grants, loaded by the workflow from administrator settings.
    pub fn with_security_policy(
        mut self,
        policy: temps_entities::compose_security::ComposeSecurityPolicy,
    ) -> Self {
        self.policy = policy;
        self.policy_authoritative = true;
        self
    }

    fn enforced(&self, check: PolicyCheck) -> bool {
        self.policy.enforced(check)
    }

    fn field_enforced(&self, field: &str) -> bool {
        let check = match field {
            "privileged" => PolicyCheck::Privileged,
            "use_api_socket" => PolicyCheck::DockerSocket,
            "cap_add" => PolicyCheck::Capabilities,
            "devices" => PolicyCheck::Devices,
            "device_cgroup_rules" => PolicyCheck::DeviceRules,
            "security_opt" => PolicyCheck::SecurityOptions,
            "gpus" => PolicyCheck::Gpu,
            "extends" => PolicyCheck::Extends,
            "volumes_from" => PolicyCheck::VolumesFrom,
            "external_links" => PolicyCheck::ExternalLinks,
            "label_file" => PolicyCheck::LabelFiles,
            "post_start" | "pre_stop" => PolicyCheck::LifecycleHooks,
            "provider" => PolicyCheck::Provider,
            "container_name" => PolicyCheck::ContainerName,
            "blkio_config" => PolicyCheck::Blkio,
            "storage_opt" => PolicyCheck::StorageOptions,
            "memswap_limit" => PolicyCheck::Swap,
            "sysctls" => PolicyCheck::Sysctls,
            "group_add" => PolicyCheck::Groups,
            "cgroup_parent" => PolicyCheck::CgroupParent,
            "runtime" => PolicyCheck::Runtime,
            "oom_kill_disable" => PolicyCheck::OomKiller,
            "tmpfs" => PolicyCheck::Tmpfs,
            "ulimits" => PolicyCheck::Ulimits,
            "build.privileged" => PolicyCheck::BuildPrivileged,
            "build.entitlements" => PolicyCheck::BuildEntitlements,
            "build.additional_contexts" => PolicyCheck::BuildAdditionalContexts,
            "build.cache_from" => PolicyCheck::BuildCacheFrom,
            "build.cache_to" => PolicyCheck::BuildCacheTo,
            "build.tags" => PolicyCheck::BuildTags,
            "build.ssh" => PolicyCheck::BuildSsh,
            "build.shm_size" => PolicyCheck::BuildShm,
            "build.ulimits" => PolicyCheck::BuildUlimits,
            _ => return true,
        };
        self.enforced(check)
    }

    /// Construct from a concrete Docker client.
    ///
    /// Existing callers pass an `Arc<Docker>` directly; this wraps it into a
    /// [`DockerHandle::available`] so their call sites need not change.
    /// New call sites -- and any that must work in the control-plane serve
    /// profile, where no client is ever constructed -- should use
    /// [`Self::new_with_handle`] instead.
    pub fn new(docker: Arc<Docker>, data_dir: PathBuf) -> Self {
        Self::new_with_handle(Arc::new(DockerHandle::available(docker)), data_dir)
    }

    /// Construct from the process-wide [`DockerHandle`], which may carry an
    /// available client or a typed explanation of why none exists in this
    /// process (e.g. the control-plane serve profile). Constructing this
    /// executor never itself requires or probes a daemon; absence is only
    /// surfaced the first time an operation actually needs one, via
    /// [`Self::require_docker`].
    pub fn new_with_handle(docker: Arc<DockerHandle>, data_dir: PathBuf) -> Self {
        Self {
            docker,
            data_dir,
            policy: Default::default(),
            policy_authoritative: false,
            published_host_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        }
    }

    /// Resolve the Docker client for an operation that cannot proceed
    /// without it. Call this as late as possible -- immediately before the
    /// first `docker compose` CLI invocation or bollard call an operation
    /// makes, never after -- so a control-plane process with no daemon fails
    /// with this typed, actionable error instead of a raw CLI/connection
    /// failure part-way through.
    fn require_docker(&self) -> Result<Arc<Docker>, ComposeError> {
        self.docker.require().map_err(ComposeError::from)
    }

    /// What each container the legacy path started looks like, for the
    /// ADR-055 shadow comparison: image, variable names (never values),
    /// restart policy, networks and published container ports. One inspect
    /// per service.
    pub async fn observe_services(
        &self,
        project_name: &str,
        services: &[ComposeServiceResult],
    ) -> Result<Vec<crate::compose_translation::ObservedService>, ComposeError> {
        let docker = self.require_docker()?;
        let mut observed = Vec::with_capacity(services.len());
        for service in services {
            let inspect = docker
                .inspect_container(&service.container_id, None)
                .await
                .map_err(|e| ComposeError::DiscoveryFailed {
                    project: project_name.to_string(),
                    reason: format!(
                        "inspecting container {} of service '{}': {e}",
                        service.container_id, service.service_name
                    ),
                })?;
            let config = inspect.config.unwrap_or_default();
            let host_config = inspect.host_config.unwrap_or_default();
            let restart = host_config
                .restart_policy
                .and_then(|policy| policy.name)
                .and_then(|name| serde_json::to_value(name).ok())
                .and_then(|value| value.as_str().map(str::to_string));
            let published_ports = host_config
                .port_bindings
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, bindings)| bindings.as_ref().is_some_and(|b| !b.is_empty()))
                .filter_map(|(port, _)| port.split('/').next()?.parse::<u16>().ok())
                .collect();
            observed.push(crate::compose_translation::ObservedService {
                service: service.service_name.clone(),
                image: config.image.unwrap_or_else(|| service.image_name.clone()),
                env_names: config
                    .env
                    .unwrap_or_default()
                    .iter()
                    .map(|entry| entry.split('=').next().unwrap_or_default().to_string())
                    .collect(),
                restart,
                networks: inspect
                    .network_settings
                    .and_then(|settings| settings.networks)
                    .map(|networks| networks.into_keys().collect())
                    .unwrap_or_default(),
                published_ports,
            });
        }
        Ok(observed)
    }

    /// Get the work directory for a compose project.
    fn project_dir(&self, project_name: &str) -> PathBuf {
        self.data_dir.join("compose").join(project_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker can never ask to publish on every interface.
    #[test]
    fn published_host_ip_accepts_only_loopback_and_private_addresses() {
        let executor = || {
            ComposeExecutor::new_with_handle(
                Arc::new(DockerHandle::disabled(
                    temps_core::PROFILE_CONTROL_PLANE,
                    temps_core::CONTROL_PLANE_DOCKER_REASON,
                )),
                std::env::temp_dir(),
            )
        };
        for refused in ["0.0.0.0", "::", "239.0.0.1", "203.0.113.10", "2001:db8::10"] {
            let error = executor()
                .with_published_host_ip(refused.parse().unwrap())
                .unwrap_err();
            assert!(
                matches!(error, ComposeError::InvalidPublishedHostAddress { .. }),
                "{refused}: {error}"
            );
        }
        for accepted in [
            "10.20.0.5",
            "192.168.1.9",
            "fd00::7",
            "127.0.0.1",
            "127.0.0.53",
        ] {
            let executor = executor()
                .with_published_host_ip(accepted.parse().unwrap())
                .unwrap();
            assert_eq!(executor.published_host_ip().to_string(), accepted);
        }
    }
}
