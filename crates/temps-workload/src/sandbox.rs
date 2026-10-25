// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox-only driver operations (ADR-055 §6, phase 6).
//!
//! [`RuntimeDriver`] is what every runtime implements. Sandboxes need more:
//! file access, interactive terminals, the model, git and MCP relays,
//! snapshots and restore, disk resize. Those live in the optional
//! [`SandboxExt`] trait, which a driver implements when it can host
//! sandboxes. The sandbox product's `SandboxProvider` is a facade over
//! `RuntimeDriver + SandboxExt`.
//!
//! **Capabilities are filters, not hints.** [`SandboxCapabilities`] says
//! which sandbox-only operations a driver supports. [`choose_driver`] never
//! places a sandbox on a driver that lacks what its spec needs
//! ([`SandboxRequirements`]); a request nothing can serve is a typed
//! [`NoEligibleDriver`] that names every driver and what it lacks. A
//! requested limit a driver cannot apply is not a reason to refuse: it is
//! reported as `LimitEnforced = False` with the reason
//! ([`limit_condition`]), never ignored silently (§5).

use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::{
    Condition, ConditionKind, ConditionStatus, DriverCapabilities, DriverError, EgressProfile,
    InstanceHandle, Isolation, RuntimeDriver, SandboxEgressPolicy, WorkloadRef,
};

/// Which egress profiles a driver can enforce for a sandbox.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressSupport {
    pub open: bool,
    pub restricted: bool,
    pub none: bool,
}

impl EgressSupport {
    pub fn supports(&self, profile: EgressProfile) -> bool {
        match profile {
            EgressProfile::Open => self.open,
            EgressProfile::Restricted => self.restricted,
            EgressProfile::None => self.none,
        }
    }
}

/// The sandbox-only operations a driver supports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCapabilities {
    /// Read and write files and directories in the instance.
    pub file_access: bool,
    /// Interactive terminals through the in-sandbox PTY agent.
    pub pty: bool,
    /// The retained agent runtime transport and harness recovery.
    pub agent_runtime: bool,
    pub model_relay: bool,
    pub git_relay: bool,
    pub mcp_relay: bool,
    /// Capture a snapshot of a running sandbox.
    pub snapshot: bool,
    /// Create a sandbox from a snapshot this driver took.
    pub restore: bool,
    /// Grow the root disk of an existing sandbox.
    pub resize_disk: bool,
    /// Join an isolated per-application network with its data services.
    pub application_network: bool,
    /// Mount a named workspace volume at the work dir.
    pub workspace_volume: bool,
    /// Run Docker inside the guest (never the host's socket).
    pub guest_docker: bool,
    pub egress: EgressSupport,
    /// Enforce a `restricted` [`SandboxEgressPolicy`]: outbound traffic
    /// limited to named hosts and CIDR blocks.
    pub egress_rules: bool,
}

/// One thing a sandbox can need from a driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxFeature {
    Exec,
    FileAccess,
    Pty,
    AgentRuntime,
    ModelRelay,
    GitRelay,
    McpRelay,
    Snapshot,
    Restore,
    ResizeDisk,
    ApplicationNetwork,
    WorkspaceVolume,
    GuestDocker,
    Egress(EgressProfile),
    /// A `restricted` egress policy: an allow-list of hosts and CIDR blocks.
    EgressRules,
    /// The `Untrusted` safety caps (§4) a process-isolated driver must be
    /// able to apply: a memory ceiling and a pids cap. A microVM bounds both
    /// by its own size and kernel.
    SafetyCaps,
}

impl fmt::Display for SandboxFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SandboxFeature::Exec => "exec",
            SandboxFeature::FileAccess => "file access",
            SandboxFeature::Pty => "interactive terminals",
            SandboxFeature::AgentRuntime => "the retained agent runtime",
            SandboxFeature::ModelRelay => "the model relay",
            SandboxFeature::GitRelay => "the git relay",
            SandboxFeature::McpRelay => "the harness MCP relay",
            SandboxFeature::Snapshot => "snapshots",
            SandboxFeature::Restore => "snapshot restore",
            SandboxFeature::ResizeDisk => "disk resize",
            SandboxFeature::ApplicationNetwork => "application networks",
            SandboxFeature::WorkspaceVolume => "workspace volumes",
            SandboxFeature::GuestDocker => "guest Docker",
            SandboxFeature::Egress(EgressProfile::Open) => "open egress",
            SandboxFeature::Egress(EgressProfile::Restricted) => "restricted egress",
            SandboxFeature::Egress(EgressProfile::None) => "no-network egress",
            SandboxFeature::EgressRules => "egress allow-lists (a restricted egress policy)",
            SandboxFeature::SafetyCaps => "the sandbox safety caps (memory and pids)",
        })
    }
}

/// Whether a driver with these capabilities supports `feature`.
pub fn supports(
    driver: &DriverCapabilities,
    sandbox: &SandboxCapabilities,
    feature: SandboxFeature,
) -> bool {
    match feature {
        SandboxFeature::Exec => driver.exec,
        SandboxFeature::FileAccess => sandbox.file_access,
        SandboxFeature::Pty => sandbox.pty,
        SandboxFeature::AgentRuntime => sandbox.agent_runtime,
        SandboxFeature::ModelRelay => sandbox.model_relay,
        SandboxFeature::GitRelay => sandbox.git_relay,
        SandboxFeature::McpRelay => sandbox.mcp_relay,
        SandboxFeature::Snapshot => driver.snapshot && sandbox.snapshot,
        SandboxFeature::Restore => sandbox.restore,
        SandboxFeature::ResizeDisk => driver.resize_disk && sandbox.resize_disk,
        SandboxFeature::ApplicationNetwork => sandbox.application_network,
        SandboxFeature::WorkspaceVolume => sandbox.workspace_volume,
        SandboxFeature::GuestDocker => sandbox.guest_docker,
        SandboxFeature::Egress(profile) => sandbox.egress.supports(profile),
        SandboxFeature::EgressRules => sandbox.egress_rules,
        SandboxFeature::SafetyCaps => match driver.isolation {
            Isolation::MicroVm => true,
            Isolation::Process | Isolation::InHost => driver.limits.memory && driver.limits.pids,
        },
    }
}

/// Limits a sandbox asked for. Soft: a driver that cannot apply one still
/// runs the sandbox and reports `LimitEnforced = False`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedLimits {
    pub cpu: bool,
    pub memory: bool,
    pub pids: bool,
    pub disk: bool,
}

/// What a sandbox spec needs from its driver.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRequirements {
    /// Hard requirements: a driver lacking any of these is never chosen.
    pub features: Vec<SandboxFeature>,
    /// Soft: reported as a condition when the driver cannot apply them.
    pub limits: RequestedLimits,
}

impl SandboxRequirements {
    /// Every sandbox execs commands and must hold the safety caps.
    pub fn baseline() -> Self {
        Self {
            features: vec![SandboxFeature::Exec, SandboxFeature::SafetyCaps],
            limits: RequestedLimits::default(),
        }
    }

    pub fn require(mut self, feature: SandboxFeature) -> Self {
        if !self.features.contains(&feature) {
            self.features.push(feature);
        }
        self
    }

    /// What a driver lacks of these requirements, in requirement order.
    pub fn missing(
        &self,
        driver: &DriverCapabilities,
        sandbox: &SandboxCapabilities,
    ) -> Vec<SandboxFeature> {
        self.features
            .iter()
            .copied()
            .filter(|feature| !supports(driver, sandbox, *feature))
            .collect()
    }
}

/// A driver the scheduler may pick, with its capabilities.
#[derive(Debug, Clone, Copy)]
pub struct DriverCandidate<'a> {
    pub driver: &'a DriverCapabilities,
    pub sandbox: &'a SandboxCapabilities,
}

/// Why a driver was not chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverRejection {
    pub driver: String,
    pub missing: Vec<SandboxFeature>,
}

impl fmt::Display for DriverRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let missing: Vec<String> = self.missing.iter().map(ToString::to_string).collect();
        write!(f, "{} lacks {}", self.driver, missing.join(", "))
    }
}

/// No driver can host a sandbox with these requirements. The message names
/// every driver considered and what it lacks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoEligibleDriver {
    #[error("sandbox '{sandbox}' asked for the '{requested}' driver, which is not available on this node (available: {})", available.join(", "))]
    NotAvailable {
        sandbox: String,
        requested: String,
        available: Vec<String>,
    },
    #[error("sandbox '{sandbox}': 0/{} drivers eligible: {}", rejections.len(), describe(rejections))]
    Ineligible {
        sandbox: String,
        rejections: Vec<DriverRejection>,
    },
}

fn describe(rejections: &[DriverRejection]) -> String {
    if rejections.is_empty() {
        return "no sandbox driver is installed on this node".to_string();
    }
    rejections
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// The driver a sandbox is placed on, and the drivers passed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverChoice {
    /// Index into the candidates.
    pub index: usize,
    /// Drivers tried before it, with what they lacked. Non-empty only when
    /// the preferred driver could not take the sandbox.
    pub passed_over: Vec<DriverRejection>,
}

/// Filter, then rank (§6, §8). `requested` is a driver the caller asked for
/// by name: it is the only candidate, and it is refused if it lacks a
/// requirement. Otherwise `preferred` (the node's default driver) is tried
/// first, then the rest in candidate order.
pub fn choose_driver(
    sandbox: &str,
    candidates: &[DriverCandidate<'_>],
    requested: Option<&str>,
    preferred: Option<&str>,
    requirements: &SandboxRequirements,
) -> Result<DriverChoice, NoEligibleDriver> {
    let rejection = |candidate: &DriverCandidate<'_>| DriverRejection {
        driver: candidate.driver.name.clone(),
        missing: requirements.missing(candidate.driver, candidate.sandbox),
    };
    if let Some(requested) = requested {
        let Some(index) = candidates.iter().position(|c| c.driver.name == requested) else {
            return Err(NoEligibleDriver::NotAvailable {
                sandbox: sandbox.to_string(),
                requested: requested.to_string(),
                available: candidates.iter().map(|c| c.driver.name.clone()).collect(),
            });
        };
        let rejected = rejection(&candidates[index]);
        if rejected.missing.is_empty() {
            return Ok(DriverChoice {
                index,
                passed_over: Vec::new(),
            });
        }
        return Err(NoEligibleDriver::Ineligible {
            sandbox: sandbox.to_string(),
            rejections: vec![rejected],
        });
    }

    let first = preferred.and_then(|p| candidates.iter().position(|c| c.driver.name == p));
    let order = first
        .into_iter()
        .chain((0..candidates.len()).filter(move |i| Some(*i) != first));

    let mut passed_over = Vec::new();
    for index in order {
        let rejected = rejection(&candidates[index]);
        if rejected.missing.is_empty() {
            return Ok(DriverChoice { index, passed_over });
        }
        passed_over.push(rejected);
    }
    Err(NoEligibleDriver::Ineligible {
        sandbox: sandbox.to_string(),
        rejections: passed_over,
    })
}

/// One kind of limit, for messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Cpu,
    Memory,
    Pids,
    Disk,
}

impl LimitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitKind::Cpu => "cpu",
            LimitKind::Memory => "memory",
            LimitKind::Pids => "pids",
            LimitKind::Disk => "disk",
        }
    }
}

/// Requested limits the driver cannot apply.
pub fn unenforced_limits(
    driver: &DriverCapabilities,
    requested: RequestedLimits,
) -> Vec<LimitKind> {
    let mut gaps = Vec::new();
    if requested.cpu && !driver.limits.cpu {
        gaps.push(LimitKind::Cpu);
    }
    if requested.memory && !driver.limits.memory {
        gaps.push(LimitKind::Memory);
    }
    if requested.pids && !driver.limits.pids {
        gaps.push(LimitKind::Pids);
    }
    if requested.disk && !driver.limits.disk {
        gaps.push(LimitKind::Disk);
    }
    gaps
}

/// The `LimitEnforced` condition for a sandbox on `driver`, or `None` when
/// it requested no limit. `explain` gives the driver's reason for a limit it
/// cannot apply ("docker: disk limits need an XFS/overlay2 project quota on
/// this host").
pub fn limit_condition(
    driver: &DriverCapabilities,
    requested: RequestedLimits,
    explain: impl Fn(LimitKind) -> String,
    now: DateTime<Utc>,
) -> Option<Condition> {
    if requested == RequestedLimits::default() {
        return None;
    }
    let gaps = unenforced_limits(driver, requested);
    if gaps.is_empty() {
        return Some(Condition {
            kind: ConditionKind::LimitEnforced,
            status: ConditionStatus::True,
            reason: "LimitsApplied".to_string(),
            message: format!("driver '{}' applies every requested limit", driver.name),
            last_transition_at: now,
        });
    }
    let reasons: Vec<String> = gaps.iter().map(|gap| explain(*gap)).collect();
    Some(Condition {
        kind: ConditionKind::LimitEnforced,
        status: ConditionStatus::False,
        reason: "LimitNotSupported".to_string(),
        message: reasons.join("; "),
        last_transition_at: now,
    })
}

// ── Operation types ────────────────────────────────────────────────────

/// Limits for a sandbox, in the units sandbox requests use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SandboxLimits {
    pub cpu_cores: Option<f64>,
    pub memory_mb: Option<u64>,
    pub pids: Option<i64>,
    pub disk_mb: Option<u64>,
}

impl SandboxLimits {
    pub fn requested(&self) -> RequestedLimits {
        RequestedLimits {
            cpu: self.cpu_cores.is_some(),
            memory: self.memory_mb.is_some(),
            pids: self.pids.is_some(),
            disk: self.disk_mb.is_some(),
        }
    }
}

/// Everything a driver needs to create one sandbox. Transient: it holds
/// environment values, so it is never stored or logged.
#[derive(Clone, PartialEq)]
pub struct SandboxCreate {
    pub owner: WorkloadRef,
    /// The agent run it belongs to (container suffix fallback, home volume).
    pub run_id: i32,
    /// Explicit container suffix (a standalone sandbox's public id).
    pub label: Option<String>,
    /// `None`: the driver's default image.
    pub image: Option<String>,
    pub env: Vec<(String, String)>,
    pub limits: SandboxLimits,
    /// `None`: the driver's configured default.
    pub egress: Option<EgressProfile>,
    /// Which destinations the sandbox may reach (normalized).
    pub egress_policy: SandboxEgressPolicy,
    /// Host checkout seeding the work dir.
    pub host_work_dir: PathBuf,
    /// Named volume mounted at the work dir instead.
    pub workspace_volume: Option<String>,
    pub idle_timeout: Duration,
    pub allow_docker: bool,
}

impl fmt::Debug for SandboxCreate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env_keys: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("SandboxCreate")
            .field("owner", &self.owner)
            .field("run_id", &self.run_id)
            .field("label", &self.label)
            .field("image", &self.image)
            .field("env_keys", &env_keys)
            .field("limits", &self.limits)
            .field("egress", &self.egress)
            .field("egress_policy", &self.egress_policy)
            .field("host_work_dir", &self.host_work_dir)
            .field("workspace_volume", &self.workspace_volume)
            .field("idle_timeout", &self.idle_timeout)
            .field("allow_docker", &self.allow_docker)
            .finish()
    }
}

impl SandboxCreate {
    /// The hard requirements this request puts on a driver.
    pub fn requirements(&self) -> SandboxRequirements {
        let mut requirements = SandboxRequirements::baseline();
        requirements.limits = self.limits.requested();
        if let Some(egress) = self.egress {
            requirements = requirements.require(SandboxFeature::Egress(egress));
        }
        if self.egress_policy.is_restricted() {
            requirements = requirements.require(SandboxFeature::EgressRules);
        }
        if self.workspace_volume.is_some() {
            requirements = requirements.require(SandboxFeature::WorkspaceVolume);
        }
        if self.allow_docker {
            requirements = requirements.require(SandboxFeature::GuestDocker);
        }
        requirements
    }
}

/// A sandbox a driver created or recovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxInstance {
    pub handle: InstanceHandle,
    /// The image the driver resolved (its default when none was asked for);
    /// empty on a recovered instance.
    pub image: String,
    /// The work dir inside the sandbox.
    pub work_dir: PathBuf,
}

/// Who a command runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecUser {
    /// The sandbox user.
    Default,
    Root,
    /// A `uid[:gid]` or name the driver accepts.
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxExec {
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
    pub user: ExecUser,
    /// Deliver stderr lines to the sink tagged as stderr. When `false` the
    /// sink gets the lines a plain exec reports (stdout), as the providers'
    /// `exec` always has.
    pub split_streams: bool,
}

/// One line of output, as it is produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecLine {
    pub stderr: bool,
    pub line: String,
}

/// Receives output lines while a command runs.
pub type ExecLineSink = Arc<dyn Fn(ExecLine) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The only signals a sandbox caller may send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessSignal {
    Term,
    Kill,
}

/// A bidirectional byte channel into an in-sandbox agent (the PTY agent or
/// the retained agent runtime). Framing is the caller's job.
pub struct ByteChannel {
    pub output: BoxStream<'static, Result<Bytes, DriverError>>,
    pub input: Pin<Box<dyn tokio::io::AsyncWrite + Send>>,
}

impl fmt::Debug for ByteChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ByteChannel")
    }
}

/// A file a snapshot is stored in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: PathBuf,
    pub digest: String,
    pub size_bytes: u64,
}

/// A snapshot a driver took: where its files are on the node, and how to
/// verify them on restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSnapshot {
    pub driver: String,
    /// The primary artifact (a Docker image tarball, an ext4 image).
    pub primary: SnapshotFile,
    /// Digest of the whole logical snapshot (primary plus companions).
    pub content_digest: String,
    /// Every file, companions included.
    pub size_bytes: u64,
    /// The image tag the driver committed, when it has an image store.
    pub image_ref: Option<String>,
    /// The immutable image id behind `image_ref`.
    pub image_id: Option<String>,
    /// The workspace, stored apart from the primary artifact.
    pub workspace: Option<SnapshotFile>,
}

/// Whether the retained agent runtime in a sandbox can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRuntimeCompatibility {
    Compatible,
    Unavailable { reason: String },
    Incompatible { reason: String },
}

fn unsupported(
    driver: &DriverCapabilities,
    operation: &'static str,
    instance: &str,
) -> DriverError {
    DriverError::Unsupported {
        driver: driver.name.clone(),
        operation,
        instance: instance.to_string(),
    }
}

/// The sandbox-only operations (§6). Optional operations default to
/// [`DriverError::Unsupported`]; the matching [`SandboxCapabilities`] flag
/// must be `false` for a driver that keeps the default.
#[async_trait]
pub trait SandboxExt: RuntimeDriver {
    fn sandbox_capabilities(&self) -> &SandboxCapabilities;

    /// Why the driver cannot apply a limit of this kind, for
    /// `LimitEnforced = False`.
    fn limit_gap(&self, limit: LimitKind) -> String {
        format!(
            "{}: {} limits are not supported by this driver",
            self.capabilities().name,
            limit.as_str()
        )
    }

    /// Create and start a sandbox.
    async fn create_sandbox(&self, create: SandboxCreate) -> Result<SandboxInstance, DriverError>;

    /// Find a sandbox by its full instance name, running or not.
    async fn recover_sandbox(&self, name: &str) -> Result<Option<SandboxInstance>, DriverError>;

    /// Find the sandbox of an agent run by its naming convention.
    async fn recover_run(&self, run_id: i32) -> Result<Option<SandboxInstance>, DriverError>;

    /// Release what a sandbox whose instance is already gone left behind.
    async fn release_orphaned(&self, name: &str) -> Result<(), DriverError> {
        let _ = name;
        Ok(())
    }

    async fn is_alive(&self, handle: &InstanceHandle) -> Result<bool, DriverError>;

    async fn exec_in(
        &self,
        handle: &InstanceHandle,
        exec: SandboxExec,
        sink: Option<ExecLineSink>,
    ) -> Result<ExecOutput, DriverError>;

    async fn kill_processes(
        &self,
        handle: &InstanceHandle,
        pattern: &str,
        signal: ProcessSignal,
    ) -> Result<(), DriverError>;

    /// Read a file; `max_bytes` refuses larger files.
    async fn read_file(
        &self,
        handle: &InstanceHandle,
        path: &str,
        max_bytes: Option<u64>,
    ) -> Result<Vec<u8>, DriverError> {
        let _ = (path, max_bytes);
        Err(unsupported(self.capabilities(), "read_file", &handle.name))
    }

    async fn write_file(
        &self,
        handle: &InstanceHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), DriverError> {
        let _ = (path, contents, mode);
        Err(unsupported(self.capabilities(), "write_file", &handle.name))
    }

    async fn write_directory(
        &self,
        handle: &InstanceHandle,
        local_dir: &Path,
        target_path: &str,
    ) -> Result<(), DriverError> {
        let _ = (local_dir, target_path);
        Err(unsupported(
            self.capabilities(),
            "write_directory",
            &handle.name,
        ))
    }

    /// Copy the work dir back to the host checkout. Drivers whose work dir
    /// is the host checkout (a bind mount) have nothing to do.
    async fn sync_workspace_to_host(
        &self,
        handle: &InstanceHandle,
        host_dir: &Path,
    ) -> Result<(), DriverError> {
        let _ = (handle, host_dir);
        Ok(())
    }

    async fn attach_pty(&self, handle: &InstanceHandle) -> Result<ByteChannel, DriverError> {
        Err(unsupported(self.capabilities(), "attach_pty", &handle.name))
    }

    async fn connect_agent_runtime(
        &self,
        handle: &InstanceHandle,
    ) -> Result<ByteChannel, DriverError> {
        Err(unsupported(
            self.capabilities(),
            "connect_agent_runtime",
            &handle.name,
        ))
    }

    /// A byte channel to TCP `port` on the instance's loopback: what a
    /// preview URL of the sandbox reaches when the ingress cannot dial the
    /// instance directly (it runs on another node).
    async fn connect_port(
        &self,
        handle: &InstanceHandle,
        port: u16,
    ) -> Result<ByteChannel, DriverError> {
        let _ = port;
        Err(unsupported(
            self.capabilities(),
            "connect_port",
            &handle.name,
        ))
    }

    async fn check_agent_runtime(
        &self,
        handle: &InstanceHandle,
    ) -> Result<AgentRuntimeCompatibility, DriverError> {
        Err(unsupported(
            self.capabilities(),
            "check_agent_runtime",
            &handle.name,
        ))
    }

    async fn recover_agent_harness(
        &self,
        handle: &InstanceHandle,
        epoch: u64,
    ) -> Result<(), DriverError> {
        let _ = epoch;
        Err(unsupported(
            self.capabilities(),
            "recover_agent_harness",
            &handle.name,
        ))
    }

    async fn image_identity(&self, handle: &InstanceHandle) -> Result<String, DriverError> {
        Err(unsupported(
            self.capabilities(),
            "image_identity",
            &handle.name,
        ))
    }

    async fn model_relay_base_url(
        &self,
        handle: &InstanceHandle,
        control_plane_url: &str,
    ) -> Result<String, DriverError> {
        let _ = control_plane_url;
        Err(unsupported(
            self.capabilities(),
            "model_relay_base_url",
            &handle.name,
        ))
    }

    async fn git_relay_base_url(
        &self,
        handle: &InstanceHandle,
        control_plane_url: &str,
    ) -> Result<String, DriverError> {
        let _ = control_plane_url;
        Err(unsupported(
            self.capabilities(),
            "git_relay_base_url",
            &handle.name,
        ))
    }

    async fn harness_mcp_url(
        &self,
        handle: &InstanceHandle,
        control_plane_url: &str,
        registered_url: &str,
    ) -> Result<String, DriverError> {
        let _ = (control_plane_url, registered_url);
        Err(unsupported(
            self.capabilities(),
            "harness_mcp_url",
            &handle.name,
        ))
    }

    async fn configure_application_network(
        &self,
        handle: &InstanceHandle,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), DriverError> {
        let _ = (network_name, service_containers);
        Err(unsupported(
            self.capabilities(),
            "configure_application_network",
            &handle.name,
        ))
    }

    async fn take_snapshot(
        &self,
        handle: &InstanceHandle,
        label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<SandboxSnapshot, DriverError> {
        let _ = (label, max_size_bytes);
        Err(unsupported(
            self.capabilities(),
            "take_snapshot",
            &handle.name,
        ))
    }

    async fn restore_snapshot(
        &self,
        snapshot: &SandboxSnapshot,
        create: SandboxCreate,
    ) -> Result<SandboxInstance, DriverError> {
        let _ = snapshot;
        let name = create.label.unwrap_or_else(|| create.run_id.to_string());
        Err(unsupported(self.capabilities(), "restore_snapshot", &name))
    }

    /// Remove a snapshot's image from the driver's image store. Drivers
    /// without one have nothing to remove.
    async fn delete_snapshot_image(&self, image_ref: &str) -> Result<(), DriverError> {
        let _ = image_ref;
        Ok(())
    }

    async fn resize_disk(
        &self,
        handle: &InstanceHandle,
        new_size_mb: u64,
    ) -> Result<(), DriverError> {
        let _ = new_size_mb;
        Err(unsupported(
            self.capabilities(),
            "resize_disk",
            &handle.name,
        ))
    }

    /// Replace the egress policy of an existing sandbox. `policy` is
    /// normalized. A driver without `egress_rules` already enforces the
    /// default (`internet`) policy, so only a `restricted` one is refused.
    async fn set_egress_policy(
        &self,
        handle: &InstanceHandle,
        policy: &SandboxEgressPolicy,
    ) -> Result<(), DriverError> {
        if policy.is_restricted() {
            return Err(unsupported(
                self.capabilities(),
                "set_egress_policy",
                &handle.name,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactKinds, AttachmentKind, LimitSupport, VolumeKinds};

    fn driver(name: &str, isolation: Isolation, limits: LimitSupport) -> DriverCapabilities {
        DriverCapabilities {
            name: name.to_string(),
            artifacts: ArtifactKinds {
                oci: true,
                ..ArtifactKinds::default()
            },
            isolation,
            network_attachment: AttachmentKind::DockerNetwork,
            volumes: VolumeKinds::default(),
            exec: true,
            snapshot: true,
            resize_disk: false,
            limits,
        }
    }

    fn all_limits() -> LimitSupport {
        LimitSupport {
            cpu: true,
            memory: true,
            pids: true,
            disk: true,
        }
    }

    fn sandbox_supports_rules(caps: &SandboxCapabilities) -> bool {
        let driver = driver("docker", Isolation::Process, all_limits());
        supports(&driver, caps, SandboxFeature::EgressRules)
    }

    fn sandbox(restricted: bool, workspace_volume: bool) -> SandboxCapabilities {
        SandboxCapabilities {
            file_access: true,
            snapshot: true,
            restore: true,
            workspace_volume,
            egress: EgressSupport {
                open: true,
                restricted,
                none: true,
            },
            ..SandboxCapabilities::default()
        }
    }

    #[test]
    fn a_requested_driver_lacking_a_requirement_is_refused_with_what_it_lacks() {
        let fc = driver("firecracker", Isolation::MicroVm, all_limits());
        let fc_sandbox = sandbox(false, false);
        let candidates = [DriverCandidate {
            driver: &fc,
            sandbox: &fc_sandbox,
        }];
        let requirements = SandboxRequirements::baseline()
            .require(SandboxFeature::Egress(EgressProfile::Restricted));
        let error = choose_driver(
            "temps-sandbox-abc",
            &candidates,
            Some("firecracker"),
            None,
            &requirements,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "sandbox 'temps-sandbox-abc': 0/1 drivers eligible: firecracker lacks restricted egress"
        );
    }

    #[test]
    fn the_default_driver_is_passed_over_for_one_that_has_the_capability() {
        let fc = driver("firecracker", Isolation::MicroVm, all_limits());
        let docker = driver("docker", Isolation::Process, all_limits());
        let fc_sandbox = sandbox(false, false);
        let docker_sandbox = sandbox(true, true);
        let candidates = [
            DriverCandidate {
                driver: &docker,
                sandbox: &docker_sandbox,
            },
            DriverCandidate {
                driver: &fc,
                sandbox: &fc_sandbox,
            },
        ];
        let requirements = SandboxRequirements::baseline()
            .require(SandboxFeature::Egress(EgressProfile::Restricted));
        let choice = choose_driver("s", &candidates, None, Some("firecracker"), &requirements)
            .expect("docker can take it");
        assert_eq!(choice.index, 0);
        assert_eq!(
            choice.passed_over,
            vec![DriverRejection {
                driver: "firecracker".into(),
                missing: vec![SandboxFeature::Egress(EgressProfile::Restricted)],
            }]
        );
        // With nothing special asked, the preferred driver wins.
        let choice = choose_driver(
            "s",
            &candidates,
            None,
            Some("firecracker"),
            &SandboxRequirements::baseline(),
        )
        .unwrap();
        assert_eq!(choice.index, 1);
        assert!(choice.passed_over.is_empty());
    }

    #[test]
    fn nothing_eligible_names_every_driver() {
        let docker = driver("docker", Isolation::Process, all_limits());
        let docker_sandbox = sandbox(true, true);
        let candidates = [DriverCandidate {
            driver: &docker,
            sandbox: &docker_sandbox,
        }];
        let requirements = SandboxRequirements::baseline().require(SandboxFeature::GuestDocker);
        let error = choose_driver("s", &candidates, None, None, &requirements).unwrap_err();
        assert_eq!(
            error.to_string(),
            "sandbox 's': 0/1 drivers eligible: docker lacks guest Docker"
        );
        let error = choose_driver("s", &[], None, None, &requirements).unwrap_err();
        assert!(error.to_string().contains("no sandbox driver is installed"));
        let error =
            choose_driver("s", &candidates, Some("microsandbox"), None, &requirements).unwrap_err();
        assert_eq!(
            error.to_string(),
            "sandbox 's' asked for the 'microsandbox' driver, which is not available on this node (available: docker)"
        );
    }

    #[test]
    fn a_process_driver_without_pids_caps_never_hosts_a_sandbox() {
        let weak = driver(
            "weak",
            Isolation::Process,
            LimitSupport {
                cpu: true,
                memory: true,
                pids: false,
                disk: false,
            },
        );
        let vm = driver(
            "vm",
            Isolation::MicroVm,
            LimitSupport {
                cpu: true,
                memory: true,
                pids: false,
                disk: true,
            },
        );
        let caps = sandbox(true, false);
        assert!(!supports(&weak, &caps, SandboxFeature::SafetyCaps));
        // A microVM bounds memory and processes by its own size and kernel.
        assert!(supports(&vm, &caps, SandboxFeature::SafetyCaps));
    }

    #[test]
    fn an_unapplied_limit_is_a_false_condition_with_the_drivers_reason() {
        let docker = driver(
            "docker",
            Isolation::Process,
            LimitSupport {
                cpu: true,
                memory: true,
                pids: true,
                disk: false,
            },
        );
        let now = Utc::now();
        let requested = SandboxLimits {
            cpu_cores: Some(1.0),
            memory_mb: Some(512),
            pids: None,
            disk_mb: Some(2048),
        }
        .requested();
        let condition = limit_condition(
            &docker,
            requested,
            |limit| format!("docker: {} limits need a project quota", limit.as_str()),
            now,
        )
        .expect("limits were requested");
        assert_eq!(condition.kind, ConditionKind::LimitEnforced);
        assert_eq!(condition.status, ConditionStatus::False);
        assert_eq!(condition.reason, "LimitNotSupported");
        assert_eq!(
            condition.message,
            "docker: disk limits need a project quota"
        );

        let applied = limit_condition(
            &docker,
            RequestedLimits {
                cpu: true,
                ..RequestedLimits::default()
            },
            |_| String::new(),
            now,
        )
        .unwrap();
        assert_eq!(applied.status, ConditionStatus::True);
        assert!(
            limit_condition(&docker, RequestedLimits::default(), |_| String::new(), now).is_none()
        );
    }

    #[test]
    fn create_requests_turn_into_hard_requirements() {
        let create = SandboxCreate {
            owner: WorkloadRef::new(crate::WorkloadKind::Sandbox, 7),
            run_id: 7,
            label: Some("abc".into()),
            image: None,
            env: vec![("TOKEN".into(), "seeded-secret".into())],
            limits: SandboxLimits {
                disk_mb: Some(10),
                ..SandboxLimits::default()
            },
            egress: Some(EgressProfile::Restricted),
            egress_policy: SandboxEgressPolicy::restricted(Vec::new()),
            host_work_dir: PathBuf::from("/tmp/x"),
            workspace_volume: Some("vol".into()),
            idle_timeout: Duration::from_secs(1),
            allow_docker: true,
        };
        let requirements = create.requirements();
        assert_eq!(
            requirements.features,
            vec![
                SandboxFeature::Exec,
                SandboxFeature::SafetyCaps,
                SandboxFeature::Egress(EgressProfile::Restricted),
                SandboxFeature::EgressRules,
                SandboxFeature::WorkspaceVolume,
                SandboxFeature::GuestDocker,
            ]
        );
        // The default (internet) policy asks nothing extra of a driver.
        let open = SandboxCreate {
            egress_policy: SandboxEgressPolicy::default(),
            ..create.clone()
        };
        assert!(!open
            .requirements()
            .features
            .contains(&SandboxFeature::EgressRules));
        let caps = SandboxCapabilities::default();
        assert!(!sandbox_supports_rules(&caps));
        assert!(sandbox_supports_rules(&SandboxCapabilities {
            egress_rules: true,
            ..caps
        }));
        assert!(requirements.limits.disk);
        // Environment values never reach logs.
        let debug = format!("{create:?}");
        assert!(debug.contains("TOKEN"));
        assert!(!debug.contains("seeded-secret"), "{debug}");
    }
}
