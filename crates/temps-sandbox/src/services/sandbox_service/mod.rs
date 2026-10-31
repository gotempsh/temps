// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Core service for the standalone sandbox API.
//!
//! Responsibilities:
//! - Lifecycle (create/get/list/stop/extend_timeout) against the
//!   `sandboxes` DB table + the in-memory `StandaloneSandboxRegistry`.
//! - Ownership check (every operation validates `user_id` matches).
//! - Translating between the public opaque ID and the internal `i32`
//!   used by the underlying `SandboxProvider`.
//!
//! Exec/fs/domain methods live in sibling modules but are re-exported
//! here so handlers have a single service to call into.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait,
    DatabaseBackend, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Statement, TransactionTrait,
};

use temps_ai_agents::error::AgentError;
use temps_ai_agents::sandbox::SandboxCreateConfig;
use temps_ai_agents::services::run_service::TERMINAL_RUN_STATUSES;
use temps_config::ConfigService;
use temps_entities::ingress_mode::SandboxPreviewAccess;
use temps_entities::{
    agent_runs, ai_application_projects, ai_application_workspaces, ai_applications, environments,
    external_services, nodes, project_services, projects, sandbox_events, sandbox_services,
    sandboxes, service_members, users,
};
use temps_git::GitProviderManager;

use crate::error::{from_agent_error, SandboxError};
use crate::services::create_policy;
use crate::services::exec::ExecOptions;
use crate::services::job_tracker::JobTracker;
use crate::services::placement;
use crate::services::preview_access::access_after_clearing_password;
use crate::services::preview_urls::{self, PreviewUrlParts};
use crate::services::public_id;
use crate::services::registry::{StandaloneSandboxRegistry, VmProbe};
use crate::services::row_status::{self, Expect};
use crate::services::snapshot_service::SnapshotService;

/// Optional initial content to seed into the sandbox after create.
/// Mirrors `@vercel/sandbox`'s `source: { type, url, revision?, username?,
/// password?, depth? }` option, plus a temps-native `git_connection_id`
/// that resolves a stored provider token server-side.
#[derive(Debug, Clone)]
pub enum SandboxSource {
    /// Clone a git repository into the sandbox work dir.
    Git {
        url: String,
        /// Branch, tag, or commit SHA. `None` → default branch.
        revision: Option<String>,
        /// Shallow-clone depth. `None` → full history.
        depth: Option<u32>,
        /// HTTP Basic username for private repos. For GitHub tokens the
        /// conventional value is `"x-access-token"`.
        username: Option<String>,
        /// HTTP Basic password / token. Paired with `username`. The token
        /// is injected via `GIT_ASKPASS` — never in the URL or argv.
        password: Option<String>,
        /// Reference to a stored git provider connection. When set, temps
        /// resolves the token server-side via `GitProviderManager` and
        /// injects it as `username="x-access-token" + password=<token>`.
        /// Mutually exclusive with `username`/`password`.
        git_connection_id: Option<i32>,
        /// Optional path relative to the sandbox work directory. This lets a
        /// persistent sandbox host multiple projects without granting callers
        /// an arbitrary container filesystem write primitive.
        destination: Option<String>,
        /// Remove the imported repository's `.git` directory after a
        /// successful clone. AI workspaces use one outer repository and must
        /// not retain nested Git metadata.
        strip_git_metadata: bool,
    },
    /// Download a tarball (tar, tar.gz, tgz) from `url` and extract it
    /// into the sandbox work dir. The file at `url` must be reachable
    /// from inside the container (public URL, or the container network
    /// can reach it).
    Tarball { url: String },
}

/// Lifecycle class of a sandbox (ADR-036).
///
/// All sandbox compute may suspend on idle. A workspace is durable because its
/// bind-mounted files and home volume never expire and are resumed or rebuilt
/// automatically on the next access; it does not keep idle compute running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxLifecycle {
    /// Throwaway sandbox. Default for every caller that doesn't ask.
    #[default]
    Ephemeral,
    /// Long-lived files: compute may suspend, but is never auto-destroyed.
    Workspace,
}

impl SandboxLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxLifecycle::Ephemeral => "ephemeral",
            SandboxLifecycle::Workspace => "workspace",
        }
    }

    /// Parse the API-facing string. Unknown values are a validation
    /// error rather than a silent fallback: a caller who typo'd
    /// `"workspaces"` and got an ephemeral sandbox would lose their work
    /// to the first idle sweep without ever being told.
    pub fn parse(value: &str) -> Result<Self, SandboxError> {
        match value {
            "ephemeral" => Ok(SandboxLifecycle::Ephemeral),
            "workspace" => Ok(SandboxLifecycle::Workspace),
            other => Err(SandboxError::Validation {
                message: format!(
                    "unknown lifecycle '{}' (expected \"ephemeral\" or \"workspace\")",
                    other
                ),
            }),
        }
    }

    /// Read the class off a persisted row. Unlike [`Self::parse`] this
    /// cannot fail: a row carrying a value we don't recognise (written
    /// by a newer binary, then rolled back) is treated as ephemeral,
    /// which is the conservative reading — it never auto-starts a
    /// container the caller didn't ask for.
    pub fn from_row(row: &sandboxes::Model) -> Self {
        match row.lifecycle.as_str() {
            "workspace" => SandboxLifecycle::Workspace,
            _ => SandboxLifecycle::Ephemeral,
        }
    }
}

impl std::fmt::Display for SandboxLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Input to `create_sandbox`. A subset of the `@vercel/sandbox` create
/// options — we accept what SDK clients send and ignore the rest.
#[derive(Debug, Clone, Default)]
pub struct CreateSandboxRequest {
    /// Opt in to normal guest-root semantics for Docker (microsandbox only).
    pub allow_docker: bool,
    /// Optional Docker image override. `None` → platform default.
    pub image: Option<String>,
    /// Optional human-readable name. Defaults to the internal ID.
    pub name: Option<String>,
    /// Idle timeout in seconds. Clamped to `[60, 86400]`.
    pub timeout_secs: Option<u64>,
    /// Environment variables to bake into the container at startup.
    pub env: HashMap<String, String>,
    /// Resource limits (null → provider defaults).
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<u64>,
    pub pids_limit: Option<i64>,
    /// Root disk size in MB. Only honored by the Firecracker backend
    /// (Docker ignores it). `None` uses the provider default (1 GiB).
    pub disk_size_mb: Option<u64>,
    /// Optional initial content to seed into the work dir.
    pub source: Option<SandboxSource>,
    /// Optional preview-URL password applied atomically at create. Same
    /// validation rules as `set_preview_password` (8–256 chars, argon2-
    /// hashed server-side). `None` leaves preview URLs open (public once
    /// the sandbox ID is known). The plaintext is never returned — only
    /// the last-4 hint round-trips in `SandboxSummary`.
    pub preview_password: Option<String>,
    /// Ports the sandbox is expected to listen on. Persisted to the
    /// `metadata` JSON column and surfaced as `routes[]` in the SDK-
    /// shaped responses.
    pub ports: Vec<u16>,
    /// Isolation backend: "docker" (default), "firecracker" or
    /// "microsandbox" (ADR-050). `None` →
    /// the host's configured default (docker unless the operator changed
    /// it). Unknown values are a validation error, and requesting a
    /// backend the host doesn't have fails create rather than downgrading.
    pub backend: Option<String>,
    /// Lifecycle class (ADR-036). `None` → ephemeral, so existing
    /// callers and SDK clients are unaffected.
    pub lifecycle: Option<String>,
    /// Project to derive the repo from when `source` is absent, and to
    /// attribute the sandbox to. `None` → unattached sandbox.
    pub project_id: Option<i32>,
    /// Pre-resolved snapshot artifact (ADR-037). When `Some`, the sandbox
    /// is created via `provider.create_from_snapshot` instead of
    /// `provider.create`. Mutually exclusive with `image`; validated in the
    /// handler before this field is populated.
    pub from_snapshot_artifact: Option<temps_ai_agents::sandbox::SnapshotArtifact>,
    /// Trusted host work directory for an internal caller. HTTP handlers
    /// always leave this unset; it exists so the AI application workspace can
    /// remain in its user-owned `<TEMPS_DATA_DIR>/ai-applications` tree while
    /// still receiving a first-class sandbox row and preview identity.
    pub host_work_dir_override: Option<PathBuf>,
    /// Requested node (ADR-048): name, id, or `control-plane`. `None` =
    /// automatic placement. Resolved against the operator allow-list by
    /// [`crate::services::placement`].
    pub node: Option<String>,
    /// Which outside destinations the sandbox may reach. The default,
    /// `internet`, is any public destination on ports 80 and 443; a
    /// `restricted` policy needs a backend that can enforce it (docker), and
    /// create fails with a 422 rather than starting the sandbox without it.
    pub egress_policy: temps_ai_agents::sandbox::SandboxEgressPolicy,
    /// Check the request against the operator's sandbox limits
    /// (`agent_sandbox.max_*` settings, the host's memory and the per-user
    /// cap; see [`crate::services::create_policy`]). Set by the HTTP create
    /// handler. Internal callers (application workspaces, agent runs) leave
    /// it `false`: they choose their own limits and are not the abuse case.
    pub enforce_operator_limits: bool,
}

/// First-class sandbox identity for a durable AI application workspace.
/// `public_id` is safe to use with the preview gateway; the host path never
/// crosses an HTTP boundary.
#[derive(Debug, Clone)]
pub struct ApplicationWorkspaceSandbox {
    pub public_id: String,
    /// Authoritative provider handle adopted through SandboxService. Internal
    /// harness callers use this instead of recovering the same live sandbox a
    /// second time and mistaking a cold local cache for an orphan.
    pub handle: Option<temps_ai_agents::sandbox::SandboxHandle>,
}

#[derive(Debug, Clone)]
pub struct ApplicationWorkspaceConfig {
    pub desired_state: String,
    pub image: Option<String>,
    pub cpu_limit: f64,
    pub memory_limit_mb: u64,
    pub pids_limit: i64,
    pub disk_limit_mb: u64,
    pub idle_timeout_secs: u64,
}

/// Resolve application runtime choices to the pinned, managed daemon images.
///
/// This mapping is intentionally separate from the legacy agent-sandbox image
/// resolver. Application workspaces run the daemon protocol and must never
/// silently fall back to a general-purpose agent image when their desired
/// image is NULL.
pub fn managed_application_workspace_image(runtime: &str) -> Option<&'static str> {
    use temps_core::release_images;
    match runtime {
        "node" | "bun" => Some(pinned_or_fallback(
            release_images::DAEMON_NODEJS,
            "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta",
        )),
        "python" => Some(pinned_or_fallback(
            release_images::DAEMON_PYTHON,
            "ghcr.io/gotempsh/temps-sandbox-python:0.3.4-beta",
        )),
        "rust" | "go" | "full" => Some(pinned_or_fallback(
            release_images::DAEMON_ALL,
            "ghcr.io/gotempsh/temps-sandbox-all:0.3.4-beta",
        )),
        _ => None,
    }
}

/// The image a release manifest pinned, or the fallback for builds without
/// one. The fallback must be a tag that is actually published: stable tags
/// only exist once a release publishes them, so source builds use the `-beta`
/// tag that every main-branch build publishes (see
/// `tools/sandbox-runtime/image_metadata.py`, which reads the version from the
/// literals in `managed_application_workspace_image`).
fn pinned_or_fallback(pinned: Option<&'static str>, fallback: &'static str) -> &'static str {
    pinned.unwrap_or(fallback)
}

pub fn is_managed_application_workspace_image(image: &str) -> bool {
    ["nodejs", "python", "all"].iter().any(|flavor| {
        ["0.2.0", "0.3.0", "0.3.1", "0.3.2", "0.3.3", "0.3.4"]
            .iter()
            .any(|version| image == format!("ghcr.io/gotempsh/temps-sandbox-{flavor}:{version}"))
    }) || ["nodejs", "python", "all"].iter().any(|flavor| {
        // Builds without a release manifest fall back to the `-beta` tag that
        // main-branch builds publish; the stable tag only exists once the
        // release publishes it.
        image == format!("ghcr.io/gotempsh/temps-sandbox-{flavor}:0.3.4-beta")
    }) || image == "ghcr.io/gotempsh/temps-sandbox-node:0.1.0"
        || ["nodejs", "python", "all"].iter().any(|flavor| {
            let prefix = format!("ghcr.io/gotempsh/temps-sandbox-{flavor}@sha256:");
            image.strip_prefix(&prefix).is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        })
}

impl Default for ApplicationWorkspaceConfig {
    fn default() -> Self {
        Self {
            desired_state: "running".to_string(),
            // Persist and attest the effective default just like an explicit
            // runtime selection. Leaving this as `None` made creation store
            // the provider-resolved Node image, then made the very next lookup
            // treat that healthy row as configuration-mismatched and rebuild
            // the shared workspace container on every new chat.
            image: managed_application_workspace_image("node").map(str::to_string),
            cpu_limit: 4.0,
            memory_limit_mb: 8192,
            pids_limit: 512,
            disk_limit_mb: 10_240,
            idle_timeout_secs: 900,
        }
    }
}

impl From<&ai_application_workspaces::Model> for ApplicationWorkspaceConfig {
    fn from(value: &ai_application_workspaces::Model) -> Self {
        Self {
            desired_state: value.desired_state.clone(),
            image: value.image.clone().or_else(|| {
                managed_application_workspace_image(&value.runtime).map(str::to_string)
            }),
            cpu_limit: value.cpu_limit,
            memory_limit_mb: value.memory_limit_mb.max(0) as u64,
            pids_limit: value.pids_limit,
            disk_limit_mb: value.disk_limit_mb.max(0) as u64,
            idle_timeout_secs: value.idle_timeout_secs.max(0) as u64,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApplicationWorkspaceUsage {
    pub memory_used_bytes: Option<u64>,
    pub pids_used: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub cpu_usage_usec: Option<u64>,
    pub open_ports: Vec<u16>,
}

/// `reason` of the `stopped` event recorded when a started sandbox is
/// stopped again because its restricted egress policy could not be
/// re-applied.
pub const EGRESS_POLICY_REAPPLY_FAILED: &str = "egress_policy_reapply_failed";

const APPLICATION_WORKSPACE_NAME_PREFIX: &str = "ai-application:";

/// `metadata.source` of a sandbox created through `POST /sandboxes`.
const API_SANDBOX_SOURCE: &str = "api";

/// First key of the two-key Postgres advisory lock serializing the per-user
/// sandbox cap (the second key is the user id).
const USER_SANDBOX_CAP_LOCK_NAMESPACE: i32 = 0x7365_6263;
const SOURCE_IMPORT_TIMEOUT: Duration = Duration::from_secs(120);
const SOURCE_IMPORT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(130);
const SOURCE_IMPORT_MAX_FILES: usize = 5_000;
const SOURCE_IMPORT_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SOURCE_IMPORT_SYSTEM_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

fn managed_application_id_from_name(name: &str) -> Option<&str> {
    name.strip_prefix(APPLICATION_WORKSPACE_NAME_PREFIX)
        .filter(|application_id| !application_id.is_empty())
}

fn application_workspace_identity_is_attested(
    row: &sandboxes::Model,
    user_id: i32,
    application_public_id: &str,
    host_work_dir: &Path,
) -> bool {
    let metadata = row.metadata.as_ref().and_then(serde_json::Value::as_object);
    let attested_application = metadata
        .and_then(|value| value.get("managed_application_id"))
        .and_then(serde_json::Value::as_str);
    let attested_work_dir = metadata
        .and_then(|value| value.get("managed_host_work_dir"))
        .and_then(serde_json::Value::as_str);
    row.user_id == Some(user_id)
        && row.agent_run_id.is_none()
        && public_id::is_valid(&row.public_id)
        && row.name == format!("{APPLICATION_WORKSPACE_NAME_PREFIX}{application_public_id}")
        && attested_application == Some(application_public_id)
        && attested_work_dir == host_work_dir.to_str()
        && row.lifecycle == "workspace"
        && row.work_dir == temps_ai_agents::sandbox::SANDBOX_WORK_DIR
        && row
            .image
            .as_deref()
            .is_some_and(is_managed_application_workspace_image)
}

fn application_workspace_config_has_drift(
    row: &sandboxes::Model,
    config: &ApplicationWorkspaceConfig,
) -> bool {
    row.image != config.image
        || row.timeout_secs != config.idle_timeout_secs as i32
        || row
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("disk_size_mb"))
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|disk_size_mb| disk_size_mb != config.disk_limit_mb)
}

const APPLICATION_WORKSPACE_USAGE_COMMAND: &str = "memory=$(cat /sys/fs/cgroup/memory.current 2>/dev/null || true); pids=$(cat /sys/fs/cgroup/pids.current 2>/dev/null || true); cpu=$(sed -n 's/^usage_usec //p' /sys/fs/cgroup/cpu.stat 2>/dev/null || true); disk=$(du -sb /home/temps/workspace 2>/dev/null | awk 'NR==1 {print $1}'); ports=$(if command -v ss >/dev/null 2>&1; then ss -ltnH 2>/dev/null | awk '{print $4}' | sed 's/.*://'; else for socket_table in /proc/net/tcp /proc/net/tcp6; do test -r \"$socket_table\" || continue; awk 'NR > 1 && $4 == \"0A\" {split($2, address, \":\"); print address[2]}' \"$socket_table\"; done | while IFS= read -r hex_port; do printf '%d\\n' \"0x$hex_port\"; done; fi | sort -nu | paste -sd, -); printf 'memory=%s\\n' \"$memory\"; printf 'pids=%s\\n' \"$pids\"; printf 'cpu=%s\\n' \"$cpu\"; printf 'disk=%s\\n' \"$disk\"; printf 'ports=%s\\n' \"$ports\"";

fn parse_application_workspace_usage(stdout: &str) -> ApplicationWorkspaceUsage {
    let mut usage = ApplicationWorkspaceUsage::default();
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("memory=") {
            usage.memory_used_bytes = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("pids=") {
            usage.pids_used = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("cpu=") {
            usage.cpu_usage_usec = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("disk=") {
            usage.disk_used_bytes = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("ports=") {
            usage.open_ports = value
                .split(',')
                .filter_map(|port| port.trim().parse::<u16>().ok())
                .collect();
        }
    }
    usage
}

/// Page size bounds for [`SandboxService::node_sandboxes`].
pub const NODE_SANDBOXES_DEFAULT_PAGE_SIZE: u64 = 20;
pub const NODE_SANDBOXES_MAX_PAGE_SIZE: u64 = 100;
/// Highest page a listing accepts, so `page * page_size` can't overflow in
/// the query. Pages past the data are empty anyway.
const MAX_PAGE: u64 = 1_000_000;

/// A live sandbox on a node, with its owner (operator view).
#[derive(Debug, Clone)]
pub struct NodeSandbox {
    pub summary: SandboxSummary,
    pub owner_user_id: Option<i32>,
    pub owner_email: Option<String>,
}

/// Result of [`SandboxService::evict_node`].
#[derive(Debug, Clone)]
pub struct NodeEviction {
    pub node: placement::PlacementNode,
    /// Public ids of the sandboxes whose rows were destroyed, including
    /// those in `containers_unconfirmed`.
    pub destroyed: Vec<String>,
    /// Destroyed sandboxes whose container the node did not confirm
    /// removing (unreachable, timed out, or the destroy failed), with the
    /// reason. The container may still be running on the node.
    pub containers_unconfirmed: Vec<(String, String)>,
    /// Sandboxes that could not be destroyed, with the reason. Retrying the
    /// eviction picks them up again; destroyed ones are not revisited.
    pub failed: Vec<(String, String)>,
    /// Owners (user ids) of the destroyed sandboxes, sorted and unique, so
    /// the audit record says whose sandboxes the operator removed.
    pub owner_user_ids: Vec<i32>,
}

/// How many sandboxes an eviction destroys at once.
const EVICTION_CONCURRENCY: usize = 8;

/// How many VM liveness checks the startup reconciliation runs at once.
const VM_RECONCILE_CONCURRENCY: usize = 8;

/// How long the startup reconciliation waits for one VM's liveness check.
/// Longer than a vsock ping, shorter than anything an operator would call a
/// hung start.
const VM_RECONCILE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// How many ids the startup reconciliation names in its summary.
const VM_RECONCILE_REPORTED_IDS: usize = 50;

/// Rows loaded per page by [`SandboxService::reconcile_all_running_vms`]:
/// the most rows the reconciliation holds at once, however many sandboxes the
/// instance has.
const VM_RECONCILE_PAGE_SIZE: u64 = 100;

/// What the startup reconciliation found, for the one summary log line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VmReconcileReport {
    /// Running rows examined.
    pub checked: usize,
    /// Rows whose VM answered: left alone.
    pub alive: usize,
    /// Public ids of rows moved from `running` to `stopped` because their VM
    /// is not running, capped at [`VM_RECONCILE_REPORTED_IDS`] so the report
    /// stays small however many VMs a restart took down. Each move also has
    /// its own `vm_lost` event and log line.
    pub marked_stopped: Vec<String>,
    /// How many rows were moved to `stopped` (the full count behind
    /// `marked_stopped`).
    pub marked_stopped_total: usize,
    /// Rows whose VM is down but whose row was left as it was: a managed
    /// application workspace (which reports a lost runtime as `recovering`),
    /// a row that changed since it was read, or a failed database write.
    pub not_marked: usize,
    /// Rows left as they were because the check neither confirmed nor ruled
    /// out a live VM (error or timeout).
    pub unverified: usize,
}

impl VmReconcileReport {
    /// Fold the report of another page into this one.
    fn absorb(&mut self, page: VmReconcileReport) {
        self.checked += page.checked;
        self.alive += page.alive;
        self.marked_stopped_total += page.marked_stopped_total;
        self.not_marked += page.not_marked;
        self.unverified += page.unverified;
        let room = VM_RECONCILE_REPORTED_IDS.saturating_sub(self.marked_stopped.len());
        self.marked_stopped
            .extend(page.marked_stopped.into_iter().take(room));
    }

    /// Whether every row examined has a confirmed live VM.
    pub fn all_alive(&self) -> bool {
        self.alive == self.checked
    }
}

/// Whether the startup reconciliation may judge this row. Firecracker and
/// microsandbox VMMs are started by Temps alone, so a down one is final.
/// Docker containers carry `unless-stopped`: after a host reboot dockerd may
/// still be bringing them back when Temps starts, so a down container proves
/// nothing yet and is left to the check on first access.
pub fn reconcilable_at_startup(row: &sandboxes::Model) -> bool {
    matches!(row.backend.as_deref(), Some("firecracker" | "microsandbox"))
}

/// One row's liveness verdict in [`SandboxService::reconcile_running_vms`].
enum VmCheck {
    Alive,
    Down(temps_ai_agents::error::AgentError),
    Unknown(String),
}

/// How long an eviction waits for one container destroy. A node that
/// accepts connections but never answers (overloaded, frozen) would
/// otherwise hold every call for the provider's full lifecycle timeout.
const EVICTION_DESTROY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long removing the container of a failed worker create may take: the
/// same per-node deadline as an eviction's container destroy.
const CREATE_CLEANUP_DEADLINE: std::time::Duration = EVICTION_DESTROY_DEADLINE;

/// How long destroying one sandbox on a worker may take before the call
/// gives up and reports the node unreachable (the row is kept).
const WORKER_DESTROY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Consecutive unanswered destroys after which an eviction writes off a
/// node that did answer earlier: one full round of concurrent calls.
const EVICTION_UNANSWERED_LIMIT: usize = EVICTION_CONCURRENCY;

/// Shared by the destroys of one eviction. The node is treated as down, and
/// the remaining container calls are skipped instead of each waiting out
/// the deadline, when it fails as unreachable or times out and either has
/// never answered or has now failed [`EVICTION_UNANSWERED_LIMIT`] calls in a
/// row (it froze part-way). A single slow call from a node that is
/// answering does not write it off. Rows are marked destroyed either way.
struct EvictionGate {
    deadline: std::time::Duration,
    node_down: std::sync::atomic::AtomicBool,
    node_answered: std::sync::atomic::AtomicBool,
    unanswered_in_a_row: std::sync::atomic::AtomicUsize,
}

impl EvictionGate {
    fn new(deadline: std::time::Duration) -> Self {
        Self {
            deadline,
            node_down: std::sync::atomic::AtomicBool::new(false),
            node_answered: std::sync::atomic::AtomicBool::new(false),
            unanswered_in_a_row: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn is_down(&self) -> bool {
        self.node_down.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn answered(&self) {
        self.node_answered
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.unanswered_in_a_row
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// The node failed to answer one call.
    fn unanswered(&self) {
        let in_a_row = self
            .unanswered_in_a_row
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let never_answered = !self
            .node_answered
            .load(std::sync::atomic::Ordering::Relaxed);
        if never_answered || in_a_row >= EVICTION_UNANSWERED_LIMIT {
            self.node_down
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Page and page size for [`SandboxService::node_sandboxes`], clamped to
/// what is served (and echoed back, so a client sees what it got).
fn node_sandboxes_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    (
        page.unwrap_or(1).clamp(1, MAX_PAGE),
        page_size
            .unwrap_or(NODE_SANDBOXES_DEFAULT_PAGE_SIZE)
            .clamp(1, NODE_SANDBOXES_MAX_PAGE_SIZE),
    )
}

/// Sort an eviction's per-sandbox results into destroyed rows (confirmed
/// or not), rows whose container the node never confirmed removing (with
/// the reason), and rows that could not be destroyed (with the error).
/// Each list is sorted by sandbox id.
#[allow(clippy::type_complexity)]
fn split_eviction_outcomes(
    outcomes: Vec<(String, Result<ContainerOutcome, SandboxError>)>,
) -> (Vec<String>, Vec<(String, String)>, Vec<(String, String)>) {
    let mut destroyed = Vec::with_capacity(outcomes.len());
    let mut containers_unconfirmed = Vec::new();
    let mut failed = Vec::new();
    for (public_id, outcome) in outcomes {
        match outcome {
            Ok(ContainerOutcome::Destroyed) => destroyed.push(public_id),
            Ok(ContainerOutcome::Unconfirmed(reason)) => {
                containers_unconfirmed.push((public_id.clone(), reason));
                destroyed.push(public_id);
            }
            Err(e) => failed.push((public_id, bounded_reason(&e.to_string()))),
        }
    }
    destroyed.sort();
    containers_unconfirmed.sort();
    failed.sort();
    (destroyed, containers_unconfirmed, failed)
}

/// Owners of the `destroyed` sandboxes, sorted and unique. `owners` maps
/// each evicted sandbox's public id to its owner.
fn eviction_owners(destroyed: &[String], owners: &HashMap<String, Option<i32>>) -> Vec<i32> {
    let ids: std::collections::BTreeSet<i32> = destroyed
        .iter()
        .filter_map(|id| owners.get(id).copied().flatten())
        .collect();
    ids.into_iter().collect()
}

/// How [`SandboxService::destroy_row`] treats a node that does not answer.
enum DestroyMode<'a> {
    /// One sandbox destroyed on request. If its worker node is unreachable
    /// or does not answer within the deadline, the row is kept and the
    /// caller gets [`SandboxError::NodeUnreachable`]: marking it destroyed
    /// would leave a running container that nothing tracks any more.
    Single {
        worker_deadline: std::time::Duration,
    },
    /// Part of evicting a node. The row is marked destroyed whatever the
    /// node does, and the outcome says whether the container is gone.
    Evict(&'a EvictionGate),
}

/// Longest reason kept from a provider or worker error.
const MAX_REASON_CHARS: usize = 512;

/// Make an error reason safe to hand back to an API client or print in a
/// terminal: a worker node controls part of this text, so drop control
/// characters (terminal escapes included) and cap the length.
pub(crate) fn bounded_reason(reason: &str) -> String {
    let mut out: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_REASON_CHARS)
        .collect();
    if reason.chars().count() > MAX_REASON_CHARS {
        out.push('…');
    }
    out
}

/// What happened to a sandbox's container when its row was destroyed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ContainerOutcome {
    /// The provider destroyed it, or it was already gone.
    Destroyed,
    /// The provider could not confirm it; the container may still be
    /// running on its node. Carries the reason.
    Unconfirmed(String),
}

/// Result of [`SandboxService::node_sandboxes`].
#[derive(Debug, Clone)]
pub struct NodeSandboxes {
    pub node: placement::PlacementNode,
    /// One page, newest first.
    pub sandboxes: Vec<NodeSandbox>,
    /// Every live sandbox on the node, across all pages.
    pub total: u64,
    /// The page and page size actually served, after clamping.
    pub page: u64,
    pub page_size: u64,
}

/// The outcome of [`SandboxService::set_preview_access`]: the updated row
/// plus what it replaced, for the caller's audit record.
#[derive(Debug, Clone)]
pub struct PreviewAccessChange {
    pub previous_access: SandboxPreviewAccess,
    pub previous_allowed_cidrs: Vec<String>,
    pub sandbox: sandboxes::Model,
}

/// Output DTO — what the service returns to handlers and what handlers
/// serialize into the response JSON. Wraps the DB model to keep internal
/// columns out of the public surface.
#[derive(Debug, Clone)]
pub struct SandboxSummary {
    /// Whether normal guest-root semantics were requested for Docker.
    pub allow_docker: bool,
    pub public_id: String,
    pub name: String,
    pub status: String,
    pub image: Option<String>,
    pub work_dir: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Present iff a preview password is configured. The hint is the
    /// last 4 chars of the plaintext — safe to display in the UI so
    /// users can tell two passwords apart.
    pub preview_password_hint: Option<String>,
    /// Who may open the preview URLs: `temps_login` | `password` | `public`.
    pub preview_access: SandboxPreviewAccess,
    /// Client networks allowed to open the preview URLs (normalised CIDRs).
    /// Empty = any address. Checked before `preview_access`.
    pub preview_allowed_cidrs: Vec<String>,
    /// Ports the sandbox advertises (read from the `metadata` JSON
    /// column). Empty when the sandbox was created without declaring
    /// any ports.
    pub ports: Vec<u16>,
    /// Isolation backend the sandbox runs on ("docker" | "firecracker" |
    /// "microsandbox").
    /// `None` on rows created before the column existed.
    pub backend: Option<String>,
    /// Configured root disk size in MB (from metadata). `None` = default.
    pub disk_size_mb: Option<u64>,
    /// Set when this sandbox executes an agent run (autofixer / workflow
    /// agent). `None` for sandboxes created via the standalone API.
    pub agent_run_id: Option<i32>,
    /// Lifecycle class: `"ephemeral"` or `"workspace"` (ADR-036).
    pub lifecycle: String,
    /// Project this sandbox belongs to, when created from one.
    pub project_id: Option<i32>,
    /// Repo the work dir was seeded from, for display.
    pub source_repo_url: Option<String>,
    /// Worker node hosting the sandbox (ADR-048). `None` = control plane.
    pub node_id: Option<i32>,
    /// Display name of the hosting node. `From<&Model>` fills in
    /// `"control-plane"` or a `node-<id>` placeholder; service methods that
    /// return summaries replace the placeholder with the real node name.
    pub node_name: String,
    /// The egress policy the sandbox enforces.
    pub egress_policy: temps_ai_agents::sandbox::SandboxEgressPolicy,
}

impl From<&sandboxes::Model> for SandboxSummary {
    fn from(m: &sandboxes::Model) -> Self {
        Self {
            allow_docker: m
                .metadata
                .as_ref()
                .and_then(|v| v.get("allow_docker"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            public_id: m.public_id.clone(),
            name: m.name.clone(),
            status: m.status.clone(),
            image: m.image.clone(),
            work_dir: m.work_dir.clone(),
            created_at: m.created_at,
            expires_at: m.expires_at,
            preview_password_hint: m.preview_password_hint.clone(),
            preview_access: m.preview_access,
            preview_allowed_cidrs: m.preview_allowed_cidrs.clone(),
            ports: ports_from_metadata(m.metadata.as_ref()),
            backend: m.backend.clone(),
            disk_size_mb: m
                .metadata
                .as_ref()
                .and_then(|v| v.get("disk_size_mb"))
                .and_then(|v| v.as_u64()),
            agent_run_id: m.agent_run_id,
            lifecycle: m.lifecycle.clone(),
            project_id: m.project_id,
            source_repo_url: m.source_repo_url.clone(),
            node_id: m.node_id,
            node_name: match m.node_id {
                None => crate::services::placement::CONTROL_PLANE_NAME.to_string(),
                Some(id) => format!("node-{id}"),
            },
            egress_policy: m.egress_policy.0.clone(),
        }
    }
}

fn source_kind(source: &SandboxSource) -> &'static str {
    match source {
        SandboxSource::Git { .. } => "git",
        SandboxSource::Tarball { .. } => "tarball",
    }
}

/// Validate an optional source destination beneath the sandbox work dir.
///
/// This validation lives in the service layer as well as the HTTP DTO path
/// because project-derived and future internal callers can bypass handlers.
pub(crate) fn validate_source_destination(destination: Option<&str>) -> Result<(), SandboxError> {
    let Some(destination) = destination else {
        return Ok(());
    };
    if destination.is_empty() || destination.len() > 512 {
        return Err(SandboxError::Validation {
            message: "source destination must contain 1 to 512 characters".into(),
        });
    }
    let path = Path::new(destination);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SandboxError::Validation {
            message:
                "source destination must be a relative path without '.', '..', or platform prefixes"
                    .into(),
        });
    }
    Ok(())
}

fn bounded_command_error(stderr: &str, stdout: &str) -> String {
    const MAX_CHARS: usize = 1_000;
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        return "command exited without diagnostic output".to_string();
    }
    detail.chars().take(MAX_CHARS).collect()
}

/// Extract the `ports` array from a sandbox's `metadata` JSON blob.
/// Tolerates missing/malformed data — a sandbox created before ports
/// were tracked simply returns `[]`, and a value we can't parse is
/// dropped silently rather than erroring out list reads.
fn ports_from_metadata(metadata: Option<&serde_json::Value>) -> Vec<u16> {
    metadata
        .and_then(|v| v.get("ports"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.as_u64())
                .filter_map(|p| u16::try_from(p).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The idle deadline for a sandbox: the instant it becomes eligible for
/// suspension if nothing touches it in the meantime.
///
/// One helper rather than five open-coded `now + Duration::seconds(...)`
/// sites, because every path that records activity — create, touch,
/// resume, wake — must agree on it. When they disagreed, `expires_at`
/// meant "wall-clock deadline" on some paths and "idle deadline" on
/// others, which is the bug ADR-036 §3 describes.
fn idle_deadline(
    now: chrono::DateTime<chrono::Utc>,
    timeout_secs: i32,
) -> chrono::DateTime<chrono::Utc> {
    now + chrono::Duration::seconds(timeout_secs as i64)
}

/// Security validation for a seed source *after* it has been resolved.
///
/// The handler validates the caller-supplied `source` DTO, which covers the
/// request-shape rules it owns (mutually exclusive auth fields, and so on).
/// It cannot cover a source this layer derives from a project row, so the
/// two checks that actually matter for safety are repeated here on whatever
/// we finally decided to clone:
///
/// - **SSRF**, including DNS resolution. `validate_external_url` rejects
///   hosts that resolve to private, loopback, link-local or cloud-metadata
///   addresses — the project-side URL guard only checks IP *literals*, so a
///   name like `metadata.google.internal` would otherwise pass.
/// - **Embedded credentials.** A `user:password@` in the URL would be
///   persisted to `sandboxes.source_repo_url` and echoed back out of the
///   API. Rows predating the project-side guard can still carry one.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceNetworkPin {
    host: String,
    port: u16,
    address: std::net::IpAddr,
}

impl SourceNetworkPin {
    fn curl_resolve_value(&self) -> String {
        match self.address {
            std::net::IpAddr::V4(address) => {
                format!("{}:{}:{}", self.host, self.port, address)
            }
            std::net::IpAddr::V6(address) => {
                format!("{}:{}:[{}]", self.host, self.port, address)
            }
        }
    }
}

fn tarball_extract_command(
    url: &str,
    staging_dir: &str,
    network_pin: Option<&SourceNetworkPin>,
) -> String {
    let resolve = network_pin
        .map(|pin| {
            format!(
                " --resolve {}",
                shell_escape_service(&pin.curl_resolve_value())
            )
        })
        .unwrap_or_default();
    let network_environment = format!(
        "env -i PATH={} HOME=/var/empty/temps-source-import CURL_HOME=/var/empty/temps-source-import XDG_CONFIG_HOME=/var/empty/temps-source-import NO_PROXY='*'",
        shell_escape_service(SOURCE_IMPORT_SYSTEM_PATH),
    );
    format!(
        "{network_environment} curl -q --noproxy '*' --proxy '' -fsS --max-redirs 0{resolve} {url} | {network_environment} tar --no-same-owner --no-same-permissions -C {stage} -xzf -",
        url = shell_escape_service(url),
        stage = shell_escape_service(staging_dir),
    )
}

fn clean_git_environment(auth_dir: &str, askpass_path: Option<&str>) -> String {
    let mut environment = format!(
        "env -i PATH={} HOME={} GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_COUNT=0 GIT_SSL_NO_VERIFY=false GIT_TERMINAL_PROMPT=0",
        shell_escape_service(SOURCE_IMPORT_SYSTEM_PATH),
        shell_escape_service(auth_dir),
    );
    if let Some(askpass_path) = askpass_path {
        environment.push_str(&format!(
            " GIT_ASKPASS={}",
            shell_escape_service(askpass_path)
        ));
    }
    environment
}

async fn validate_resolved_source(
    source: &SandboxSource,
) -> Result<Option<SourceNetworkPin>, SandboxError> {
    let (url, kind) = match source {
        SandboxSource::Git { url, .. } => (url.as_str(), "git"),
        SandboxSource::Tarball { url } => (url.as_str(), "tarball"),
    };

    if let SandboxSource::Git {
        depth,
        destination,
        strip_git_metadata,
        ..
    } = source
    {
        if depth.is_some_and(|value| value == 0 || value > 1_000) {
            return Err(SandboxError::Validation {
                message: "git clone depth must be between 1 and 1000".into(),
            });
        }
        validate_source_destination(destination.as_deref())?;
        if *strip_git_metadata && destination.is_none() {
            return Err(SandboxError::Validation {
                message: "strip_git_metadata requires a destination so the sandbox root repository cannot be removed"
                    .into(),
            });
        }
    }

    if url_has_embedded_credentials(url) {
        return Err(SandboxError::Validation {
            message: format!(
                "{kind} source: url must not contain embedded credentials — \
                 use a stored git connection instead"
            ),
        });
    }

    // Sync pass first: scheme + IP-literal blocklist. Cheap, and it avoids a
    // DNS lookup for URLs that are already disqualified.
    let parsed = temps_core::url_validation::validate_external_url(url).map_err(|_| {
        SandboxError::Validation {
            message: format!(
                "{kind} source: scheme must be http or https, and the host must \
                 not be a private, loopback, or metadata address"
            ),
        }
    })?;

    // Then resolve: an IP literal is already covered above, but a *name*
    // pointing at 169.254.169.254 is only caught here.
    if let Some(url::Host::Domain(domain)) = parsed.host() {
        let port = parsed.port_or_known_default().unwrap_or(443);
        let addresses = temps_core::url_validation::resolve_and_validate_domain(domain, port)
            .await
            .map_err(|_| SandboxError::Validation {
                message: format!(
                    "{kind} source: host resolves to a private, loopback, or \
                     metadata address"
                ),
            })?;
        let address = addresses
            .iter()
            .find(|address| address.is_ipv4())
            .or_else(|| addresses.first())
            .map(std::net::SocketAddr::ip)
            .ok_or_else(|| SandboxError::Validation {
                message: format!("{kind} source: host did not resolve to a usable address"),
            })?;
        return Ok(Some(SourceNetworkPin {
            host: domain.to_string(),
            port,
            address,
        }));
    }
    Ok(None)
}

/// Does the URL carry any userinfo before the host?
///
/// Even a bare `user@` may be a token or sensitive account identifier. Clone
/// credentials must travel only through the dedicated ephemeral credential
/// channel, never through argv, process inspection, errors, or `.git/config`.
pub(crate) fn url_has_embedded_credentials(url: &str) -> bool {
    url::Url::parse(url)
        .map(|parsed| !parsed.username().is_empty() || parsed.password().is_some())
        .unwrap_or(false)
}

/// A credential passed to a process inside a custom image is already disclosed
/// to that image: it controls `/bin/sh`, `git`, dynamic-loader configuration,
/// and the network stack. Until credentialed clones move to a host-owned
/// staging helper, allow them only in the versioned images built by Temps.
fn sandbox_image_is_trusted_for_credentials(
    handle: &temps_ai_agents::sandbox::SandboxHandle,
) -> bool {
    handle.backend == temps_ai_agents::sandbox::SandboxBackend::Docker
        && handle.image.starts_with("ghcr.io/gotempsh/temps-sandbox-")
}

/// Docker exec inherits the image's configured environment. A custom image
/// could otherwise preload code or ask the shell to source a workspace file
/// before a root-owned import command begins. Explicitly neutralize the
/// interpreter, loader, Git, and language startup hooks relevant to the fixed
/// import toolchain.
fn sanitize_privileged_import_environment(environment: &mut HashMap<String, String>) {
    for name in [
        "BASH_ENV",
        "ENV",
        "GIT_EXEC_PATH",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "LD_AUDIT",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "NODE_OPTIONS",
        "PERL5LIB",
        "PYTHONHOME",
        "PYTHONPATH",
        "RUBYLIB",
    ] {
        environment.insert(name.to_string(), String::new());
    }
    for name in [
        "ALL_PROXY",
        "FTP_PROXY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "all_proxy",
        "ftp_proxy",
        "http_proxy",
        "https_proxy",
        "CURL_CA_BUNDLE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_PROXY_COMMAND",
        "GIT_SSL_CAINFO",
        "GIT_SSL_CAPATH",
        "SSL_CERT_DIR",
        "SSL_CERT_FILE",
    ] {
        environment.insert(name.to_string(), String::new());
    }
    environment.insert("NO_PROXY".to_string(), "*".to_string());
    environment.insert("no_proxy".to_string(), "*".to_string());
    environment.insert("GIT_CONFIG_COUNT".to_string(), "0".to_string());
    environment.insert("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string());
    environment.insert("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string());
    environment.insert("GIT_CONFIG_SYSTEM".to_string(), "/dev/null".to_string());
    environment.insert("GIT_SSL_NO_VERIFY".to_string(), "false".to_string());
    // Root-owned import helpers must not inherit image-provided user config.
    // `curl -q` remains the primary defense because a custom image can ship
    // files at any path, while these values also keep other tooling away from
    // the sandbox user's writable home directory.
    for name in ["HOME", "CURL_HOME", "XDG_CONFIG_HOME"] {
        environment.insert(
            name.to_string(),
            "/var/empty/temps-source-import".to_string(),
        );
    }
}

fn default_git_provider_base_url(provider_type: &str) -> Option<&'static str> {
    match provider_type.to_ascii_lowercase().as_str() {
        "github" => Some("https://github.com"),
        "gitlab" => Some("https://gitlab.com"),
        "bitbucket" => Some("https://bitbucket.org"),
        _ => None,
    }
}

fn validate_connection_clone_origin(
    clone_url: &str,
    provider: &temps_entities::git_providers::Model,
) -> Result<(), SandboxError> {
    let provider_url = provider
        .base_url
        .as_deref()
        .or_else(|| default_git_provider_base_url(&provider.provider_type))
        .ok_or_else(|| SandboxError::Validation {
            message: format!(
                "Git provider {} has no clone origin configured",
                provider.id
            ),
        })?;
    let clone = url::Url::parse(clone_url).map_err(|error| SandboxError::Validation {
        message: format!("git source: invalid clone URL: {error}"),
    })?;
    let allowed = url::Url::parse(provider_url).map_err(|error| SandboxError::Validation {
        message: format!(
            "Git provider {} has an invalid base URL: {error}",
            provider.id
        ),
    })?;
    let same_origin = clone.scheme() == allowed.scheme()
        && clone
            .host_str()
            .zip(allowed.host_str())
            .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        && clone.port_or_known_default() == allowed.port_or_known_default();
    if !same_origin {
        return Err(SandboxError::Validation {
            message: format!(
                "Git connection {} can only authenticate clones from {}",
                provider.id,
                allowed.origin().ascii_serialization()
            ),
        });
    }
    Ok(())
}

/// Reject standalone lifecycle ops (`pause`/`resume`/`restart`/`resize`)
/// on rows attributed to an agent run. Those containers are named
/// `temps-sandbox-<run_id>` — not by the row's public id — so the
/// standalone registry's name-based recovery would miss them and the DB
/// row would drift from the real container state. The run owns the
/// lifecycle; the user must act on the run instead.
fn ensure_not_agent_run(row: &sandboxes::Model) -> Result<(), SandboxError> {
    if let Some(run_id) = row.agent_run_id {
        return Err(SandboxError::ManagedByAgentRun {
            sandbox_id: row.public_id.clone(),
            run_id,
        });
    }
    Ok(())
}

/// Is the owning agent run finished (or gone)? `None` means the run row
/// no longer exists — nothing owns the sandbox anymore, so treat it as
/// terminal and let cleanup proceed. Shared by `destroy_sandbox`'s
/// agent-run branch and `release_orphaned_agent_run_sandboxes`.
fn run_status_is_terminal(status: Option<&str>) -> bool {
    status
        .map(|s| TERMINAL_RUN_STATUSES.contains(&s))
        .unwrap_or(true)
}

/// The container-name suffix for a standalone sandbox: the `public_id` with
/// its `sbx_` prefix stripped.
///
/// The same label the preview hostname embeds, so the gateway can
/// DNS-resolve `temps-sandbox-<label>` straight from the URL — and, because
/// the Docker provider derives the home volume name from the container
/// name, the thing that decides which volume this sandbox owns. Extracted
/// so `sandbox_service_and_provider_agree_on_volume_naming` can pin the
/// cross-crate half of that agreement; the original leak was exactly two
/// places deriving one identity independently.
fn container_label_for(public_id_value: &str) -> &str {
    public_id_value
        .strip_prefix(public_id::PUBLIC_ID_PREFIX)
        .unwrap_or(public_id_value)
}

/// Shell command an operator runs on a node to remove a sandbox's leftover
/// containers (the sandbox and its egress proxy both carry its container
/// name), for when an eviction could not confirm removing them. A no-op if
/// they are already gone (the node may have removed them and lost the
/// reply). Worker nodes run Linux, so `xargs -r` is available.
pub fn node_cleanup_command(public_id_value: &str) -> String {
    format!(
        "docker ps -aq --filter name={}{} | xargs -r docker rm -f",
        temps_ai_agents::sandbox::docker::SANDBOX_NAME_PREFIX,
        container_label_for(public_id_value)
    )
}

/// Which host directory (if any) `destroy_sandbox` may recursively delete
/// for a given public id.
///
/// Split out from `remove_work_dir` so the guard is unit-testable without
/// standing up a whole service. `data_root.join(s)` silently resolves
/// absolute paths and `..` segments, so a public id of `../../etc` would
/// aim a recursive delete outside the data dir entirely. Ids are generated
/// internally today, but "the caller is trusted" is not something a
/// `remove_dir_all` should rely on — accept only the exact `sbx_<16 hex>`
/// shape `public_id::generate` produces.
fn work_dir_to_remove(data_root: &Path, public_id_value: &str) -> Option<PathBuf> {
    if !public_id::is_valid(public_id_value) {
        return None;
    }
    Some(data_root.join(public_id_value))
}

/// Bounds on `timeout_secs` at the service layer. The upper bound
/// protects against "sandbox leaks" where a caller creates sandboxes
/// with absurd timeouts and relies on the server never cleaning up.
/// How long a wake-on-access container start may take before the request
/// gives up. A wake blocks a user's exec/fs call, so it needs a ceiling: a
/// wedged container runtime must surface as a clear error, not an
/// indefinitely hung request.
const WAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

const MIN_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60; // 24 hours
const DEFAULT_TIMEOUT_SECS: u64 = 60 * 60; // 1 hour

// Application workspaces are repositories, not disposable command scratch
// space. This bootstrap runs *inside* the sandbox after create/recovery, so
// both repository metadata and commits live on the same durable bind mount as
// the files the development harness edits.
const APPLICATION_GIT_BOOTSTRAP: &str = r#"set -eu
# The workspace is a bind mount whose owner can differ from the sandbox user
# (rootless/remote Docker, macOS VMs). Git then refuses the repository as
# "dubious ownership": `rev-parse` fails, `git init` only re-initializes, and
# `git config --local` dies with "--local can only be used inside a git
# repository". Trust this directory once, in the user's persistent config.
git config --global --get-all safe.directory 2>/dev/null | grep -qxF "$PWD" \
  || git config --global --add safe.directory "$PWD"
if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git init --initial-branch=main . >/dev/null
fi
git config --local --get user.name >/dev/null 2>&1 || git config --local user.name 'Temps Workspace'
git config --local --get user.email >/dev/null 2>&1 || git config --local user.email 'workspace@temps.sh'
git config --local --get commit.gpgSign >/dev/null 2>&1 || git config --local commit.gpgSign false
"#;

fn application_workspace_mutex(
    locks: &mut HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
    application_id: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    locks.retain(|_, weak| weak.strong_count() > 0);
    if let Some(existing) = locks.get(application_id).and_then(std::sync::Weak::upgrade) {
        existing
    } else {
        let created = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(application_id.to_string(), Arc::downgrade(&created));
        created
    }
}

pub struct SandboxService {
    db: Arc<DatabaseConnection>,
    registry: Arc<StandaloneSandboxRegistry>,
    jobs: Arc<JobTracker>,
    platform_config: Arc<ConfigService>,
    cookie_crypto: Arc<temps_core::CookieCrypto>,
    /// Resolves stored provider connections to access tokens so callers
    /// can clone private repos by `git_connection_id` without handing us
    /// raw credentials. Required — the git plugin registers it, and the
    /// sandbox plugin fails to start if it's absent (`require_service`).
    git_provider_manager: Arc<GitProviderManager>,
    /// Root on the host where per-sandbox working directories are
    /// allocated. Each sandbox gets `{data_dir}/{public_id}/` bind-mounted
    /// to `/workspace` inside the container.
    data_root: PathBuf,
    /// Snapshot service for nullifying `source_sandbox_id` references when
    /// a sandbox is destroyed (ADR-037). `None` when snapshots are not
    /// enabled for this deployment (e.g. Firecracker-only).
    snapshot_service: Option<Arc<SnapshotService>>,
    /// Serializes one-to-one application workspace realization. Concurrent
    /// preview, diff and chat requests must never create two containers over
    /// the same persistent bind mount.
    application_workspace_locks:
        Arc<tokio::sync::Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>>,
    /// One source import per process prevents two requests targeting the same
    /// sandbox from sharing its root-owned staging boundary.
    source_import_lock: Arc<tokio::sync::Mutex<()>>,
    /// Issues project/environment-scoped database variables without giving a
    /// sandbox a reusable Temps API token. Values are held only for the caller
    /// that launches the sandbox process and are never persisted here.
    runtime_credentials: Option<Arc<dyn temps_core::SandboxRuntimeCredentialsProvider>>,
    /// Asks a worker chosen for a new sandbox whether it can run one
    /// (ADR-048). Never called for the control plane.
    node_probe: Arc<dyn placement::NodeProbe>,
    /// Worker nodes whose sandboxes are being evicted right now. They take
    /// no new sandboxes, and a second eviction of the same node is refused.
    evicting_nodes: Arc<EvictingNodes>,
    /// Sandboxes whose create is still running in this process. Their rows
    /// already say `running`, but no container exists yet: reads report them
    /// as `starting`, and exec/filesystem calls are refused instead of
    /// trying to wake a container that was never made.
    creates_in_progress: Arc<CreatesInProgress>,
    /// This server is a control plane that runs no workloads (ADR 063):
    /// placement never picks it, and anything that needs its own host is
    /// refused with a typed error. `false` on every install that runs
    /// workloads.
    control_plane_runs_no_workloads: bool,
}

/// Worker nodes with an eviction in progress in this process (ADR-048).
#[derive(Default)]
struct EvictingNodes(std::sync::Mutex<std::collections::HashSet<i32>>);

impl EvictingNodes {
    fn set(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<i32>> {
        // The set holds plain ids; a panic elsewhere cannot leave it
        // half-updated, so a poisoned lock is still usable.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Mark `node_id` as being evicted. `None` when an eviction of it is
    /// already running. The mark is cleared when the guard drops, however
    /// the eviction ends.
    fn begin(self: &Arc<Self>, node_id: i32) -> Option<EvictionInProgress> {
        self.set().insert(node_id).then(|| EvictionInProgress {
            nodes: self.clone(),
            node_id,
        })
    }

    fn snapshot(&self) -> std::collections::HashSet<i32> {
        self.set().clone()
    }

    fn contains(&self, node_id: i32) -> bool {
        self.set().contains(&node_id)
    }
}

/// Public ids of the sandboxes this process is creating right now. Bounded by
/// the number of concurrent creates.
#[derive(Default)]
struct CreatesInProgress(std::sync::Mutex<std::collections::HashSet<String>>);

impl CreatesInProgress {
    fn set(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<String>> {
        // Plain ids only; a poisoned lock is still consistent.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Mark `public_id` as being created until the guard drops, however the
    /// create ends.
    fn begin(self: &Arc<Self>, public_id: &str) -> CreateInProgress {
        self.set().insert(public_id.to_string());
        CreateInProgress {
            creates: self.clone(),
            public_id: public_id.to_string(),
        }
    }

    fn contains(&self, public_id: &str) -> bool {
        self.set().contains(public_id)
    }
}

/// Holds a sandbox's create mark; see [`CreatesInProgress::begin`].
struct CreateInProgress {
    creates: Arc<CreatesInProgress>,
    public_id: String,
}

impl Drop for CreateInProgress {
    fn drop(&mut self) {
        self.creates.set().remove(&self.public_id);
    }
}

/// Holds a node's eviction mark; see [`EvictingNodes::begin`].
struct EvictionInProgress {
    nodes: Arc<EvictingNodes>,
    node_id: i32,
}

impl Drop for EvictionInProgress {
    fn drop(&mut self) {
        self.nodes.set().remove(&self.node_id);
    }
}

impl SandboxService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DatabaseConnection>,
        registry: Arc<StandaloneSandboxRegistry>,
        jobs: Arc<JobTracker>,
        platform_config: Arc<ConfigService>,
        cookie_crypto: Arc<temps_core::CookieCrypto>,
        git_provider_manager: Arc<GitProviderManager>,
        data_root: PathBuf,
        node_probe: Arc<dyn placement::NodeProbe>,
    ) -> Self {
        Self {
            db,
            registry,
            jobs,
            platform_config,
            cookie_crypto,
            git_provider_manager,
            data_root,
            snapshot_service: None,
            application_workspace_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            source_import_lock: Arc::new(tokio::sync::Mutex::new(())),
            runtime_credentials: None,
            node_probe,
            evicting_nodes: Arc::new(EvictingNodes::default()),
            creates_in_progress: Arc::new(CreatesInProgress::default()),
            control_plane_runs_no_workloads: false,
        }
    }

    /// Whether this server runs workloads itself (ADR 063). With `false`,
    /// sandboxes are placed on worker nodes only.
    pub fn with_local_workloads(mut self, enabled: bool) -> Self {
        self.control_plane_runs_no_workloads = !enabled;
        self
    }

    /// Whether this server runs sandboxes on its own host.
    pub fn runs_local_sandboxes(&self) -> bool {
        !self.control_plane_runs_no_workloads
    }

    pub fn with_runtime_credentials(
        mut self,
        provider: Arc<dyn temps_core::SandboxRuntimeCredentialsProvider>,
    ) -> Self {
        self.runtime_credentials = Some(provider);
        self
    }

    async fn lock_application_workspace(
        &self,
        application_id: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.application_workspace_locks.lock().await;
            application_workspace_mutex(&mut locks, application_id)
        };
        lock.lock_owned().await
    }

    /// Set the snapshot service after construction (two-phase init).
    /// Called by the plugin once both services are registered (ADR-037).
    pub fn with_snapshot_service(mut self, svc: Arc<SnapshotService>) -> Self {
        self.snapshot_service = Some(svc);
        self
    }

    pub fn registry(&self) -> &StandaloneSandboxRegistry {
        self.registry.as_ref()
    }

    pub fn jobs(&self) -> &JobTracker {
        self.jobs.as_ref()
    }

    /// Compute preview URL parts once per call. Platform settings can
    /// change while the server is live (admin edits), so we don't cache.
    pub async fn preview_parts(&self) -> PreviewUrlParts {
        preview_urls::load(&self.platform_config).await
    }

    // ── Lookups ──────────────────────────────────────────────────────────

    /// Load a sandbox row by public ID, enforcing ownership. The typical
    /// entrypoint for every op that takes an ID from the URL.
    pub async fn find_by_public_id(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        if !public_id::is_valid(public_id_value) {
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id_value))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            })?;
        if row.user_id != Some(user_id) {
            // Don't leak existence to non-owners.
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        if row.status == "destroyed" {
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        Ok(row)
    }

    /// Application workspaces are managed exclusively through the application
    /// API, where every linked project is re-authorized. The generic sandbox
    /// HTTP surface uses this discriminator to hide them completely.
    pub async fn is_application_workspace(
        &self,
        public_id_value: &str,
    ) -> Result<bool, SandboxError> {
        if !public_id::is_valid(public_id_value) {
            return Ok(false);
        }
        Ok(sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id_value))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(sandboxes::Column::Name.starts_with("ai-application:"))
            .count(self.db.as_ref())
            .await?
            > 0)
    }

    /// List all of the caller's non-destroyed sandboxes, newest first.
    /// Creation source is presentation metadata, never a capability boundary.
    ///
    /// `lifecycle` and `project_id` are optional filters, both backed by
    /// partial indexes from the ADR-036 migration — but only for the
    /// selective side. The lifecycle index is `WHERE lifecycle <> 'ephemeral'`,
    /// so `?lifecycle=workspace` avoids the ephemeral rows (the overwhelming
    /// majority) while `?lifecycle=ephemeral` — an equally valid call — gets
    /// no index and falls back to the `user_id` scan, which is the right
    /// trade: one user's sandboxes are few, and indexing the common value
    /// would cost writes on every row for no gain.
    pub async fn list_for_user(
        &self,
        user_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
        lifecycle: Option<SandboxLifecycle>,
        project_id: Option<i32>,
    ) -> Result<(Vec<SandboxSummary>, u64), SandboxError> {
        let page = page.unwrap_or(1).clamp(1, MAX_PAGE);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let mut query = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(user_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(
                sandboxes::Column::Name.not_like(format!("{APPLICATION_WORKSPACE_NAME_PREFIX}%")),
            );
        if let Some(lifecycle) = lifecycle {
            query = query.filter(sandboxes::Column::Lifecycle.eq(lifecycle.as_str()));
        }
        if let Some(project_id) = project_id {
            query = query.filter(sandboxes::Column::ProjectId.eq(project_id));
        }
        let paginator = query
            .order_by_desc(sandboxes::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;
        let items = rows.iter().map(SandboxSummary::from).collect();
        Ok((items, total))
    }

    // ── Agent-run sandboxes ──────────────────────────────────────────────

    /// Release every non-destroyed sandbox row attributed to an agent run
    /// that is already terminal (or whose run row no longer exists).
    ///
    /// Called by the plugin on startup, *after* the agents plugin's
    /// `AgentRunService::recover_stuck_runs` (register phase) has failed
    /// every run that was in flight when the server died. Without this,
    /// those runs' `sandboxes` rows stay "running" forever: the standalone
    /// recovery scan and the expiration sweeper both skip agent-run rows
    /// by design, and the run itself will never call `release` again —
    /// zombie row, possibly a leaked container.
    ///
    /// Per-run failures are logged and skipped so one bad row can't block
    /// cleanup of the rest. Returns the number of runs whose sandboxes
    /// were released.
    pub async fn release_orphaned_agent_run_sandboxes(&self) -> Result<usize, SandboxError> {
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.is_not_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        if rows.is_empty() {
            return Ok(0);
        }

        // One lookup for all runs (no per-row query), deduped: a run can
        // in principle have several stale rows, and `release_for_agent_run`
        // already sweeps every row for its run.
        let mut run_ids: Vec<i32> = rows.iter().filter_map(|r| r.agent_run_id).collect();
        run_ids.sort_unstable();
        run_ids.dedup();
        let runs = agent_runs::Entity::find()
            .filter(agent_runs::Column::Id.is_in(run_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        let status_by_run: HashMap<i32, String> =
            runs.into_iter().map(|r| (r.id, r.status)).collect();

        let mut released = 0usize;
        for run_id in run_ids {
            let terminal = run_status_is_terminal(status_by_run.get(&run_id).map(|s| s.as_str()));
            if !terminal {
                // Legitimately active run (e.g. re-triggered right after
                // restart) — its sandbox is alive and owned; leave it.
                continue;
            }
            match self.release_for_agent_run(run_id, None).await {
                Ok(()) => released += 1,
                Err(e) => {
                    tracing::warn!(
                        "orphaned agent-run sandbox cleanup: failed to release sandbox(es) \
                         for terminal agent run {}: {}",
                        run_id,
                        e
                    );
                }
            }
        }
        if released > 0 {
            tracing::info!(
                "Released sandboxes for {} terminal agent run(s) on startup",
                released
            );
        }
        Ok(released)
    }

    /// Create a sandbox for an agent run (autofixer / workflow agent) as a
    /// first-class `sandboxes` row, then create the container through the
    /// shared provider. Called by the agents' `SandboxRegistry` via the
    /// `RunSandboxService` seam (see `temps_ai_agents::sandbox::managed`).
    ///
    /// Unlike standalone sandboxes, the container keeps the historical
    /// `temps-sandbox-<run_id>` naming (no `container_name_override`) so
    /// run recovery after a server restart keeps working, and the handle is
    /// keyed by `run_id` in the agents' registry — NOT by this row's `id`.
    /// The row is bookkeeping + API visibility; the agent run owns the
    /// lifecycle (the expiration sweeper skips rows with `agent_run_id`).
    pub async fn create_for_agent_run(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<temps_ai_agents::sandbox::SandboxHandle, SandboxError> {
        let run_id = config.run_id;

        // A run can recreate its sandbox after a dead container — retire
        // any previous non-destroyed row for the same run first so the API
        // never shows two "running" sandboxes for one run.
        let stale = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in stale {
            self.mark_destroyed(row.id).await.ok();
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "reason": "superseded by new container for agent run" })),
            )
            .await;
        }

        let public_id_value = public_id::generate();
        let now = Utc::now();
        let timeout = config
            .idle_timeout
            .as_secs()
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(config.owner_user_id),
            agent_run_id: Set(Some(run_id)),
            name: Set(format!("temps-sandbox-{}", run_id)),
            status: Set("running".to_string()),
            image: Set(config.image.clone()),
            work_dir: Set(temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(Some(serde_json::json!({ "source": "agent_run" }))),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(now + chrono::Duration::seconds(timeout as i64)),
            preview_access: Set(crate::services::preview_access::initial_access(false)),
            ..Default::default()
        };
        let row = active.insert(self.db.as_ref()).await?;

        let handle = match self.registry.provider_arc().create(config).await {
            Ok(h) => h,
            Err(e) => {
                // Roll the row over to destroyed so the API doesn't show a
                // zombie "running" sandbox for a container that never started.
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id: 0,
                    reason: format!("agent run {}: {}", run_id, e),
                });
            }
        };

        if let Err(e) = self
            .record_backend(
                row.id,
                handle.backend,
                (!handle.image.is_empty()).then(|| handle.image.clone()),
            )
            .await
        {
            tracing::warn!(
                "failed to record backend/image for agent-run sandbox {} (run {}): {}",
                public_id_value,
                run_id,
                e
            );
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "agent_run_id": run_id,
                "backend": handle.backend.to_string(),
                "image": handle.image,
            })),
        )
        .await;
        self.record_driver_conditions(row.id, &handle).await;

        tracing::info!(
            "Created agent-run sandbox {} (internal {}) for run {}",
            public_id_value,
            row.id,
            run_id
        );

        Ok(handle)
    }

    /// Destroy the container backing an agent run's sandbox and mark its
    /// row(s) destroyed. `handle` is the agents' registry cached handle
    /// when available; otherwise the container is recovered by run id.
    pub async fn release_for_agent_run(
        &self,
        run_id: i32,
        handle: Option<&temps_ai_agents::sandbox::SandboxHandle>,
    ) -> Result<(), SandboxError> {
        let provider = self.registry.provider_arc();
        let resolved = match handle {
            Some(h) => Some(h.clone()),
            None => provider.recover(run_id).await.unwrap_or_else(|e| {
                tracing::warn!(
                    "release_for_agent_run: recover for run {} failed: {}",
                    run_id,
                    e
                );
                None
            }),
        };
        if let Some(h) = resolved {
            // Agent runs are ephemeral — purge the home volume too.
            if let Err(e) = provider.destroy(&h, true).await {
                tracing::warn!(
                    "Failed to destroy agent-run sandbox {} for run {}: {}",
                    h.sandbox_name,
                    run_id,
                    e
                );
            }
        }

        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in rows {
            self.mark_destroyed(row.id).await?;
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "agent_run_id": run_id })),
            )
            .await;
        }
        Ok(())
    }

    /// Derive a git seed source from a project's connected repository.
    ///
    /// The point of `project_id` on create: a user who already deploys a
    /// project should be able to say "give me a workspace on this" without
    /// re-typing the clone URL or re-picking the credential. The project
    /// row already knows both, so we read them here.
    ///
    /// Checks out the project's `main_branch` and reuses its
    /// `git_provider_connection_id`, so private repos clone without the
    /// caller ever handling a token. Full history (no shallow depth) —
    /// this is a workspace someone will branch and rebase in, not a
    /// one-shot build.
    ///
    /// Authorization is the caller's responsibility: handlers gate this
    /// with `project_scope_guard!` + `project_access_guard!` before the
    /// request reaches the service.
    async fn source_from_project(
        &self,
        project_id: i32,
    ) -> Result<Option<SandboxSource>, SandboxError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or(SandboxError::ProjectNotFound { project_id })?;

        let url = project
            .git_url
            .as_ref()
            .map(|u| u.trim())
            .filter(|u| !u.is_empty());

        let Some(url) = url else {
            // A Git project with no repository is inconsistent state and
            // must still fail closed. Git-less project types, however, are
            // intentionally valid deployment targets: their persistent
            // workspace begins empty and is populated by the agent or a file
            // upload instead of a clone.
            if project.source_type.requires_git_info() {
                return Err(SandboxError::ProjectHasNoRepo {
                    project_id,
                    name: project.name.clone(),
                });
            }
            return Ok(None);
        };

        Ok(Some(SandboxSource::Git {
            url: url.to_string(),
            revision: Some(project.main_branch.clone()),
            depth: None,
            username: None,
            password: None,
            git_connection_id: project.git_provider_connection_id,
            destination: None,
            strip_git_metadata: false,
        }))
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    // ── Placement (ADR-048) ─────────────────────────────────────────────

    async fn placement_policy(&self) -> Result<placement::PlacementPolicy, SandboxError> {
        // Read the allow-list on its own: the whole-settings getter falls
        // back to defaults when any field is malformed, which would read as
        // "every node is allowed". This fails closed instead.
        let allowed_node_ids = self
            .platform_config
            .get_sandbox_allowed_node_ids()
            .await
            .map_err(|source| SandboxError::PlacementSettings {
                operation: "load",
                source,
            })?;
        Ok(placement::PlacementPolicy {
            allowed_node_ids,
            control_plane_runs_no_workloads: self.control_plane_runs_no_workloads,
        })
    }

    /// Every worker, with those being evicted marked unplaceable.
    async fn placement_workers(&self) -> Result<Vec<placement::WorkerNode>, SandboxError> {
        let mut workers = placement::load_workers(self.db.as_ref()).await?;
        placement::mark_evicting(&mut workers, &self.evicting_nodes.snapshot());
        Ok(workers)
    }

    /// Sandboxes `user_id` created through the API that are not destroyed.
    /// Agent runs, managed application workspaces and the sandboxes the
    /// platform creates for AI chat or autofix are not marked as API-created
    /// and belong to the platform, not to the user's quota. Sandboxes created
    /// before the mark existed are not counted.
    async fn live_standalone_sandboxes<C: ConnectionTrait>(
        conn: &C,
        user_id: i32,
    ) -> Result<u64, SandboxError> {
        sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(user_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(Expr::cust_with_values(
                "\"sandboxes\".\"metadata\"->>'source' = $1",
                [API_SANDBOX_SOURCE],
            ))
            .count(conn)
            .await
            .map_err(SandboxError::from)
    }

    /// Node a new sandbox runs on: `None` = control plane.
    async fn place_sandbox(&self, req: &CreateSandboxRequest) -> Result<Option<i32>, SandboxError> {
        let requested = req.node.as_deref().map(placement::RequestedNode::parse);
        // Managed AI application workspaces live in a control-plane
        // directory; they cannot be placed elsewhere.
        if req.host_work_dir_override.is_some() {
            // ADR 063: that directory is on a host that runs nothing.
            if self.control_plane_runs_no_workloads {
                return Err(SandboxError::no_local_host("An application workspace"));
            }
            return match requested {
                None | Some(placement::RequestedNode::ControlPlane) => Ok(None),
                Some(_) => Err(SandboxError::Validation {
                    message: "application workspaces always run on the control plane".into(),
                }),
            };
        }
        let policy = self.placement_policy().await?;
        let control_plane_allowed = policy.allows(placement::CONTROL_PLANE_NODE_ID);
        // The default path — no node asked for, control plane allowed — is
        // every single-node install; it needs no node lookups at all.
        let (workers, live) = match &requested {
            None if control_plane_allowed => (Vec::new(), HashMap::new()),
            Some(placement::RequestedNode::ControlPlane) => (Vec::new(), HashMap::new()),
            None => (
                self.placement_workers().await?,
                placement::live_counts(self.db.as_ref()).await?,
            ),
            Some(_) => (self.placement_workers().await?, HashMap::new()),
        };
        let candidates = placement::candidates(&policy, &workers, &live, requested.as_ref())?;
        // A chosen worker is asked whether it can run a sandbox (its agent
        // may predate sandbox support, or its Docker may be down) before a
        // row is written for it. The control plane is never asked.
        placement::settle(candidates, self.node_probe.as_ref()).await
    }

    /// Whether `requested` names the control plane's own `nodes` row (role
    /// `control-plane`), which [`placement::classify`] leaves out of the
    /// worker list. Only asked when a lookup missed.
    async fn is_control_plane_row(
        &self,
        requested: &placement::RequestedNode,
    ) -> Result<bool, SandboxError> {
        let query = match requested {
            placement::RequestedNode::Id(id) => nodes::Entity::find_by_id(*id),
            placement::RequestedNode::Name(name) => {
                nodes::Entity::find().filter(nodes::Column::Name.eq(name.as_str()))
            }
            placement::RequestedNode::ControlPlane => return Ok(false),
        };
        Ok(query
            .one(self.db.as_ref())
            .await?
            .is_some_and(|row| placement::is_control_plane_role(&row.role)))
    }

    /// Every node with its sandbox placement state (control plane first).
    pub async fn placement_nodes(&self) -> Result<Vec<placement::PlacementNode>, SandboxError> {
        let policy = self.placement_policy().await?;
        let workers = self.placement_workers().await?;
        let live = placement::live_counts(self.db.as_ref()).await?;
        let mut nodes = placement::describe(&policy, &workers, &live);
        if let Some(control_plane) = nodes.iter_mut().find(|n| n.is_control_plane) {
            control_plane.backends = self.control_plane_backends();
        }
        Ok(nodes)
    }

    /// What the control plane's own provider can run, with the setup step
    /// for each backend it cannot. Registration is decided at startup, so
    /// this is a read of in-memory state plus, for an unavailable
    /// microsandbox, the host probe that names what is missing.
    fn control_plane_backends(&self) -> Vec<placement::NodeBackend> {
        let provider = self.registry.provider_arc();
        temps_ai_agents::sandbox::SELECTABLE_SANDBOX_BACKENDS
            .into_iter()
            .map(|backend| {
                let available = provider.supports_backend(backend);
                placement::NodeBackend {
                    backend,
                    available,
                    reason: (!available).then(|| backend_unavailable_message(backend)),
                }
            })
            .collect()
    }

    /// Live sandboxes on one node, from every owner (the operator view on
    /// the node page, ADR-048). `node` takes the same forms as
    /// `create --node`: a worker name or id, or `control-plane` / `0`.
    pub async fn node_sandboxes(
        &self,
        node: &str,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<NodeSandboxes, SandboxError> {
        let (page, page_size) = node_sandboxes_page(page, page_size);
        let mut requested = placement::RequestedNode::parse(node);
        let listing = self.placement_nodes().await?;
        // The node page addresses every node by its `nodes` row id,
        // including the control plane's own row, which placement knows only
        // as `0` / `control-plane`.
        if placement::find_node(listing.clone(), &requested).is_none()
            && self.is_control_plane_row(&requested).await?
        {
            requested = placement::RequestedNode::ControlPlane;
        }
        let found = placement::find_node(listing, &requested).ok_or_else(|| {
            SandboxError::NodeNotFound {
                node: node.to_string(),
            }
        })?;

        let mut query = sandboxes::Entity::find().filter(sandboxes::Column::Status.ne("destroyed"));
        query = if found.is_control_plane {
            query.filter(sandboxes::Column::NodeId.is_null())
        } else {
            query.filter(sandboxes::Column::NodeId.eq(found.id))
        };
        let paginator = query
            .order_by_desc(sandboxes::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;

        let owner_ids: std::collections::HashSet<i32> =
            rows.iter().filter_map(|r| r.user_id).collect();
        let owners: HashMap<i32, String> = if owner_ids.is_empty() {
            HashMap::new()
        } else {
            users::Entity::find()
                .filter(users::Column::Id.is_in(owner_ids))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|u| (u.id, u.email))
                .collect()
        };

        let sandboxes = rows
            .iter()
            .map(|row| {
                let mut summary = SandboxSummary::from(row);
                summary.node_name = found.name.clone();
                NodeSandbox {
                    summary,
                    owner_user_id: row.user_id,
                    owner_email: row.user_id.and_then(|id| owners.get(&id).cloned()),
                }
            })
            .collect();
        Ok(NodeSandboxes {
            node: found,
            sandboxes,
            total,
            page,
            page_size,
        })
    }

    /// Destroy every live sandbox on a worker node, whoever owns it — the
    /// operator's way to clear a node before removing it, including a node
    /// that is gone for good (ADR-048). Containers are destroyed best-effort:
    /// an unreachable node cannot answer, and its rows are marked destroyed
    /// regardless so the node can be removed. Every sandbox is attempted
    /// even if some fail, and the result says which ones did.
    pub async fn evict_node(&self, node: &str) -> Result<NodeEviction, SandboxError> {
        use futures::StreamExt;

        let requested = placement::RequestedNode::parse(node);
        if requested == placement::RequestedNode::ControlPlane {
            return Err(SandboxError::Validation {
                message: "the control plane's sandboxes cannot be evicted; destroy them \
                          individually with `bunx @temps-sdk/cli sandbox destroy <id>`"
                    .to_string(),
            });
        }
        let found = placement::find_node(self.placement_nodes().await?, &requested)
            .filter(|n| !n.is_control_plane)
            .ok_or_else(|| SandboxError::NodeNotFound {
                node: node.to_string(),
            })?;
        // Cordon the node for the whole eviction: placement skips it, so no
        // new sandbox lands on it behind the eviction's back, and a second
        // eviction of it is refused instead of racing this one.
        let in_progress = self.evicting_nodes.begin(found.id).ok_or_else(|| {
            SandboxError::NodeEvictionInProgress {
                node: found.name.clone(),
            }
        })?;
        tracing::info!(
            node_id = found.id,
            node = %found.name,
            "sandbox placement: evicting every sandbox on the node"
        );
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::NodeId.eq(found.id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        let owners: HashMap<String, Option<i32>> = rows
            .iter()
            .map(|r| (r.public_id.clone(), r.user_id))
            .collect();
        let gate = EvictionGate::new(EVICTION_DESTROY_DEADLINE);
        let gate = &gate;
        let outcomes: Vec<(String, Result<ContainerOutcome, SandboxError>)> =
            futures::stream::iter(rows)
                .map(|row| async move {
                    let public_id = row.public_id.clone();
                    (
                        public_id,
                        self.destroy_row(row, true, DestroyMode::Evict(gate)).await,
                    )
                })
                .buffer_unordered(EVICTION_CONCURRENCY)
                .collect()
                .await;
        let (destroyed, containers_unconfirmed, failed) = split_eviction_outcomes(outcomes);
        let owner_user_ids = eviction_owners(&destroyed, &owners);
        // The node takes sandboxes again before it is reported.
        drop(in_progress);
        // Report the node as it is now, not as it was before the destroys.
        // Best effort: sandboxes are already destroyed, so a failure here
        // must not turn the eviction into an error (the handler would then
        // skip its audit record).
        let node = match self.placement_nodes().await {
            Ok(nodes) => placement::find_node(nodes, &placement::RequestedNode::Id(found.id))
                .unwrap_or(found),
            Err(e) => {
                tracing::warn!(
                    node_id = found.id,
                    node = %found.name,
                    error = %e,
                    "sandbox placement: could not refresh the node after evicting it; \
                     reporting it as it was before the eviction"
                );
                found
            }
        };
        Ok(NodeEviction {
            node,
            destroyed,
            containers_unconfirmed,
            failed,
            owner_user_ids,
        })
    }

    /// Current allow-list (`None` = all nodes).
    /// Removed nodes are dropped from the list: their ids can never come
    /// back (node ids are not reused), and a stale id would make every later
    /// save fail validation.
    pub async fn allowed_node_ids(&self) -> Result<Option<Vec<i32>>, SandboxError> {
        let allowed = self.placement_policy().await?.allowed_node_ids;
        if allowed.is_none() {
            return Ok(None);
        }
        let workers = placement::load_workers(self.db.as_ref()).await?;
        Ok(placement::existing_node_ids(allowed, &workers))
    }

    /// Replace the allow-list. `None` = allow every node. Existing sandboxes
    /// are unaffected: the list only governs where new sandboxes go.
    pub async fn set_allowed_node_ids(
        &self,
        allowed: Option<Vec<i32>>,
    ) -> Result<Option<Vec<i32>>, SandboxError> {
        if let Some(ids) = allowed.as_ref() {
            let workers = placement::load_workers(self.db.as_ref()).await?;
            placement::validate_allowed_ids(ids, &workers)?;
        }
        let normalized = allowed.map(|mut ids| {
            ids.sort_unstable();
            ids
        });
        self.platform_config
            .set_sandbox_allowed_node_ids(normalized.clone())
            .await
            .map_err(|source| SandboxError::PlacementSettings {
                operation: "save",
                source,
            })?;
        Ok(normalized)
    }

    /// Replace the `node-<id>` placeholders from `From<&Model>` with real
    /// node names (one query for the whole batch).
    pub async fn fill_node_names(&self, summaries: &mut [SandboxSummary]) {
        self.mark_creates_in_progress(summaries);
        let ids: std::collections::HashSet<i32> =
            summaries.iter().filter_map(|s| s.node_id).collect();
        if ids.is_empty() {
            return;
        }
        match placement::node_names(self.db.as_ref(), ids).await {
            Ok(names) => {
                for s in summaries.iter_mut() {
                    if let Some(name) = s.node_id.and_then(|id| names.get(&id)) {
                        s.node_name = name.clone();
                    }
                }
            }
            Err(e) => tracing::warn!("resolve sandbox node names: {}", e),
        }
    }

    /// Create a new standalone sandbox. Inserts the DB row first (to get
    /// the internal ID the provider indexes by), then asks the provider
    /// to create the container. On provider failure the DB row is marked
    /// "destroyed" so list doesn't show zombie entries.
    /// Report sandboxes this process is still creating as `starting`: their
    /// rows say `running` from the insert on, before any container exists.
    pub fn mark_creates_in_progress(&self, summaries: &mut [SandboxSummary]) {
        for summary in summaries.iter_mut() {
            if summary.status == "running" && self.creates_in_progress.contains(&summary.public_id)
            {
                summary.status = "starting".to_string();
            }
        }
    }

    /// [`Self::create_sandbox`] in a task of its own, so it always reaches its
    /// success or cleanup path, even when the caller stops waiting.
    ///
    /// The first sandbox on a node can spend minutes building its image. An
    /// HTTP client or proxy that gives up drops the request's future, and a
    /// create cancelled there left the row "running" with no backend while
    /// the worker tore down the container it had just made, because the
    /// control plane stopped waiting. `hold` is dropped when the create ends:
    /// a lock the caller must keep for exactly that long.
    pub fn spawn_create<H: Send + 'static>(
        self: &Arc<Self>,
        user_id: i32,
        req: CreateSandboxRequest,
        hold: H,
    ) -> tokio::task::JoinHandle<Result<sandboxes::Model, SandboxError>> {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let result = service.create_sandbox(user_id, req).await;
            drop(hold);
            result
        })
    }

    pub async fn create_sandbox(
        &self,
        user_id: i32,
        mut req: CreateSandboxRequest,
    ) -> Result<sandboxes::Model, SandboxError> {
        let lifecycle = match req.lifecycle.as_deref() {
            None => SandboxLifecycle::default(),
            Some(value) => SandboxLifecycle::parse(value)?,
        };
        let egress_policy =
            req.egress_policy
                .normalized()
                .map_err(|error| SandboxError::Validation {
                    message: format!("egress_policy: {error}"),
                })?;

        if req.from_snapshot_artifact.is_some() && req.source.is_some() {
            return Err(SandboxError::Validation {
                message: "source cannot be combined with from_snapshot; the snapshot already owns the workspace contents"
                    .to_string(),
            });
        }

        // Resolve the effective seed source before anything is created.
        // An explicit `source` always wins — `project_id` only supplies a
        // default, so a caller can create a workspace attributed to a
        // project while seeding it from a fork or a different branch.
        let source = if req.from_snapshot_artifact.is_some() {
            // project_id remains valid attribution, but must not implicitly
            // clone over the workspace restored from the snapshot.
            None
        } else {
            match (req.source.clone(), req.project_id) {
                // Caller-supplied: already validated by the handler, which owns
                // the request-shape rules too. Re-checking here would duplicate a
                // DNS lookup on every create for no gain.
                (Some(explicit), _) => Some(explicit),
                // Project-derived: the handler never saw this URL, so it is
                // validated here — the layer that produced it. `projects.git_url`
                // is checked for IP literals on the project side but never
                // resolved, so a name pointing at the metadata endpoint would
                // otherwise pass; and a row predating that guard can still carry
                // `user:password@`, which would land in `source_repo_url` and be
                // echoed back out of the API.
                (None, Some(project_id)) => {
                    let derived = self.source_from_project(project_id).await?;
                    if let Some(source) = derived.as_ref() {
                        validate_resolved_source(source).await?;
                    }
                    derived
                }
                (None, None) => None,
            }
        };

        let source_repo_url = source.as_ref().and_then(|s| match s {
            SandboxSource::Git { url, .. } => Some(url.clone()),
            SandboxSource::Tarball { .. } => None,
        });

        let timeout = req
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let public_id_value = public_id::generate();
        let name = req.name.clone().unwrap_or_else(|| public_id_value.clone());
        let managed_application_id = managed_application_id_from_name(&name);
        if managed_application_id.is_some() && req.host_work_dir_override.is_none() {
            return Err(SandboxError::Validation {
                message: format!(
                    "sandbox names beginning with '{APPLICATION_WORKSPACE_NAME_PREFIX}' are reserved for managed application workspaces"
                ),
            });
        }
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(timeout as i64);

        // Validate the requested backend before any DB/container work.
        // Only the public backends are accepted — "local" is a dev
        // fallback, never a caller choice. `None` = host default (docker
        // unless the operator changed it), so existing clients see no
        // behavior change.
        let backend = parse_requested_backend(req.backend.as_deref())?;
        validate_allow_docker(req.allow_docker, backend)?;

        // ADR-048: decide which node hosts the sandbox before anything is
        // created. Explicit requests are validated against the operator
        // allow-list and never fall back to another node.
        let node_id = self.place_sandbox(&req).await?;

        // Operator limits (HTTP create path only). The settings are read once
        // and fail closed: an unreadable section must not turn into "no
        // limits". Host memory is only known for the control plane.
        let operator_limits = if req.enforce_operator_limits {
            let settings = self
                .platform_config
                .get_agent_sandbox_settings()
                .await
                .map_err(|source| SandboxError::LimitSettings { user_id, source })?;
            let host_memory_mb = if node_id.is_none() {
                create_policy::control_plane_memory_mb()
            } else {
                None
            };
            // On the control plane the provider says what it defaults to; a
            // worker applies the control plane's configured sandbox
            // defaults (Docker only, ADR-048) and Docker's PID default.
            let defaults = if node_id.is_none() {
                self.registry
                    .provider_arc()
                    .default_resource_limits(backend)
            } else {
                temps_ai_agents::sandbox::DefaultResourceLimits {
                    cpu_limit: Some(settings.cpu_limit),
                    memory_limit_mb: Some(settings.memory_limit_mb),
                    pids_limit: Some(temps_ai_agents::sandbox::docker::DEFAULT_PIDS_LIMIT),
                    cpu_whole_cores: false,
                }
            };
            // The host-memory check sees only what the caller sent; the
            // clamp then fills in omitted limits, and the maximums are
            // checked against the result.
            create_policy::check_host_memory(&req, host_memory_mb)?;
            create_policy::clamp_omitted_limits(&mut req, &settings, defaults);
            create_policy::pin_omitted_memory_to_host(&mut req, defaults, host_memory_mb);
            create_policy::check_requested_limits(&req, &settings, defaults)?;
            Some(settings)
        } else {
            None
        };

        if node_id.is_some() {
            if req.from_snapshot_artifact.is_some() {
                return Err(SandboxError::Validation {
                    message: "restoring a snapshot onto a worker node is not available yet; \
                              create the sandbox on the control plane (node: \"control-plane\")"
                        .to_string(),
                });
            }
            if let Some(b) =
                backend.filter(|b| !temps_ai_agents::sandbox::WORKER_SANDBOX_BACKENDS.contains(b))
            {
                return Err(SandboxError::Validation {
                    message: placement::worker_backend_unavailable(b),
                });
            }
        }

        // Fail closed if the caller asked for a backend this host can't
        // provide (e.g. `firecracker` on a Docker-only host). Isolation
        // level is a security property — silently downgrading to Docker
        // would be worse than a clear error.
        if let (Some(b), None) = (backend, node_id) {
            if !self.registry.provider_arc().supports_backend(b) {
                return Err(SandboxError::Validation {
                    message: backend_unavailable_message(b),
                });
            }
        }

        // Validate + hash the optional preview password *before* any
        // container/workdir work starts. A caller passing junk should fail
        // fast with a 400 rather than leaving an orphan container behind.
        let preview = match req.preview_password.as_deref() {
            Some(pw) => {
                crate::services::preview_password::validate(pw)
                    .map_err(|message| SandboxError::Validation { message })?;
                let hp =
                    crate::services::preview_password::hash_password(pw).map_err(|reason| {
                        SandboxError::PasswordHashFailed {
                            sandbox_id: public_id_value.clone(),
                            reason,
                        }
                    })?;
                Some(hp)
            }
            None => None,
        };

        let metadata_value = {
            let mut meta = serde_json::Map::new();
            meta.insert("allow_docker".into(), serde_json::json!(req.allow_docker));
            if !req.ports.is_empty() {
                meta.insert("ports".into(), serde_json::json!(req.ports));
            }
            if let Some(disk) = req.disk_size_mb {
                meta.insert("disk_size_mb".into(), serde_json::json!(disk));
            }
            // Marks the sandboxes a caller asked for through the API, the
            // only ones the per-user cap counts. Sandboxes the platform
            // creates on a user's behalf (AI chat, autofix) carry no mark.
            if req.enforce_operator_limits {
                meta.insert("source".into(), serde_json::json!(API_SANDBOX_SOURCE));
            }
            if let Some(application_id) = managed_application_id {
                meta.insert(
                    "managed_application_id".into(),
                    serde_json::json!(application_id),
                );
                if let Some(host_work_dir) = req.host_work_dir_override.as_ref() {
                    meta.insert(
                        "managed_host_work_dir".into(),
                        serde_json::json!(host_work_dir.to_string_lossy()),
                    );
                }
            }
            (!meta.is_empty()).then_some(serde_json::Value::Object(meta))
        };
        // Per-user cap: count and insert in one transaction holding a
        // per-user advisory lock, so a burst of simultaneous creates cannot
        // overshoot it, on this control plane or on another replica sharing
        // the database. The lock is released when the transaction ends.
        let cap_txn = match operator_limits
            .as_ref()
            .filter(|settings| settings.max_sandboxes_per_user.is_some())
        {
            Some(settings) => {
                let txn = self.db.begin().await?;
                txn.execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT pg_advisory_xact_lock($1, $2)",
                    [USER_SANDBOX_CAP_LOCK_NAMESPACE.into(), user_id.into()],
                ))
                .await?;
                let live = Self::live_standalone_sandboxes(&txn, user_id).await?;
                // Returning here drops the transaction, which rolls it back.
                create_policy::check_user_sandbox_cap(user_id, live, settings)?;
                Some(txn)
            }
            None => None,
        };
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(Some(user_id)),
            name: Set(name.clone()),
            status: Set("running".to_string()),
            image: Set(req.image.clone()),
            // The real directory inside the container, not a nominal
            // "/workspace" — that path does not exist in the sandbox image
            // and never has. The agent-run path above already used this
            // constant; this one was hardcoded and wrong, which stayed
            // invisible because `exec` and the filesystem ops take the work
            // dir from the provider handle rather than from this row. It
            // only surfaced once the terminal used the row's value as a
            // PTY cwd and every shell died with ENOENT.
            work_dir: Set(temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(metadata_value),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(expires_at),
            preview_password_hash: Set(preview.as_ref().map(|p| p.hash.clone())),
            preview_password_hint: Set(preview.as_ref().map(|p| p.hint.clone())),
            preview_access: Set(crate::services::preview_access::initial_access(
                preview.is_some(),
            )),
            lifecycle: Set(lifecycle.as_str().to_string()),
            project_id: Set(req.project_id),
            source_repo_url: Set(source_repo_url),
            node_id: Set(node_id),
            // Part of the create config below, so the sandbox enforces it
            // from its first instant; the row is marked destroyed if the
            // create fails.
            egress_policy: Set(sandboxes::StoredEgressPolicy(egress_policy.clone())),
            ..Default::default()
        };
        let inserted = match cap_txn {
            Some(txn) => match active.insert(&txn).await {
                Ok(row) => txn.commit().await.map(|()| row),
                Err(e) => Err(e),
            },
            None => active.insert(self.db.as_ref()).await,
        };
        let mut row = inserted.map_err(|e| {
            // The chosen node was removed between placement and insert.
            if node_id.is_some()
                && matches!(
                    e.sql_err(),
                    Some(sea_orm::SqlErr::ForeignKeyConstraintViolation(_))
                )
            {
                SandboxError::NodeNotFound {
                    node: node_id.map(|id| id.to_string()).unwrap_or_default(),
                }
            } else {
                e.into()
            }
        })?;
        let _create_in_progress = self.creates_in_progress.begin(&public_id_value);

        // Allocate host-side working directory. A worker node derives and
        // creates its own work directory, so nothing is made here for it.
        let using_trusted_work_dir = req.host_work_dir_override.is_some();
        let host_work_dir = req
            .host_work_dir_override
            .clone()
            .unwrap_or_else(|| self.data_root.join(&public_id_value));
        let work_dir_ready = if node_id.is_some() {
            Ok(())
        } else {
            tokio::fs::create_dir_all(&host_work_dir).await
        };
        if let Err(e) = work_dir_ready {
            // Roll back the DB row so a failed-to-create sandbox doesn't
            // linger as a "running" record with no container.
            self.mark_destroyed(row.id).await.ok();
            return Err(SandboxError::CreateFailed {
                user_id,
                reason: format!("create work dir: {}", e),
            });
        }

        let container_label = container_label_for(&public_id_value).to_string();

        let config = SandboxCreateConfig {
            node_id,
            owner_user_id: None,
            run_id: row.id,
            container_name_override: Some(container_label.clone()),
            host_work_dir,
            workspace_volume: None,
            image: req.image.clone(),
            cpu_limit: req.cpu_limit,
            memory_limit_mb: req.memory_limit_mb,
            pids_limit: req.pids_limit,
            disk_size_mb: req.disk_size_mb,
            allow_docker: req.allow_docker,
            egress_policy,
            network_mode: None,
            env_vars: req.env,
            idle_timeout: Duration::from_secs(timeout),
            backend,
        };

        // Choose create path: snapshot restore vs normal create.
        let create_result = if let Some(ref artifact) = req.from_snapshot_artifact {
            // ADR-037: restore from snapshot. The provider ensures the image
            // is loaded into the daemon before creating the container.
            self.registry.create_from_snapshot(artifact, config).await
        } else {
            self.registry.create(config).await
        };

        let handle = match create_result {
            Ok(h) => h,
            // The worker refused because the label already names a live
            // sandbox there. That container is not this request's: never
            // destroy it, only retire this row.
            Err(e @ temps_ai_agents::error::AgentError::SandboxConflictOnNode { .. }) => {
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                return Err(from_agent_error(&public_id_value, e));
            }
            Err(e) => {
                // Tear the container down before touching the work dir.
                // A provider `create` can fail *after* the container is
                // running — the ownership-normalisation step propagates its
                // error — and containers carry `restart_policy: unless-stopped`,
                // so skipping this leaves a live container holding the
                // caller's env vars, unreachable through the API once the
                // row is destroyed, with its /workspace deleted underneath
                // it. Same order as the seeding-failure arm below.
                self.remove_failed_create(&row, "the create failed").await;
                // The work dir was already created above; without this the
                // row goes to "destroyed" and no later `destroy_sandbox`
                // can ever reach the directory again.
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                if e.image_pull_failure().is_some() || e.network_pool_exhaustion().is_some() {
                    // The image could not be pulled, or the host has no
                    // address space left for the sandbox's network: report
                    // which, so the caller can tell a missing tag or a full
                    // host from a sandbox runtime fault.
                    return Err(from_agent_error(&public_id_value, e));
                }
                return Err(SandboxError::CreateFailed {
                    user_id,
                    reason: e.to_string(),
                });
            }
        };

        if node_id.is_some() {
            self.confirm_worker_create(&row, user_id).await?;
        }

        // Persist the *effective* backend + image the provider actually
        // used. When the request omitted them, the host default / backend
        // default decided; the handle carries the real answers. Stored so
        // the API/UI show what actually booted (e.g. "alpine:3.20") instead
        // of a vague "platform default".
        let effective_image =
            (req.image.is_none() && !handle.image.is_empty()).then(|| handle.image.clone());
        if let Err(e) = self
            .record_backend(row.id, handle.backend, effective_image.clone())
            .await
        {
            tracing::warn!(
                "failed to record backend/image for sandbox {}: {}",
                public_id_value,
                e
            );
        }
        // Mirror the recorded values into the in-memory row so the create
        // response matches what a later GET returns (record_backend wrote
        // them to the DB after the initial insert).
        row.backend = Some(handle.backend.to_string());
        if let Some(image) = effective_image {
            row.image = Some(image);
        }

        // If the caller asked us to seed the work dir, run the clone /
        // extract now. On failure we tear the sandbox down so the user
        // isn't left with a half-initialized container that's billing
        // their timeout budget.
        if let Some(source) = source {
            if let Err(e) = self
                .seed_source(row.id, &public_id_value, user_id, &source)
                .await
            {
                tracing::warn!(
                    "Seeding source into sandbox {} failed: {} — destroying",
                    public_id_value,
                    e
                );
                self.remove_failed_create(&row, "seeding its source failed")
                    .await;
                // Seeding runs after the clone/extract, so by here the work
                // dir can already hold a full repository — the largest
                // single thing this service puts on disk.
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                return Err(e);
            }
            self.record_event(
                row.id,
                "source_seeded",
                Some(serde_json::json!({ "type": source_kind(&source) })),
            )
            .await;
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "backend": row.backend,
                "image": row.image,
                "disk_size_mb": req.disk_size_mb,
                "allow_docker": req.allow_docker,
                "node_id": node_id,
            })),
        )
        .await;
        self.record_driver_conditions(row.id, &handle).await;

        tracing::info!(
            "Created standalone sandbox {} (internal {}) for user {}",
            public_id_value,
            row.id,
            user_id
        );
        Ok(row)
    }

    /// Return the durable, preview-addressable sandbox assigned to one AI
    /// application.  This is deliberately an internal service method rather
    /// than an HTTP option: callers cannot choose an arbitrary host directory
    /// or container label.
    pub async fn get_or_create_application_workspace(
        &self,
        user_id: i32,
        application_public_id: &str,
        project_id: Option<i32>,
        host_work_dir: PathBuf,
    ) -> Result<ApplicationWorkspaceSandbox, SandboxError> {
        let authorized_project_ids = project_id.into_iter().collect::<Vec<_>>();
        self.get_or_create_application_workspace_with_config(
            user_id,
            application_public_id,
            project_id,
            host_work_dir,
            ApplicationWorkspaceConfig::default(),
            &authorized_project_ids,
        )
        .await
    }

    pub async fn get_or_create_application_workspace_with_config(
        &self,
        user_id: i32,
        application_public_id: &str,
        project_id: Option<i32>,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
        authorized_project_ids: &[i32],
    ) -> Result<ApplicationWorkspaceSandbox, SandboxError> {
        let _workspace_mutation = self.lock_application_workspace(application_public_id).await;
        if application_public_id.is_empty()
            || application_public_id.len() > 200
            || !application_public_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(SandboxError::Validation {
                message: "application workspace identifier is invalid".to_string(),
            });
        }
        if !host_work_dir.is_absolute() {
            return Err(SandboxError::Validation {
                message: "application workspace path must be absolute".to_string(),
            });
        }

        let name = format!("{APPLICATION_WORKSPACE_NAME_PREFIX}{application_public_id}");
        let existing = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(&name))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await?;
        if let Some(row) = existing.as_ref() {
            if !application_workspace_identity_is_attested(
                row,
                user_id,
                application_public_id,
                &host_work_dir,
            ) {
                return Err(SandboxError::InvalidState {
                    sandbox_id: row.public_id.clone(),
                    state: "managed workspace identity or host path is not attested".to_string(),
                    operation: "reuse application workspace".to_string(),
                });
            }
            if !config
                .image
                .as_deref()
                .is_some_and(is_managed_application_workspace_image)
            {
                return Err(SandboxError::Validation {
                    message: format!(
                        "application workspace '{}' requires a managed runtime image",
                        row.public_id
                    ),
                });
            }
            if application_workspace_config_has_drift(row, &config)
                && config.desired_state == "running"
            {
                tracing::info!(
                    sandbox_id = %row.public_id,
                    application_id = %application_public_id,
                    old_image = ?row.image,
                    requested_image = ?config.image,
                    "Rebuilding attested application workspace compute while retaining its identity and home volume"
                );
                self.rebuild_application_workspace_locked(
                    user_id,
                    &row.public_id,
                    host_work_dir.clone(),
                    config.clone(),
                )
                .await?;
            }
        }
        let sandbox_public_id = if let Some(mut existing) = existing {
            // `project_id` is the generic sandbox credential scope. Keep it
            // aligned when an application changes its primary project.
            if existing.project_id != project_id {
                let mut active: sandboxes::ActiveModel = existing.clone().into();
                active.project_id = Set(project_id);
                existing = active.update(self.db.as_ref()).await?;
            }
            // Docker/VM state is not authoritative. If an operator or the
            // daemon removed running compute, recreate it against the same
            // application-owned host directory and named home volume.
            if existing.status == "running"
                && self
                    .registry
                    .get(existing.id, &existing.public_id)
                    .await
                    .is_err()
            {
                self.rebuild_application_workspace_locked(
                    user_id,
                    &existing.public_id,
                    host_work_dir.clone(),
                    config.clone(),
                )
                .await?;
            }
            existing.public_id
        } else {
            // A per-workspace password makes the bare opaque hostname useless on
            // its own. The chat API returns only short-lived preview grants; this
            // random value is immediately hashed and is never persisted or shown.
            let preview_password =
                format!("temps-preview-{}", hex::encode(rand::random::<[u8; 16]>()));
            let row = self
                .create_sandbox(
                    user_id,
                    CreateSandboxRequest {
                        name: Some(name),
                        timeout_secs: Some(config.idle_timeout_secs),
                        preview_password: Some(preview_password),
                        ports: Vec::new(),
                        lifecycle: Some("workspace".to_string()),
                        project_id,
                        image: config.image,
                        cpu_limit: Some(config.cpu_limit),
                        memory_limit_mb: Some(config.memory_limit_mb),
                        pids_limit: Some(config.pids_limit),
                        disk_size_mb: Some(config.disk_limit_mb),
                        host_work_dir_override: Some(host_work_dir),
                        ..Default::default()
                    },
                )
                .await?;
            row.public_id
        };

        // Reconcile the complete authorized data plane while stopped compute
        // cannot use stale memberships. Only after this succeeds may an idle
        // workspace be resumed. New/rebuilt compute starts with no application
        // data network; if reconciliation fails, stop it before returning.
        let row = self.find_by_public_id(&sandbox_public_id, user_id).await?;
        self.registry
            .fence_recovered_harness_processes(row.id, &sandbox_public_id)
            .await
            .map_err(|error| from_agent_error(&sandbox_public_id, error))?;
        let configure_result = self
            .configure_application_data_network_locked(&row, authorized_project_ids)
            .await;
        if let Err(error) = configure_result {
            if row.status == "running" {
                if let Err(stop_error) = self.pause_sandbox(&row.public_id, user_id).await {
                    tracing::error!(
                        sandbox_id = %row.public_id,
                        %stop_error,
                        "failed to stop application workspace after data-network reconciliation failed"
                    );
                }
            }
            return Err(error);
        }
        if row.status == "stopped" && config.desired_state == "running" {
            self.resume_sandbox(&sandbox_public_id, user_id).await?;
        }
        if config.desired_state == "running" {
            self.prepare_application_git_workspace(&sandbox_public_id, user_id)
                .await?;
        }
        let handle = if config.desired_state == "running" {
            Some(
                self.registry
                    .get(row.id, &sandbox_public_id)
                    .await
                    .map_err(|error| from_agent_error(&sandbox_public_id, error))?,
            )
        } else {
            None
        };
        Ok(ApplicationWorkspaceSandbox {
            public_id: sandbox_public_id,
            handle,
        })
    }

    pub async fn rebuild_application_workspace(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
    ) -> Result<SandboxSummary, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        self.rebuild_application_workspace_locked(user_id, sandbox_public_id, host_work_dir, config)
            .await
    }

    /// Explicitly replace an application runtime only after a disposable,
    /// volume-free instance has passed the daemon protocol probe.
    #[allow(clippy::too_many_arguments)] // Identity, authorization scope and old-volume path stay explicit at the irreversible boundary.
    pub async fn update_application_workspace_runtime(
        &self,
        user_id: i32,
        application_id: i64,
        application_public_id: &str,
        sandbox_public_id: &str,
        host_work_dir: PathBuf,
        runtime: &str,
        authorized_project_ids: &[i32],
        config: ApplicationWorkspaceConfig,
    ) -> Result<SandboxSummary, SandboxError> {
        let _workspace_mutation = self.lock_application_workspace(application_public_id).await;
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        ensure_not_agent_run(&row)?;
        if managed_application_id_from_name(&row.name) != Some(application_public_id) {
            return Err(SandboxError::InvalidState {
                sandbox_id: sandbox_public_id.to_string(),
                state: format!("does not belong to application {application_public_id}"),
                operation: "update runtime".to_string(),
            });
        }
        let image = config
            .image
            .as_deref()
            .ok_or_else(|| SandboxError::Validation {
                message: format!(
                    "runtime update for sandbox {sandbox_public_id} requires a pinned image"
                ),
            })?;
        if !is_managed_application_workspace_image(image) {
            return Err(SandboxError::Validation {
                message: format!("runtime update for sandbox {sandbox_public_id} rejects unmanaged image {image}"),
            });
        }
        if managed_application_workspace_image(runtime) != Some(image) {
            return Err(SandboxError::Validation {
                message: format!("runtime {runtime} and pinned image {image} disagree for application {application_public_id}"),
            });
        }
        let app_workspace = ai_application_workspaces::Entity::find()
            .filter(ai_application_workspaces::Column::ApplicationId.eq(application_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SandboxError::NotFound {
                sandbox_id: format!("application-workspace:{application_public_id}"),
            })?;
        let _recorded_old_image = row
            .image
            .clone()
            .ok_or_else(|| SandboxError::InvalidState {
                sandbox_id: sandbox_public_id.to_string(),
                state: "missing recorded image".to_string(),
                operation: "update runtime with rollback".to_string(),
            })?;
        let old_handle = self
            .registry
            .get(row.id, sandbox_public_id)
            .await
            .map_err(|error| from_agent_error(sandbox_public_id, error))?;
        let immutable_old_image = self
            .registry
            .provider()
            .image_identity(&old_handle)
            .await
            .map_err(|error| from_agent_error(sandbox_public_id, error))?;
        let mut previous_config = ApplicationWorkspaceConfig::from(&app_workspace);
        previous_config.image = Some(immutable_old_image);
        let previous =
            application_sandbox_create_config(&row, host_work_dir.clone(), &previous_config)?;
        let mut replacement =
            application_sandbox_create_config(&row, host_work_dir.clone(), &config)?;

        let probe_id = hex::encode(rand::random::<[u8; 12]>());
        let probe_dir = std::env::temp_dir().join(format!("temps-runtime-probe-{probe_id}"));
        tokio::fs::create_dir(&probe_dir)
            .await
            .map_err(|source| SandboxError::CreateFailed {
                user_id,
                reason: format!(
                    "create isolated runtime probe directory {}: {source}",
                    probe_dir.display()
                ),
            })?;
        let mut probe = application_sandbox_create_config(&row, probe_dir.clone(), &config)?;
        probe.container_name_override = Some(format!("runtime-probe-{probe_id}"));
        probe.workspace_volume = None;
        probe.network_mode = Some("none".to_string());
        let provider = self.registry.provider_arc();
        let probe_result = async {
            let handle = provider
                .create(probe)
                .await
                .map_err(|error| from_agent_error(sandbox_public_id, error))?;
            let compatibility = provider.check_agent_runtime(&handle).await;
            let candidate_identity = if matches!(
                compatibility,
                Ok(temps_ai_agents::sandbox::RuntimeCompatibility::Compatible)
            ) {
                provider
                    .image_identity(&handle)
                    .await
                    .map_err(|error| from_agent_error(sandbox_public_id, error))
            } else {
                Ok(String::new())
            };
            let cleanup = provider.destroy(&handle, true).await;
            if let Err(error) = cleanup {
                return Err(from_agent_error(sandbox_public_id, error));
            }
            Ok::<_, SandboxError>((
                compatibility.map_err(|error| from_agent_error(sandbox_public_id, error))?,
                candidate_identity?,
            ))
        }
        .await;
        let _ = tokio::fs::remove_dir(&probe_dir).await;
        let (compatibility, candidate_identity) = probe_result?;
        if !matches!(
            compatibility,
            temps_ai_agents::sandbox::RuntimeCompatibility::Compatible
        ) {
            return Err(SandboxError::InvalidState {
                sandbox_id: sandbox_public_id.to_string(),
                state: format!("candidate image {image} failed daemon protocol preflight"),
                operation: "update runtime".to_string(),
            });
        }
        replacement.image = Some(candidate_identity);
        let handle = match self
            .registry
            .replace_with_rollback(row.id, sandbox_public_id, replacement, previous)
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                // A failed candidate create may already have recreated the
                // previous container. Its data network is not reattached by
                // provider.create, so restoring compute alone is incomplete.
                let old_compute = self.registry.get(row.id, sandbox_public_id).await;
                if old_compute.is_ok() {
                    self.configure_application_data_network_locked(&row, authorized_project_ids)
                        .await
                        .map_err(|network_error| SandboxError::CreateFailed {
                            user_id,
                            reason: format!("runtime update for {sandbox_public_id} failed: {error}; previous compute was recreated but its data network could not be restored: {network_error}"),
                        })?;
                }
                return Err(from_agent_error(sandbox_public_id, error));
            }
        };
        let actual_compatibility = self.registry.provider().check_agent_runtime(&handle).await;
        if !matches!(
            actual_compatibility,
            Ok(temps_ai_agents::sandbox::RuntimeCompatibility::Compatible)
        ) {
            let restore =
                application_sandbox_create_config(&row, host_work_dir.clone(), &previous_config)?;
            let mut retry =
                application_sandbox_create_config(&row, host_work_dir.clone(), &config)?;
            retry.image = Some(handle.image.clone());
            self.registry
                .replace_with_rollback(row.id, sandbox_public_id, restore, retry)
                .await
                .map_err(|error| from_agent_error(sandbox_public_id, error))?;
            self.configure_application_data_network_locked(&row, authorized_project_ids)
                .await?;
            return Err(SandboxError::InvalidState {
                sandbox_id: sandbox_public_id.to_string(),
                state: format!("replacement failed actual daemon protocol check ({actual_compatibility:?}); previous compute restored"),
                operation: "update runtime".to_string(),
            });
        }
        if let Err(network_error) = self
            .configure_application_data_network_locked(&row, authorized_project_ids)
            .await
        {
            let restore =
                application_sandbox_create_config(&row, host_work_dir.clone(), &previous_config)?;
            let mut retry =
                application_sandbox_create_config(&row, host_work_dir.clone(), &config)?;
            retry.image = Some(handle.image.clone());
            self.registry.replace_with_rollback(row.id, sandbox_public_id, restore, retry).await
                .map_err(|rollback_error| SandboxError::CreateFailed {
                    user_id,
                    reason: format!("runtime update for {sandbox_public_id} could not configure data network: {network_error}; rollback failed: {rollback_error}"),
                })?;
            self.configure_application_data_network_locked(&row, authorized_project_ids)
                .await?;
            return Err(network_error);
        }
        let mut active: sandboxes::ActiveModel = row.clone().into();
        active.image = Set(Some(image.to_string()));
        active.status = Set("running".to_string());
        active.last_activity_at = Set(Utc::now());
        // Why persisting the update failed. Only the sandbox row having been
        // destroyed means the new compute should go; any other failure
        // (including the workspace row not updating) rolls the compute back.
        enum PersistError {
            SandboxGone,
            Db(sea_orm::DbErr),
        }
        let db_result = async {
            let txn = self.db.begin().await.map_err(PersistError::Db)?;
            // Only a live row: a destroy that landed during the replacement
            // wins (handled below).
            let updated = row_status::update_when(&txn, active, Expect::Live)
                .await
                .map_err(PersistError::Db)?
                .ok_or(PersistError::SandboxGone)?;
            let mut app_active: ai_application_workspaces::ActiveModel = app_workspace.into();
            app_active.runtime = Set(runtime.to_string());
            app_active.image = Set(Some(image.to_string()));
            app_active.last_error = Set(None);
            app_active.updated_at = Set(Utc::now());
            app_active.update(&txn).await.map_err(PersistError::Db)?;
            txn.commit().await.map_err(PersistError::Db)?;
            Ok::<_, PersistError>(updated)
        }
        .await;
        let updated = match db_result {
            Ok(updated) => updated,
            // The sandbox was destroyed while its compute was replaced:
            // remove the replacement instead of restoring the old compute
            // for a row that no longer exists.
            Err(PersistError::SandboxGone) => {
                tracing::warn!(
                    sandbox_id = %sandbox_public_id,
                    internal_id = row.id,
                    "runtime update: the sandbox was destroyed during the update; \
                     removing the replacement compute"
                );
                self.remove_failed_create(&row, "it was destroyed during a runtime update")
                    .await;
                return Err(SandboxError::NotFound {
                    sandbox_id: sandbox_public_id.to_string(),
                });
            }
            Err(PersistError::Db(db_error)) => {
                let restore = application_sandbox_create_config(
                    &row,
                    host_work_dir.clone(),
                    &previous_config,
                )?;
                let replacement_for_rollback =
                    application_sandbox_create_config(&row, host_work_dir, &config)?;
                self.registry.replace_with_rollback(row.id, sandbox_public_id, restore, replacement_for_rollback).await
                    .map_err(|rollback_error| SandboxError::CreateFailed {
                        user_id,
                        reason: format!("runtime update for sandbox {sandbox_public_id} could not save image {image}: {db_error}; rollback failed: {rollback_error}"),
                    })?;
                self.configure_application_data_network_locked(&row, authorized_project_ids)
                    .await?;
                return Err(SandboxError::CreateFailed {
                    user_id,
                    reason: format!("runtime update for sandbox {sandbox_public_id} rolled back after database persistence failed: {db_error}"),
                });
            }
        };
        self.record_event(updated.id, "runtime_updated", None).await;
        Ok(SandboxSummary::from(&updated))
    }

    pub async fn application_workspace_runtime_compatibility(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<temps_ai_agents::sandbox::RuntimeCompatibility, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.registry
            .runtime_compatibility(row.id, sandbox_public_id)
            .await
            .map_err(|error| from_agent_error(sandbox_public_id, error))
    }

    async fn rebuild_application_workspace_locked(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
    ) -> Result<SandboxSummary, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        ensure_not_agent_run(&row)?;
        self.jobs.abort_all(row.id).await;
        let create_config =
            application_sandbox_create_config(&row, host_work_dir.clone(), &config)?;
        let handle = self
            .registry
            .rebuild(row.id, sandbox_public_id, create_config)
            .await
            .map_err(|error| from_agent_error(sandbox_public_id, error))?;
        let managed_application_id = managed_application_id_from_name(&row.name).map(str::to_owned);
        let rebuilt = row.clone();
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("running".to_string());
        active.image = Set(Some(handle.image));
        active.timeout_secs = Set(config.idle_timeout_secs as i32);
        active.last_activity_at = Set(Utc::now());
        active.expires_at = Set(idle_deadline(Utc::now(), config.idle_timeout_secs as i32));
        active.metadata = Set(Some(serde_json::json!({
            "disk_size_mb": config.disk_limit_mb,
            "managed_application_id": managed_application_id,
            "managed_host_work_dir": host_work_dir.to_string_lossy(),
        })));
        let updated = self
            .update_new_compute_row(&rebuilt, active, "rebuild")
            .await?;
        self.record_event(updated.id, "rebuilt", None).await;
        Ok(SandboxSummary::from(&updated))
    }

    pub async fn restore_application_workspace(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
        artifact: &temps_ai_agents::sandbox::SnapshotArtifact,
    ) -> Result<SandboxSummary, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        ensure_not_agent_run(&row)?;
        self.jobs.abort_all(row.id).await;
        let backup = host_work_dir
            .with_extension(format!("restore-backup-{}", Utc::now().timestamp_micros()));
        tokio::fs::rename(&host_work_dir, &backup)
            .await
            .map_err(|source| SandboxError::CreateFailed {
                user_id,
                reason: format!("preserve workspace before restore: {source}"),
            })?;
        if let Err(source) = tokio::fs::create_dir_all(&host_work_dir).await {
            let _ = tokio::fs::rename(&backup, &host_work_dir).await;
            return Err(SandboxError::CreateFailed {
                user_id,
                reason: format!("create empty workspace restore target: {source}"),
            });
        }
        let create_config =
            application_sandbox_create_config(&row, host_work_dir.clone(), &config)?;
        let restored = self
            .registry
            .restore(row.id, sandbox_public_id, artifact, create_config)
            .await;
        let handle = match restored {
            Ok(handle) => handle,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&host_work_dir).await;
                let _ = tokio::fs::rename(&backup, &host_work_dir).await;
                return Err(from_agent_error(sandbox_public_id, error));
            }
        };
        if let Err(error) = tokio::fs::remove_dir_all(&backup).await {
            tracing::warn!(path = %backup.display(), %error, "restored workspace but could not remove backup directory");
        }
        let restored_row = row.clone();
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("running".to_string());
        active.image = Set(Some(handle.image));
        active.last_activity_at = Set(Utc::now());
        active.expires_at = Set(idle_deadline(Utc::now(), config.idle_timeout_secs as i32));
        let updated = self
            .update_new_compute_row(&restored_row, active, "restore")
            .await?;
        self.record_event(updated.id, "restored", None).await;
        Ok(SandboxSummary::from(&updated))
    }

    /// Write `changes` to `row` only while its status still matches
    /// `expect`. Lifecycle calls read the row, wait on the provider (tens of
    /// seconds on a worker node), then write it; a destroy or node eviction
    /// that lands in between wins, and the sandbox is reported gone
    /// (`NotFound`) instead of being written back to life. If `expect` names
    /// a status and the row has meanwhile reached `settled` (the expiry sweep
    /// stopped it while a pause was in flight), that row is the result. Any
    /// other status is an `InvalidState` for `operation`.
    async fn update_row_if(
        &self,
        row: &sandboxes::Model,
        changes: sandboxes::ActiveModel,
        expect: Expect<'_>,
        settled: Option<&str>,
        operation: &str,
    ) -> Result<sandboxes::Model, SandboxError> {
        if let Some(updated) = row_status::update_when(self.db.as_ref(), changes, expect).await? {
            return Ok(updated);
        }
        let current = sandboxes::Entity::find_by_id(row.id)
            .one(self.db.as_ref())
            .await?;
        match current {
            Some(current) if settled == Some(current.status.as_str()) => Ok(current),
            Some(current) if current.status != row_status::DESTROYED => {
                Err(SandboxError::InvalidState {
                    sandbox_id: row.public_id.clone(),
                    state: current.status,
                    operation: operation.to_string(),
                })
            }
            _ => {
                tracing::info!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    node_id = ?row.node_id,
                    operation,
                    "sandbox was destroyed while the {operation} was in flight; \
                     leaving it destroyed"
                );
                Err(SandboxError::NotFound {
                    sandbox_id: row.public_id.clone(),
                })
            }
        }
    }

    /// [`Self::update_row_if`] for a row whose compute was just (re)created.
    /// If the row was destroyed meanwhile, the new compute is removed too,
    /// so no container outlives its row.
    async fn update_new_compute_row(
        &self,
        row: &sandboxes::Model,
        changes: sandboxes::ActiveModel,
        operation: &str,
    ) -> Result<sandboxes::Model, SandboxError> {
        match self
            .update_row_if(row, changes, Expect::Live, None, operation)
            .await
        {
            Err(gone @ SandboxError::NotFound { .. }) => {
                self.remove_failed_create(row, &format!("it was destroyed during a {operation}"))
                    .await;
                Err(gone)
            }
            other => other,
        }
    }

    pub async fn application_workspace_summary(
        &self,
        user_id: i32,
        application_public_id: &str,
    ) -> Result<Option<SandboxSummary>, SandboxError> {
        let name = format!("{APPLICATION_WORKSPACE_NAME_PREFIX}{application_public_id}");
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(name))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut summary = SandboxSummary::from(&row);
        if row.status == "running" && self.registry.get(row.id, &row.public_id).await.is_err() {
            // The durable database row is desired state, not proof that
            // disposable compute still exists. Callers can now distinguish a
            // killed container and trigger idempotent recovery with the saved
            // application configuration and host volume.
            summary.status = "recovering".to_string();
        }
        Ok(Some(summary))
    }

    /// Make a newly-created application project directory writable by the
    /// non-root sandbox user. A stopped or not-yet-created sandbox needs no
    /// special handling: the provider normalizes the complete bind mount when
    /// compute is next created or resumed.
    pub async fn normalize_application_project_permissions(
        &self,
        user_id: i32,
        application_public_id: &str,
        project_slug: &str,
    ) -> Result<(), SandboxError> {
        if application_public_id.is_empty()
            || project_slug.is_empty()
            || !project_slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(SandboxError::Validation {
                message: "application project workspace identifier is invalid".to_string(),
            });
        }
        let Some(row) = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(format!("ai-application:{application_public_id}")))
            .filter(sandboxes::Column::Status.eq("running"))
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(());
        };
        let container_path = format!(
            "{}/projects/{project_slug}",
            temps_ai_agents::sandbox::user::SANDBOX_WORK_DIR
        );
        self.registry
            .normalize_workspace_path(row.id, &row.public_id, &container_path)
            .await
            .map_err(|error| from_agent_error(&row.public_id, error))
    }

    /// Put the workspace and only the databases linked to its application
    /// projects on a private, internal Docker network. This grants direct
    /// data-plane reachability without placing a reusable Temps API token in
    /// the sandbox and without exposing unrelated tenants' services.
    pub async fn synchronize_application_data_network(
        &self,
        user_id: i32,
        _application_public_id: &str,
        sandbox_public_id: &str,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        self.synchronize_data_network(user_id, sandbox_public_id, project_ids)
            .await
    }

    /// Reconcile the isolated data network for any sandbox from an authorized
    /// list of project IDs. The network identity belongs to the sandbox, not
    /// to whichever product surface created it.
    pub async fn synchronize_data_network(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.configure_application_data_network_locked(&row, project_ids)
            .await
    }

    async fn configure_application_data_network_locked(
        &self,
        row: &sandboxes::Model,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let containers = self
            .application_data_service_containers(project_ids)
            .await?;
        let network_name = sandbox_data_network_name(&row.public_id)?;
        self.registry
            .configure_application_network(row.id, &row.public_id, &network_name, &containers)
            .await
            .map_err(|error| from_agent_error(&row.public_id, error))?;
        Ok(containers)
    }

    /// Return the database containers currently implied by the committed
    /// project topology without changing container or network state.
    pub async fn application_data_service_count(
        &self,
        project_ids: &[i32],
    ) -> Result<usize, SandboxError> {
        Ok(self
            .application_data_service_containers(project_ids)
            .await?
            .len())
    }

    /// Resolve the runtime variables available to this sandbox's attached
    /// project. This is a sandbox primitive: application workspaces and
    /// ordinary API-created sandboxes use the same path.
    ///
    /// The sandbox row is the authorization boundary. A caller cannot pass a
    /// different project ID and use sandbox ownership to reveal that project's
    /// credentials. Application workspaces keep `project_id` synchronized to
    /// their primary project when they are prepared.
    pub async fn runtime_environment(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<HashMap<String, String>, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let project_id = row.project_id.ok_or_else(|| SandboxError::Validation {
            message: format!(
                "sandbox {sandbox_public_id} is not attached to a project; attach a project before requesting database runtime variables"
            ),
        })?;
        // Credentials without connectivity are unusable. Reconcile the generic
        // sandbox data network at issuance time so API-created sandboxes get
        // the same behavior as application workspaces, including services
        // linked after the sandbox itself was created.
        self.synchronize_data_network(user_id, sandbox_public_id, &[project_id])
            .await?;
        let environments = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;
        let environment = environments
            .iter()
            .find(|environment| environment.name.eq_ignore_ascii_case("production"))
            .or_else(|| environments.first())
            .ok_or_else(|| SandboxError::RuntimeEnvironmentNotFound {
                sandbox_id: sandbox_public_id.to_string(),
                project_id,
            })?;
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .order_by_asc(project_services::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let provider =
            self.runtime_credentials
                .as_ref()
                .ok_or_else(|| SandboxError::Unavailable {
                    reason: "runtime credential provider is not registered".to_string(),
                })?;
        let service_count = links.len();
        let mut variables = HashMap::new();
        for link in links {
            // A sandbox runs code nobody reviewed, so it gets credentials of
            // its own where the service can mint them (ADR-051). Only a
            // service type with no such mechanism yet keeps the credentials
            // a deployment of the environment would receive.
            let scope = temps_core::SandboxScope {
                sandbox_public_id: sandbox_public_id.to_string(),
                project_id: Some(project_id),
            };
            let issued = match provider.issue_for_sandbox(link.service_id, &scope).await {
                Ok(issued) => issued,
                Err(temps_core::SandboxRuntimeCredentialsError::NoScopedCredentials { .. }) => {
                    provider
                        .issue(link.service_id, project_id, environment.id)
                        .await
                        .map_err(|error| SandboxError::RuntimeCredentialsFailed {
                            sandbox_id: sandbox_public_id.to_string(),
                            service_id: link.service_id,
                            reason: error.to_string(),
                        })?
                }
                Err(error) => {
                    return Err(SandboxError::RuntimeCredentialsFailed {
                        sandbox_id: sandbox_public_id.to_string(),
                        service_id: link.service_id,
                        reason: error.to_string(),
                    })
                }
            };
            merge_runtime_variables(&mut variables, issued).map_err(|variable| {
                SandboxError::RuntimeVariableConflict {
                    sandbox_id: sandbox_public_id.to_string(),
                    variable,
                }
            })?;
        }
        let mut variable_names = variables.keys().cloned().collect::<Vec<_>>();
        variable_names.sort();
        self.record_event(
            row.id,
            "runtime_credentials_issued",
            Some(serde_json::json!({
                "project_id": project_id,
                "environment_id": environment.id,
                "service_count": service_count,
                "variable_names": variable_names,
            })),
        )
        .await;
        Ok(variables)
    }

    /// The services `sandbox_public_id` is linked to (ADR-051), oldest first.
    pub async fn linked_services(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<Vec<LinkedService>, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.linked_services_of(row.id).await
    }

    async fn linked_services_of(
        &self,
        sandbox_id: i32,
    ) -> Result<Vec<LinkedService>, SandboxError> {
        Ok(sandbox_services::Entity::find()
            .filter(sandbox_services::Column::SandboxId.eq(sandbox_id))
            .order_by_asc(sandbox_services::Column::Id)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|link| LinkedService {
                service_id: link.service_id,
                project_id: link.project_id,
            })
            .collect())
    }

    /// The services of `project_id` a sandbox can be linked to: those whose
    /// type has a sandbox access engine and that run where a sandbox can
    /// reach them. A service the sandbox cannot use is not offered.
    pub async fn linkable_project_services(
        &self,
        project_id: i32,
    ) -> Result<Vec<i32>, SandboxError> {
        let service_ids = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .order_by_asc(project_services::Column::ServiceId)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|link| link.service_id)
            .collect::<Vec<_>>();
        let mut linkable = Vec::new();
        for service_id in service_ids {
            if self.service_supports_sandbox(service_id).await? {
                linkable.push(service_id);
            }
        }
        Ok(linkable)
    }

    /// Whether a sandbox can be linked to `service_id`: its type has a sandbox
    /// access engine and it runs where a sandbox can reach it.
    pub async fn service_supports_sandbox(&self, service_id: i32) -> Result<bool, SandboxError> {
        let containers = self
            .credentials_provider()?
            .sandbox_access_containers(service_id)
            .await
            .map_err(|error| match error {
                temps_core::SandboxRuntimeCredentialsError::ServiceNotFound { service_id } => {
                    SandboxError::ServiceNotFound { service_id }
                }
                other => SandboxError::RuntimeCredentialsFailed {
                    sandbox_id: String::new(),
                    service_id,
                    reason: other.to_string(),
                },
            })?;
        Ok(!containers.is_empty())
    }

    /// Link `sandbox_public_id` to `links` and give it what it needs to use
    /// them: the services' containers on its private network, and the
    /// variables to run code with. A link that is already there is kept as it
    /// is.
    ///
    /// Nothing is left half done: if the network or a credential fails, or two
    /// services would set the same variable, the links this call added are
    /// removed again and the error returned.
    pub async fn link_services(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        links: &[LinkedService],
    ) -> Result<ScopedRuntime, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        let existing = self.linked_services_of(row.id).await?;
        let mut added = Vec::new();
        for link in links {
            if existing
                .iter()
                .any(|known| known.service_id == link.service_id)
                || added.contains(&link.service_id)
            {
                continue;
            }
            sandbox_services::ActiveModel {
                sandbox_id: Set(row.id),
                service_id: Set(link.service_id),
                project_id: Set(link.project_id),
                created_by: Set(Some(user_id)),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await?;
            added.push(link.service_id);
        }
        match self.rebuild_scoped(&row).await {
            Ok(runtime) => Ok(runtime),
            Err(error) => {
                if !added.is_empty() {
                    self.delete_links(row.id, &added).await?;
                    if let Err(restore) = self.rebuild_network(&row).await {
                        tracing::warn!(
                            sandbox_id = %row.public_id,
                            "could not restore the sandbox's connections after a failed link: {restore}"
                        );
                    }
                }
                Err(error)
            }
        }
    }

    /// Unlink `sandbox_public_id` from `service_ids` and take their containers
    /// off its private network, which drops the sandbox's open connections to
    /// them. The data in the sandbox's database or bucket stays.
    pub async fn unlink_services(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        service_ids: &[i32],
    ) -> Result<ScopedRuntime, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        self.delete_links(row.id, service_ids).await?;
        self.rebuild_scoped(&row).await
    }

    /// Remove every link of `sandbox_public_id` made through `project_id`.
    /// Returns the services that were unlinked.
    pub async fn unlink_project(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        project_id: i32,
    ) -> Result<(Vec<i32>, ScopedRuntime), SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        let removed = self
            .linked_services_of(row.id)
            .await?
            .into_iter()
            .filter(|link| link.project_id == Some(project_id))
            .map(|link| link.service_id)
            .collect::<Vec<_>>();
        self.delete_links(row.id, &removed).await?;
        let runtime = self.rebuild_scoped(&row).await?;
        Ok((removed, runtime))
    }

    /// The variables for running a command in `sandbox_public_id`, resolved
    /// fresh from its links and without touching the network.
    pub async fn scoped_runtime_variables(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<ScopedRuntime, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.issue_scoped(&row).await
    }

    async fn delete_links(&self, sandbox_id: i32, service_ids: &[i32]) -> Result<(), SandboxError> {
        if service_ids.is_empty() {
            return Ok(());
        }
        sandbox_services::Entity::delete_many()
            .filter(sandbox_services::Column::SandboxId.eq(sandbox_id))
            .filter(sandbox_services::Column::ServiceId.is_in(service_ids.iter().copied()))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Rebuild the private network from the links, then issue the variables.
    async fn rebuild_scoped(&self, row: &sandboxes::Model) -> Result<ScopedRuntime, SandboxError> {
        self.rebuild_network(row).await?;
        self.issue_scoped(row).await
    }

    async fn rebuild_network(&self, row: &sandboxes::Model) -> Result<(), SandboxError> {
        let containers = self.scoped_service_containers(row).await?;
        let network_name = sandbox_data_network_name(&row.public_id)?;
        self.registry
            .configure_application_network(row.id, &row.public_id, &network_name, &containers)
            .await
            .map_err(|error| from_agent_error(&row.public_id, error))
    }

    fn credentials_provider(
        &self,
    ) -> Result<&Arc<dyn temps_core::SandboxRuntimeCredentialsProvider>, SandboxError> {
        self.runtime_credentials
            .as_ref()
            .ok_or_else(|| SandboxError::Unavailable {
                reason: "runtime credential provider is not registered".to_string(),
            })
    }

    async fn issue_scoped(&self, row: &sandboxes::Model) -> Result<ScopedRuntime, SandboxError> {
        let mut runtime = ScopedRuntime::default();
        let links = self.linked_services_of(row.id).await?;
        if links.is_empty() {
            return Ok(runtime);
        }
        let provider = self.credentials_provider()?;
        for link in &links {
            let scope = temps_core::SandboxScope {
                sandbox_public_id: row.public_id.clone(),
                project_id: link.project_id,
            };
            let issued = match provider.issue_for_sandbox(link.service_id, &scope).await {
                Ok(issued) => issued,
                Err(temps_core::SandboxRuntimeCredentialsError::NoScopedCredentials { .. }) => {
                    runtime.skipped_service_ids.push(link.service_id);
                    continue;
                }
                Err(error) => {
                    return Err(SandboxError::RuntimeCredentialsFailed {
                        sandbox_id: row.public_id.clone(),
                        service_id: link.service_id,
                        reason: error.to_string(),
                    })
                }
            };
            merge_runtime_variables(&mut runtime.variables, issued).map_err(|variable| {
                SandboxError::RuntimeVariableConflict {
                    sandbox_id: row.public_id.clone(),
                    variable,
                }
            })?;
            runtime.connected_service_ids.push(link.service_id);
        }
        let mut variable_names = runtime.variable_names();
        variable_names.sort();
        self.record_event(
            row.id,
            "scoped_credentials_issued",
            Some(serde_json::json!({
                "service_ids": runtime.connected_service_ids,
                "variable_names": variable_names,
            })),
        )
        .await;
        Ok(runtime)
    }

    /// The containers of the services the sandbox is linked to, as each
    /// service's engine names them. A service no engine covers contributes
    /// none, so it stays off the sandbox's network.
    async fn scoped_service_containers(
        &self,
        row: &sandboxes::Model,
    ) -> Result<Vec<String>, SandboxError> {
        let links = self.linked_services_of(row.id).await?;
        if links.is_empty() {
            return Ok(Vec::new());
        }
        let provider = self.credentials_provider()?;
        let mut containers = Vec::new();
        for link in links {
            let named = provider
                .sandbox_access_containers(link.service_id)
                .await
                .map_err(|error| SandboxError::RuntimeCredentialsFailed {
                    sandbox_id: row.public_id.clone(),
                    service_id: link.service_id,
                    reason: error.to_string(),
                })?;
            containers.extend(named);
        }
        containers.sort();
        containers.dedup();
        Ok(containers)
    }

    async fn application_data_service_containers(
        &self,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.is_in(project_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        let service_ids = links
            .iter()
            .map(|link| link.service_id)
            .collect::<std::collections::HashSet<_>>();
        let services = if service_ids.is_empty() {
            Vec::new()
        } else {
            external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids.iter().copied()))
                .all(self.db.as_ref())
                .await?
        };
        let service_ids = services
            .iter()
            .map(|service| service.id)
            .collect::<std::collections::HashSet<_>>();
        let members = if service_ids.is_empty() {
            Vec::new()
        } else {
            service_members::Entity::find()
                .filter(service_members::Column::ServiceId.is_in(service_ids.iter().copied()))
                .filter(service_members::Column::NodeId.is_null())
                .all(self.db.as_ref())
                .await?
        };
        let mut containers = Vec::new();
        for service in services {
            if service.node_id.is_some() {
                continue;
            }
            let service_members = members
                .iter()
                .filter(|member| member.service_id == service.id)
                .map(|member| member.container_name.clone())
                .collect::<Vec<_>>();
            if !service_members.is_empty() {
                containers.extend(service_members);
                continue;
            }
            let derived =
                service
                    .container_name
                    .unwrap_or_else(|| match service.service_type.as_str() {
                        "postgres" => format!("postgres-{}", service.name),
                        "redis" => format!("redis-{}", service.name),
                        "mongodb" => format!("temps-mongodb-{}", service.name),
                        "mariadb" | "mysql" => format!("mariadb-{}", service.name),
                        _ => String::new(),
                    });
            if !derived.is_empty() {
                containers.push(derived);
            }
        }
        containers.sort();
        containers.dedup();
        Ok(containers)
    }

    /// Reconcile every running application workspace linked to a project
    /// after that project's service topology changes. Revocations are applied
    /// from the committed database topology; if Docker cannot confirm the
    /// narrower membership, compute is stopped so existing sockets cannot
    /// retain access behind the control plane's back.
    pub async fn reconcile_application_networks_for_project(
        &self,
        project_id: i32,
    ) -> Result<(), SandboxError> {
        let affected_links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        if affected_links.is_empty() {
            return Ok(());
        }
        let application_ids = affected_links
            .iter()
            .map(|link| link.application_id)
            .collect::<Vec<_>>();
        let applications = ai_applications::Entity::find()
            .filter(ai_applications::Column::Id.is_in(application_ids))
            .filter(ai_applications::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;

        let mut first_error = None;
        for application in applications {
            let project_ids = ai_application_projects::Entity::find()
                .filter(ai_application_projects::Column::ApplicationId.eq(application.id))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|link| link.project_id)
                .collect::<Vec<_>>();
            let Some(summary) = self
                .application_workspace_summary(application.created_by, &application.public_id)
                .await?
                .filter(|summary| matches!(summary.status.as_str(), "running" | "recovering"))
            else {
                continue;
            };
            if let Err(error) = self
                .synchronize_application_data_network(
                    application.created_by,
                    &application.public_id,
                    &summary.public_id,
                    &project_ids,
                )
                .await
            {
                let quarantine_error = format!(
                    "Application data-network reconciliation failed; workspace compute was stopped: {error}"
                );
                if let Err(persist_error) = ai_application_workspaces::Entity::update_many()
                    .col_expr(
                        ai_application_workspaces::Column::DesiredState,
                        Expr::value("quarantined"),
                    )
                    .col_expr(
                        ai_application_workspaces::Column::LastError,
                        Expr::value(Some(quarantine_error)),
                    )
                    .col_expr(
                        ai_application_workspaces::Column::UpdatedAt,
                        Expr::value(Utc::now()),
                    )
                    .filter(ai_application_workspaces::Column::ApplicationId.eq(application.id))
                    .exec(self.db.as_ref())
                    .await
                {
                    tracing::error!(
                        application_id = %application.public_id,
                        error = %persist_error,
                        "Failed to persist fail-closed application workspace quarantine"
                    );
                }
                if let Err(stop_error) = self
                    .pause_sandbox(&summary.public_id, application.created_by)
                    .await
                {
                    tracing::error!(
                        application_id = %application.public_id,
                        sandbox_id = %summary.public_id,
                        error = %stop_error,
                        "Failed to stop workspace after data-network reconciliation failure"
                    );
                }
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub async fn application_workspace_usage(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<ApplicationWorkspaceUsage, SandboxError> {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), SOURCE_IMPORT_SYSTEM_PATH.to_string());
        let output = self
            .exec(
                sandbox_public_id,
                user_id,
                ExecOptions {
                    cmd: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        APPLICATION_WORKSPACE_USAGE_COMMAND.to_string(),
                    ],
                    env,
                    ..Default::default()
                },
            )
            .await?;
        let usage = parse_application_workspace_usage(&output.stdout);
        if usage.disk_used_bytes.is_none() {
            return Err(SandboxError::ExecFailed {
                sandbox_id: sandbox_public_id.to_string(),
                reason: "workspace disk usage measurement returned no value".to_string(),
            });
        }
        Ok(usage)
    }

    async fn prepare_application_git_workspace(
        &self,
        sandbox_public_id: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let result = self
            .exec(
                sandbox_public_id,
                user_id,
                ExecOptions {
                    cmd: vec![
                        "sh".to_string(),
                        "-lc".to_string(),
                        APPLICATION_GIT_BOOTSTRAP.to_string(),
                    ],
                    ..Default::default()
                },
            )
            .await?;
        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: sandbox_public_id.to_string(),
                reason: format!(
                    "initialize persistent Git workspace: {}",
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    /// Seed a fresh sandbox's `/workspace` with the requested content.
    /// Uses the provider's exec to keep the source-specific commands
    /// (`git`, `curl`, `tar`) out of the service crate.
    ///
    /// For git sources, credentials are injected via `GIT_ASKPASS` + a
    /// per-clone shim script rather than embedded in the URL or argv.
    /// This keeps the token out of `.git/config`, `ps`, and the provider's
    /// exec logs. The shim is shredded immediately after clone.
    pub(crate) async fn seed_source(
        &self,
        internal_id: i32,
        public_id: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<(), SandboxError> {
        // Resolve once immediately before the network operation and force the
        // client to dial that validated address. Validation performed earlier
        // in request handling is useful for fast failure, but cannot prevent a
        // DNS answer from changing before this command actually connects.
        let network_pin = validate_resolved_source(source).await?;
        let handle = self
            .registry
            .get(internal_id, public_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("resolve handle for seed: {}", e),
            })?;

        let work_dir = handle.work_dir.to_string_lossy().to_string();

        match source {
            SandboxSource::Git {
                url,
                revision,
                depth,
                username,
                password,
                git_connection_id,
                destination,
                strip_git_metadata,
            } => {
                let credentialed = git_connection_id.is_some()
                    || (username.as_deref().is_some() && password.as_deref().is_some());
                if credentialed && !sandbox_image_is_trusted_for_credentials(&handle) {
                    return Err(SandboxError::Validation {
                        message: format!(
                            "credentialed Git imports require a Temps-managed sandbox image; sandbox '{}' uses an untrusted custom or local runtime",
                            public_id
                        ),
                    });
                }
                // Resolve credentials. Priority: explicit (username,password)
                // pair > git_connection_id > anonymous. The validator rejects
                // the "both set" combination before we get here.
                let creds = if let Some(conn_id) = git_connection_id {
                    Some(
                        self.resolve_connection_creds(user_id, *conn_id, url)
                            .await?,
                    )
                } else if let (Some(u), Some(p)) = (username.as_deref(), password.as_deref()) {
                    Some((u.to_string(), p.to_string()))
                } else {
                    None
                };

                validate_source_destination(destination.as_deref())?;
                if *strip_git_metadata && destination.is_none() {
                    return Err(SandboxError::Validation {
                        message: "strip_git_metadata requires a destination so the sandbox root repository cannot be removed"
                            .into(),
                    });
                }
                let target_dir = destination
                    .as_deref()
                    .map(|relative| Path::new(&work_dir).join(relative))
                    .unwrap_or_else(|| PathBuf::from(&work_dir));
                let target_dir = target_dir.to_string_lossy().to_string();

                self.run_git_clone(
                    &handle,
                    internal_id,
                    &work_dir,
                    &target_dir,
                    url,
                    revision.as_deref(),
                    *depth,
                    creds,
                    *strip_git_metadata,
                    network_pin.as_ref(),
                )
                .await
            }
            SandboxSource::Tarball { url } => {
                // Stream into a bounded staging directory. The monitor kills
                // both curl and tar if extraction exceeds the deadline/quota,
                // and only a complete import is copied into the workspace.
                let staging_dir = source_import_staging_dir(handle.backend, internal_id);
                let target_guard = source_target_guard_script(&work_dir, &work_dir);
                let staging_prepare = source_import_staging_prepare_script(&staging_dir);
                let extract = tarball_extract_command(url, &staging_dir, network_pin.as_ref());
                let bounded_extract = source_import_command_script(&staging_dir, &extract);
                let bounds = source_import_bounds_script(&staging_dir);
                let aggregate_bounds =
                    source_import_aggregate_bounds_script(&work_dir, &staging_dir);
                let finalize = source_import_staging_finalize_script(&staging_dir);
                let restore_target = source_target_restore_script();
                let script = format!(
                    "set -eu; {target_guard} trap '{restore_target}' EXIT; {staging_prepare} \
                     {bounded_extract}{bounds}{aggregate_bounds}; {finalize}",
                );
                self.exec_seed_script_as_root(&handle, internal_id, script)
                    .await
            }
        }
    }

    /// Resolve a stored git provider connection to an HTTP-Basic
    /// (username, password) pair. Enforces that the connection is owned
    /// by the calling user so one user can't clone repos using another
    /// user's token.
    async fn resolve_connection_creds(
        &self,
        user_id: i32,
        connection_id: i32,
        clone_url: &str,
    ) -> Result<(String, String), SandboxError> {
        let connection = self
            .git_provider_manager
            .get_connection(connection_id)
            .await
            .map_err(|e| SandboxError::Validation {
                message: format!("Git connection {} not available: {}", connection_id, e),
            })?;

        // Ownership check. Connections without a user_id are
        // organization/platform-level and not usable from per-user
        // sandboxes.
        match connection.user_id {
            Some(owner) if owner == user_id => {}
            _ => {
                return Err(SandboxError::Validation {
                    message: format!(
                        "Git connection {} is not owned by the requesting user",
                        connection_id
                    ),
                });
            }
        }

        let provider = self
            .git_provider_manager
            .get_provider(connection.provider_id)
            .await
            .map_err(|error| SandboxError::Validation {
                message: format!(
                    "Git provider {} for connection {} is not available: {}",
                    connection.provider_id, connection_id, error
                ),
            })?;
        if !provider.is_active {
            return Err(SandboxError::Validation {
                message: format!(
                    "Git provider {} for connection {} is inactive",
                    provider.id, connection_id
                ),
            });
        }
        validate_connection_clone_origin(clone_url, &provider)?;

        let token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: format!("connection#{}", connection_id),
                reason: format!("resolve git token: {}", e),
            })?;

        // GitHub/GitLab both accept `x-access-token` as the username for
        // token-based HTTPS auth. The token goes in the password slot and
        // is injected via GIT_ASKPASS (never argv, never URL).
        Ok(("x-access-token".to_string(), token))
    }

    /// Run the actual `git clone` inside the sandbox. Credentials (if any)
    /// are injected via an ephemeral `GIT_ASKPASS` shim and exported env
    /// vars. We never log `env_map` values and never embed the token in
    /// argv or the URL.
    #[allow(clippy::too_many_arguments)]
    async fn run_git_clone(
        &self,
        handle: &temps_ai_agents::sandbox::SandboxHandle,
        internal_id: i32,
        work_dir: &str,
        target_dir: &str,
        url: &str,
        revision: Option<&str>,
        depth: Option<u32>,
        creds: Option<(String, String)>,
        strip_git_metadata: bool,
        network_pin: Option<&SourceNetworkPin>,
    ) -> Result<(), SandboxError> {
        // Destination creation is deliberately performed as the sandbox user.
        // The privileged phase only opens and validates an already-existing
        // directory, so a malicious symlink cannot make root create paths
        // outside the workspace before validation.
        self.ensure_source_target_as_sandbox_user(handle, internal_id, target_dir)
            .await?;
        let private_root = match handle.backend {
            temps_ai_agents::sandbox::SandboxBackend::Local => "/tmp",
            // Docker mounts /run/secrets as tmpfs. Credential material must
            // never enter the writable layer where a concurrent `commit`
            // snapshot could retain it.
            _ => "/run/secrets",
        };
        let auth_dir = format!("{private_root}/.temps-source-auth-{internal_id}");
        let askpass_path = format!("{auth_dir}/askpass.sh");
        let askpass_user_path = format!("{auth_dir}/username");
        let askpass_password_path = format!("{auth_dir}/password");
        let staging_dir = source_import_staging_dir(handle.backend, internal_id);

        // Build the clone command. `-c credential.helper=` disables any
        // host-level credential helper so the password lands only via
        // the askpass shim and is never persisted to `.git/config`.
        // Redirects are disabled for anonymous and authenticated clones. The
        // source host was DNS-validated before this point; following a new
        // location would otherwise bypass that decision.
        let git_network_config = network_pin
            .map(|pin| {
                format!(
                    " -c http.curloptResolve={}",
                    shell_escape_service(&pin.curl_resolve_value())
                )
            })
            .unwrap_or_default();
        let git_environment =
            clean_git_environment(&auth_dir, creds.is_some().then_some(askpass_path.as_str()));
        let mut clone_cmd = format!(
            "{git_environment} git -c credential.helper= -c http.followRedirects=false{git_network_config}"
        );
        clone_cmd.push_str(&format!(
            " clone --depth {} --filter=blob:limit=33554432",
            depth.unwrap_or(1)
        ));
        if let Some(r) = revision {
            if !r.is_empty() {
                // `--branch` accepts branches and tags. For raw commit
                // SHAs this fails, and we fall back to a post-clone
                // `checkout` below.
                clone_cmd.push_str(&format!(" --branch {}", shell_escape_service(r)));
            }
        }
        clone_cmd.push_str(&format!(
            " {} {}",
            shell_escape_service(url),
            shell_escape_service(&staging_dir)
        ));

        // If revision didn't resolve to a branch/tag (e.g. raw SHA),
        // fall back to a post-clone checkout. Harmless when --branch
        // already did the right thing.
        let checkout_cmd = match revision {
            Some(r) if !r.is_empty() => format!(
                " || ({git_environment} git -C {wd} -c credential.helper= -c http.followRedirects=false{git_network_config} fetch origin {rev} && {git_environment} git -C {wd} checkout {rev})",
                wd = shell_escape_service(&staging_dir),
                rev = shell_escape_service(r),
            ),
            _ => String::new(),
        };
        let target_guard = source_target_guard_script(work_dir, target_dir);
        let staging_prepare = source_import_staging_prepare_script(&staging_dir);
        let auth_prepare = source_import_staging_prepare_script(&auth_dir);
        let bounded_clone =
            source_import_command_script(&staging_dir, &format!("{clone_cmd}{checkout_cmd}"));
        let cleanup = git_metadata_cleanup_script(&staging_dir, strip_git_metadata);
        let bounds = source_import_bounds_script(&staging_dir);
        let aggregate_bounds = source_import_aggregate_bounds_script(work_dir, &staging_dir);
        let finalize = source_import_staging_finalize_script(&staging_dir);
        let restore_target = source_target_restore_script();

        // Compose the shell script. When creds are present we write the
        // root-only credential files plus an askpass shim. Git itself runs
        // under `env -i`; the shim reads those files, so secrets enter neither
        // Git's argv nor its inherited environment. We always shred/remove the
        // whole auth directory before returning so a subsequent user
        // shell in the sandbox can't read stale state.
        let script = if creds.is_some() {
            format!(
                "set +x; set -eu; \
                 {target_guard} {staging_prepare} {auth_prepare} \
                 trap 'find {auth_dir} -type f -exec shred -u {{}} \\; 2>/dev/null || true; find {auth_dir} -depth -delete 2>/dev/null || true; {restore_target}' EXIT; \
                 umask 077; printf '%s' \"$GIT_USER\" > {user_file}; printf '%s' \"$GIT_PASS\" > {password_file}; \
                 cat > {ask} <<'TEMPS_ASKPASS_EOF'\n\
#!/bin/sh\n\
case \"$1\" in\n\
  Username*) cat {user_file} ;;\n\
  *)         cat {password_file} ;;\n\
esac\n\
TEMPS_ASKPASS_EOF\n\
                 chmod 700 {ask}; \
                 {bounded_clone}{cleanup}{bounds}{aggregate_bounds}; {finalize}",
                target_guard = target_guard,
                staging_prepare = staging_prepare,
                auth_prepare = auth_prepare,
                ask = askpass_path,
                user_file = shell_escape_service(&askpass_user_path),
                password_file = shell_escape_service(&askpass_password_path),
                auth_dir = shell_escape_service(&auth_dir),
                restore_target = restore_target,
                bounded_clone = bounded_clone,
                cleanup = cleanup,
                bounds = bounds,
                aggregate_bounds = aggregate_bounds,
                finalize = finalize,
            )
        } else {
            format!(
                "set -eu; {target_guard} {staging_prepare} {auth_prepare} \
                 trap 'find {auth_dir} -depth -delete 2>/dev/null || true; {restore_target}' EXIT; \
                 GIT_TERMINAL_PROMPT=0 {bounded_clone}{cleanup}{bounds}{aggregate_bounds}; {finalize}",
                target_guard = target_guard,
                staging_prepare = staging_prepare,
                auth_prepare = auth_prepare,
                auth_dir = shell_escape_service(&auth_dir),
                restore_target = restore_target,
                bounded_clone = bounded_clone,
                cleanup = cleanup,
                bounds = bounds,
                aggregate_bounds = aggregate_bounds,
                finalize = finalize,
            )
        };

        let mut env_map: HashMap<String, String> = HashMap::new();
        if let Some((u, p)) = creds {
            env_map.insert("GIT_USER".into(), u);
            env_map.insert("GIT_PASS".into(), p);
        }
        env_map.insert("HOME".into(), auth_dir);
        env_map.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env_map.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        env_map.insert("GIT_CONFIG_SYSTEM".into(), "/dev/null".into());

        self.exec_seed_script_with_env_as_root(handle, internal_id, script, env_map)
            .await
    }

    async fn ensure_source_target_as_sandbox_user(
        &self,
        handle: &temps_ai_agents::sandbox::SandboxHandle,
        internal_id: i32,
        target_dir: &str,
    ) -> Result<(), SandboxError> {
        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("mkdir -p -- {}", shell_escape_service(target_dir)),
        ];
        let result = self
            .registry
            .provider()
            .exec(
                handle,
                cmd,
                HashMap::from([("PATH".to_string(), SOURCE_IMPORT_SYSTEM_PATH.to_string())]),
                None,
            )
            .await
            .map_err(|error| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("prepare source destination: {error}"),
            })?;
        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "prepare source destination exited with code {}: {}",
                    result.exit_code,
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    async fn exec_seed_script_as_root(
        &self,
        handle: &temps_ai_agents::sandbox::SandboxHandle,
        internal_id: i32,
        script: String,
    ) -> Result<(), SandboxError> {
        self.exec_seed_script_with_env_as_root(handle, internal_id, script, HashMap::new())
            .await
    }

    /// Execute a seed script with an env map. Never logs env values — the
    /// map may contain tokens. On non-zero exit we surface stdout/stderr
    /// but the sandbox layer scrubs them before they reach the user (the
    /// provider's exec impl is expected to honor this).
    async fn exec_seed_script_with_env_as_root(
        &self,
        handle: &temps_ai_agents::sandbox::SandboxHandle,
        internal_id: i32,
        script: String,
        mut env_map: HashMap<String, String>,
    ) -> Result<(), SandboxError> {
        sanitize_privileged_import_environment(&mut env_map);
        env_map.insert("PATH".into(), SOURCE_IMPORT_SYSTEM_PATH.into());
        let cmd = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let execution = self
            .registry
            .provider()
            .exec_as_root(handle, cmd, env_map, None);
        let result = tokio::time::timeout(SOURCE_IMPORT_PROVIDER_TIMEOUT, execution)
            .await
            .map_err(|_| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "source import exceeded the {} second safety limit",
                    SOURCE_IMPORT_PROVIDER_TIMEOUT.as_secs()
                ),
            })?
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("seed source exec: {}", e),
            })?;

        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "seed source exited with code {}: {}",
                    result.exit_code,
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    /// Stop + destroy a sandbox. Aborts any background jobs, asks the
    /// provider to tear down the container + volumes, and marks the
    /// DB row "destroyed".
    ///
    /// Agent-run sandboxes (`agent_run_id` set) are special-cased: their
    /// container is named `temps-sandbox-<run_id>`, so the standalone
    /// registry (which recovers by `public_id`) would miss it, flip the
    /// row to "destroyed", and leave the real container running. While
    /// the run is active we refuse with [`SandboxError::ManagedByAgentRun`]
    /// (destroying the sandbox under a live run would break it); once the
    /// run is terminal we route through `release_for_agent_run`, which
    /// targets the container by run id.
    pub async fn destroy_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        self.destroy_row(
            row,
            false,
            DestroyMode::Single {
                worker_deadline: WORKER_DESTROY_DEADLINE,
            },
        )
        .await
        .map(|_| ())
    }

    /// Destroy one sandbox row, whoever owns it (callers check ownership).
    /// `force` also destroys a sandbox still managed by a live agent run —
    /// only for operator actions such as [`Self::evict_node`]. `mode` bounds
    /// how long a worker's container destroy may take and decides what an
    /// unreachable worker means (see [`DestroyMode`]). Otherwise the row is
    /// marked destroyed even when the container destroy fails, and the
    /// outcome says whether the container is confirmed gone.
    async fn destroy_row(
        &self,
        row: sandboxes::Model,
        force: bool,
        mode: DestroyMode<'_>,
    ) -> Result<ContainerOutcome, SandboxError> {
        let public_id_value = row.public_id.as_str();
        if let Some(run_id) = row.agent_run_id {
            let run = agent_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await?;
            let terminal = run_status_is_terminal(run.as_ref().map(|r| r.status.as_str()));
            if !terminal && !force {
                return Err(SandboxError::ManagedByAgentRun {
                    sandbox_id: public_id_value.to_string(),
                    run_id,
                });
            }
            self.jobs.abort_all(row.id).await;
            return self
                .release_for_agent_run(run_id, None)
                .await
                .map(|_| ContainerOutcome::Destroyed);
        }

        self.jobs.abort_all(row.id).await;
        let destroyed = match mode {
            DestroyMode::Single { .. } if row.node_id.is_none() => {
                self.registry.destroy(row.id, public_id_value).await
            }
            DestroyMode::Single { worker_deadline } => {
                let unreachable = match tokio::time::timeout(
                    worker_deadline,
                    self.registry.destroy(row.id, public_id_value),
                )
                .await
                {
                    Ok(Err(
                        e @ (AgentError::SandboxNodeUnavailable { .. }
                        | AgentError::SandboxProviderUnavailable { .. }),
                    )) => e.to_string(),
                    Ok(result) => return self.finish_destroy(&row, result).await,
                    Err(_) => format!("it did not answer within {}s", worker_deadline.as_secs()),
                };
                return Err(self.node_unreachable(&row, &unreachable).await);
            }
            DestroyMode::Evict(gate) if gate.is_down() => {
                Err(AgentError::SandboxProviderUnavailable {
                    provider: "eviction".to_string(),
                    reason: "the node did not answer earlier destroys in this eviction; \
                         skipped the container call"
                        .to_string(),
                })
            }
            DestroyMode::Evict(gate) => {
                match tokio::time::timeout(
                    gate.deadline,
                    self.registry.destroy(row.id, public_id_value),
                )
                .await
                {
                    // A worker answering 503 (its Docker is down) did reply,
                    // but the rest of its destroys would fail the same way,
                    // so it counts as unanswered too.
                    Ok(Err(
                        e @ (AgentError::SandboxNodeUnavailable { .. }
                        | AgentError::SandboxProviderUnavailable { .. }),
                    )) => {
                        gate.unanswered();
                        Err(e)
                    }
                    Ok(result) => {
                        gate.answered();
                        result
                    }
                    Err(_) => {
                        gate.unanswered();
                        Err(AgentError::SandboxProviderUnavailable {
                            provider: "eviction".to_string(),
                            reason: format!(
                                "the node did not answer within {}s",
                                gate.deadline.as_secs()
                            ),
                        })
                    }
                }
            }
        };
        self.finish_destroy(&row, destroyed).await
    }

    /// An eviction can destroy a worker sandbox's row while its container
    /// is still being created: it finds nothing to remove yet and marks the
    /// row destroyed. Placement also only checks the eviction cordon before
    /// the row exists, so an eviction that started in between has not
    /// listed this row and would leave it behind. Called once the container
    /// exists: if the row is gone (or can't be read), or its node is being
    /// evicted, remove what was just created so no container outlives its
    /// row and an eviction always leaves its node empty. An eviction that
    /// starts after this check lists the row and removes it.
    async fn confirm_worker_create(
        &self,
        row: &sandboxes::Model,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        if let Some(node_id) = row.node_id {
            if self.evicting_nodes.contains(node_id) {
                self.remove_failed_create(row, "its node started being evicted during the create")
                    .await;
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id,
                    reason: format!(
                        "the sandbox's node (id {node_id}) started evicting its sandboxes while \
                         this one was being created; create it again to place it on another node"
                    ),
                });
            }
        }
        let still_live = sandboxes::Entity::find_by_id(row.id)
            .one(self.db.as_ref())
            .await
            .map(|r| r.is_some_and(|r| r.status != "destroyed"));
        let reason = match still_live {
            Ok(true) => return Ok(()),
            Ok(false) => "the sandbox was destroyed while it was being created \
                          (its node was evicted); create it again"
                .to_string(),
            // Can't tell whether the row survived: fail closed, like every
            // other failure after the container exists.
            Err(e) => {
                format!("could not confirm the sandbox after creating it ({e}); create it again")
            }
        };
        self.remove_failed_create(row, "its row was destroyed or unreadable after the create")
            .await;
        self.mark_destroyed(row.id).await.ok();
        Err(SandboxError::CreateFailed { user_id, reason })
    }

    /// Remove the container of a sandbox whose create failed. A worker
    /// node gets the same per-node deadline as an eviction's container
    /// destroy: a node that accepts the connection but never answers must
    /// not hold the create request for the provider's whole lifecycle
    /// timeout. When the container cannot be confirmed gone, the error log
    /// carries what an operator needs to remove it by hand (node, sandbox,
    /// cleanup command); nothing else tracks it once the row is destroyed.
    async fn remove_failed_create(&self, row: &sandboxes::Model, why: &str) {
        let removed = match row.node_id {
            None => self
                .registry
                .destroy(row.id, &row.public_id)
                .await
                .map_err(|e| e.to_string()),
            Some(_) => match tokio::time::timeout(
                CREATE_CLEANUP_DEADLINE,
                self.registry.destroy(row.id, &row.public_id),
            )
            .await
            {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(_) => Err(format!(
                    "the node did not answer within {}s",
                    CREATE_CLEANUP_DEADLINE.as_secs()
                )),
            },
        };
        let Err(reason) = removed else {
            return;
        };
        let node = match row.node_id {
            Some(id) => placement::node_names(self.db.as_ref(), [id])
                .await
                .ok()
                .and_then(|mut names| names.remove(&id))
                .unwrap_or_else(|| format!("id {id}")),
            None => placement::CONTROL_PLANE_NAME.to_string(),
        };
        tracing::error!(
            sandbox_id = %row.public_id,
            internal_id = row.id,
            node_id = ?row.node_id,
            node = %node,
            cleanup_command = %node_cleanup_command(&row.public_id),
            reason = %bounded_reason(&reason),
            "sandbox create cleanup: {why}, and the sandbox's container could not be \
             confirmed removed; it may still be running on node '{node}'. Run the \
             cleanup command on that node to remove it"
        );
    }

    /// The node hosting `row` could not destroy its container. The row is
    /// left as it is; the error names the node and says what to do.
    async fn node_unreachable(&self, row: &sandboxes::Model, reason: &str) -> SandboxError {
        let node = match row.node_id {
            Some(id) => placement::node_names(self.db.as_ref(), [id])
                .await
                .ok()
                .and_then(|mut names| names.remove(&id))
                .unwrap_or_else(|| format!("id {id}")),
            None => placement::CONTROL_PLANE_NAME.to_string(),
        };
        tracing::warn!(
            sandbox_id = %row.public_id,
            node = %node,
            "not destroying sandbox: its node is unreachable ({reason}); the row is kept"
        );
        SandboxError::NodeUnreachable {
            sandbox_id: row.public_id.clone(),
            node,
            reason: bounded_reason(reason),
        }
    }

    /// Mark `row` destroyed after its container destroy was attempted, and
    /// clean up what belongs to it.
    async fn finish_destroy(
        &self,
        row: &sandboxes::Model,
        destroyed: Result<(), AgentError>,
    ) -> Result<ContainerOutcome, SandboxError> {
        let public_id_value = row.public_id.as_str();
        let outcome = match destroyed {
            Ok(()) => ContainerOutcome::Destroyed,
            Err(e) => {
                // Even if the container destroy failed, mark the row
                // destroyed — otherwise the user is stuck with a zombie
                // they can't delete. Log the provider error loudly, and
                // report it so callers don't claim the container is gone.
                tracing::error!(
                    "Provider destroy failed for sandbox {} (internal {}): {} — marking row destroyed anyway",
                    public_id_value,
                    row.id,
                    e
                );
                ContainerOutcome::Unconfirmed(bounded_reason(&e.to_string()))
            }
        };
        // Remove the work dir even when the provider destroy failed. There
        // is no background sweep to fall back on — this call is the only
        // thing that ever frees the directory, so skipping it here means
        // leaking it permanently, which is the bug this fixes. The tradeoff
        // is that a failed destroy may leave a container still running with
        // this directory bind-mounted at /workspace, and the delete then
        // races that writer. Deleting is still the right call: the user
        // explicitly asked for the sandbox to be gone, and the row is
        // marked destroyed either way.
        self.remove_work_dir(public_id_value).await;
        self.record_event(row.id, "destroyed", None).await;
        self.mark_destroyed(row.id).await?;

        // Standalone snapshots may outlive their deleted sandbox without
        // retaining an internal identity. Managed application snapshots keep
        // immutable provenance so they can never become visible through the
        // generic snapshot API after the compute row is destroyed.
        if !row.name.starts_with(APPLICATION_WORKSPACE_NAME_PREFIX) {
            if let Some(ref snap_svc) = self.snapshot_service {
                if let Err(e) = snap_svc.nullify_source_sandbox(row.id).await {
                    tracing::warn!(
                        sandbox_id = %public_id_value,
                        internal_id = row.id,
                        "destroy: failed to nullify source_sandbox_id on snapshots: {}",
                        e
                    );
                }
            }
        }

        Ok(outcome)
    }

    /// Delete the host-side `/workspace` directory backing a destroyed
    /// sandbox.
    ///
    /// `create_sandbox` makes `data_root/<public_id>` and bind-mounts it
    /// into the container; nothing used to remove it, so every
    /// create+destroy cycle left the full working tree (node_modules, build
    /// output, cloned repos — routinely hundreds of MB) on the host
    /// forever. Deliberately best-effort and non-fatal: the sandbox is
    /// already gone from the user's point of view, and failing the API call
    /// over leftover bytes would strand the row instead.
    ///
    /// Only called for standalone sandboxes. Agent-run sandboxes get their
    /// work dir from the executor, which owns that directory's lifecycle.
    async fn remove_work_dir(&self, public_id_value: &str) {
        let Some(work_dir) = work_dir_to_remove(&self.data_root, public_id_value) else {
            tracing::warn!(
                "Refusing to remove sandbox work dir for malformed public id {:?}",
                public_id_value
            );
            return;
        };
        match tokio::fs::remove_dir_all(&work_dir).await {
            Ok(()) => tracing::debug!("Removed sandbox work dir {}", work_dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                "Failed to remove sandbox work dir {}: {} — disk space stays claimed",
                work_dir.display(),
                e
            ),
        }
    }

    /// Pause a running sandbox (non-destructive). Stops the underlying
    /// container but leaves the DB row, volumes, and filesystem intact so
    /// the user can resume later. Idempotent on already-stopped sandboxes.
    pub async fn pause_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "stopped" {
            return Ok(row);
        }
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "pause".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        match self.registry.stop(row.id, public_id_value).await {
            Ok(()) | Err(temps_ai_agents::error::AgentError::SandboxNotFound { .. }) => {}
            Err(error) => return Err(from_agent_error(public_id_value, error)),
        }
        let now = Utc::now();
        let sandbox_id = row.id;
        // A worker's "not found" above may mean an eviction or destroy
        // removed the container meanwhile: only a row still `running` is
        // moved to `stopped`, never a destroyed one.
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("stopped".to_string()),
            last_activity_at: Set(now),
            ..Default::default()
        };
        let updated = self
            .update_row_if(
                &row,
                changes,
                Expect::Status("running"),
                Some("stopped"),
                "pause",
            )
            .await?;
        self.record_event(sandbox_id, "stopped", None).await;
        Ok(updated)
    }

    /// Resume a paused sandbox. Restarts the container and bumps
    /// `expires_at` to `now + timeout_secs` so the user gets a fresh
    /// idle window. Idempotent on already-running sandboxes.
    pub async fn resume_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let mut row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "running" {
            // A row can say `running` for a VM that died with the Temps service
            // or the host. Resume is what the "VM not running" error tells the
            // caller to run, so it must not be a no-op for that row: when the
            // provider proves the VM is down, reconcile the row and boot it.
            // Anything short of proof (alive, or the check failed) keeps the
            // idempotent answer.
            match self.registry.probe(row.id, public_id_value).await {
                VmProbe::Down(cause) if self.mark_vm_lost(&row, &cause, "resume").await => {
                    row.status = "stopped".to_string();
                }
                VmProbe::Alive(_) | VmProbe::Down(_) | VmProbe::Unknown(_) => return Ok(row),
            }
        }
        if row.status != "stopped" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "resume".into(),
            });
        }
        self.registry
            .start(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        self.enforce_stored_egress_policy(&row).await?;
        let now = Utc::now();
        let new_expires = idle_deadline(now, row.timeout_secs);
        let sandbox_id = row.id;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("running".to_string()),
            last_activity_at: Set(now),
            expires_at: Set(new_expires),
            ..Default::default()
        };
        let updated = self
            .update_row_if(
                &row,
                changes,
                Expect::Status("stopped"),
                Some("running"),
                "resume",
            )
            .await?;
        self.record_event(sandbox_id, "resumed", None).await;
        Ok(updated)
    }

    /// Restart a running sandbox in-place (stop + start). Filesystem and
    /// volumes survive. Rejected on stopped sandboxes (use resume instead).
    pub async fn restart_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "restart".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        self.registry
            .restart(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        self.enforce_stored_egress_policy(&row).await?;
        let now = Utc::now();
        let sandbox_id = row.id;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            last_activity_at: Set(now),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "restart")
            .await?;
        self.record_event(sandbox_id, "restarted", None).await;
        Ok(updated)
    }

    /// Grow the sandbox's root disk to `new_size_mb`. Firecracker only —
    /// the resize is done offline (stop → grow ext4 → start), so the VM
    /// reboots but its filesystem and data survive. Grow-only. Records a
    /// `resized` event and persists the new size in `metadata`.
    pub async fn resize_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
        new_size_mb: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        const MIN_DISK_MB: u64 = 256;
        const MAX_DISK_MB: u64 = 64 * 1024;
        if !(MIN_DISK_MB..=MAX_DISK_MB).contains(&new_size_mb) {
            return Err(SandboxError::Validation {
                message: format!(
                    "disk_size_mb must be between {} and {}",
                    MIN_DISK_MB, MAX_DISK_MB
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.backend.as_deref() != Some("firecracker") {
            return Err(SandboxError::Validation {
                message: "disk resize is only supported on Firecracker sandboxes".into(),
            });
        }
        let old_mb = row
            .metadata
            .as_ref()
            .and_then(|v| v.get("disk_size_mb"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1024);
        let sandbox_id = row.id;

        self.registry
            .resize_disk(row.id, public_id_value, new_size_mb)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;

        // Persist the new size into metadata (preserving ports).
        let mut meta = row
            .metadata
            .clone()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        meta.insert("disk_size_mb".into(), serde_json::json!(new_size_mb));
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            metadata: Set(Some(serde_json::Value::Object(meta))),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "resize")
            .await?;

        self.record_event(
            sandbox_id,
            "resized",
            Some(serde_json::json!({ "from_mb": old_mb, "to_mb": new_size_mb })),
        )
        .await;
        Ok(updated)
    }

    /// Replace the sandbox's egress policy: which outside destinations it
    /// may reach.
    ///
    /// The backend applies the policy first and the row is written only
    /// once it has, so the stored policy is never one the sandbox does not
    /// enforce. If that write then fails (the sandbox was destroyed
    /// meanwhile, or the database is unavailable), the previous policy is
    /// applied again before the error is returned. A running Docker sandbox
    /// gets a new egress proxy, which cuts its outbound connections for a
    /// second or two; a stopped one keeps the policy for its next start.
    pub async fn set_egress_policy(
        &self,
        public_id_value: &str,
        user_id: i32,
        policy: temps_ai_agents::sandbox::SandboxEgressPolicy,
    ) -> Result<sandboxes::Model, SandboxError> {
        let policy = policy
            .normalized()
            .map_err(|error| SandboxError::Validation {
                message: format!("egress policy for sandbox {public_id_value}: {error}"),
            })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status != "running" && row.status != "stopped" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "change its egress policy".into(),
            });
        }
        // Serializes with every other change to this sandbox's compute
        // (restart, resize, a resume re-applying its policy), so two updates
        // cannot leave the row naming one policy and the sandbox enforcing
        // the other.
        let _serialized = self.registry.begin_planned_downtime(row.id).await;
        let previous = row.egress_policy.0.clone();
        self.registry
            .set_egress_policy(row.id, public_id_value, &policy)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            egress_policy: Set(sandboxes::StoredEgressPolicy(policy.clone())),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let updated = match self
            .update_row_if(&row, changes, Expect::Live, None, "egress policy update")
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                if let Err(rollback) = self
                    .registry
                    .set_egress_policy(row.id, public_id_value, &previous)
                    .await
                {
                    tracing::error!(
                        sandbox_id = %public_id_value,
                        internal_id = row.id,
                        applied_mode = policy.mode.as_str(),
                        stored_mode = previous.mode.as_str(),
                        "egress policy was applied but not stored ({error}), and restoring \
                         the stored policy failed too ({rollback}); the sandbox enforces a \
                         policy its row does not show until the policy is set again"
                    );
                }
                return Err(error);
            }
        };
        tracing::info!(
            sandbox_id = %public_id_value,
            internal_id = row.id,
            from_mode = previous.mode.as_str(),
            to_mode = policy.mode.as_str(),
            rules = policy.rules.len(),
            "sandbox egress policy updated"
        );
        self.record_event(
            row.id,
            "egress_policy_updated",
            Some(serde_json::json!({
                "from_mode": previous.mode.as_str(),
                "to_mode": policy.mode.as_str(),
                "rules": policy.rules.len(),
            })),
        )
        .await;
        Ok(updated)
    }

    /// Make a sandbox that has just started enforce its stored egress
    /// policy. The backend carries the policy across a stop (Docker keeps it
    /// on the parked egress proxy), so this is normally a no-op; it puts the
    /// policy back on a sandbox whose proxy was lost while it was stopped.
    /// A restricted sandbox that cannot get its policy back is stopped again
    /// rather than left running with wider egress than its owner allowed.
    async fn enforce_stored_egress_policy(
        &self,
        row: &sandboxes::Model,
    ) -> Result<(), SandboxError> {
        // `internet` is what a backend without a stored policy enforces, so
        // only a restricted policy can need re-applying. A concurrent change
        // to restricted is applied by `set_egress_policy` itself.
        if !row.egress_policy.0.is_restricted() {
            return Ok(());
        }
        let _serialized = self.registry.begin_planned_downtime(row.id).await;
        // Re-read under the lock: a concurrent `set_egress_policy` may have
        // replaced the policy since `row` was loaded.
        let Some(current) = sandboxes::Entity::find_by_id(row.id)
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(());
        };
        let policy = current.egress_policy.0;
        let Err(error) = self
            .registry
            .set_egress_policy(row.id, &row.public_id, &policy)
            .await
        else {
            return Ok(());
        };
        tracing::error!(
            sandbox_id = %row.public_id,
            internal_id = row.id,
            "could not re-apply the restricted egress policy of a started sandbox ({error}); \
             stopping it so it does not run with wider egress"
        );
        self.jobs.abort_all(row.id).await;
        match self.registry.stop(row.id, &row.public_id).await {
            Ok(()) | Err(temps_ai_agents::error::AgentError::SandboxNotFound { .. }) => {
                self.mark_stopped_for_egress_failure(row, &error).await;
            }
            Err(stop) => {
                // The compute may still be running; a `stopped` row would hide
                // it, so the row keeps its status and the error says why.
                tracing::error!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    "could not stop sandbox after failing to re-apply its egress policy: {stop}"
                );
            }
        }
        Err(from_agent_error(&row.public_id, error))
    }

    /// Record that the service stopped a sandbox because its stored egress
    /// policy could not be re-applied: the row moves to `stopped` (unless it
    /// was destroyed meanwhile) and a `stopped` event carries the reason, so
    /// the console can say why the sandbox is not running.
    async fn mark_stopped_for_egress_failure(
        &self,
        row: &sandboxes::Model,
        cause: &temps_ai_agents::error::AgentError,
    ) {
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("stopped".to_string()),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        match row_status::update_when(self.db.as_ref(), changes, Expect::Live).await {
            Ok(Some(_)) => {
                self.record_event(
                    row.id,
                    "stopped",
                    Some(serde_json::json!({
                        "reason": EGRESS_POLICY_REAPPLY_FAILED,
                        "detail": format!(
                            "the sandbox's restricted egress policy could not be re-applied \
                             after it started, so it was stopped rather than run with wider \
                             egress: {cause}"
                        ),
                    })),
                )
                .await;
            }
            Ok(None) => {}
            Err(db_error) => {
                tracing::error!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    error = %db_error,
                    "could not mark sandbox stopped after failing to re-apply its egress policy"
                );
            }
        }
    }

    /// Seed an already-running sandbox with additional content. Mirrors
    /// the SDK's ability to attach a source *after* create; useful when
    /// the caller wants to clone a repo using a token that wasn't
    /// available at create time, or to layer a second repo on top.
    ///
    /// Rejects non-running sandboxes with `InvalidState`. The underlying
    /// `seed_source` applies the same credential-safe flow used on create.
    pub async fn clone_source(
        &self,
        public_id_value: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<sandboxes::Model, SandboxError> {
        let _source_import = self.source_import_lock.lock().await;
        validate_resolved_source(source).await?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "clone source".into(),
            });
        }
        self.seed_source(row.id, &row.public_id, user_id, source)
            .await?;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        self.update_row_if(&row, changes, Expect::Live, None, "clone source")
            .await
    }

    /// Extend the sandbox's `expires_at` by `extra_secs`. Does not
    /// change `timeout_secs` — just pushes the deadline forward. Used
    /// by the SDK's `extendTimeout()` so long-running operations can
    /// keep the sandbox alive without recreating it.
    pub async fn extend_timeout(
        &self,
        public_id_value: &str,
        user_id: i32,
        extra_secs: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        if extra_secs == 0 {
            return Err(SandboxError::Validation {
                message: "extra_secs must be greater than zero".into(),
            });
        }
        if extra_secs > MAX_TIMEOUT_SECS {
            return Err(SandboxError::Validation {
                message: format!(
                    "extra_secs {} exceeds maximum of {}",
                    extra_secs, MAX_TIMEOUT_SECS
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let sandbox_id = row.id;
        let new_expires = row.expires_at + chrono::Duration::seconds(extra_secs as i64);
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            expires_at: Set(new_expires),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "extend timeout")
            .await?;
        self.record_event(
            sandbox_id,
            "timeout_extended",
            Some(serde_json::json!({
                "extra_secs": extra_secs,
                "expires_at": new_expires.to_rfc3339(),
            })),
        )
        .await;
        Ok(updated)
    }

    /// Record activity on a sandbox: bump `last_activity_at` **and** push
    /// `expires_at` out to `now + timeout_secs`.
    ///
    /// Moving `expires_at` here is what makes `timeout_secs` an *idle*
    /// timeout, which is what the column has always been documented as
    /// (and what the API calls it). Before this, `expires_at` was set once
    /// at create and never moved by activity, so a sandbox in continuous
    /// use was still stopped at its original deadline.
    ///
    /// The deadline is materialised in the indexed column rather than
    /// computed by the sweeper (`last_activity_at + timeout_secs`) on
    /// purpose: the sweep query rides the partial index on
    /// `(expires_at) WHERE status = 'running'`, and a predicate over an
    /// expression would not use it.
    ///
    /// Called on every exec and filesystem operation, so it must stay a
    /// single UPDATE with no read-modify-write. `timeout_secs` is taken
    /// from the caller (who already has the row) to avoid a SELECT here.
    ///
    /// **The deadline only ever moves forward.** `extend_timeout` (the SDK's
    /// `extendTimeout()`) pushes `expires_at` past `now + timeout_secs`
    /// precisely so a long operation can outlive the idle window — and the
    /// first thing that operation does is exec, which lands here. Assigning
    /// the idle deadline unconditionally would silently undo the extension
    /// the caller just paid for, so the write is `CASE WHEN`-guarded in SQL:
    /// still one statement, still no read-modify-write, but never a
    /// regression. `last_activity_at` is always bumped — it records activity,
    /// not a deadline.
    ///
    /// Swallows DB errors: activity bumps are best-effort, and failing a
    /// user's exec because a bookkeeping write lost a race would be worse
    /// than a sandbox expiring slightly early.
    pub async fn touch(&self, sandbox_id: i32, timeout_secs: i32) {
        let now = Utc::now();
        let deadline = idle_deadline(now, timeout_secs);
        let result = sandboxes::Entity::update_many()
            .col_expr(sandboxes::Column::LastActivityAt, Expr::value(now))
            .col_expr(
                sandboxes::Column::ExpiresAt,
                Expr::cust_with_values(
                    "CASE WHEN expires_at < $1 THEN $2 ELSE expires_at END",
                    [deadline, deadline],
                ),
            )
            .filter(sandboxes::Column::Id.eq(sandbox_id))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = result {
            tracing::debug!(
                "touch: failed to bump last_activity_at for {}: {}",
                sandbox_id,
                e
            );
        }
    }

    async fn mark_destroyed(&self, id: i32) -> Result<(), SandboxError> {
        let now = Utc::now();
        let active = sandboxes::ActiveModel {
            id: Set(id),
            status: Set("destroyed".to_string()),
            last_activity_at: Set(now),
            ..Default::default()
        };
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Rootfs storage inventory across backends (Firecracker cache + per-VM
    /// disks). Powers the management API's inspection endpoint.
    pub async fn rootfs_report(
        &self,
    ) -> Result<temps_ai_agents::sandbox::RootfsReport, SandboxError> {
        self.registry
            .provider_arc()
            .rootfs_report()
            .await
            .map_err(|e| SandboxError::Unavailable {
                reason: format!("rootfs report: {}", e),
            })
    }

    /// Reclaim rootfs cache entries not backing any live sandbox.
    pub async fn gc_rootfs(
        &self,
    ) -> Result<temps_ai_agents::sandbox::RootfsGcReport, SandboxError> {
        self.registry
            .provider_arc()
            .gc_rootfs()
            .await
            .map_err(|e| SandboxError::Unavailable {
                reason: format!("rootfs gc: {}", e),
            })
    }

    /// Put what the driver facade reported when it placed the sandbox on its
    /// timeline (ADR-055 §5): a requested limit the driver could not apply
    /// (`LimitEnforced = False`, with the reason) or a passed-over driver.
    /// Nothing is recorded when there is nothing to say, which is always the
    /// case outside `enforce` mode.
    async fn record_driver_conditions(
        &self,
        sandbox_id: i32,
        handle: &temps_ai_agents::sandbox::SandboxHandle,
    ) {
        if handle.conditions.is_empty() {
            return;
        }
        let conditions = match serde_json::to_value(&handle.conditions) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    "failed to encode driver conditions for sandbox {} ({}): {}",
                    sandbox_id,
                    handle.sandbox_name,
                    e
                );
                return;
            }
        };
        self.record_event(
            sandbox_id,
            "driver_conditions",
            Some(serde_json::json!({ "conditions": conditions })),
        )
        .await;
    }

    /// Append one operation to a sandbox's timeline. Best-effort — a failed
    /// insert is logged, never fatal to the operation it records.
    async fn record_event(
        &self,
        sandbox_id: i32,
        event_type: &str,
        detail: Option<serde_json::Value>,
    ) {
        let active = sandbox_events::ActiveModel {
            sandbox_id: Set(sandbox_id),
            event_type: Set(event_type.to_string()),
            detail: Set(detail),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        if let Err(e) = active.insert(self.db.as_ref()).await {
            tracing::warn!(
                "failed to record '{}' event for sandbox {}: {}",
                event_type,
                sandbox_id,
                e
            );
        }
    }

    /// The operations timeline for a sandbox, newest first. Ownership is
    /// enforced by resolving the sandbox through `find_by_public_id`.
    pub async fn list_events(
        &self,
        public_id_value: &str,
        user_id: i32,
        limit: u64,
    ) -> Result<Vec<sandbox_events::Model>, SandboxError> {
        // Bounded — the timeline is append-only and a UI only ever shows the
        // most recent slice. Cap the page so a long-lived sandbox can't force
        // an unbounded scan.
        let limit = limit.clamp(1, 500);
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let events = sandbox_events::Entity::find()
            .filter(sandbox_events::Column::SandboxId.eq(row.id))
            .order_by_desc(sandbox_events::Column::CreatedAt)
            .order_by_desc(sandbox_events::Column::Id)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;
        Ok(events)
    }

    /// Persist the effective backend (and, when the request didn't specify
    /// one, the resolved image) a sandbox runs on.
    async fn record_backend(
        &self,
        id: i32,
        backend: temps_ai_agents::sandbox::SandboxBackend,
        effective_image: Option<String>,
    ) -> Result<(), SandboxError> {
        let mut active = sandboxes::ActiveModel {
            id: Set(id),
            backend: Set(Some(backend.to_string())),
            ..Default::default()
        };
        if let Some(image) = effective_image {
            active.image = Set(Some(image));
        }
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    // ── Preview password ────────────────────────────────────────────────

    /// Set (or rotate) the preview password for a sandbox. The plaintext
    /// is hashed with argon2id and only the hash + last-4 hint are stored.
    /// Returns the hint to the caller — the plaintext is never persisted
    /// or echoed back (the caller already has it).
    ///
    /// Rotating an existing password invalidates every live preview cookie
    /// immediately: the proxy folds a digest of the argon2 hash into the
    /// cookie payload, so a new hash = a new fingerprint = every existing
    /// cookie fails verification.
    pub async fn set_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
        plaintext: &str,
    ) -> Result<String, SandboxError> {
        crate::services::preview_password::validate(plaintext)
            .map_err(|message| SandboxError::Validation { message })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let hp = crate::services::preview_password::hash_password(plaintext).map_err(|reason| {
            SandboxError::PasswordHashFailed {
                sandbox_id: public_id_value.to_string(),
                reason,
            }
        })?;
        let sandbox_id = row.id;
        let previous_access = row.preview_access;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(Some(hp.hash));
        active.preview_password_hint = Set(Some(hp.hint.clone()));
        // Setting a password is choosing the password wall.
        active.preview_access = Set(SandboxPreviewAccess::Password);
        active.update(self.db.as_ref()).await?;
        self.record_event(
            sandbox_id,
            "preview_password_set",
            Some(serde_json::json!({
                "previous_access": previous_access.as_str(),
                "access": SandboxPreviewAccess::Password.as_str(),
            })),
        )
        .await;
        Ok(hp.hint)
    }

    /// Remove the preview password. A sandbox in `password` mode returns to
    /// `temps_login` -- never to an open preview. Idempotent: clearing an
    /// already-unset password is a no-op, not an error. Returns the access
    /// mode the sandbox has afterwards.
    pub async fn clear_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<SandboxPreviewAccess, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let had_password = row.preview_password_hash.is_some();
        let previous_access = row.preview_access;
        let access = access_after_clearing_password(previous_access);
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(None);
        active.preview_password_hint = Set(None);
        active.preview_access = Set(access);
        active.update(self.db.as_ref()).await?;
        if had_password {
            self.record_event(
                sandbox_id,
                "preview_password_cleared",
                Some(serde_json::json!({
                    "previous_access": previous_access.as_str(),
                    "access": access.as_str(),
                })),
            )
            .await;
        }
        Ok(access)
    }

    /// Change who may open the sandbox's preview URLs and from which
    /// networks.
    ///
    /// `allowed_cidrs` replaces the stored allow-list (empty = any address)
    /// after validation and normalisation. `password` is refused while no
    /// preview password is set: the mode would admit nobody, and the caller
    /// almost certainly meant to set one first. The proxy picks a change up
    /// within its sandbox lookup cache TTL (30 s).
    pub async fn set_preview_access(
        &self,
        public_id_value: &str,
        user_id: i32,
        access: SandboxPreviewAccess,
        allowed_cidrs: &[String],
    ) -> Result<PreviewAccessChange, SandboxError> {
        let allowed_cidrs = crate::services::preview_access::normalize_allowed_cidrs(allowed_cidrs)
            .map_err(|message| SandboxError::Validation {
                message: format!(
                    "Invalid preview allow-list for sandbox {public_id_value}: {message}"
                ),
            })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if access == SandboxPreviewAccess::Password && row.preview_password_hash.is_none() {
            return Err(SandboxError::Validation {
                message: format!(
                    "Sandbox {public_id_value} has no preview password, so `password` access \
                     would admit nobody. Set one first with PUT \
                     /v1/sandboxes/{public_id_value}/preview-password (which also switches \
                     the sandbox to `password` access)."
                ),
            });
        }
        let previous_access = row.preview_access;
        let previous_cidrs = row.preview_allowed_cidrs.clone();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_access = Set(access);
        active.preview_allowed_cidrs = Set(allowed_cidrs.clone());
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(
            sandbox_id,
            "preview_access_changed",
            Some(serde_json::json!({
                "previous_access": previous_access.as_str(),
                "access": access.as_str(),
                "previous_allowed_cidrs": previous_cidrs,
                "allowed_cidrs": allowed_cidrs,
            })),
        )
        .await;
        Ok(PreviewAccessChange {
            previous_access,
            previous_allowed_cidrs: previous_cidrs,
            sandbox: updated,
        })
    }

    // ── Helpers shared with the exec/fs modules ──────────────────────────

    /// A provider handle for a sandbox that must be running, for exec, files
    /// and terminals. Maps provider errors to typed [`SandboxError`]s and, when
    /// the provider reports the VM is down while the row still says `running`,
    /// reconciles the row to `stopped` so listings stop lying and
    /// `sandbox resume` has something to resume.
    pub(crate) async fn live_handle(
        &self,
        row: &sandboxes::Model,
    ) -> Result<temps_ai_agents::sandbox::SandboxHandle, SandboxError> {
        match self.registry.probe(row.id, &row.public_id).await {
            VmProbe::Alive(handle) => Ok(handle),
            VmProbe::Down(error) => {
                self.mark_vm_lost(row, &error, "access").await;
                Err(from_agent_error(&row.public_id, error))
            }
            VmProbe::Unknown(error) => Err(from_agent_error(&row.public_id, error)),
        }
    }

    /// Move a `running` row whose VM the provider reports as down to `stopped`,
    /// leaving `expires_at` and the rest of the row untouched, and record a
    /// `vm_lost` event naming the cause. Conditional on the row still being
    /// `running` *and* untouched since `row` was read, so it never overwrites a
    /// concurrent pause or destroy, nor a resume that landed after the probe
    /// (a resume moves `last_activity_at`).
    /// Returns whether this call changed the row. Failures are logged, not
    /// raised: the caller is already reporting the underlying problem.
    pub(crate) async fn mark_vm_lost(
        &self,
        row: &sandboxes::Model,
        cause: &temps_ai_agents::error::AgentError,
        source: &'static str,
    ) -> bool {
        // Managed application workspaces report a lost runtime as the derived
        // status `recovering`, which is what makes their own recovery (and the
        // data-network reconciliation, which only visits `running | recovering`
        // rows) run. Stopping the row would hide that.
        if row.status != "running" || managed_application_id_from_name(&row.name).is_some() {
            return false;
        }
        // The verdict was reached from a probe that may have run while the
        // control plane itself had the VM down (snapshot capture, restart,
        // disk resize); those start it again and leave the row `running`.
        // Take the claim that excludes them, then ask again: the first answer
        // may describe a stop that has since finished.
        let Some(_unplanned) = self.registry.observe_unplanned(row.id) else {
            tracing::debug!(
                sandbox_id = %row.public_id,
                internal_id = row.id,
                source,
                "VM is down for a planned stop; not marking the sandbox lost"
            );
            return false;
        };
        match tokio::time::timeout(
            VM_RECONCILE_DEADLINE,
            self.registry.probe(row.id, &row.public_id),
        )
        .await
        {
            Ok(VmProbe::Down(_)) => {}
            Ok(VmProbe::Alive(_)) | Ok(VmProbe::Unknown(_)) | Err(_) => return false,
        }
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("stopped".to_string()),
            ..Default::default()
        };
        match row_status::update_when(
            self.db.as_ref(),
            changes,
            Expect::RunningSince(row.last_activity_at),
        )
        .await
        {
            Ok(Some(_)) => {
                // Only once the row really moved: a stale copy of the row must
                // not kill the jobs of a sandbox that was resumed meanwhile.
                self.jobs.abort_all(row.id).await;
                tracing::warn!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    backend = ?row.backend,
                    node_id = ?row.node_id,
                    source,
                    reason = %cause,
                    "Sandbox row said running but its VM is not; marked stopped (resume it to boot the VM again)"
                );
                self.record_event(
                    row.id,
                    "vm_lost",
                    Some(serde_json::json!({
                        "source": source,
                        "backend": row.backend,
                        "reason": cause.to_string(),
                    })),
                )
                .await;
                true
            }
            Ok(None) => false,
            Err(error) => {
                tracing::error!(
                    sandbox_id = %row.public_id,
                    internal_id = row.id,
                    error = %error,
                    "Could not mark a sandbox whose VM is not running as stopped"
                );
                false
            }
        }
    }

    /// Startup reconciliation: for every control-plane VM sandbox row that
    /// says `running`, ask the provider whether its VM really is, and move the
    /// ones that are not to `stopped`. Rows are read in id-ordered pages of
    /// [`VM_RECONCILE_PAGE_SIZE`], each page checked before the next is read,
    /// so memory stays constant however many sandboxes exist and slow checks
    /// cannot pile rows up. A page that cannot be read ends the run early
    /// (logged); the lazy check on first access covers whatever was not seen.
    pub async fn reconcile_all_running_vms(&self) -> VmReconcileReport {
        self.reconcile_running_vms_paged(VM_RECONCILE_PAGE_SIZE)
            .await
    }

    async fn reconcile_running_vms_paged(&self, page_size: u64) -> VmReconcileReport {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

        let mut report = VmReconcileReport::default();
        let mut after_id = 0;
        loop {
            let page = sandboxes::Entity::find()
                .filter(sandboxes::Column::Status.eq("running"))
                .filter(sandboxes::Column::NodeId.is_null())
                .filter(sandboxes::Column::AgentRunId.is_null())
                .filter(sandboxes::Column::Backend.is_in(["firecracker", "microsandbox"]))
                .filter(sandboxes::Column::Id.gt(after_id))
                .order_by_asc(sandboxes::Column::Id)
                .limit(page_size)
                .all(self.db.as_ref())
                .await;
            let rows = match page {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(
                        after_id,
                        error = %error,
                        "Could not read the next page of running sandboxes for VM reconciliation; stopping early"
                    );
                    break;
                }
            };
            let Some(last) = rows.last() else { break };
            after_id = last.id;
            let full_page = u64::try_from(rows.len()).is_ok_and(|n| n >= page_size);
            report.absorb(self.reconcile_running_vms(rows).await);
            if !full_page {
                break;
            }
        }
        report
    }

    /// Check one bounded page of rows (see [`Self::reconcile_all_running_vms`]),
    /// with bounded concurrency and a per-sandbox deadline so a wedged VM
    /// cannot stall startup. A liveness check that fails without proving the
    /// VM is down (error, timeout) leaves the row alone: the lazy check on
    /// first access decides later.
    pub async fn reconcile_running_vms(&self, rows: Vec<sandboxes::Model>) -> VmReconcileReport {
        use futures::StreamExt;

        let total = rows.len();
        // Verdicts are consumed as they arrive: only the bounded number of
        // in-flight checks is ever held, not one result per row.
        let mut verdicts = futures::stream::iter(rows)
            .map(|row| async move {
                let check = match tokio::time::timeout(
                    VM_RECONCILE_DEADLINE,
                    self.registry.probe(row.id, &row.public_id),
                )
                .await
                {
                    Ok(VmProbe::Alive(_)) => VmCheck::Alive,
                    Ok(VmProbe::Down(error)) => VmCheck::Down(error),
                    Ok(VmProbe::Unknown(error)) => VmCheck::Unknown(error.to_string()),
                    Err(_) => VmCheck::Unknown(format!(
                        "liveness check did not answer within {}s",
                        VM_RECONCILE_DEADLINE.as_secs()
                    )),
                };
                (row, check)
            })
            .buffer_unordered(VM_RECONCILE_CONCURRENCY);

        let mut report = VmReconcileReport {
            checked: total,
            ..Default::default()
        };
        while let Some((row, check)) = verdicts.next().await {
            match check {
                VmCheck::Alive => report.alive += 1,
                VmCheck::Down(cause) => {
                    if self.mark_vm_lost(&row, &cause, "startup").await {
                        report.marked_stopped_total += 1;
                        if report.marked_stopped.len() < VM_RECONCILE_REPORTED_IDS {
                            report.marked_stopped.push(row.public_id.clone());
                        }
                    } else {
                        report.not_marked += 1;
                    }
                }
                VmCheck::Unknown(reason) => {
                    tracing::warn!(
                        sandbox_id = %row.public_id,
                        internal_id = row.id,
                        backend = ?row.backend,
                        reason = %reason,
                        "Could not tell whether a running sandbox's VM is alive at startup; leaving its row as is"
                    );
                    report.unverified += 1;
                }
            }
        }
        report
    }

    /// Load + authorize + return the internal ID **without** waking a
    /// suspended workspace.
    ///
    /// For endpoints that only need to prove ownership and then answer from
    /// the row or from in-memory state — preview URLs, job listings, job
    /// status. Those are reads; booting a container as a side effect of a
    /// `GET` is a surprise that costs real memory on a 3 vCPU / 4 GB host, and
    /// in the job cases it is pure waste: the in-memory job table was emptied
    /// when the container stopped, so waking it cannot change the answer.
    ///
    /// Use [`Self::resolve_id`] on paths that genuinely need the container
    /// running (exec, filesystem, terminal).
    pub async fn resolve_id_no_wake(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let id = row.id;
        Ok((row, id))
    }

    /// Read-only preview renewal preflight. A stale `running` row is not enough:
    /// the provider runtime must still be alive. This never wakes a workspace.
    pub async fn preview_ready_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if row.status != "running" || self.registry.get(row.id, public_id_value).await.is_err() {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: "unavailable".into(),
                operation: "renew preview access while the workspace is running".into(),
            });
        }
        Ok(row)
    }

    /// Load + authorize + return the internal ID, or a typed error that
    /// already includes the public ID. Exec/fs modules call this first.
    /// Explicitly stopped ephemeral sandboxes return `InvalidState` (→ HTTP
    /// 409); durable workspaces wake transparently when either the row or the
    /// provider reports that the runtime is stopped.
    ///
    /// **Wakes a suspended workspace as a side effect** (ADR-036), so only
    /// call it where the container must actually be running.
    pub async fn resolve_id(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if self.creates_in_progress.contains(public_id_value) {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: "starting".to_string(),
                operation: "run exec or filesystem operations until its create finishes \
                            (the first sandbox on a node builds its image, which can take \
                            several minutes); retry when its status is running"
                    .to_string(),
            });
        }
        let lifecycle = SandboxLifecycle::from_row(&row);
        if row.status == "stopped" {
            // Workspaces (ADR-036) are places a human comes back to, so a
            // suspended one wakes instead of erroring. Ephemeral sandboxes
            // keep the 409: their lifetime belongs to whoever created them,
            // and `@vercel/sandbox` consumers rely on a stopped sandbox
            // staying stopped rather than silently restarting under them.
            if lifecycle == SandboxLifecycle::Workspace {
                return self.wake_workspace(row).await;
            }
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: format!(
                    "run exec or filesystem operations until it is resumed \
                     (`bunx @temps-sdk/cli sandbox resume {public_id_value}`)"
                ),
            });
        }

        // Docker (or another provider) can stop a container without passing
        // through SandboxService: daemon pressure, an operator `docker kill`,
        // or the application harness hard deadline are all legitimate ways to
        // reach that state. In those cases the DB row still says `running`.
        // Trusting only the row makes the first application-workspace access
        // fail in `registry.get()` before the later harness recovery path gets
        // a chance to start the container.
        //
        // Only workspaces auto-recover. Ephemeral sandboxes retain their
        // caller-owned lifecycle semantics, and `registry.get()` below the
        // service layer will still return a typed not-found error for them.
        if row.status == "running" && lifecycle == SandboxLifecycle::Workspace {
            match self.registry.get(row.id, public_id_value).await {
                Ok(_) => {}
                Err(
                    temps_ai_agents::error::AgentError::SandboxNotFound { .. }
                    | temps_ai_agents::error::AgentError::SandboxNotRunning { .. },
                ) => {
                    tracing::warn!(
                        sandbox_id = public_id_value,
                        internal_id = row.id,
                        "Workspace row says running but its provider runtime is stopped; waking it on access"
                    );
                    return self.wake_workspace(row).await;
                }
                Err(error) => return Err(from_agent_error(public_id_value, error)),
            }
        }
        let id = row.id;
        Ok((row, id))
    }

    /// Start a suspended workspace's container and reconcile its row to
    /// `running`. Called from [`Self::resolve_id`] on the access path for both
    /// an explicitly stopped row and a stale `running` row whose provider
    /// runtime has died, so the caller experiences recovery as a slow first
    /// request rather than a failure.
    ///
    /// A wake is a container start, not a create: the image is local, the
    /// home volume exists, and the work dir is still populated on the
    /// host. Nothing is re-cloned and nothing is lost.
    ///
    /// If the provider can't start it, the row stays `stopped` and the
    /// caller gets the provider's error — deliberately not swallowed. A
    /// workspace that silently fails to wake and then reports "not found"
    /// on every subsequent call is exactly the debugging dead end
    /// self-hosted users can't escape.
    async fn wake_workspace(
        &self,
        row: sandboxes::Model,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let public_id_value = row.public_id.clone();
        let sandbox_id = row.id;
        tracing::info!(
            "Waking workspace sandbox {} (internal {}) on access",
            public_id_value,
            sandbox_id
        );
        // Bounded: a wake happens inline on a user's request, and a wedged
        // container runtime would otherwise hang that request forever with no
        // way for the caller to tell a slow start from a dead daemon.
        match tokio::time::timeout(
            WAKE_TIMEOUT,
            self.registry.start(sandbox_id, &public_id_value),
        )
        .await
        {
            Ok(result) => result.map_err(|e| from_agent_error(&public_id_value, e))?,
            Err(_) => {
                return Err(SandboxError::InvalidState {
                    sandbox_id: public_id_value.clone(),
                    state: "stopped".to_string(),
                    operation: format!(
                        "wake timed out after {}s; the container runtime did not \
                         start the workspace. Check the daemon, then retry",
                        WAKE_TIMEOUT.as_secs()
                    ),
                });
            }
        }
        self.enforce_stored_egress_policy(&row).await?;

        let now = Utc::now();
        let new_expires = idle_deadline(now, row.timeout_secs);
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("running".to_string()),
            last_activity_at: Set(now),
            expires_at: Set(new_expires),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "wake")
            .await?;
        self.record_event(sandbox_id, "woken", None).await;
        Ok((updated, sandbox_id))
    }

    /// Build a typed provider error into a `SandboxError` carrying the
    /// public ID. Thin wrapper — module-private modules call this.
    pub(crate) fn provider_err(
        public_id_value: &str,
        err: temps_ai_agents::error::AgentError,
    ) -> SandboxError {
        from_agent_error(public_id_value, err)
    }

    // ── Preview URL (`sandbox.domain(port)`) ─────────────────────────────

    /// Resolve the public URL for a port inside the sandbox. Returns the
    /// same `ws-<id>-<port>.<domain>` hostname the preview gateway already
    /// routes for workspace sessions, so standalone sandboxes don't
    /// require any gateway changes.
    ///
    /// Validation: `port` must be in [1, 65535]. Port `0` is rejected
    /// because the gateway matches exact numbers — surfacing a URL with
    /// `port=0` would be useless.
    pub async fn domain(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
    ) -> Result<String, SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        // Ownership + validity check. The numeric id is intentionally
        // discarded — the preview URL never embeds it. No wake: building a
        // URL string does not need the container running.
        let _ = self.resolve_id_no_wake(public_id_value, user_id).await?;
        let parts = self.preview_parts().await;
        Ok(parts.url_for(public_id_value, port))
    }

    /// Build a shareable preview link that carries its own authorization.
    ///
    /// `domain()` returns the bare preview URL, which is useless to anyone who
    /// does not already hold the preview password — so sharing a protected
    /// preview meant sharing the password itself, which then cannot be revoked
    /// for one recipient. This returns that URL plus a minted `session_grant`
    /// aimed at the login bridge: the recipient's browser exchanges it for the
    /// ordinary preview cookie and lands on `path`.
    ///
    /// The grant is scoped to this one sandbox, expires, and is never
    /// forwarded to the sandbox itself.
    pub async fn preview_share_link(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
        path: &str,
        ttl: Duration,
    ) -> Result<(String, u64), SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        // No wake: minting a share link reads the row's password hash.
        let (row, _) = self.resolve_id_no_wake(public_id_value, user_id).await?;
        // A share link stands in for the password, so it only exists while
        // the password wall is the sandbox's access mode.
        let password_hash = row
            .preview_password_hash
            .as_deref()
            .filter(|_| row.preview_access == SandboxPreviewAccess::Password)
            .ok_or_else(|| SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: "preview password disabled".into(),
                operation: "mint an expiring preview link; set a preview password first".into(),
            })?;

        let now = SystemTime::now();
        let (grant, expires_at) = temps_core::encode_preview_session_grant(
            &self.cookie_crypto,
            public_id_value,
            password_hash,
            ttl,
            now,
        )
        .map_err(|source| SandboxError::PreviewGrantFailed {
            sandbox_id: public_id_value.to_string(),
            source,
        })?;
        let granted_ttl = ttl.min(temps_core::PREVIEW_SESSION_GRANT_MAX_TTL);

        let next = temps_core::sanitize_preview_next(path);

        let base = self.preview_parts().await.url_for(public_id_value, port);
        let url = format!(
            "{}/__temps/preview/login?grant=1&next={}#session_grant={}",
            base.trim_end_matches('/'),
            url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>(),
            url::form_urlencoded::byte_serialize(grant.as_bytes()).collect::<String>(),
        );
        self.record_event(
            row.id,
            "preview_share_link_created",
            Some(serde_json::json!({
                "actor_user_id": user_id,
                "port": port,
                "path": next,
                "ttl_seconds": granted_ttl.as_secs(),
                "expires_at": expires_at,
            })),
        )
        .await;
        Ok((url, expires_at))
    }
}

/// A service a sandbox is linked to (ADR-051).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedService {
    pub service_id: i32,
    /// The project the link was made through, which names the sandbox's
    /// database and bucket; `None` for a link made to the service directly.
    pub project_id: Option<i32>,
}

/// What a sandbox was connected to, and the variables to run code with.
#[derive(Default)]
pub struct ScopedRuntime {
    /// Secret values: never log or persist them.
    pub variables: HashMap<String, String>,
    /// Services the variables reach.
    pub connected_service_ids: Vec<i32>,
    /// Linked services left out because their type has no scoped credentials
    /// yet.
    pub skipped_service_ids: Vec<i32>,
}

impl ScopedRuntime {
    pub fn variable_names(&self) -> Vec<String> {
        self.variables.keys().cloned().collect()
    }
}

impl std::fmt::Debug for ScopedRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names = self.variable_names();
        names.sort();
        formatter
            .debug_struct("ScopedRuntime")
            .field("variable_names", &names)
            .field("connected_service_ids", &self.connected_service_ids)
            .field("skipped_service_ids", &self.skipped_service_ids)
            .finish()
    }
}

fn merge_runtime_variables(
    destination: &mut HashMap<String, String>,
    issued: HashMap<String, String>,
) -> Result<(), String> {
    for (name, value) in issued {
        if destination
            .get(&name)
            .is_some_and(|existing| existing != &value)
        {
            return Err(name);
        }
        destination.insert(name, value);
    }
    Ok(())
}

#[async_trait::async_trait]
impl temps_core::ApplicationDataNetworkReconciler for SandboxService {
    async fn project_topology_changed(
        &self,
        project_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reconcile_application_networks_for_project(project_id)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

fn sandbox_data_network_name(sandbox_public_id: &str) -> Result<String, SandboxError> {
    let opaque_id = sandbox_public_id
        .strip_prefix("sbx_")
        .unwrap_or(sandbox_public_id);
    if opaque_id.is_empty()
        || opaque_id.len() > 40
        || !opaque_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SandboxError::Validation {
            message: "sandbox data-network identifier is invalid".to_string(),
        });
    }
    Ok(format!("temps-sandbox-data-{opaque_id}"))
}

fn application_sandbox_create_config(
    row: &sandboxes::Model,
    host_work_dir: PathBuf,
    config: &ApplicationWorkspaceConfig,
) -> Result<SandboxCreateConfig, SandboxError> {
    let backend = row
        .backend
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|_| SandboxError::Validation {
            message: format!(
                "application workspace '{}' has an unsupported backend",
                row.public_id
            ),
        })?;
    Ok(SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id: row.id,
        container_name_override: Some(container_label_for(&row.public_id).to_string()),
        host_work_dir,
        workspace_volume: None,
        image: config.image.clone(),
        cpu_limit: Some(config.cpu_limit),
        memory_limit_mb: Some(config.memory_limit_mb),
        pids_limit: Some(config.pids_limit),
        disk_size_mb: Some(config.disk_limit_mb),
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: None,
        env_vars: HashMap::new(),
        idle_timeout: Duration::from_secs(config.idle_timeout_secs),
        backend,
    })
}

/// Placeholder "public id" used in error messages when the source-seed
/// step fails before we propagate it upward. We already mapped the
/// real public ID into the top-level Create error; this just gives the
/// inner ExecFailed a non-empty identifier.
fn handle_id_fallback(internal_id: i32) -> String {
    format!("sandbox#{}", internal_id)
}

/// POSIX-style single-quoted escape for embedding into `sh -c` scripts
/// from the service layer. Duplicated from `services::exec::shell_escape`
/// so we don't introduce a module cycle for a 10-line helper.
fn shell_escape_service(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

/// Open and lock the destination directory for a source import.
///
/// Publication later uses the retained descriptor at `/dev/fd/9`, not
/// the mutable pathname. The descriptor is validated after it is opened and
/// the directory is made root-only for the duration of the import. A sandbox
/// process can therefore rename or replace the pathname while Git is running,
/// but it cannot redirect the privileged copy or add symlinks beneath the
/// directory being published into.
fn source_target_guard_script(work_dir: &str, target_dir: &str) -> String {
    let work_dir = shell_escape_service(work_dir);
    let target_dir = shell_escape_service(target_dir);
    if work_dir == target_dir {
        return format!(
            "test -d {work_dir} && test ! -L {work_dir} || {{ echo 'sandbox work directory must be a real directory' >&2; exit 64; }}; \
             work_real=$(readlink -f -- {work_dir}); exec 9< {work_dir}; target_fd=/dev/fd/9; \
             target_real=$(readlink -f -- \"$target_fd\"); [ \"$target_real\" = \"$work_real\" ] || {{ echo 'source destination changed while opening it' >&2; exit 64; }}; \
             test -z \"$(find \"$target_fd\" -mindepth 1 -print -quit)\" || {{ echo 'source destination must be empty' >&2; exit 66; }}; \
             if [ \"$(id -u)\" -eq 0 ]; then chown root:root \"$target_fd\"; chmod 700 \"$target_fd\"; fi;"
        );
    }
    format!(
        "test -d {work_dir} && test ! -L {work_dir} || {{ echo 'sandbox work directory must be a real directory' >&2; exit 64; }}; \
         test -d {target_dir} || {{ echo 'source destination must already be a directory' >&2; exit 64; }}; \
         work_real=$(readlink -f -- {work_dir}); exec 9< {target_dir}; target_fd=/dev/fd/9; \
         target_real=$(readlink -f -- \"$target_fd\"); \
         case \"$target_real/\" in \"$work_real/\"*) ;; *) echo 'source destination escapes the sandbox work directory' >&2; exit 64 ;; esac; \
         test -z \"$(find \"$target_fd\" -mindepth 1 -print -quit)\" || {{ echo 'source destination must be an empty directory' >&2; exit 66; }}; \
         if [ \"$(id -u)\" -eq 0 ]; then chown root:root \"$target_fd\"; chmod 700 \"$target_fd\"; fi;"
    )
}

fn source_target_restore_script() -> &'static str {
    "if [ \"$(id -u)\" -eq 0 ] && [ -d /dev/fd/9 ] && id -u temps >/dev/null 2>&1; then chown \"$(id -u temps):$(id -g temps)\" /dev/fd/9; chmod 755 /dev/fd/9; fi"
}

/// Guest privilege escalation is a creation-time, explicit backend opt-in.
fn validate_allow_docker(
    allow_docker: bool,
    backend: Option<temps_ai_agents::sandbox::SandboxBackend>,
) -> Result<(), SandboxError> {
    if allow_docker && backend != Some(temps_ai_agents::sandbox::SandboxBackend::Microsandbox) {
        return Err(SandboxError::Validation {
            message: "allow_docker requires backend=microsandbox; Docker inside Firecracker is not supported yet".into(),
        });
    }
    Ok(())
}

/// Parse the backend a create request asked for. `None` = host default.
fn parse_requested_backend(
    requested: Option<&str>,
) -> Result<Option<temps_ai_agents::sandbox::SandboxBackend>, SandboxError> {
    use temps_ai_agents::sandbox::SandboxBackend;
    match requested {
        None => Ok(None),
        Some("docker") => Ok(Some(SandboxBackend::Docker)),
        Some("firecracker") => Ok(Some(SandboxBackend::Firecracker)),
        Some("microsandbox") => Ok(Some(SandboxBackend::Microsandbox)),
        Some(other) => Err(SandboxError::Validation {
            message: format!(
                "unknown backend '{}' (expected \"docker\", \"firecracker\" or \"microsandbox\")",
                other
            ),
        }),
    }
}

/// Why an explicitly requested backend was rejected on this host. For
/// microsandbox the host probe says exactly what is missing (platform,
/// hypervisor, or runtime) and where to fix it.
fn backend_unavailable_message(backend: temps_ai_agents::sandbox::SandboxBackend) -> String {
    match backend {
        temps_ai_agents::sandbox::SandboxBackend::Microsandbox => {
            let capability = temps_ai_agents::sandbox::microsandbox::microsandbox_capability(
                &temps_ai_agents::sandbox::microsandbox::host_data_dir(),
            );
            match capability.reason {
                Some(reason) => {
                    // Name the command with the server's own data directory:
                    // a bare `temps microsandbox setup` from another shell
                    // can install the runtime somewhere the server never reads.
                    let fix = capability
                        .setup_command
                        .map(|command| format!("; to fix, run `{command}` on the server host"))
                        .unwrap_or_default();
                    format!(
                        "backend 'microsandbox' is not available on this host: {}{} (see {})",
                        reason,
                        fix,
                        temps_ai_agents::sandbox::microsandbox::SETUP_PATH
                    )
                }
                // Ready now but not registered: the server started before
                // the runtime was installed.
                None => "backend 'microsandbox' is not available on this host: the runtime \
                         was installed after the server started; restart temps to enable it"
                    .to_string(),
            }
        }
        temps_ai_agents::sandbox::SandboxBackend::Firecracker => {
            // Registration is decided once, at startup, from the server's own
            // data directory. Name that directory and the command that
            // provisions it: a bare `temps firecracker setup` from a shell
            // without the server's TEMPS_DATA_DIR provisions a different one.
            let data_dir = temps_ai_agents::sandbox::microsandbox::host_data_dir();
            let command = temps_ai_agents::sandbox::microsandbox::command_with_data_dir(
                "temps firecracker setup",
                &data_dir,
            );
            format!(
                "backend 'firecracker' is not available on this host: it is not provisioned \
                 under {} (or the server has not been restarted since setup); run `{}` on \
                 the server host, then restart temps",
                data_dir.join("firecracker").display(),
                command
            )
        }
        temps_ai_agents::sandbox::SandboxBackend::Docker
        | temps_ai_agents::sandbox::SandboxBackend::Local => {
            format!("backend '{}' is not available on this host", backend)
        }
    }
}

fn source_import_staging_dir(
    backend: temps_ai_agents::sandbox::SandboxBackend,
    internal_id: i32,
) -> String {
    let private_root = match backend {
        temps_ai_agents::sandbox::SandboxBackend::Local => "/tmp",
        temps_ai_agents::sandbox::SandboxBackend::Docker => "/run/temps-source-import",
        temps_ai_agents::sandbox::SandboxBackend::Firecracker
        | temps_ai_agents::sandbox::SandboxBackend::Microsandbox => "/root",
    };
    format!("{private_root}/.temps-source-import-{internal_id}")
}

fn source_import_staging_prepare_script(staging_dir: &str) -> String {
    let staging_dir = shell_escape_service(staging_dir);
    let require_hard_limit = if staging_dir.starts_with("/run/temps-source-import/") {
        "grep -q ' /run/temps-source-import ' /proc/self/mountinfo || { echo 'secure source-import staging is unavailable; rebuild this sandbox before importing' >&2; exit 69; }; "
    } else if staging_dir.starts_with("/run/secrets/") {
        "grep -q ' /run/secrets ' /proc/self/mountinfo || { echo 'secure source-import credential storage is unavailable; rebuild this sandbox before importing' >&2; exit 69; }; "
    } else {
        // Local and Firecracker do not currently expose a provider-enforced,
        // size-bounded staging filesystem. Sampling `du` after extraction is
        // not a quota and can exhaust the host/VM disk between samples, so
        // reject the import instead of pretending the soft monitor is safe.
        "echo 'secure source-import staging is unavailable for this sandbox backend' >&2; exit 69; "
    };
    format!(
        "{require_hard_limit}find {staging_dir} -depth -delete 2>/dev/null || true; mkdir -m 700 -p {staging_dir}; \
         test ! -L {staging_dir} || {{ echo 'source staging directory must not be a symbolic link' >&2; exit 64; }};"
    )
}

fn source_import_staging_finalize_script(staging_dir: &str) -> String {
    let staging_dir = shell_escape_service(staging_dir);
    format!(
        "target_fd=/dev/fd/9; test -d \"$target_fd\" || exit 67; \
         cd -P -- \"$target_fd\"; \
         test -z \"$(find . -mindepth 1 -print -quit)\" || {{ echo 'source destination changed during import' >&2; exit 66; }}; \
         cp -a {staging_dir}/. . || {{ find . -mindepth 1 -depth -delete 2>/dev/null || true; exit 67; }}; \
         find {staging_dir} -depth -delete; \
         if id -u temps >/dev/null 2>&1; then \
           chown -R \"$(id -u temps):$(id -g temps)\" . || {{ find . -mindepth 1 -depth -delete 2>/dev/null || true; exit 67; }}; \
           chmod 755 .; \
         fi"
    )
}

/// Execute Git under an in-container deadline while continuously enforcing
/// the import quota. The outer Tokio timeout only drops a future; this inner
/// process is what actually terminates Git and removes a partial checkout.
fn source_import_command_script(target_dir: &str, command: &str) -> String {
    let target_dir = shell_escape_service(target_dir);
    let command = shell_escape_service(command);
    format!(
        "set +e; timeout -k 5 {timeout_secs} sh -c {command} & import_pid=$!; limit_exceeded=0; \
         while kill -0 \"$import_pid\" 2>/dev/null; do \
           entry_count=$(find {target_dir} -mindepth 1 -print 2>/dev/null | head -n {entry_probe} | wc -l | tr -d ' '); \
           byte_count=$(du -sk {target_dir} 2>/dev/null | awk '{{print $1 * 1024}}'); byte_count=${{byte_count:-0}}; \
           if [ \"$entry_count\" -gt {max_entries} ] || [ \"$byte_count\" -gt {max_bytes} ]; then \
             limit_exceeded=1; kill -TERM \"$import_pid\" 2>/dev/null || true; break; \
           fi; sleep 1; \
         done; \
         wait \"$import_pid\"; import_status=$?; set -e; \
         if [ \"$limit_exceeded\" -eq 1 ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import exceeds the 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi; \
         if [ \"$import_status\" -ne 0 ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; exit \"$import_status\"; \
         fi",
        timeout_secs = SOURCE_IMPORT_TIMEOUT.as_secs(),
        entry_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_entries = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

fn git_metadata_cleanup_script(target_dir: &str, strip_git_metadata: bool) -> String {
    if strip_git_metadata {
        format!(
            "; find {git_dir} -depth -delete",
            git_dir = shell_escape_service(&Path::new(target_dir).join(".git").to_string_lossy())
        )
    } else {
        String::new()
    }
}

fn source_import_bounds_script(target_dir: &str) -> String {
    let target_dir = shell_escape_service(target_dir);
    format!(
        "; file_count=$(find {target_dir} -mindepth 1 -print | head -n {file_probe} | wc -l | tr -d ' '); \
         byte_count=$(du -sk {target_dir} | awk '{{print $1 * 1024}}'); \
         if [ \"$file_count\" -gt {max_files} ] || [ \"$byte_count\" -gt {max_bytes} ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import exceeds the 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi",
        file_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_files = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

/// Enforce the quota across the durable workspace plus the staged import.
/// Checking before finalization lets us reject only the new staging tree and
/// preserve files that already belong to the workspace.
fn source_import_aggregate_bounds_script(work_dir: &str, staging_dir: &str) -> String {
    let work_dir = shell_escape_service(work_dir);
    let staging_dir = shell_escape_service(staging_dir);
    format!(
        "; existing_count=$(find {work_dir} -mindepth 1 -print | head -n {entry_probe} | wc -l | tr -d ' '); \
         staged_count=$(find {staging_dir} -mindepth 1 -print | head -n {entry_probe} | wc -l | tr -d ' '); \
         existing_bytes=$(du -sk {work_dir} | awk '{{print $1 * 1024}}'); \
         staged_bytes=$(du -sk {staging_dir} | awk '{{print $1 * 1024}}'); \
         if [ $((existing_count + staged_count)) -gt {max_entries} ] || \
            [ $((existing_bytes + staged_bytes)) -gt {max_bytes} ]; then \
           find {staging_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import would exceed the aggregate 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi",
        entry_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_entries = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::{Job, JobQueue, JobReceiver, QueueError};

    #[tokio::test]
    async fn application_workspace_mutations_serialize_only_the_same_application() {
        let mut locks = HashMap::new();
        let first = application_workspace_mutex(&mut locks, "app-a");
        let same = application_workspace_mutex(&mut locks, "app-a");
        let other = application_workspace_mutex(&mut locks, "app-b");
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other));
        let _held = first.lock_owned().await;
        assert!(same.try_lock_owned().is_err());
        assert!(other.try_lock_owned().is_ok());
    }

    #[test]
    fn release_manifest_pin_wins_over_the_published_beta_fallback() {
        let pinned = "ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let fallback = "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta";
        assert_eq!(pinned_or_fallback(Some(pinned), fallback), pinned);
        assert_eq!(pinned_or_fallback(None, fallback), fallback);
    }

    #[test]
    fn source_build_fallbacks_are_beta_tags_the_allowlist_accepts() {
        for flavor in ["nodejs", "python", "all"] {
            let image = format!("ghcr.io/gotempsh/temps-sandbox-{flavor}:0.3.4-beta");
            assert!(is_managed_application_workspace_image(&image), "{image}");
        }
        if temps_core::release_images::REVISION.is_none() {
            for runtime in ["node", "bun", "python", "rust", "go", "full"] {
                let image = managed_application_workspace_image(runtime)
                    .expect("managed runtime resolves to an image");
                assert!(image.ends_with(":0.3.4-beta"), "{runtime} -> {image}");
            }
        }
    }

    #[test]
    fn application_workspace_runtimes_resolve_to_managed_daemon_images() {
        // A candidate can pass runtime preflight but fail persistence if its
        // pinned tag is absent from the database image CHECK constraint.
        let migration = include_str!("../../../temps-migrations/src/migration/m20260913_000002_managed_daemon_workspace_images_v034.rs");
        for runtime in ["node", "bun", "python", "rust", "go", "full"] {
            let image = managed_application_workspace_image(runtime)
                .expect("managed runtime must resolve to an image");
            if temps_core::release_images::REVISION.is_none() {
                let beta_migration = include_str!("../../../temps-migrations/src/migration/m20261012_000003_managed_daemon_beta_images.rs");
                assert!(
                    migration.contains(&format!("'{image}'"))
                        || (image.ends_with("-beta")
                            && beta_migration.contains(
                                "temps-sandbox-(nodejs|python|all):[0-9]+[.][0-9]+[.][0-9]+-beta"
                            )),
                    "no migration allows {image}"
                );
            } else {
                assert!(is_managed_application_workspace_image(image));
            }
        }
        if temps_core::release_images::REVISION.is_some() {
            return;
        }
        assert_eq!(
            managed_application_workspace_image("node"),
            Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")
        );
        assert_eq!(
            managed_application_workspace_image("bun"),
            Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta")
        );
        assert_eq!(
            managed_application_workspace_image("python"),
            Some("ghcr.io/gotempsh/temps-sandbox-python:0.3.4-beta")
        );
        for runtime in ["rust", "go", "full"] {
            assert_eq!(
                managed_application_workspace_image(runtime),
                Some("ghcr.io/gotempsh/temps-sandbox-all:0.3.4-beta")
            );
        }
        assert_eq!(managed_application_workspace_image("custom"), None);
        assert_eq!(managed_application_workspace_image("unknown"), None);
        assert!(is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0"
        ));
        assert!(is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-node:0.1.0"
        ));
        assert!(is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-all:0.3.0"
        ));
        assert!(is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-all:0.3.1"
        ));
        assert!(is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-all:0.3.2"
        ));
        assert!(is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-all:0.3.3"
        ));
        assert!(is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-all:0.3.4"
        ));
        for flavor in ["nodejs", "python", "all"] {
            assert!(
                is_managed_application_workspace_image(&format!(
                    "ghcr.io/gotempsh/temps-sandbox-{flavor}:0.3.4-beta"
                )),
                "the beta tag published by main builds must stay accepted for {flavor}"
            );
        }
        assert!(!is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-nodejs:9.9.9-beta"
        ));
        assert!(!is_managed_application_workspace_image(
            "ghcr.io/gotempsh/temps-sandbox-nodejs:0.1.0"
        ));
        for flavor in ["nodejs", "python", "all"] {
            let image = format!(
                "ghcr.io/gotempsh/temps-sandbox-{flavor}@sha256:{}",
                "a".repeat(64)
            );
            assert!(is_managed_application_workspace_image(&image));
        }
        for image in [
            format!(
                "ghcr.io/gotempsh/temps-sandbox-bun@sha256:{}",
                "a".repeat(64)
            ),
            format!(
                "ghcr.io/other/temps-sandbox-nodejs@sha256:{}",
                "a".repeat(64)
            ),
            format!(
                "ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:{}",
                "A".repeat(64)
            ),
            "ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:short".into(),
        ] {
            assert!(
                !is_managed_application_workspace_image(&image),
                "accepted {image}"
            );
        }
    }

    #[test]
    fn application_workspace_config_preserves_explicit_image_and_maps_null_runtime() {
        let now = Utc::now();
        let mut row = ai_application_workspaces::Model {
            id: 1,
            application_id: 2,
            sandbox_public_id: None,
            desired_state: "running".to_string(),
            runtime: "python".to_string(),
            image: None,
            cpu_limit: 1.0,
            memory_limit_mb: 1024,
            pids_limit: 128,
            disk_limit_mb: 2048,
            idle_timeout_secs: 900,
            last_error: None,
            created_at: now,
            updated_at: now,
        };

        let mapped = ApplicationWorkspaceConfig::from(&row);
        assert_eq!(
            mapped.image.as_deref(),
            managed_application_workspace_image("python")
        );

        row.image = Some("ghcr.io/gotempsh/temps-sandbox-all:0.2.0".to_string());
        let explicit = ApplicationWorkspaceConfig::from(&row);
        assert_eq!(
            explicit.image.as_deref(),
            Some("ghcr.io/gotempsh/temps-sandbox-all:0.2.0")
        );
    }

    #[test]
    fn scoped_runtime_debug_output_names_variables_but_never_values() {
        let runtime = ScopedRuntime {
            variables: HashMap::from([(
                "POSTGRES_PASSWORD".to_string(),
                "s3cr3t-value".to_string(),
            )]),
            connected_service_ids: vec![4],
            skipped_service_ids: vec![9],
        };
        let shown = format!("{runtime:?}");
        assert!(shown.contains("POSTGRES_PASSWORD"));
        assert!(!shown.contains("s3cr3t-value"), "{shown}");
        assert!(shown.contains("skipped_service_ids"));
    }

    #[test]
    fn sandbox_data_network_names_are_stable_and_scoped() {
        assert_eq!(
            sandbox_data_network_name("sbx_abc123").expect("valid sandbox id"),
            "temps-sandbox-data-abc123"
        );
    }

    #[test]
    fn source_destinations_are_bounded_relative_paths() {
        for valid in ["project", "projects/web", "packages/api-v2"] {
            assert!(validate_source_destination(Some(valid)).is_ok(), "{valid}");
        }
        for invalid in ["", ".", "../project", "projects/../escape", "/workspace"] {
            assert!(
                validate_source_destination(Some(invalid)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn stripped_git_metadata_targets_only_the_imported_repository() {
        let cleanup = git_metadata_cleanup_script("/workspace/projects/web", true);
        assert!(cleanup.contains("/workspace/projects/web/.git"));
        assert!(!cleanup.contains("/workspace/.git "));
        assert!(git_metadata_cleanup_script("/workspace/projects/web", false).is_empty());
    }

    #[test]
    fn requested_backend_accepts_public_backends_only() {
        use temps_ai_agents::sandbox::SandboxBackend;
        assert_eq!(parse_requested_backend(None).unwrap(), None);
        assert_eq!(
            parse_requested_backend(Some("docker")).unwrap(),
            Some(SandboxBackend::Docker)
        );
        assert_eq!(
            parse_requested_backend(Some("firecracker")).unwrap(),
            Some(SandboxBackend::Firecracker)
        );
        assert_eq!(
            parse_requested_backend(Some("microsandbox")).unwrap(),
            Some(SandboxBackend::Microsandbox)
        );
        // "local" is a dev fallback, never a caller choice.
        for rejected in ["local", "libkrun", ""] {
            match parse_requested_backend(Some(rejected)) {
                Err(SandboxError::Validation { message }) => {
                    assert!(message.contains(&format!("'{rejected}'")), "{message}");
                    assert!(message.contains("microsandbox"), "{message}");
                }
                other => panic!("expected validation error for {rejected:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn unavailable_microsandbox_explains_itself() {
        let message =
            backend_unavailable_message(temps_ai_agents::sandbox::SandboxBackend::Microsandbox);
        assert!(
            message.starts_with("backend 'microsandbox' is not available on this host: "),
            "{message}"
        );
        let firecracker =
            backend_unavailable_message(temps_ai_agents::sandbox::SandboxBackend::Firecracker);
        assert!(
            firecracker.starts_with("backend 'firecracker' is not available on this host: "),
            "{firecracker}"
        );
        assert!(
            firecracker.contains("temps firecracker setup"),
            "{firecracker}"
        );
        assert!(firecracker.contains("restart temps"), "{firecracker}");
        assert_eq!(
            backend_unavailable_message(temps_ai_agents::sandbox::SandboxBackend::Docker),
            "backend 'docker' is not available on this host"
        );
    }

    #[test]
    fn microvm_backends_stage_source_imports_under_root_home() {
        assert_eq!(
            source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Microsandbox, 42),
            "/root/.temps-source-import-42"
        );
        assert_eq!(
            source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Firecracker, 42),
            "/root/.temps-source-import-42"
        );
    }

    #[test]
    fn source_imports_use_bounded_staging_and_process_deadlines() {
        let nested_stage =
            source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Docker, 42);
        assert_eq!(
            nested_stage,
            "/run/temps-source-import/.temps-source-import-42"
        );
        let local_stage =
            source_import_staging_dir(temps_ai_agents::sandbox::SandboxBackend::Local, 42);
        assert_eq!(local_stage, "/tmp/.temps-source-import-42");

        let command = source_import_command_script(&nested_stage, "git clone repo target");
        assert!(command.contains("timeout -k 5 120"));
        assert!(
            command.contains("find /run/temps-source-import/.temps-source-import-42 -mindepth 1")
        );
        assert!(command.contains("kill -TERM"));
        assert!(command
            .contains("find /run/temps-source-import/.temps-source-import-42 -depth -delete"));

        let prepare = source_import_staging_prepare_script(&nested_stage);
        assert!(prepare.contains(" /run/temps-source-import "));
        assert!(prepare.contains("rebuild this sandbox before importing"));

        let local_prepare = source_import_staging_prepare_script(&local_stage);
        assert!(local_prepare.contains("unavailable for this sandbox backend"));
        assert!(local_prepare.contains("exit 69"));

        let auth_prepare =
            source_import_staging_prepare_script("/run/secrets/.temps-source-auth-42");
        assert!(auth_prepare.contains(" /run/secrets "));
        assert!(auth_prepare.contains("credential storage is unavailable"));

        let bounds = source_import_bounds_script(&nested_stage);
        assert!(bounds.contains("-mindepth 1"));
        assert!(!bounds.contains("-type f"));
    }

    #[test]
    fn source_import_quota_includes_existing_workspace_content() {
        let bounds = source_import_aggregate_bounds_script(
            "/workspace",
            "/run/temps-source-import/.temps-source-import-7",
        );
        assert!(bounds.contains("existing_count + staged_count"));
        assert!(bounds.contains("existing_bytes + staged_bytes"));
        assert!(bounds.contains("aggregate 5000 entry or 256 MiB"));
    }

    #[test]
    fn source_destination_guard_rejects_non_empty_targets() {
        let guard = source_target_guard_script("/workspace", "/workspace/projects/web");
        assert!(guard.contains("source destination must be an empty directory"));
        assert!(guard.contains("exec 9< /workspace/projects/web"));
        assert!(guard.contains("find \"$target_fd\" -mindepth 1"));
        assert!(guard.contains("chmod 700 \"$target_fd\""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_publish_uses_retained_destination_after_symlink_swap() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("temporary source import root");
        let workspace = temp.path().join("workspace");
        let target = workspace.join("projects/web");
        let original = workspace.join("projects/web-original");
        let outside = temp.path().join("outside");
        let staging = temp.path().join("staging");
        std::fs::create_dir_all(&target).expect("source destination");
        std::fs::create_dir_all(&outside).expect("outside directory");
        std::fs::create_dir_all(&staging).expect("staging directory");
        std::fs::write(staging.join("index.js"), b"safe").expect("staged source");

        let guard = source_target_guard_script(
            workspace.to_str().expect("utf-8 workspace"),
            target.to_str().expect("utf-8 target"),
        );
        let finalize = source_import_staging_finalize_script(
            staging.to_str().expect("utf-8 staging directory"),
        );
        let script = format!(
            "set -eu; {guard} mv {target} {original}; ln -s {outside} {target}; {finalize}",
            target = shell_escape_service(target.to_str().expect("utf-8 target")),
            original = shell_escape_service(original.to_str().expect("utf-8 original")),
            outside = shell_escape_service(outside.to_str().expect("utf-8 outside")),
        );
        let status = Command::new("/bin/sh")
            .args(["-c", &script])
            .status()
            .expect("execute source publication script");

        assert!(status.success());
        assert_eq!(
            std::fs::read(original.join("index.js")).expect("published source"),
            b"safe"
        );
        assert!(!outside.join("index.js").exists());
        assert!(std::fs::symlink_metadata(&target)
            .expect("replacement symlink")
            .file_type()
            .is_symlink());
    }

    #[test]
    fn sandbox_data_network_names_reject_docker_name_injection() {
        for invalid in ["", "../shared", "app id", "app/name"] {
            assert!(matches!(
                sandbox_data_network_name(invalid),
                Err(SandboxError::Validation { .. })
            ));
        }
    }

    #[test]
    fn application_workspace_reuse_requires_server_attestation() {
        let host_work_dir = PathBuf::from("/var/lib/temps/ai-applications/app_example");
        let config = ApplicationWorkspaceConfig::default();
        let now = Utc::now();
        let mut managed = sandboxes::Model {
            id: 7,
            node_id: None,
            public_id: "sbx_deadbeef01234567".to_string(),
            user_id: Some(1),
            agent_run_id: None,
            name: "ai-application:app_example".to_string(),
            status: "running".to_string(),
            image: config.image.clone(),
            work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            timeout_secs: 3600,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": host_work_dir,
            })),
            backend: Some("docker".to_string()),
            created_at: now,
            last_activity_at: now,
            expires_at: now + chrono::Duration::hours(1),
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: Default::default(),
            preview_allowed_cidrs: Vec::new(),
            egress_policy: Default::default(),
            lifecycle: "workspace".to_string(),
            project_id: None,
            source_repo_url: None,
        };

        assert!(config.image.is_some(), "the default image must be attested");

        assert!(application_workspace_identity_is_attested(
            &managed,
            1,
            "app_example",
            &host_work_dir,
        ));

        let old_image = "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2";
        managed.image = Some(old_image.to_string());
        assert!(application_workspace_identity_is_attested(
            &managed,
            1,
            "app_example",
            &host_work_dir,
        ));
        assert!(application_workspace_config_has_drift(&managed, &config));

        managed.user_id = Some(2);
        assert!(!application_workspace_identity_is_attested(
            &managed,
            1,
            "app_example",
            &host_work_dir,
        ));
        managed.user_id = Some(1);
        let wrong_path = PathBuf::from("/var/lib/temps/ai-applications/another-app");
        assert!(!application_workspace_identity_is_attested(
            &managed,
            1,
            "app_example",
            &wrong_path,
        ));

        managed.image = Some("attacker/custom-image:latest".to_string());
        assert!(!application_workspace_identity_is_attested(
            &managed,
            1,
            "app_example",
            &host_work_dir,
        ));
        assert_eq!(
            managed_application_id_from_name("ai-application:app_example"),
            Some("app_example")
        );
        assert_eq!(managed_application_id_from_name("ordinary-sandbox"), None);
    }

    struct NoopJobQueue;

    #[async_trait::async_trait]
    impl JobQueue for NoopJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("preview link tests never subscribe to the job queue")
        }
    }

    fn test_server_config() -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:0".into(),
            database_url: "postgres://test".into(),
            tls_address: None,
            console_address: "127.0.0.1:0".into(),
            console_admin_address: None,
            console_ingress_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: PathBuf::from("/tmp/temps-sandbox-preview-tests"),
            auth_secret: "default-32-byte-key-for-testing!".into(),
            encryption_key: "another-32-byte-key-for-testing!".into(),
            api_base_url: "/api".into(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    fn preview_test_service(
        query_results: Vec<Vec<sandboxes::Model>>,
    ) -> (SandboxService, Arc<DatabaseConnection>) {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(query_results)
                .into_connection(),
        );
        let service = preview_test_service_with_db(db.clone());
        (service, db)
    }

    pub(crate) fn preview_test_service_with_db(db: Arc<DatabaseConnection>) -> SandboxService {
        let platform_config = Arc::new(ConfigService::new(
            Arc::new(test_server_config()),
            db.clone(),
        ));
        let git_provider_manager = Arc::new(GitProviderManager::new(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password("test")),
            Arc::new(NoopJobQueue),
            platform_config.clone(),
        ));
        let registry = Arc::new(StandaloneSandboxRegistry::local_only(Arc::new(
            temps_ai_agents::sandbox::local::LocalSandboxProvider::new(),
        )));
        SandboxService::new(
            db.clone(),
            registry,
            Arc::new(JobTracker::new()),
            platform_config,
            Arc::new(
                temps_core::CookieCrypto::new("default-32-byte-key-for-testing!")
                    .expect("valid test cookie key"),
            ),
            git_provider_manager,
            PathBuf::from("/tmp/temps-sandbox-preview-tests"),
            Arc::new(crate::services::placement::EveryNodeReady),
        )
    }

    #[test]
    fn default_request_is_empty() {
        let r = CreateSandboxRequest::default();
        assert!(r.image.is_none());
        assert!(r.env.is_empty());
        assert!(r.timeout_secs.is_none());
        assert!(r.preview_password.is_none());
    }

    #[test]
    fn allow_docker_requires_explicit_microsandbox_backend() {
        use temps_ai_agents::sandbox::SandboxBackend;
        for backend in [
            None,
            Some(SandboxBackend::Docker),
            Some(SandboxBackend::Firecracker),
        ] {
            assert!(validate_allow_docker(true, backend).is_err());
            assert!(validate_allow_docker(false, backend).is_ok());
        }
        assert!(validate_allow_docker(true, Some(SandboxBackend::Microsandbox)).is_ok());
    }

    #[test]
    fn request_carries_preview_password() {
        // The field is plumbed through the service input DTO so handlers
        // don't need to reach around the service to wire it in.
        let r = CreateSandboxRequest {
            preview_password: Some("hunter2secret".to_string()),
            ..Default::default()
        };
        assert_eq!(r.preview_password.as_deref(), Some("hunter2secret"));
    }

    /// Destroy must actually target the directory create allocated —
    /// nothing removed it before, so every create/destroy cycle left the
    /// full working tree (node_modules, build output, cloned repos) on the
    /// host and the disk filled up with directories no API could reach.
    #[test]
    fn work_dir_to_remove_targets_the_directory_create_allocated() {
        let root = Path::new("/var/lib/temps/sandboxes");
        let id = "sbx_deadbeef01234567";
        assert_eq!(
            work_dir_to_remove(root, id),
            // Same expression create_sandbox uses to build host_work_dir.
            Some(root.join(id))
        );
    }

    /// The guard in front of a recursive delete. A public id is only ever
    /// generated internally, but if a malformed one ever reached here
    /// `data_root.join()` would happily resolve out of the data dir — an
    /// absolute id replaces the root outright.
    #[test]
    fn work_dir_to_remove_refuses_ids_that_escape_the_data_root() {
        let root = Path::new("/var/lib/temps/sandboxes");
        for bad in [
            "../../etc",
            "sbx_../../etc",
            "/etc",
            "sbx_",
            "sbx_short",
            "sbx_zzzzzzzzzzzzzzzz",
            "",
        ] {
            assert_eq!(
                work_dir_to_remove(root, bad),
                None,
                "{:?} must not resolve to a directory we would recursively delete",
                bad
            );
        }
    }

    /// The cross-crate half of the naming agreement.
    ///
    /// `temps-sandbox` decides the container label; `temps-ai-agents` turns a
    /// container name into a volume name. Nothing at the type level forces
    /// those to line up, and the leak this PR fixes was exactly one identity
    /// derived independently in two places. If either side changes its
    /// scheme without the other, this goes red — and it needs no Docker.
    #[test]
    fn sandbox_service_and_provider_agree_on_volume_naming() {
        let public_id = "sbx_a1b2c3d4e5f60718";
        let label = container_label_for(public_id);
        assert_eq!(label, "a1b2c3d4e5f60718", "label is public_id minus sbx_");

        // What the provider will name the volume for a container built from
        // this label, i.e. what `destroy` will try to remove.
        assert_eq!(
            temps_ai_agents::sandbox::home_volume_name_for_label(label),
            "temps-sandbox-home-v2-a1b2c3d4e5f60718"
        );

        // And it must not be a name from the pre-fix namespace, where a
        // stranded volume from another tenant could still be sitting.
        assert!(!temps_ai_agents::sandbox::home_volume_name_for_label(label)
            .trim_start_matches("temps-sandbox-home-")
            .chars()
            .all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn timeout_constants_are_sane() {
        const _: () = assert!(MIN_TIMEOUT_SECS < DEFAULT_TIMEOUT_SECS);
        const _: () = assert!(DEFAULT_TIMEOUT_SECS < MAX_TIMEOUT_SECS);
        assert_eq!(MAX_TIMEOUT_SECS, 86400);
    }

    #[test]
    fn allow_docker_is_reported_from_persisted_metadata() {
        let mut row = agent_run_row(None);
        assert!(!SandboxSummary::from(&row).allow_docker);
        row.metadata = Some(serde_json::json!({"allow_docker": true}));
        assert!(SandboxSummary::from(&row).allow_docker);
        row.metadata = Some(serde_json::json!({"allow_docker": false}));
        assert!(!SandboxSummary::from(&row).allow_docker);
    }

    fn agent_run_row(run_id: Option<i32>) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 7,
            node_id: None,
            public_id: "sbx_deadbeef01234567".into(),
            user_id: Some(1),
            agent_run_id: run_id,
            name: match run_id {
                Some(id) => format!("temps-sandbox-{}", id),
                None => "sbx-7".into(),
            },
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: Default::default(),
            preview_allowed_cidrs: Vec::new(),
            egress_policy: Default::default(),
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        }
    }

    fn protected_preview_row(user_id: i32) -> sandboxes::Model {
        let mut row = agent_run_row(None);
        row.user_id = Some(user_id);
        row.preview_password_hash = Some("argon2-current".into());
        row.preview_password_hint = Some("rent".into());
        row.preview_access = SandboxPreviewAccess::Password;
        row
    }

    fn statements_of(service: SandboxService, db: Arc<DatabaseConnection>) -> String {
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service owns no database references after drop")
            .into_transaction_log();
        format!("{log:?}")
    }

    #[tokio::test]
    async fn set_preview_access_normalises_the_allow_list_and_records_an_event() {
        let row = agent_run_row(None);
        let mut updated = row.clone();
        updated.preview_access = SandboxPreviewAccess::Public;
        updated.preview_allowed_cidrs = vec!["192.0.2.0/24".into(), "2001:db8::1/128".into()];
        let (service, db) = preview_test_service(vec![vec![row], vec![updated.clone()]]);

        let got = service
            .set_preview_access(
                "sbx_deadbeef01234567",
                1,
                SandboxPreviewAccess::Public,
                &["192.0.2.9/24".into(), "2001:db8::1".into()],
            )
            .await
            .expect("owner may change preview access");
        assert_eq!(got.sandbox, updated);
        assert_eq!(got.previous_access, SandboxPreviewAccess::TempsLogin);
        assert!(got.previous_allowed_cidrs.is_empty());

        let statements = statements_of(service, db);
        assert!(statements.contains("192.0.2.0/24"), "log: {statements}");
        assert!(statements.contains("2001:db8::1/128"), "log: {statements}");
        assert!(
            statements.contains("preview_access_changed"),
            "log: {statements}"
        );
    }

    #[tokio::test]
    async fn set_preview_access_refuses_password_mode_without_a_password() {
        let (service, _) = preview_test_service(vec![vec![agent_run_row(None)]]);
        let error = service
            .set_preview_access(
                "sbx_deadbeef01234567",
                1,
                SandboxPreviewAccess::Password,
                &[],
            )
            .await
            .expect_err("password mode needs a password");
        assert!(matches!(error, SandboxError::Validation { .. }));
        let message = error.to_string();
        assert!(message.contains("sbx_deadbeef01234567"), "{message}");
        assert!(
            message.contains("/v1/sandboxes/sbx_deadbeef01234567/preview-password"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn set_preview_access_rejects_a_bad_cidr_before_touching_the_database() {
        let (service, _) = preview_test_service(Vec::new());
        let error = service
            .set_preview_access(
                "sbx_deadbeef01234567",
                1,
                SandboxPreviewAccess::TempsLogin,
                &["10.0.0.0/8".into(), "10.0.0.0/40".into()],
            )
            .await
            .expect_err("an invalid CIDR is a validation error");
        assert!(matches!(error, SandboxError::Validation { .. }));
        assert!(error.to_string().contains("allowed_cidrs[1]"), "{error}");
    }

    #[tokio::test]
    async fn set_preview_access_hides_other_users_sandboxes() {
        let (service, _) = preview_test_service(vec![vec![protected_preview_row(2)]]);
        let error = service
            .set_preview_access("sbx_deadbeef01234567", 1, SandboxPreviewAccess::Public, &[])
            .await
            .expect_err("a non-owner must not change preview access");
        assert!(matches!(error, SandboxError::NotFound { .. }));
    }

    #[tokio::test]
    async fn setting_a_preview_password_selects_password_access() {
        let row = agent_run_row(None);
        let updated = protected_preview_row(1);
        let (service, db) = preview_test_service(vec![vec![row], vec![updated]]);
        service
            .set_preview_password("sbx_deadbeef01234567", 1, "correct horse battery")
            .await
            .expect("owner may set a password");
        let statements = statements_of(service, db);
        assert!(statements.contains("\"password\""), "log: {statements}");
        assert!(
            statements.contains("preview_password_set"),
            "log: {statements}"
        );
    }

    #[tokio::test]
    async fn clearing_the_preview_password_returns_to_temps_login() {
        let row = protected_preview_row(1);
        let mut updated = agent_run_row(None);
        updated.preview_access = SandboxPreviewAccess::TempsLogin;
        let (service, db) = preview_test_service(vec![vec![row], vec![updated]]);
        let access = service
            .clear_preview_password("sbx_deadbeef01234567", 1)
            .await
            .expect("owner may clear the password");
        assert_eq!(access, SandboxPreviewAccess::TempsLogin);
        let statements = statements_of(service, db);
        assert!(statements.contains("\"temps_login\""), "log: {statements}");
        assert!(
            statements.contains("preview_password_cleared"),
            "log: {statements}"
        );
    }

    #[tokio::test]
    async fn preview_share_link_requires_password_access_even_with_a_hash() {
        let mut row = protected_preview_row(1);
        row.preview_access = SandboxPreviewAccess::TempsLogin;
        let (service, _) = preview_test_service(vec![vec![row]]);
        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/",
                Duration::from_secs(60),
            )
            .await
            .expect_err("a share link only exists in password mode");
        assert!(matches!(error, SandboxError::InvalidState { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_uses_fragment_clamps_ttl_and_records_safe_event() {
        let row = protected_preview_row(1);
        let (service, db) = preview_test_service(vec![vec![row]]);

        let before = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock after epoch")
            .as_secs();
        let (url, expires_at) = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/\\evil.example",
                temps_core::PREVIEW_SESSION_GRANT_MAX_TTL * 30,
            )
            .await
            .expect("protected sandbox should mint a link");

        assert!(url.starts_with(
            "http://ws-deadbeef01234567-3000.localho.st:0/__temps/preview/login?grant=1&next=%2F#session_grant="
        ));
        assert!(!url.contains("?session_grant="));
        assert!(expires_at >= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs());
        assert!(expires_at <= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs() + 1);

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service owns no database references after drop")
            .into_transaction_log();
        let statements = format!("{log:?}");
        assert!(statements.contains("sandbox_events"), "log: {statements}");
        assert!(
            statements.contains("preview_share_link_created"),
            "log: {statements}"
        );
        assert!(!statements.contains("session_grant"), "log: {statements}");
    }

    #[tokio::test]
    async fn preview_renewal_does_not_wake_stopped_workspace() {
        let mut row = protected_preview_row(1);
        row.status = "stopped".to_string();
        let (service, _) = preview_test_service(vec![vec![row]]);
        let error = service
            .preview_ready_sandbox("sbx_deadbeef01234567", 1)
            .await
            .expect_err("stopped preview runtime is unavailable");
        assert!(matches!(error, SandboxError::InvalidState { .. }));
    }

    #[tokio::test]
    async fn preview_renewal_hides_unowned_sandbox() {
        let row = protected_preview_row(2);
        let (service, _) = preview_test_service(vec![vec![row]]);
        let error = service
            .preview_ready_sandbox("sbx_deadbeef01234567", 1)
            .await
            .expect_err("another user's preview sandbox must stay hidden");
        assert!(matches!(error, SandboxError::NotFound { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_requires_a_preview_password() {
        let row = agent_run_row(None);
        let (service, _) = preview_test_service(vec![vec![row]]);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/pricing",
                Duration::from_secs(60),
            )
            .await
            .expect_err("open sandbox must not receive a non-expiring share link");

        assert!(matches!(error, SandboxError::InvalidState { .. }));
        assert!(error.to_string().contains("set a preview password first"));
    }

    #[tokio::test]
    async fn preview_share_link_hides_cross_user_sandboxes() {
        let row = protected_preview_row(2);
        let (service, _) = preview_test_service(vec![vec![row]]);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/",
                Duration::from_secs(60),
            )
            .await
            .expect_err("another user's sandbox must stay hidden");

        assert!(matches!(error, SandboxError::NotFound { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_rejects_zero_port_before_database_access() {
        let (service, _) = preview_test_service(Vec::new());

        let error = service
            .preview_share_link("sbx_deadbeef01234567", 1, 0, "/", Duration::from_secs(60))
            .await
            .expect_err("port zero must be rejected");

        assert!(matches!(error, SandboxError::Validation { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_preserves_database_errors() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "preview lookup unavailable".to_string(),
                )])
                .into_connection(),
        );
        let service = preview_test_service_with_db(db);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/",
                Duration::from_secs(60),
            )
            .await
            .expect_err("database failures must not be collapsed into not-found");

        assert!(matches!(error, SandboxError::Database(_)));
        assert!(error.to_string().contains("preview lookup unavailable"));
    }

    /// Lifecycle ops on agent-run rows must be rejected with a typed
    /// error naming both the sandbox and the run — pre-fix, pause/resume/
    /// restart went through the standalone registry, missed the run's
    /// `temps-sandbox-<run_id>` container, and let the DB row drift from
    /// the real container state.
    #[test]
    fn ensure_not_agent_run_rejects_attributed_rows() {
        let row = agent_run_row(Some(42));
        let err = ensure_not_agent_run(&row).expect_err("agent-run row must be rejected");
        match err {
            SandboxError::ManagedByAgentRun { sandbox_id, run_id } => {
                assert_eq!(sandbox_id, "sbx_deadbeef01234567");
                assert_eq!(run_id, 42);
            }
            other => panic!("expected ManagedByAgentRun, got {:?}", other),
        }
    }

    #[test]
    fn ensure_not_agent_run_allows_standalone_rows() {
        let row = agent_run_row(None);
        assert!(ensure_not_agent_run(&row).is_ok());
    }

    /// Terminal decision shared by `destroy_sandbox` (agent-run branch)
    /// and `release_orphaned_agent_run_sandboxes`: every terminal status
    /// allows cleanup, every active status keeps the run's ownership, and
    /// a missing run row (deleted run) counts as terminal so the sandbox
    /// isn't orphaned forever.
    #[test]
    fn run_status_is_terminal_matches_run_lifecycle() {
        for s in TERMINAL_RUN_STATUSES {
            assert!(run_status_is_terminal(Some(s)), "{} must be terminal", s);
        }
        for s in temps_ai_agents::services::run_service::ACTIVE_RUN_STATUSES {
            assert!(
                !run_status_is_terminal(Some(s)),
                "{} must NOT be terminal",
                s
            );
        }
        assert!(
            run_status_is_terminal(None),
            "missing run row must count as terminal so cleanup can proceed"
        );
    }

    #[test]
    fn summary_from_model_copies_fields() {
        let now = Utc::now();
        let m = sandboxes::Model {
            id: 1_000_042,
            node_id: None,
            public_id: "sbx_abc1234567890def".into(),
            user_id: Some(7),
            agent_run_id: Some(42),
            name: "my-sbx".into(),
            status: "running".into(),
            image: Some("node:20".into()),
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: Default::default(),
            preview_allowed_cidrs: Vec::new(),
            egress_policy: Default::default(),
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        };
        let s = SandboxSummary::from(&m);
        assert_eq!(s.public_id, "sbx_abc1234567890def");
        assert_eq!(s.status, "running");
        assert_eq!(s.image.as_deref(), Some("node:20"));
        assert_eq!(s.agent_run_id, Some(42));
    }

    /// `extend_timeout` deliberately pushes `expires_at` past the idle window
    /// and leaves `timeout_secs` alone. `touch` then runs on the very next
    /// exec — the long operation the extension was bought for — so if it
    /// assigned the idle deadline unconditionally it would silently cancel the
    /// extension the caller just paid for. The guard has to live in the SQL,
    /// because `touch` must stay a single statement with no read-modify-write
    /// on the exec hot path.
    #[tokio::test]
    async fn touch_never_moves_the_deadline_backwards() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = preview_test_service_with_db(db.clone());

        service.touch(42, 3600).await;

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service owns no database references after drop")
            .into_transaction_log();
        let statements = format!("{log:?}");

        assert!(
            statements.contains("CASE WHEN"),
            "touch must guard the deadline in SQL so an extendTimeout() is \
             never silently undone; log: {statements}"
        );
        assert!(statements.contains("expires_at"), "log: {statements}");
        assert!(
            statements.contains("last_activity_at"),
            "activity is always recorded, only the deadline is guarded; \
             log: {statements}"
        );
    }
}

/// Service-level tests for the storage-cleanup paths.
///
/// These exist because the unit tests above only cover the *guard*
/// (`work_dir_to_remove`), not the call sites — and the call sites are where
/// the bug was. Deleting the `remove_work_dir` call from `destroy_sandbox`,
/// or from either create-failure arm, used to leave every test green.
///
/// No Docker daemon required: the provider is faked and the database is
/// `MockDatabase`. The work dir is real, under a unique temp directory, so
/// a `remove_dir_all` → `remove_dir` regression (non-recursive) fails here.
#[cfg(test)]
mod storage_cleanup_tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use temps_ai_agents::error::AgentError;
    use temps_ai_agents::sandbox::{
        SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider,
    };

    /// Minimal provider. Each knob corresponds to a failure the service is
    /// supposed to clean up after.
    struct FakeProvider {
        /// `create` fails — the provider-failure arm of `create_sandbox`.
        fail_create: bool,
        /// `exec` returns a non-zero exit — the seeding-failure arm.
        fail_exec: bool,
        /// `destroy` errors — the case where the container may still be
        /// running and we delete the work dir anyway.
        fail_destroy: bool,
        /// `start` errors — the wake-on-access failure path.
        fail_start: bool,
        /// `configure_application_network` errors before compute may start.
        fail_network_config: bool,
        /// `destroy` never answers, like a frozen node; counts attempts.
        hang_destroy: Option<Arc<AtomicUsize>>,
        destroys: AtomicUsize,
        destroy_purge_flags: Arc<Mutex<Vec<bool>>>,
        creates: Arc<AtomicUsize>,
        starts: Arc<AtomicUsize>,
        lifecycle_calls: Arc<Mutex<Vec<&'static str>>>,
        /// Provider truth can differ from the database after an out-of-band
        /// container kill. `start` flips this back to true so the complete
        /// wake-and-retry path can be exercised without Docker.
        alive: Arc<AtomicBool>,
        /// `is_alive` errors instead of answering, like the Docker provider
        /// when it cannot stop a container whose isolation policy is stale.
        /// `start` clears it, as a successful start replaces that state.
        alive_check_fails: Arc<AtomicBool>,
        /// Every egress policy applied, in order.
        egress_calls: Arc<Mutex<Vec<temps_ai_agents::sandbox::SandboxEgressPolicy>>>,
        /// Refuse restricted policies, like a backend without allow-lists.
        fail_egress: bool,
        /// `create` takes this long, like a first image build.
        create_delay: Option<Duration>,
        /// The backends this host registered; `None` claims every one.
        backends: Option<Vec<SandboxBackend>>,
    }

    impl FakeProvider {
        fn new() -> Self {
            Self {
                fail_create: false,
                fail_exec: false,
                fail_destroy: false,
                fail_start: false,
                fail_network_config: false,
                hang_destroy: None,
                destroys: AtomicUsize::new(0),
                destroy_purge_flags: Arc::new(Mutex::new(Vec::new())),
                creates: Arc::new(AtomicUsize::new(0)),
                starts: Arc::new(AtomicUsize::new(0)),
                lifecycle_calls: Arc::new(Mutex::new(Vec::new())),
                alive: Arc::new(AtomicBool::new(true)),
                alive_check_fails: Arc::new(AtomicBool::new(false)),
                egress_calls: Arc::new(Mutex::new(Vec::new())),
                fail_egress: false,
                create_delay: None,
                backends: None,
            }
        }
    }

    fn handle_for(name: &str) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: format!("docker-id-{}", name),
            sandbox_name: format!("temps-sandbox-{}", name),
            work_dir: PathBuf::from("/workspace"),
            backend: SandboxBackend::Docker,
            image: String::new(),
            conditions: Vec::new(),
        }
    }

    #[async_trait]
    impl SandboxProvider for FakeProvider {
        fn supports_backend(&self, backend: SandboxBackend) -> bool {
            self.backends
                .as_ref()
                .is_none_or(|backends| backends.contains(&backend))
        }

        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            if let Some(delay) = self.create_delay {
                tokio::time::sleep(delay).await;
            }
            if self.fail_create {
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "fake".into(),
                    reason: "image pull failed".into(),
                });
            }
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.alive.store(true, Ordering::SeqCst);
            let mut handle = handle_for(config.container_name_override.as_deref().unwrap_or("x"));
            handle.image = config.image.unwrap_or_default();
            Ok(handle)
        }

        async fn exec(
            &self,
            _handle: &SandboxHandle,
            _cmd: Vec<String>,
            _env: HashMap<String, String>,
            _on_output: Option<temps_ai_agents::ai_cli::OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            Ok(SandboxExecResult {
                exit_code: if self.fail_exec { 1 } else { 0 },
                stdout: String::new(),
                stderr: if self.fail_exec {
                    "clone failed".into()
                } else {
                    String::new()
                },
            })
        }

        async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
            if self.alive_check_fails.load(Ordering::SeqCst) {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!(
                        "stop sandbox '{}' after its isolation policy failed validation",
                        handle.sandbox_name
                    ),
                });
            }
            Ok(self.alive.load(Ordering::SeqCst))
        }

        async fn configure_application_network(
            &self,
            handle: &SandboxHandle,
            _network_name: &str,
            _service_containers: &[String],
        ) -> Result<(), AgentError> {
            self.lifecycle_calls
                .lock()
                .expect("lifecycle call mutex")
                .push("configure");
            if self.fail_network_config {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!("network reconciliation failed for {}", handle.sandbox_name),
                });
            }
            Ok(())
        }

        async fn write_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn read_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            Ok(Vec::new())
        }

        async fn write_directory(
            &self,
            _handle: &SandboxHandle,
            _local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn kill_processes(
            &self,
            _handle: &SandboxHandle,
            _pattern: &str,
            _signal: temps_ai_agents::sandbox::KillSignal,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn fence_process_trees(
            &self,
            _handle: &SandboxHandle,
            _patterns: &[&str],
        ) -> Result<(), AgentError> {
            // Restart-recovery fencing is orthogonal to the individual
            // lifecycle failure knobs exercised by this service fake.
            Ok(())
        }

        async fn destroy(
            &self,
            handle: &SandboxHandle,
            purge_volumes: bool,
        ) -> Result<(), AgentError> {
            if let Some(attempts) = &self.hang_destroy {
                attempts.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<()>().await;
            }
            self.destroys.fetch_add(1, Ordering::SeqCst);
            self.destroy_purge_flags
                .lock()
                .expect("destroy purge flags mutex")
                .push(purge_volumes);
            if self.fail_destroy {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!(
                        "daemon unreachable while destroying {}",
                        handle.sandbox_name
                    ),
                });
            }
            Ok(())
        }

        /// The trait's default `start` returns "not supported", so the
        /// wake path needs a real implementation here.
        async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
            self.lifecycle_calls
                .lock()
                .expect("lifecycle call mutex")
                .push("start");
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.fail_start {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!("daemon unreachable while starting {}", handle.sandbox_name),
                });
            }
            self.alive.store(true, Ordering::SeqCst);
            self.alive_check_fails.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn stop(&self, _handle: &SandboxHandle) -> Result<(), AgentError> {
            self.lifecycle_calls
                .lock()
                .expect("lifecycle call mutex")
                .push("stop");
            Ok(())
        }

        async fn set_egress_policy(
            &self,
            handle: &SandboxHandle,
            policy: &temps_ai_agents::sandbox::SandboxEgressPolicy,
        ) -> Result<(), AgentError> {
            if self.fail_egress && policy.is_restricted() {
                return Err(AgentError::SandboxEgressPolicyUnsupported {
                    sandbox: handle.sandbox_name.clone(),
                    backend: "firecracker".into(),
                });
            }
            self.egress_calls
                .lock()
                .expect("egress call mutex")
                .push(policy.clone());
            Ok(())
        }

        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }

        async fn recover_by_name(
            &self,
            container_name: &str,
        ) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(Some(handle_for(container_name)))
        }

        fn name(&self) -> &str {
            "fake"
        }

        async fn is_available(&self) -> bool {
            true
        }

        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "fake:latest".into()))
        }

        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("fake:latest".into())
        }
    }

    /// A `ServerConfig` built by struct literal rather than `ServerConfig::new`,
    /// which has filesystem side effects (creates `~/.temps` and writes an
    /// auth secret + encryption key). A test must not touch the developer's
    /// real data dir.
    fn test_server_config(data_dir: PathBuf) -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:3000".into(),
            database_url: "postgresql://test".into(),
            tls_address: None,
            console_address: "127.0.0.1:3001".into(),
            console_admin_address: None,
            console_ingress_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir,
            auth_secret: "test-auth-secret".into(),
            encryption_key: "0123456789abcdef0123456789abcdef\
                             0123456789abcdef0123456789abcdef"
                .into(),
            api_base_url: "http://127.0.0.1:3000".into(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    struct PanicOnSendJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for PanicOnSendJobQueue {
        async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            panic!("no job should be queued in these tests, got: {job:?}");
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            panic!("subscribe not needed for these tests")
        }
    }

    /// A unique data root per test so concurrent tests can't collide.
    fn unique_data_root(tag: &str) -> PathBuf {
        let uniq = std::process::id() as u64
            + std::sync::atomic::AtomicU64::new(0).fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("temps-sbx-test-{}-{}", tag, uniq))
    }

    fn build_service(
        db: sea_orm::DatabaseConnection,
        provider: FakeProvider,
        data_root: PathBuf,
    ) -> (Arc<SandboxService>, Arc<StandaloneSandboxRegistry>) {
        build_service_shared(Arc::new(db), provider, data_root)
    }

    /// [`build_service`] over a connection the test keeps a handle to, so it
    /// can read the statement log back once the service is dropped.
    fn build_service_shared(
        db: Arc<sea_orm::DatabaseConnection>,
        provider: FakeProvider,
        data_root: PathBuf,
    ) -> (Arc<SandboxService>, Arc<StandaloneSandboxRegistry>) {
        let registry = Arc::new(StandaloneSandboxRegistry::local_only(
            Arc::new(provider) as Arc<dyn SandboxProvider>
        ));
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(test_server_config(data_root.clone())),
            db.clone(),
        ));
        let encryption = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key"),
        );
        let git = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption,
            Arc::new(PanicOnSendJobQueue) as Arc<dyn temps_core::JobQueue>,
            config.clone(),
        ));
        let cookie_crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test cookie key"),
        );
        let service = Arc::new(SandboxService::new(
            db,
            registry.clone(),
            Arc::new(JobTracker::new()),
            config,
            cookie_crypto,
            git,
            data_root,
            Arc::new(crate::services::placement::EveryNodeReady),
        ));
        (service, registry)
    }

    fn row(public_id: &str, agent_run_id: Option<i32>) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 7,
            node_id: None,
            public_id: public_id.to_string(),
            user_id: Some(1),
            agent_run_id,
            name: "sbx-7".into(),
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now + chrono::Duration::seconds(3600),
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: Default::default(),
            preview_allowed_cidrs: Vec::new(),
            egress_policy: Default::default(),
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        }
    }

    /// Seed a work dir with nested, non-empty content — a shallow
    /// `remove_dir` would fail on this, a recursive delete succeeds.
    fn seed_work_dir(data_root: &Path, public_id: &str) -> PathBuf {
        let dir = data_root.join(public_id);
        let nested = dir.join("node_modules").join("pkg");
        std::fs::create_dir_all(&nested).expect("seed work dir");
        std::fs::write(nested.join("index.js"), b"module.exports = {}").expect("seed file");
        dir
    }

    const PUBLIC_ID: &str = "sbx_deadbeef01234567";

    fn attested_application_workspace(
        status: &str,
        application_work_dir: &Path,
    ) -> sandboxes::Model {
        sandboxes::Model {
            name: "ai-application:app_example".to_string(),
            status: status.to_string(),
            lifecycle: "workspace".to_string(),
            work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            image: ApplicationWorkspaceConfig::default().image,
            timeout_secs: ApplicationWorkspaceConfig::default().idle_timeout_secs as i32,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": application_work_dir,
            })),
            ..row(PUBLIC_ID, None)
        }
    }

    #[tokio::test]
    async fn untrusted_application_workspace_is_not_destroyed_or_rebuilt() {
        let data_root = unique_data_root("untrusted-application-workspace");
        std::fs::create_dir_all(&data_root).expect("data root");
        let host_work_dir = data_root.join("app_example");
        let mut forged = attested_application_workspace("running", &host_work_dir);
        forged.metadata = Some(serde_json::json!({
            "managed_application_id": "app_example",
            "managed_host_work_dir": data_root.join("another-app"),
        }));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![forged]])
            .into_connection();
        let provider = FakeProvider::new();
        let creates = provider.creates.clone();
        let purge_flags = provider.destroy_purge_flags.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .get_or_create_application_workspace_with_config(
                1,
                "app_example",
                None,
                host_work_dir,
                ApplicationWorkspaceConfig::default(),
                &[],
            )
            .await
            .expect_err("forged host path must fail closed");
        assert!(matches!(error, SandboxError::InvalidState { .. }));
        assert_eq!(creates.load(Ordering::SeqCst), 0);
        assert!(purge_flags.lock().expect("purge flags").is_empty());
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn managed_image_upgrade_rebuilds_same_workspace_without_purging_home() {
        let data_root = unique_data_root("managed-image-upgrade");
        std::fs::create_dir_all(&data_root).expect("data root");
        let host_work_dir = data_root.join("app_example");
        let mut old = attested_application_workspace("running", &host_work_dir);
        old.image = Some("ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2".into());
        let mut rebuilt = old.clone();
        rebuilt.image = ApplicationWorkspaceConfig::default().image;
        rebuilt.timeout_secs = ApplicationWorkspaceConfig::default().idle_timeout_secs as i32;
        rebuilt.metadata = Some(serde_json::json!({
            "disk_size_mb": ApplicationWorkspaceConfig::default().disk_limit_mb,
            "managed_application_id": "app_example",
            "managed_host_work_dir": host_work_dir,
        }));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![old.clone()]])
            .append_query_results([vec![old]])
            .append_query_results([vec![rebuilt.clone()]])
            .append_query_results([vec![rebuilt.clone()]])
            .append_query_results([vec![rebuilt.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            .append_query_results([vec![rebuilt.clone()]])
            .append_query_results([vec![rebuilt.clone()]])
            .append_query_results([vec![rebuilt]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();
        let provider = FakeProvider::new();
        let creates = provider.creates.clone();
        let purge_flags = provider.destroy_purge_flags.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let result = service
            .get_or_create_application_workspace_with_config(
                1,
                "app_example",
                None,
                host_work_dir,
                ApplicationWorkspaceConfig::default(),
                &[],
            )
            .await
            .expect("managed image change should preserve the workspace identity");
        assert_eq!(result.public_id, PUBLIC_ID);
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        assert_eq!(*purge_flags.lock().expect("purge flags"), vec![false]);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn stopped_application_workspace_reconciles_network_before_start() {
        let data_root = unique_data_root("application-network-before-start");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let stopped = attested_application_workspace("stopped", &application_work_dir);
        let running = attested_application_workspace("running", &application_work_dir);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Existing workspace lookup and the re-read used for network
            // preparation, then the authorized project's empty topology.
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            // Resume re-reads the stopped row.
            .append_query_results([vec![stopped]])
            // Resume update returns the running row.
            .append_query_results([vec![running.clone()]])
            // Resume bookkeeping and Git bootstrap re-resolve the now-running
            // workspace through the same durable identity.
            .append_query_results([vec![running.clone()]])
            .append_query_results([vec![running.clone()]])
            .append_query_results([vec![running]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let provider = FakeProvider::new();
        let lifecycle_calls = provider.lifecycle_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        service
            .get_or_create_application_workspace_with_config(
                1,
                "app_example",
                None,
                application_work_dir,
                ApplicationWorkspaceConfig::default(),
                &[42],
            )
            .await
            .expect("network reconciliation must complete before the stopped workspace starts");

        assert_eq!(
            *lifecycle_calls.lock().expect("lifecycle call mutex"),
            vec!["configure", "start"]
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn failed_network_reconciliation_never_starts_stopped_application_workspace() {
        let data_root = unique_data_root("application-network-failure");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let stopped = attested_application_workspace("stopped", &application_work_dir);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([vec![stopped]])
            .append_query_results([Vec::<project_services::Model>::new()])
            .into_connection();
        let provider = FakeProvider {
            fail_network_config: true,
            ..FakeProvider::new()
        };
        let starts = provider.starts.clone();
        let lifecycle_calls = provider.lifecycle_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .get_or_create_application_workspace_with_config(
                1,
                "app_example",
                None,
                application_work_dir,
                ApplicationWorkspaceConfig::default(),
                &[42],
            )
            .await
            .expect_err("failed network reconciliation must abort workspace preparation");

        assert!(error.to_string().contains("network reconciliation failed"));
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert_eq!(
            *lifecycle_calls.lock().expect("lifecycle call mutex"),
            vec!["configure"]
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[test]
    fn git_bootstrap_trusts_the_workspace_before_probing_the_repository() {
        let trust = APPLICATION_GIT_BOOTSTRAP
            .find("git config --global --add safe.directory")
            .expect("bootstrap must trust the workspace directory");
        let probe = APPLICATION_GIT_BOOTSTRAP
            .find("if ! git rev-parse")
            .expect("bootstrap probes for an existing repository");
        assert!(
            trust < probe,
            "an unowned bind mount must be trusted before git inspects it"
        );
    }

    #[tokio::test]
    async fn application_workspace_fails_when_in_sandbox_git_bootstrap_fails() {
        let data_root = unique_data_root("application-git-bootstrap");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let existing = sandboxes::Model {
            name: "ai-application:app_example".to_string(),
            lifecycle: "workspace".to_string(),
            work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            image: ApplicationWorkspaceConfig::default().image,
            timeout_secs: ApplicationWorkspaceConfig::default().idle_timeout_secs as i32,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": application_work_dir,
            })),
            ..row(PUBLIC_ID, None)
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Existing application sandbox lookup.
            .append_query_results([vec![existing.clone()]])
            // Network preparation re-reads the row and resolves the empty
            // authorized service topology before Git bootstrap may execute.
            .append_query_results([vec![existing.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            // `exec` ownership lookup.
            .append_query_results([vec![existing]])
            // Activity touch performed before provider exec.
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let mut provider = FakeProvider::new();
        provider.fail_exec = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .get_or_create_application_workspace(1, "app_example", None, application_work_dir)
            .await
            .expect_err("a failed Git bootstrap must fail workspace preparation");

        assert!(matches!(error, SandboxError::ExecFailed { .. }));
        assert!(error
            .to_string()
            .contains("initialize persistent Git workspace"));
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn application_workspace_rebuilds_killed_provider_before_git_bootstrap() {
        let data_root = unique_data_root("application-provider-recovery");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let existing = sandboxes::Model {
            name: "ai-application:app_example".to_string(),
            lifecycle: "workspace".to_string(),
            work_dir: temps_ai_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            image: ApplicationWorkspaceConfig::default().image,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": application_work_dir,
            })),
            ..row(PUBLIC_ID, None)
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Existing application sandbox lookup.
            .append_query_results([vec![existing.clone()]])
            // Rebuild lookup and update return the same durable identity.
            .append_query_results([vec![existing.clone()]])
            .append_query_results([vec![existing.clone()]])
            // Rebuild bookkeeping and network preparation re-read the durable
            // row, then resolve no data services for the empty authorized set.
            .append_query_results([vec![existing.clone()]])
            .append_query_results([vec![existing.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            // Git-bootstrap exec ownership lookup sees the rebuilt row.
            .append_query_results([vec![existing]])
            // Activity touch performed before provider exec.
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let creates = provider.creates.clone();
        let starts = provider.starts.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let sandbox = service
            .get_or_create_application_workspace(1, "app_example", None, application_work_dir)
            .await
            .expect("application preparation must wake its stopped provider runtime");

        assert_eq!(sandbox.public_id, PUBLIC_ID);
        assert_eq!(
            sandbox
                .handle
                .as_ref()
                .expect("running application workspace handle")
                .sandbox_name,
            "temps-sandbox-deadbeef01234567"
        );
        assert_eq!(
            creates.load(Ordering::SeqCst),
            1,
            "Git bootstrap must be preceded by exactly one provider rebuild"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A row in the given lifecycle class and status. Used by the
    /// workspace wake tests below.
    fn row_with(public_id: &str, lifecycle: &str, status: &str) -> sandboxes::Model {
        sandboxes::Model {
            status: status.to_string(),
            lifecycle: lifecycle.to_string(),
            ..row(public_id, None)
        }
    }

    // ── Lifecycle class (ADR-036) ────────────────────────────────────────

    #[test]
    fn lifecycle_parses_both_public_values() {
        assert_eq!(
            SandboxLifecycle::parse("ephemeral").expect("ephemeral is valid"),
            SandboxLifecycle::Ephemeral
        );
        assert_eq!(
            SandboxLifecycle::parse("workspace").expect("workspace is valid"),
            SandboxLifecycle::Workspace
        );
    }

    #[test]
    fn lifecycle_rejects_unknown_values_with_a_helpful_message() {
        // Silently falling back to ephemeral would cost the caller their
        // work at the first idle sweep, so a typo must be a 400.
        let err = SandboxLifecycle::parse("workspaces").expect_err("typo must not be accepted");
        let msg = err.to_string();
        assert!(msg.contains("workspaces"), "msg: {}", msg);
        assert!(msg.contains("ephemeral"), "must list valid values: {}", msg);
        assert!(msg.contains("workspace"), "must list valid values: {}", msg);
    }

    #[test]
    fn lifecycle_from_row_treats_unrecognised_values_as_ephemeral() {
        // Forward-compat: a row written by a newer binary and then rolled
        // back must not auto-start containers this binary doesn't
        // understand. Ephemeral is the conservative reading.
        let unknown = row_with(PUBLIC_ID, "hibernating", "stopped");
        assert_eq!(
            SandboxLifecycle::from_row(&unknown),
            SandboxLifecycle::Ephemeral
        );
    }

    #[test]
    fn lifecycle_default_is_ephemeral() {
        // Every existing caller and SDK client omits the field.
        assert_eq!(SandboxLifecycle::default(), SandboxLifecycle::Ephemeral);
        assert_eq!(SandboxLifecycle::default().as_str(), "ephemeral");
    }

    #[test]
    fn idle_deadline_is_now_plus_timeout() {
        let now = Utc::now();
        assert_eq!(idle_deadline(now, 3600), now + chrono::Duration::hours(1));
        // Degenerate but representable: a zero timeout is already expired
        // rather than infinite. The service clamps to >= 60 before this
        // is reached, so this only pins the arithmetic.
        assert_eq!(idle_deadline(now, 0), now);
    }

    #[tokio::test]
    async fn resolve_id_wakes_a_stopped_workspace() {
        let data_root = unique_data_root("wake-workspace");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id → a suspended workspace
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "stopped")]])
            // wake_workspace's status update (RETURNING the running row)
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "running")]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let (row, id) = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect("a stopped workspace must wake, not error");

        assert_eq!(id, 7);
        assert_eq!(
            row.status, "running",
            "the caller must receive the woken row, not the stale stopped one"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_still_rejects_a_stopped_ephemeral_sandbox() {
        // `@vercel/sandbox` consumers rely on a stopped sandbox staying
        // stopped. Waking one under them would restart billing on a
        // container they deliberately shut down.
        let data_root = unique_data_root("no-wake-ephemeral");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let err = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect_err("ephemeral sandboxes must not auto-wake");

        match err {
            SandboxError::InvalidState { state, .. } => assert_eq!(state, "stopped"),
            other => panic!("expected InvalidState, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_passes_through_a_running_workspace_without_starting_it() {
        // A running workspace is the common case and must not pay for the
        // wake path. Proven with a provider whose `start` always fails: if
        // the resolve path touched it, this would error.
        let data_root = unique_data_root("running-workspace");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "running")]])
            .into_connection();

        let provider = FakeProvider {
            fail_start: true,
            ..FakeProvider::new()
        };
        let (service, _) = build_service(db, provider, data_root.clone());
        let (row, _) = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect("a running workspace resolves without a provider start");
        assert_eq!(row.status, "running");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_wakes_workspace_when_row_is_running_but_provider_is_stopped() {
        // Reproduces an application harness hitting its hard deadline: the
        // provider stops the Docker container to kill the orphaned process,
        // but the standalone sandbox row remains `running`. The next Git
        // bootstrap/exec must recover the workspace instead of failing before
        // the harness-level recovery path can run.
        let data_root = unique_data_root("wake-stale-running-workspace");
        let stale_row = row_with(PUBLIC_ID, "workspace", "running");
        let recovered_row = stale_row.clone();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // resolve_id ownership lookup returns the stale DB state.
            .append_query_results([vec![stale_row]])
            // wake_workspace status reconciliation returns a live row.
            .append_query_results([vec![recovered_row]])
            .into_connection();

        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let starts = provider.starts.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let (row, id) = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect("a provider-stopped workspace must wake despite a stale running row");

        assert_eq!(id, 7);
        assert_eq!(row.status, "running");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "provider start must run exactly once"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_wakes_workspace_when_local_liveness_check_fails() {
        // A local Docker liveness check errors when stopping a container
        // whose isolation policy is stale fails. The registry reports that
        // as a missing sandbox for control-plane sandboxes, so the access
        // path still wakes the workspace instead of failing the exec.
        let data_root = unique_data_root("wake-after-liveness-failure");
        let stale_row = row_with(PUBLIC_ID, "workspace", "running");
        let recovered_row = stale_row.clone();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stale_row]])
            .append_query_results([vec![recovered_row]])
            .into_connection();

        let provider = FakeProvider::new();
        provider.alive_check_fails.store(true, Ordering::SeqCst);
        let starts = provider.starts.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let (row, id) = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect("a failed local liveness check must wake the workspace, not fail");

        assert_eq!(id, 7);
        assert_eq!(row.status, "running");
        assert_eq!(starts.load(Ordering::SeqCst), 1, "the wake must start it");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_surfaces_a_failed_wake_instead_of_swallowing_it() {
        // A workspace that silently fails to wake and then reports
        // "not found" on every later call is exactly the dead end a
        // self-hosted user can't debug. The provider error must reach them.
        let data_root = unique_data_root("wake-fails");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "stopped")]])
            .into_connection();

        let provider = FakeProvider {
            fail_start: true,
            ..FakeProvider::new()
        };
        let (service, _) = build_service(db, provider, data_root.clone());
        let err = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect_err("a provider start failure must not be swallowed");

        let msg = err.to_string();
        assert!(
            msg.contains("daemon unreachable"),
            "the provider's reason must reach the caller: {}",
            msg
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── A VM that is not running is not "not found" ──────────────────────

    /// Run `work` against a service on a mock database and return what it
    /// produced together with the SQL the service issued.
    async fn with_statement_log<T, F, Fut>(
        tag: &str,
        results: impl IntoIterator<Item = Vec<sandboxes::Model>>,
        events: usize,
        provider: FakeProvider,
        work: F,
    ) -> (T, String)
    where
        F: FnOnce(Arc<SandboxService>) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let data_root = unique_data_root(tag);
        let mut mock = MockDatabase::new(DatabaseBackend::Postgres).append_query_results(results);
        for _ in 0..events {
            mock = mock.append_query_results([vec![destroyed_event()]]);
        }
        let db = Arc::new(mock.into_connection());
        let (service, registry) = build_service_shared(db.clone(), provider, data_root.clone());
        let output = work(service.clone()).await;
        drop(service);
        drop(registry);
        let log = Arc::try_unwrap(db)
            .expect("nothing holds the database after the service is dropped")
            .into_transaction_log();
        let _ = std::fs::remove_dir_all(&data_root);
        (output, format!("{log:?}"))
    }

    #[tokio::test]
    async fn a_dead_vm_is_a_409_naming_the_backend_and_the_row_is_marked_stopped() {
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let row = row_with(PUBLIC_ID, "ephemeral", "running");
        let (result, sql) = with_statement_log(
            "dead-vm",
            [vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]],
            1,
            provider,
            |service| async move { service.live_handle(&row).await },
        )
        .await;

        let err = result.expect_err("a dead VM has no handle to give");
        match &err {
            SandboxError::VmNotRunning {
                sandbox_id,
                backend,
                reason,
                recovery,
            } => {
                assert_eq!(*recovery, crate::error::VmRecovery::Resume);
                assert_eq!(sandbox_id, PUBLIC_ID);
                assert_eq!(backend.as_deref(), Some("docker"));
                assert_eq!(reason, "its container is stopped");
            }
            other => panic!("expected VmNotRunning, got {other:?}"),
        }
        let problem = temps_core::problemdetails::Problem::from(err);
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
        assert!(sql.contains("UPDATE"), "the row must be reconciled: {sql}");
        assert!(sql.contains("vm_lost"), "the event must be recorded: {sql}");
    }

    /// A liveness check that errors proves nothing about the VM, so the row
    /// is left alone (a Docker stale-policy failure must not stop a sandbox).
    #[tokio::test]
    async fn a_failed_liveness_check_does_not_touch_the_row() {
        let provider = FakeProvider::new();
        provider.alive_check_fails.store(true, Ordering::SeqCst);
        let row = row_with(PUBLIC_ID, "ephemeral", "running");
        let (result, sql) = with_statement_log(
            "liveness-error",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            provider,
            |service| async move { service.live_handle(&row).await },
        )
        .await;

        assert!(
            matches!(
                result,
                Err(SandboxError::VmNotRunning { backend: None, .. })
            ),
            "{:?}",
            result.err()
        );
        assert!(!sql.contains("UPDATE"), "no write expected: {sql}");
    }

    #[tokio::test]
    async fn a_live_vm_gives_its_handle_and_writes_nothing() {
        let row = row_with(PUBLIC_ID, "ephemeral", "running");
        let (result, sql) = with_statement_log(
            "live-vm",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            FakeProvider::new(),
            |service| async move { service.live_handle(&row).await },
        )
        .await;

        assert!(result.is_ok(), "{:?}", result.err());
        assert!(!sql.contains("UPDATE"), "no write expected: {sql}");
    }

    /// A pause or destroy that lands between the liveness probe and the
    /// reconcile wins: the conditional update matches nothing and no
    /// `vm_lost` event is written for a transition that did not happen.
    #[tokio::test]
    async fn marking_a_vm_lost_does_not_override_a_concurrent_status_change() {
        let row = row_with(PUBLIC_ID, "ephemeral", "running");
        let cause = AgentError::SandboxNotRunning {
            run_id: 7,
            sandbox_id: PUBLIC_ID.into(),
            container: "temps-sandbox-deadbeef01234567".into(),
            backend: "firecracker".into(),
            reason: "its firecracker process exited".into(),
        };
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let (changed, sql) = with_statement_log(
            "mark-race",
            [Vec::<sandboxes::Model>::new()],
            0,
            provider,
            |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
        )
        .await;

        assert!(!changed);
        assert!(sql.contains("UPDATE"), "{sql}");
        assert!(!sql.contains("vm_lost"), "no event without a change: {sql}");
    }

    /// Snapshot capture, restart and disk resize take the VM down on purpose
    /// and start it again. A request that sees it down meanwhile must not
    /// rewrite the row to `stopped`, or the sandbox would come back to life
    /// unreachable.
    #[tokio::test]
    async fn a_planned_stop_is_not_a_lost_vm() {
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let row = row_with(PUBLIC_ID, "ephemeral", "running");
        let cause = AgentError::SandboxNotRunning {
            run_id: row.id,
            sandbox_id: PUBLIC_ID.into(),
            container: "temps-sandbox-deadbeef01234567".into(),
            backend: "firecracker".into(),
            reason: "its firecracker process exited".into(),
        };
        let data_root = unique_data_root("planned-stop");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
                .append_query_results([vec![destroyed_event()]])
                .into_connection(),
        );
        let (service, registry) = build_service_shared(db.clone(), provider, data_root.clone());

        let planned = registry.begin_planned_downtime(row.id).await;
        assert!(
            !service.mark_vm_lost(&row, &cause, "access").await,
            "a VM that is down for a planned stop must not be marked lost"
        );
        drop(planned);
        assert!(
            service.mark_vm_lost(&row, &cause, "access").await,
            "once the planned stop is over a down VM is lost again"
        );

        drop(service);
        drop(registry);
        let log = Arc::try_unwrap(db)
            .expect("nothing holds the database after the service is dropped")
            .into_transaction_log();
        let _ = std::fs::remove_dir_all(&data_root);
        let sql = format!("{log:?}");
        assert_eq!(sql.matches("UPDATE").count(), 1, "{sql}");
    }

    /// A verdict reached while the control plane had the VM down is stale by
    /// the time it is acted on if the planned stop has finished and the VM is
    /// back: the claim is only worth something with a fresh look.
    #[tokio::test]
    async fn a_stale_down_verdict_is_not_acted_on_when_the_vm_is_back() {
        let row = row_with(PUBLIC_ID, "ephemeral", "running");
        let cause = AgentError::SandboxNotRunning {
            run_id: row.id,
            sandbox_id: PUBLIC_ID.into(),
            container: "temps-sandbox-deadbeef01234567".into(),
            backend: "firecracker".into(),
            reason: "its firecracker process exited".into(),
        };
        let (changed, sql) = with_statement_log(
            "stale-verdict",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            FakeProvider::new(),
            |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
        )
        .await;

        assert!(!changed);
        assert!(!sql.contains("UPDATE"), "{sql}");
    }

    /// The reconciliation reads running VM rows page by page and covers every
    /// one of them, however small the page.
    #[tokio::test]
    async fn startup_reconciliation_walks_every_page() {
        let mut first = row_with(PUBLIC_ID, "ephemeral", "running");
        first.id = 1;
        let mut second = row_with(PUBLIC_ID, "ephemeral", "running");
        second.id = 2;
        let mut third = row_with(PUBLIC_ID, "ephemeral", "running");
        third.id = 3;
        let data_root = unique_data_root("reconcile-pages");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![first, second], vec![third]])
                .into_connection(),
        );
        let (service, _registry) =
            build_service_shared(db.clone(), FakeProvider::new(), data_root.clone());

        let report = service.reconcile_running_vms_paged(2).await;
        drop(service);
        drop(_registry);
        let log = Arc::try_unwrap(db)
            .expect("nothing holds the database after the service is dropped")
            .into_transaction_log();
        let _ = std::fs::remove_dir_all(&data_root);

        assert_eq!(report.checked, 3);
        assert_eq!(report.alive, 3);
        let sql = format!("{log:?}");
        assert_eq!(sql.matches("LIMIT").count(), 2, "one query per page: {sql}");
        assert!(
            sql.contains("Int(Some(2))"),
            "second page starts after the last id of the first: {sql}"
        );
    }

    #[tokio::test]
    async fn only_running_rows_are_marked_lost() {
        let row = row_with(PUBLIC_ID, "ephemeral", "stopped");
        let cause = AgentError::SandboxNotFound {
            run_id: 7,
            sandbox: "temps-sandbox-deadbeef01234567".into(),
        };
        let (changed, sql) = with_statement_log(
            "mark-stopped-row",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            FakeProvider::new(),
            |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
        )
        .await;

        assert!(!changed);
        assert!(sql.contains("[]"), "no statement expected: {sql}");
    }

    #[test]
    fn startup_reconciliation_only_judges_vmm_backends() {
        let with_backend = |backend: Option<&str>| sandboxes::Model {
            backend: backend.map(str::to_string),
            ..row(PUBLIC_ID, None)
        };
        assert!(reconcilable_at_startup(&with_backend(Some("firecracker"))));
        assert!(reconcilable_at_startup(&with_backend(Some("microsandbox"))));
        // dockerd may still be restarting these after a reboot.
        assert!(!reconcilable_at_startup(&with_backend(Some("docker"))));
        assert!(!reconcilable_at_startup(&with_backend(None)));
    }

    /// A managed application workspace reports a lost runtime as the derived
    /// status `recovering`; stopping its row would hide that from its own
    /// recovery and the data-network reconciliation.
    #[tokio::test]
    async fn managed_application_workspaces_are_never_marked_lost() {
        let row = sandboxes::Model {
            name: "ai-application:app_example".to_string(),
            lifecycle: "workspace".to_string(),
            ..row(PUBLIC_ID, None)
        };
        let cause = AgentError::SandboxNotFound {
            run_id: 7,
            sandbox: "temps-sandbox-deadbeef01234567".into(),
        };
        let (changed, sql) = with_statement_log(
            "mark-app-workspace",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            FakeProvider::new(),
            |service| async move { service.mark_vm_lost(&row, &cause, "access").await },
        )
        .await;

        assert!(!changed);
        assert!(!sql.contains("UPDATE"), "{sql}");
    }

    /// The "VM not running" error says to run `resume`; for a row that still
    /// says `running` that must boot the VM, not answer "already running".
    #[tokio::test]
    async fn resume_boots_a_running_row_whose_vm_is_down() {
        let data_root = unique_data_root("resume-dead-running");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id: the stale row.
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
            // mark_vm_lost: conditional running -> stopped update.
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
            // its `vm_lost` event.
            .append_query_results([vec![destroyed_event()]])
            // resume: stopped -> running update.
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
            // its `resumed` event.
            .append_query_results([vec![destroyed_event()]])
            .into_connection();
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let starts = provider.starts.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let row = service
            .resume_sandbox(PUBLIC_ID, 1)
            .await
            .expect("resume must boot a dead VM behind a running row");

        assert_eq!(row.status, "running");
        assert_eq!(starts.load(Ordering::SeqCst), 1, "the VM must be started");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resume_of_a_running_row_with_a_live_vm_stays_a_no_op() {
        let data_root = unique_data_root("resume-live-running");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
            .into_connection();
        let provider = FakeProvider::new();
        let starts = provider.starts.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let row = service
            .resume_sandbox(PUBLIC_ID, 1)
            .await
            .expect("idempotent on a running sandbox");

        assert_eq!(row.status, "running");
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn startup_reconciliation_stops_rows_whose_vm_is_down() {
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let rows = vec![row_with(PUBLIC_ID, "ephemeral", "running")];
        let (report, sql) = with_statement_log(
            "reconcile-dead",
            [vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]],
            1,
            provider,
            |service| async move { service.reconcile_running_vms(rows).await },
        )
        .await;

        assert_eq!(
            report,
            VmReconcileReport {
                checked: 1,
                alive: 0,
                marked_stopped: vec![PUBLIC_ID.to_string()],
                marked_stopped_total: 1,
                not_marked: 0,
                unverified: 0,
            }
        );
        assert!(sql.contains("vm_lost"), "{sql}");
    }

    /// A resume that lands after the probe moves `last_activity_at`; the
    /// verdict about the old row must not stop the sandbox that came back.
    #[tokio::test]
    async fn a_stale_verdict_is_pinned_to_the_activity_timestamp_it_was_read_at() {
        let row = row_with(PUBLIC_ID, "ephemeral", "running");
        let cause = AgentError::SandboxNotRunning {
            run_id: 7,
            sandbox_id: PUBLIC_ID.into(),
            container: "temps-sandbox-deadbeef01234567".into(),
            backend: "firecracker".into(),
            reason: "its firecracker process exited".into(),
        };
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let (_, sql) = with_statement_log(
            "mark-pinned",
            [Vec::<sandboxes::Model>::new()],
            0,
            provider,
            |service| async move { service.mark_vm_lost(&row, &cause, "startup").await },
        )
        .await;

        // The statement log is the Debug form of the transaction, so the
        // quotes around identifiers are escaped.
        assert!(sql.contains(r#"last_activity_at\" = $4"#), "{sql}");
        assert!(sql.contains("ChronoDateTimeUtc"), "{sql}");
        assert!(sql.contains(r#"String(Some("running"))"#), "{sql}");
    }

    /// A down VM whose row is not rewritten (here a managed application
    /// workspace) is counted, so the startup log cannot claim every VM is
    /// alive.
    #[tokio::test]
    async fn startup_reconciliation_counts_down_rows_it_did_not_rewrite() {
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let mut managed = row_with(PUBLIC_ID, "ephemeral", "running");
        managed.name = format!("{APPLICATION_WORKSPACE_NAME_PREFIX}app_0123456789abcdef");
        let (report, sql) = with_statement_log(
            "reconcile-managed",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            provider,
            |service| async move { service.reconcile_running_vms(vec![managed]).await },
        )
        .await;

        assert_eq!(report.checked, 1);
        assert_eq!(report.alive, 0);
        assert_eq!(report.not_marked, 1);
        assert!(report.marked_stopped.is_empty());
        assert!(!report.all_alive());
        assert!(!sql.contains("UPDATE"), "{sql}");
    }

    /// A restart that took down many VMs still yields a small report: every
    /// row is moved and counted, but only a bounded sample of ids is kept.
    #[tokio::test]
    async fn the_startup_report_names_a_bounded_sample_of_ids() {
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let total = VM_RECONCILE_REPORTED_IDS + 10;
        let rows: Vec<sandboxes::Model> = (0..total)
            .map(|index| {
                let mut row = row_with(PUBLIC_ID, "ephemeral", "running");
                row.id = i32::try_from(index).unwrap_or(i32::MAX) + 1;
                row
            })
            .collect();
        // Each move is an UPDATE then a `vm_lost` event insert, so the mock's
        // results alternate row, event.
        let data_root = unique_data_root("reconcile-many");
        let mut mock = MockDatabase::new(DatabaseBackend::Postgres);
        for _ in 0..total {
            mock = mock
                .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
                .append_query_results([vec![destroyed_event()]]);
        }
        let (service, _registry) = build_service_shared(
            Arc::new(mock.into_connection()),
            provider,
            data_root.clone(),
        );
        let report = service.reconcile_running_vms(rows).await;
        let _ = std::fs::remove_dir_all(&data_root);

        assert_eq!(report.checked, total);
        assert_eq!(report.marked_stopped_total, total);
        assert_eq!(report.marked_stopped.len(), VM_RECONCILE_REPORTED_IDS);
        assert_eq!(report.not_marked, 0);
    }

    #[tokio::test]
    async fn startup_reconciliation_leaves_live_vms_alone() {
        let rows = vec![row_with(PUBLIC_ID, "ephemeral", "running")];
        let (report, sql) = with_statement_log(
            "reconcile-alive",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            FakeProvider::new(),
            |service| async move { service.reconcile_running_vms(rows).await },
        )
        .await;

        assert_eq!(report.checked, 1);
        assert_eq!(report.alive, 1);
        assert!(report.marked_stopped.is_empty());
        assert!(!sql.contains("UPDATE"), "{sql}");
    }

    /// When the liveness check cannot say either way the row stays as it is,
    /// and the report counts it so the startup log can say so.
    #[tokio::test]
    async fn startup_reconciliation_does_not_guess_when_the_check_fails() {
        let provider = FakeProvider::new();
        provider.alive_check_fails.store(true, Ordering::SeqCst);
        let rows = vec![row_with(PUBLIC_ID, "ephemeral", "running")];
        let (report, sql) = with_statement_log(
            "reconcile-unknown",
            Vec::<Vec<sandboxes::Model>>::new(),
            0,
            provider,
            |service| async move { service.reconcile_running_vms(rows).await },
        )
        .await;

        assert_eq!(report.checked, 1);
        assert_eq!(report.unverified, 1);
        assert!(report.marked_stopped.is_empty());
        assert!(!sql.contains("UPDATE"), "{sql}");
    }

    /// An id with no row at all is still a plain 404: the 409 is only for
    /// sandboxes that exist.
    #[tokio::test]
    async fn an_id_with_no_row_is_still_not_found() {
        let (result, _) = with_statement_log(
            "no-row",
            [Vec::<sandboxes::Model>::new()],
            0,
            FakeProvider::new(),
            |service| async move { service.resolve_id(PUBLIC_ID, 1).await },
        )
        .await;

        let err = result.expect_err("no such sandbox");
        assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
        assert_eq!(
            temps_core::problemdetails::Problem::from(err).status_code,
            axum::http::StatusCode::NOT_FOUND
        );
    }

    /// An ephemeral sandbox the reconciliation stopped tells the caller how
    /// to continue instead of a bare "stopped".
    #[tokio::test]
    async fn a_stopped_ephemeral_sandbox_says_how_to_resume_it() {
        let (result, _) = with_statement_log(
            "stopped-hint",
            [vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]],
            0,
            FakeProvider::new(),
            |service| async move { service.resolve_id(PUBLIC_ID, 1).await },
        )
        .await;

        let message = result.expect_err("stopped ephemeral").to_string();
        assert!(message.contains("stopped"), "{message}");
        assert!(
            message.contains(&format!("sandbox resume {PUBLIC_ID}")),
            "{message}"
        );
    }

    // ── Workspace-from-project (ADR-036 §5) ──────────────────────────────

    fn project_row(git_url: Option<&str>) -> projects::Model {
        let now = Utc::now();
        projects::Model {
            id: 12,
            name: "acme-api".into(),
            repo_name: "api".into(),
            repo_owner: "acme".into(),
            directory: String::new(),
            pull_only_root_directory: false,
            main_branch: "main".into(),
            preset: temps_entities::preset::Preset::NextJs,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "acme-api".into(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: true,
            git_url: git_url.map(str::to_string),
            git_provider_connection_id: Some(3),
            attack_mode: false,
            shield_mode: "log".to_string(),
            ai_alert_summaries_enabled: None,
            ai_api_traffic_summary_enabled: None,
            allow_alternate_sources: None,
            hosting: Default::default(),
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            error_source_context_enabled: false,
            vulnerability_scanning_enabled: false,
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 0,
            preview_envs_wake_timeout_seconds: 0,
            source_type: Default::default(),
            project_type: temps_entities::types::ProjectType::Server,
            template_slug: None,
            service_template: None,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            github_webhook_id: None,
            github_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: false,
            serves_own_temps_api: false,
            image_retention_hours: None,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
            cloud_telemetry_fidelity:
                temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity::Metered,
            cloud_telemetry_write_mode:
                temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode::Local,
            cloud_analytics_write_mode:
                temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode::Local,
            instantiation_token: None,
            cloud_telemetry_attribute_allowlist: Vec::new(),
        }
    }

    #[tokio::test]
    async fn source_from_project_uses_the_projects_repo_branch_and_connection() {
        let data_root = unique_data_root("project-source-ok");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_row(Some("https://github.com/acme/api.git"))]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let source = service
            .source_from_project(12)
            .await
            .expect("a connected project yields a git source");

        match source.expect("a Git project has a seed source") {
            SandboxSource::Git {
                url,
                revision,
                depth,
                git_connection_id,
                username,
                password,
                destination,
                strip_git_metadata,
            } => {
                assert_eq!(url, "https://github.com/acme/api.git");
                assert_eq!(revision.as_deref(), Some("main"));
                assert!(
                    depth.is_none(),
                    "a workspace gets full history — you cannot rebase in a shallow clone"
                );
                assert_eq!(git_connection_id, Some(3));
                assert!(destination.is_none());
                assert!(!strip_git_metadata);
                assert!(
                    username.is_none() && password.is_none(),
                    "credentials must resolve server-side from the connection, \
                     never be materialised here"
                );
            }
            other => panic!("expected a git source, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn source_from_project_errors_when_the_project_has_no_repo() {
        let data_root = unique_data_root("project-no-repo");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_row(None)]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let err = service
            .source_from_project(12)
            .await
            .expect_err("a project with no git_url cannot seed a workspace");

        match err {
            SandboxError::ProjectHasNoRepo { project_id, name } => {
                assert_eq!(project_id, 12);
                assert_eq!(name, "acme-api");
            }
            other => panic!("expected ProjectHasNoRepo, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn source_from_project_allows_an_empty_workspace_for_manual_projects() {
        let data_root = unique_data_root("manual-project-no-repo");
        let mut manual_project = project_row(None);
        manual_project.source_type = temps_entities::source_type::SourceType::Manual;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![manual_project]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let source = service
            .source_from_project(12)
            .await
            .expect("a manual project may start with an empty workspace");

        assert!(source.is_none());
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn source_from_project_errors_when_the_project_is_missing() {
        let data_root = unique_data_root("project-missing");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let err = service
            .source_from_project(999)
            .await
            .expect_err("a missing project is a 404, not a silent empty workspace");

        assert!(matches!(
            err,
            SandboxError::ProjectNotFound { project_id: 999 }
        ));
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── Resolved-source validation ───────────────────────────────────────

    #[test]
    fn all_embedded_userinfo_is_rejected() {
        assert!(url_has_embedded_credentials(
            "https://user:secret@github.com/org/repo.git"
        ));
        // A bare user can itself be a token or sensitive account identifier.
        assert!(url_has_embedded_credentials(
            "https://user@github.com/org/repo.git"
        ));
        assert!(!url_has_embedded_credentials(
            "https://github.com/org/repo.git"
        ));
        // An `@` in the *path* must not be mistaken for userinfo, or every
        // pinned-ref URL would be rejected as leaking a password.
        assert!(!url_has_embedded_credentials(
            "https://github.com/org/repo@v1.git"
        ));
        assert!(!url_has_embedded_credentials("git@github.com:org/repo.git"));
        // A `:` and `@` in the query or fragment are not userinfo either —
        // userinfo always precedes the first `/`, `?` or `#`. Splitting on
        // `/` alone read this as `user:password` and rejected a valid URL.
        assert!(!url_has_embedded_credentials(
            "https://github.com/org/repo.git?ref=a:b@c"
        ));
        assert!(!url_has_embedded_credentials("https://host?ref=a:b@c"));
        assert!(!url_has_embedded_credentials("https://host#frag=a:b@c"));
        // ...but real userinfo before a query is still caught.
        assert!(url_has_embedded_credentials(
            "https://user:pw@github.com/org/repo.git?ref=main"
        ));
    }

    #[test]
    fn credentials_only_enter_temps_managed_sandbox_images() {
        let mut managed = handle_for("managed");
        managed.image = "ghcr.io/gotempsh/temps-sandbox-node:v1".to_string();
        assert!(sandbox_image_is_trusted_for_credentials(&managed));

        let mut custom = handle_for("custom");
        custom.image = "registry.example/customer/image:latest".to_string();
        assert!(!sandbox_image_is_trusted_for_credentials(&custom));

        let mut local = handle_for("local");
        local.backend = SandboxBackend::Local;
        local.image = managed.image;
        assert!(!sandbox_image_is_trusted_for_credentials(&local));
    }

    #[test]
    fn privileged_imports_clear_image_startup_hooks() {
        let mut environment = HashMap::from([
            ("LD_PRELOAD".to_string(), "/workspace/steal.so".to_string()),
            ("BASH_ENV".to_string(), "/workspace/startup.sh".to_string()),
            ("GIT_PASS".to_string(), "ephemeral".to_string()),
        ]);

        sanitize_privileged_import_environment(&mut environment);

        assert_eq!(environment.get("LD_PRELOAD").map(String::as_str), Some(""));
        assert_eq!(environment.get("BASH_ENV").map(String::as_str), Some(""));
        assert_eq!(
            environment.get("GIT_PASS").map(String::as_str),
            Some("ephemeral")
        );
        for name in ["HOME", "CURL_HOME", "XDG_CONFIG_HOME"] {
            assert_eq!(
                environment.get(name).map(String::as_str),
                Some("/var/empty/temps-source-import")
            );
        }
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "GIT_CONFIG_PARAMETERS",
            "GIT_SSL_CAINFO",
        ] {
            assert_eq!(environment.get(name).map(String::as_str), Some(""));
        }
        assert_eq!(environment.get("NO_PROXY").map(String::as_str), Some("*"));
        assert_eq!(environment.get("no_proxy").map(String::as_str), Some("*"));
        assert_eq!(
            environment.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            environment.get("GIT_SSL_NO_VERIFY").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn source_network_pin_formats_curl_resolve_values() {
        let ipv4 = SourceNetworkPin {
            host: "git.example".to_string(),
            port: 443,
            address: "203.0.113.8".parse().expect("IPv4 fixture"),
        };
        assert_eq!(ipv4.curl_resolve_value(), "git.example:443:203.0.113.8");

        let ipv6 = SourceNetworkPin {
            host: "git.example".to_string(),
            port: 443,
            address: "2001:db8::8".parse().expect("IPv6 fixture"),
        };
        assert_eq!(ipv6.curl_resolve_value(), "git.example:443:[2001:db8::8]");
    }

    #[test]
    fn tarball_import_disables_image_curl_configuration_before_other_flags() {
        let command = tarball_extract_command(
            "https://downloads.example/source.tar.gz",
            "/tmp/staging",
            None,
        );

        assert!(
            command.contains(" curl -q "),
            "unexpected command: {command}"
        );
        assert!(command.starts_with("env -i "));
        assert!(command.contains("--noproxy '*' --proxy ''"));
        assert!(command.contains("--max-redirs 0"));
        assert!(command.contains("| env -i "));
    }

    #[test]
    fn git_import_uses_an_allowlisted_environment_without_secret_values() {
        let environment = clean_git_environment(
            "/root/.temps-source-auth-7",
            Some("/root/.temps-source-auth-7/askpass.sh"),
        );

        assert!(environment.starts_with("env -i "));
        assert!(environment.contains("GIT_CONFIG_COUNT=0"));
        assert!(environment.contains("GIT_SSL_NO_VERIFY=false"));
        assert!(environment.contains("GIT_ASKPASS="));
        assert!(!environment.contains("GIT_PASS="));
        assert!(!environment.contains("GIT_USER="));
        assert!(!environment.contains("GIT_TRACE"));
    }

    fn git_provider(
        provider_type: &str,
        base_url: Option<&str>,
    ) -> temps_entities::git_providers::Model {
        let now = Utc::now();
        temps_entities::git_providers::Model {
            id: 7,
            name: "fixture".to_string(),
            provider_type: provider_type.to_string(),
            base_url: base_url.map(str::to_string),
            api_url: None,
            auth_method: "pat".to_string(),
            auth_config: serde_json::json!({}),
            webhook_secret: None,
            is_active: true,
            is_default: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn stored_git_credentials_are_bound_to_the_provider_origin() {
        let github = git_provider("github", None);
        assert!(
            validate_connection_clone_origin("https://github.com/acme/repo.git", &github).is_ok()
        );
        assert!(validate_connection_clone_origin(
            "https://credentials.example/acme/repo.git",
            &github
        )
        .is_err());

        let enterprise = git_provider("github", Some("https://git.example.test:8443"));
        assert!(validate_connection_clone_origin(
            "https://git.example.test:8443/acme/repo.git",
            &enterprise
        )
        .is_ok());
        assert!(validate_connection_clone_origin(
            "https://git.example.test/acme/repo.git",
            &enterprise
        )
        .is_err());
    }

    #[tokio::test]
    async fn resolved_source_rejects_embedded_credentials() {
        // This is the project-derived path's guard: a legacy `projects.git_url`
        // carrying a password would otherwise be persisted to
        // `source_repo_url` and echoed back out of the API.
        let source = SandboxSource::Git {
            url: "https://user:secret@github.com/org/repo.git".into(),
            revision: None,
            depth: None,
            username: None,
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        let err = validate_resolved_source(&source)
            .await
            .expect_err("credentials in the url must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("embedded credentials"), "msg: {msg}");
        // The message must not echo the secret back at the caller.
        assert!(!msg.contains("secret"), "error leaked the password: {msg}");
    }

    #[tokio::test]
    async fn resolved_source_rejects_loopback_and_metadata_hosts() {
        for url in [
            "http://127.0.0.1/repo.git",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/repo.git",
        ] {
            let source = SandboxSource::Git {
                url: url.into(),
                revision: None,
                depth: None,
                username: None,
                password: None,
                git_connection_id: None,
                destination: None,
                strip_git_metadata: false,
            };
            assert!(
                validate_resolved_source(&source).await.is_err(),
                "{url} must be rejected as an SSRF target"
            );
        }
    }

    #[tokio::test]
    async fn resolved_source_rejects_non_http_schemes() {
        let source = SandboxSource::Tarball {
            url: "file:///etc/passwd".into(),
        };
        assert!(validate_resolved_source(&source).await.is_err());
    }

    #[tokio::test]
    async fn destroy_sandbox_removes_the_work_dir() {
        let data_root = unique_data_root("destroy-ok");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // record_event insert (RETURNING)
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            // mark_destroyed update (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        service
            .destroy_sandbox(PUBLIC_ID, 1)
            .await
            .expect("destroy should succeed");

        assert!(
            !dir.exists(),
            "destroy must remove the work dir {} — nothing else ever will, \
             so leaving it is a permanent leak",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The documented tradeoff: with no background sweep, skipping cleanup
    /// when the provider destroy fails would leak the directory forever, so
    /// we delete regardless. If someone reintroduces a `container_gone`
    /// guard around the call, this goes red.
    #[tokio::test]
    async fn destroy_sandbox_removes_the_work_dir_even_when_provider_destroy_fails() {
        let data_root = unique_data_root("destroy-fail");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_destroy = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        // The user must still be able to delete a sandbox when Docker is
        // unhappy — the call succeeds and the row is marked destroyed.
        service
            .destroy_sandbox(PUBLIC_ID, 1)
            .await
            .expect("destroy must succeed even when the provider fails");

        assert!(
            !dir.exists(),
            "work dir {} must be removed even when the provider destroy \
             failed — there is no sweeper to reclaim it later",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A node that accepts connections but never answers must not hold an
    /// eviction for the provider's lifecycle timeout: the first destroy
    /// gives up at the gate's deadline, the rest skip the container call,
    /// and every row is still marked destroyed.
    #[tokio::test]
    async fn eviction_gives_up_on_a_node_that_never_answers() {
        let data_root = unique_data_root("evict-hang");
        let event = |id| sandbox_events::Model {
            id,
            sandbox_id: 7,
            event_type: "destroyed".into(),
            detail: None,
            created_at: Utc::now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![event(1)]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![event(2)]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut provider = FakeProvider::new();
        provider.hang_destroy = Some(attempts.clone());
        let (service, _) = build_service(db, provider, data_root.clone());
        let gate = EvictionGate::new(std::time::Duration::from_millis(50));

        let started = std::time::Instant::now();
        let first = service
            .destroy_row(row(PUBLIC_ID, None), true, DestroyMode::Evict(&gate))
            .await
            .expect("the row is marked destroyed even though the node hung");
        assert!(gate.is_down(), "a timeout marks the node down");
        let second = service
            .destroy_row(row(PUBLIC_ID, None), true, DestroyMode::Evict(&gate))
            .await
            .expect("later rows are still marked destroyed");

        // Neither container is confirmed gone, and the caller is told so.
        assert!(
            matches!(&first, ContainerOutcome::Unconfirmed(r) if r.contains("did not answer")),
            "{first:?}"
        );
        assert!(
            matches!(&second, ContainerOutcome::Unconfirmed(r) if r.contains("skipped")),
            "{second:?}"
        );

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "once the node is down, later destroys skip the container call"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A node that answered once is never written off because a later call
    /// was slow or unreachable; one that never answered is.
    #[test]
    fn eviction_gate_writes_off_only_a_node_that_never_answered() {
        let silent = EvictionGate::new(std::time::Duration::from_secs(1));
        silent.unanswered();
        assert!(silent.is_down());

        let slow = EvictionGate::new(std::time::Duration::from_secs(1));
        slow.answered();
        slow.unanswered();
        assert!(!slow.is_down(), "one slow reply must not skip the rest");
    }

    /// A node that answered at first and then froze part-way through an
    /// eviction is written off after one round of unanswered calls, not
    /// waited on for every remaining sandbox.
    #[test]
    fn eviction_gate_writes_off_a_node_that_stops_answering() {
        let gate = EvictionGate::new(std::time::Duration::from_secs(1));
        gate.answered();
        for _ in 1..EVICTION_UNANSWERED_LIMIT {
            gate.unanswered();
        }
        assert!(!gate.is_down(), "fewer than a round of failures");
        gate.answered();
        for _ in 1..EVICTION_UNANSWERED_LIMIT {
            gate.unanswered();
        }
        assert!(!gate.is_down(), "an answer resets the count");
        gate.unanswered();
        assert!(gate.is_down(), "a full round in a row writes it off");
    }

    fn worker_row(node_id: i32) -> sandboxes::Model {
        sandboxes::Model {
            node_id: Some(node_id),
            ..row(PUBLIC_ID, None)
        }
    }

    fn destroyed_event() -> sandbox_events::Model {
        sandbox_events::Model {
            id: 1,
            sandbox_id: 7,
            event_type: "destroyed".into(),
            detail: None,
            created_at: Utc::now(),
        }
    }

    fn single(deadline_ms: u64) -> DestroyMode<'static> {
        DestroyMode::Single {
            worker_deadline: std::time::Duration::from_millis(deadline_ms),
        }
    }

    /// ADR-048 §7: destroying a sandbox whose worker is unreachable must
    /// not mark it destroyed — that would leave its container running with
    /// nothing tracking it. The row and its files are kept, and the caller
    /// gets a 503 naming the node.
    #[tokio::test]
    async fn destroying_a_sandbox_on_an_unreachable_worker_keeps_it() {
        let data_root = unique_data_root("destroy-unreachable");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);
        // Only the node-name lookup may run; a mark_destroyed would find
        // no queued result and fail the test with a database error.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::nodes::Model>::new()])
            .into_connection();
        let mut provider = FakeProvider::new();
        provider.fail_destroy = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .destroy_row(worker_row(3), false, single(1_000))
            .await
            .expect_err("an unreachable worker must not look like a destroy");

        match &error {
            SandboxError::NodeUnreachable {
                sandbox_id,
                node,
                reason,
            } => {
                assert_eq!(sandbox_id, PUBLIC_ID);
                assert_eq!(node, "id 3");
                assert!(reason.contains("daemon unreachable"), "{reason}");
            }
            other => panic!("expected NodeUnreachable, got {other:?}"),
        }
        assert!(error.to_string().contains("sandbox nodes evict"), "{error}");
        assert!(dir.exists(), "a kept sandbox keeps its files");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A worker that accepts the connection but never answers gives up at
    /// the deadline instead of holding the request for the provider's
    /// lifecycle timeout, and the sandbox is kept.
    #[tokio::test]
    async fn destroying_a_sandbox_on_a_frozen_worker_gives_up_and_keeps_it() {
        let data_root = unique_data_root("destroy-frozen");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::nodes::Model>::new()])
            .into_connection();
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut provider = FakeProvider::new();
        provider.hang_destroy = Some(attempts.clone());
        let (service, _) = build_service(db, provider, data_root.clone());

        let started = std::time::Instant::now();
        let error = service
            .destroy_row(worker_row(3), false, single(50))
            .await
            .expect_err("a frozen worker must not look like a destroy");

        assert!(
            matches!(&error, SandboxError::NodeUnreachable { reason, .. } if reason.contains("did not answer")),
            "{error:?}"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A worker that answers destroys the sandbox like the control plane.
    #[tokio::test]
    async fn destroying_a_sandbox_on_a_reachable_worker_marks_it_destroyed() {
        let data_root = unique_data_root("destroy-worker-ok");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![destroyed_event()]])
            .append_query_results([vec![worker_row(3)]])
            .into_connection();
        let provider = FakeProvider::new();
        let purges = provider.destroy_purge_flags.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let outcome = service
            .destroy_row(worker_row(3), false, single(1_000))
            .await
            .expect("destroyed");

        assert_eq!(outcome, ContainerOutcome::Destroyed);
        assert_eq!(purges.lock().expect("purge flags").len(), 1);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    fn live_row(status: &str) -> sandboxes::Model {
        sandboxes::Model {
            status: status.to_string(),
            ..worker_row(3)
        }
    }

    /// The row survived the create: nothing is torn down.
    #[tokio::test]
    async fn worker_create_confirms_a_row_that_is_still_live() {
        let data_root = unique_data_root("confirm-live");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![live_row("running")]])
            .into_connection();
        let provider = FakeProvider::new();
        let purges = provider.destroy_purge_flags.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        service
            .confirm_worker_create(&worker_row(3), 1)
            .await
            .expect("still live");

        assert!(purges.lock().expect("purge flags").is_empty());
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// An eviction destroyed the row while the container was being
    /// created: the container is removed and the create fails, telling the
    /// user to create it again.
    #[tokio::test]
    async fn worker_create_removes_the_container_when_its_row_was_evicted() {
        let data_root = unique_data_root("confirm-evicted");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![live_row("destroyed")]])
            .append_query_results([vec![live_row("destroyed")]])
            .into_connection();
        let provider = FakeProvider::new();
        let purges = provider.destroy_purge_flags.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .confirm_worker_create(&worker_row(3), 1)
            .await
            .expect_err("the row is gone");

        assert!(
            matches!(&error, SandboxError::CreateFailed { reason, .. } if reason.contains("destroyed while it was being created")),
            "{error:?}"
        );
        assert_eq!(purges.lock().expect("purge flags").len(), 1);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// An eviction of the node started after placement checked the cordon
    /// but before the row existed, so it never listed this row: the create
    /// removes its container and fails instead of surviving the eviction.
    #[tokio::test]
    async fn worker_create_yields_to_an_eviction_that_started_during_it() {
        let data_root = unique_data_root("confirm-cordoned");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![live_row("running")]])
            .append_query_results([vec![live_row("destroyed")]])
            .into_connection();
        let provider = FakeProvider::new();
        let purges = provider.destroy_purge_flags.clone();
        let (service, _) = build_service(db, provider, data_root.clone());
        let row = worker_row(3);
        let node_id = row.node_id.expect("a worker row");
        let _eviction = service
            .evicting_nodes
            .begin(node_id)
            .expect("eviction running");

        let error = service
            .confirm_worker_create(&row, 1)
            .await
            .expect_err("the node is being evicted");

        assert!(
            matches!(&error, SandboxError::CreateFailed { reason, .. } if reason.contains("started evicting")),
            "{error:?}"
        );
        assert_eq!(purges.lock().expect("purge flags").len(), 1);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// If the row can't be read back, the create fails closed and removes
    /// the container rather than risk one that nothing tracks.
    #[tokio::test]
    async fn worker_create_fails_closed_when_the_row_cannot_be_read() {
        let data_root = unique_data_root("confirm-db-error");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let provider = FakeProvider::new();
        let purges = provider.destroy_purge_flags.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .confirm_worker_create(&worker_row(3), 1)
            .await
            .expect_err("fail closed");

        assert!(
            matches!(&error, SandboxError::CreateFailed { reason, .. } if reason.contains("could not confirm")),
            "{error:?}"
        );
        assert_eq!(purges.lock().expect("purge flags").len(), 1);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A destroy or eviction that lands while a pause waits on the worker
    /// wins: the pause reports the sandbox gone and does not write
    /// `stopped` over `destroyed` (which would block removing the node).
    #[tokio::test]
    async fn pause_does_not_resurrect_a_sandbox_destroyed_meanwhile() {
        let data_root = unique_data_root("pause-destroyed");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![worker_row(3)]])
            // The conditional UPDATE matched nothing...
            .append_query_results([Vec::<sandboxes::Model>::new()])
            // ...because the row is destroyed now.
            .append_query_results([vec![live_row("destroyed")]])
            .into_connection();
        let provider = FakeProvider::new();
        let calls = provider.lifecycle_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .pause_sandbox(PUBLIC_ID, 1)
            .await
            .expect_err("a destroyed sandbox is not paused");

        assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
        assert_eq!(*calls.lock().expect("calls"), vec!["stop"]);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The expiry sweep stopped the sandbox while the pause was in flight:
    /// the pause is still a success (it is idempotent on stopped sandboxes).
    #[tokio::test]
    async fn pause_accepts_a_sandbox_the_sweep_stopped_meanwhile() {
        let data_root = unique_data_root("pause-swept");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![worker_row(3)]])
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .append_query_results([vec![live_row("stopped")]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let row = service
            .pause_sandbox(PUBLIC_ID, 1)
            .await
            .expect("already stopped");

        assert_eq!(row.status, "stopped");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn pause_writes_stopped_only_over_a_running_row() {
        let data_root = unique_data_root("pause-running");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![worker_row(3)]])
            .append_query_results([vec![live_row("stopped")]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let row = service.pause_sandbox(PUBLIC_ID, 1).await.expect("paused");

        assert_eq!(row.status, "stopped");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resume_does_not_resurrect_a_sandbox_destroyed_meanwhile() {
        let data_root = unique_data_root("resume-destroyed");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![live_row("stopped")]])
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .append_query_results([vec![live_row("destroyed")]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let err = service
            .resume_sandbox(PUBLIC_ID, 1)
            .await
            .expect_err("a destroyed sandbox is not resumed");

        assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    fn restricted_egress() -> temps_ai_agents::sandbox::SandboxEgressPolicy {
        temps_ai_agents::sandbox::SandboxEgressPolicy::restricted(vec![
            temps_ai_agents::sandbox::EgressRule::host("API.example.com", &[443]),
            temps_ai_agents::sandbox::EgressRule::cidr("1.1.1.0/24", &[]),
        ])
    }

    fn with_egress(
        row: sandboxes::Model,
        policy: &temps_ai_agents::sandbox::SandboxEgressPolicy,
    ) -> sandboxes::Model {
        sandboxes::Model {
            egress_policy: sandboxes::StoredEgressPolicy(policy.normalized().expect("valid")),
            ..row
        }
    }

    fn egress_event() -> sandbox_events::Model {
        sandbox_events::Model {
            event_type: "egress_policy_updated".into(),
            ..destroyed_event()
        }
    }

    #[tokio::test]
    async fn an_egress_policy_is_applied_before_it_is_stored() {
        let data_root = unique_data_root("egress-set");
        let policy = restricted_egress();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
            .append_query_results([vec![with_egress(
                row_with(PUBLIC_ID, "ephemeral", "running"),
                &policy,
            )]])
            .append_query_results([vec![egress_event()]])
            .into_connection();
        let db = Arc::new(db);
        let provider = FakeProvider::new();
        let calls = provider.egress_calls.clone();
        let (service, _) = build_service_shared(db.clone(), provider, data_root.clone());

        let updated = service
            .set_egress_policy(PUBLIC_ID, 1, policy.clone())
            .await
            .expect("applied and stored");

        let normalized = policy.normalized().expect("valid");
        assert_eq!(updated.egress_policy.0, normalized);
        // The backend got the normalized policy, once.
        assert_eq!(*calls.lock().expect("calls"), vec![normalized]);
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("only handle")
            .into_transaction_log();
        let sql = format!("{log:?}");
        assert!(sql.contains("egress_policy"), "the row is written: {sql}");
        assert!(
            sql.contains("egress_policy_updated"),
            "an event is recorded: {sql}"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn an_invalid_egress_policy_never_reaches_the_backend() {
        let data_root = unique_data_root("egress-invalid");
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let provider = FakeProvider::new();
        let calls = provider.egress_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .set_egress_policy(
                PUBLIC_ID,
                1,
                temps_ai_agents::sandbox::SandboxEgressPolicy::restricted(vec![
                    temps_ai_agents::sandbox::EgressRule::cidr("10.0.0.0/8", &[]),
                ]),
            )
            .await
            .expect_err("a private range can never be allowed");

        assert!(matches!(err, SandboxError::Validation { .. }), "{err:?}");
        let text = err.to_string();
        assert!(
            text.contains(PUBLIC_ID) && text.contains("10.0.0.0/8"),
            "{text}"
        );
        assert!(calls.lock().expect("calls").is_empty());
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn a_backend_that_cannot_enforce_a_restricted_policy_is_a_typed_refusal() {
        let data_root = unique_data_root("egress-unsupported");
        // Only the lookup: no write may follow a refusal.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
            .into_connection();
        let provider = FakeProvider {
            fail_egress: true,
            ..FakeProvider::new()
        };
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .set_egress_policy(PUBLIC_ID, 1, restricted_egress())
            .await
            .expect_err("refused");

        assert!(
            matches!(&err, SandboxError::EgressPolicyUnsupported { sandbox_id, backend }
                if sandbox_id == PUBLIC_ID && backend == "firecracker"),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn a_policy_that_cannot_be_stored_is_rolled_back_on_the_backend() {
        let data_root = unique_data_root("egress-rollback");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
            // The conditional write matches nothing: destroyed meanwhile.
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "destroyed")]])
            .into_connection();
        let provider = FakeProvider::new();
        let calls = provider.egress_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .set_egress_policy(PUBLIC_ID, 1, restricted_egress())
            .await
            .expect_err("destroyed meanwhile");

        assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
        assert_eq!(
            *calls.lock().expect("calls"),
            vec![
                restricted_egress().normalized().expect("valid"),
                temps_ai_agents::sandbox::SandboxEgressPolicy::internet(),
            ],
            "the stored policy is put back"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn the_egress_policy_of_an_agent_run_sandbox_is_not_changed_here() {
        let data_root = unique_data_root("egress-agent-run");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, Some(42))]])
            .into_connection();
        let provider = FakeProvider::new();
        let calls = provider.egress_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .set_egress_policy(PUBLIC_ID, 1, restricted_egress())
            .await
            .expect_err("managed by the run");

        assert!(
            matches!(err, SandboxError::ManagedByAgentRun { run_id: 42, .. }),
            "{err:?}"
        );
        assert!(calls.lock().expect("calls").is_empty());
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resume_reapplies_a_stored_restricted_policy() {
        let data_root = unique_data_root("egress-resume");
        let policy = restricted_egress();
        let stopped = with_egress(row_with(PUBLIC_ID, "ephemeral", "stopped"), &policy);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stopped.clone()]])
            // enforce_stored_egress_policy re-reads the row.
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([vec![with_egress(
                row_with(PUBLIC_ID, "ephemeral", "running"),
                &policy,
            )]])
            .append_query_results([vec![destroyed_event()]])
            .into_connection();
        let provider = FakeProvider::new();
        let calls = provider.egress_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let row = service.resume_sandbox(PUBLIC_ID, 1).await.expect("resumed");

        assert_eq!(row.status, "running");
        assert_eq!(
            *calls.lock().expect("calls"),
            vec![policy.normalized().expect("valid")]
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resume_stops_a_sandbox_whose_restricted_policy_cannot_be_reapplied() {
        let data_root = unique_data_root("egress-resume-fail");
        let stopped = with_egress(
            row_with(PUBLIC_ID, "ephemeral", "stopped"),
            &restricted_egress(),
        );
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stopped.clone()]])
                .append_query_results([vec![stopped.clone()]])
                // mark_stopped_for_egress_failure: the row and its event.
                .append_query_results([vec![stopped]])
                .append_query_results([vec![destroyed_event()]])
                .into_connection(),
        );
        let provider = FakeProvider {
            fail_egress: true,
            ..FakeProvider::new()
        };
        let lifecycle = provider.lifecycle_calls.clone();
        let (service, _) = build_service_shared(db.clone(), provider, data_root.clone());

        let err = service
            .resume_sandbox(PUBLIC_ID, 1)
            .await
            .expect_err("never runs with wider egress");

        assert!(
            matches!(err, SandboxError::EgressPolicyUnsupported { .. }),
            "{err:?}"
        );
        assert_eq!(*lifecycle.lock().expect("calls"), vec!["start", "stop"]);
        drop(service);
        let sql = format!(
            "{:?}",
            Arc::try_unwrap(db)
                .expect("only handle")
                .into_transaction_log()
        );
        assert!(!sql.contains("\"running\""), "never marked running: {sql}");
        assert!(sql.contains(EGRESS_POLICY_REAPPLY_FAILED), "{sql}");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The row must not keep saying `running` for a sandbox the service
    /// stopped because its egress policy could not be re-applied after a
    /// restart; the `stopped` event names why.
    #[tokio::test]
    async fn restart_marks_stopped_a_sandbox_whose_restricted_policy_cannot_be_reapplied() {
        let data_root = unique_data_root("egress-restart-fail");
        let running = with_egress(
            row_with(PUBLIC_ID, "ephemeral", "running"),
            &restricted_egress(),
        );
        let stopped = with_egress(
            row_with(PUBLIC_ID, "ephemeral", "stopped"),
            &restricted_egress(),
        );
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![running.clone()]])
                // enforce_stored_egress_policy re-reads the row.
                .append_query_results([vec![running]])
                // mark_stopped_for_egress_failure: running -> stopped.
                .append_query_results([vec![stopped]])
                .append_query_results([vec![destroyed_event()]])
                .into_connection(),
        );
        let provider = FakeProvider {
            fail_egress: true,
            ..FakeProvider::new()
        };
        let lifecycle = provider.lifecycle_calls.clone();
        let (service, _) = build_service_shared(db.clone(), provider, data_root.clone());

        let err = service
            .restart_sandbox(PUBLIC_ID, 1)
            .await
            .expect_err("never runs with wider egress");

        assert!(
            matches!(err, SandboxError::EgressPolicyUnsupported { .. }),
            "{err:?}"
        );
        assert_eq!(
            lifecycle.lock().expect("calls").last().copied(),
            Some("stop"),
            "the container is stopped"
        );
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("only handle")
            .into_transaction_log();
        let sql = format!("{log:?}");
        assert!(
            sql.contains("UPDATE") && sql.contains("\"stopped\""),
            "the row is marked stopped: {sql}"
        );
        assert!(
            sql.contains(EGRESS_POLICY_REAPPLY_FAILED) && sql.contains("egress policy"),
            "the stopped event names the egress policy failure: {sql}"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resume_of_an_internet_sandbox_does_not_touch_its_policy() {
        let data_root = unique_data_root("egress-resume-internet");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "running")]])
            .append_query_results([vec![destroyed_event()]])
            .into_connection();
        let provider = FakeProvider::new();
        let calls = provider.egress_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        service.resume_sandbox(PUBLIC_ID, 1).await.expect("resumed");

        assert!(calls.lock().expect("calls").is_empty());
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[test]
    fn eviction_guard_serializes_evictions_of_one_node() {
        let nodes = Arc::new(EvictingNodes::default());
        let first = nodes.begin(3).expect("first eviction");
        assert!(nodes.begin(3).is_none(), "a second eviction is refused");
        let other = nodes.begin(4).expect("other nodes are independent");
        assert_eq!(nodes.snapshot(), std::collections::HashSet::from([3, 4]));
        drop(first);
        assert_eq!(nodes.snapshot(), std::collections::HashSet::from([4]));
        assert!(nodes.begin(3).is_some(), "available again once finished");
        drop(other);
    }

    fn settings_and_worker(db: MockDatabase) -> MockDatabase {
        db.append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .append_query_results([vec![crate::services::placement::tests::node_row(
                "worker",
                "active",
                Some(1),
            )]])
    }

    /// While a node is being evicted, a second eviction of it is a 409 and
    /// an explicit create on it is refused naming the node.
    #[tokio::test]
    async fn a_node_being_evicted_takes_no_sandboxes_and_no_second_eviction() {
        let data_root = unique_data_root("evicting");
        let db = settings_and_worker(MockDatabase::new(DatabaseBackend::Postgres))
            // live counts for the eviction's node lookup
            .append_query_results([Vec::<sandboxes::Model>::new()])
            // the create's allow-list read (never served from the settings
            // cache, so a malformed row fails placement closed)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            // the create's worker listing
            .append_query_results([vec![crate::services::placement::tests::node_row(
                "worker",
                "active",
                Some(1),
            )]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let held = service.evicting_nodes.begin(7).expect("eviction running");

        let err = service
            .evict_node("worker-7")
            .await
            .expect_err("second eviction");
        assert!(
            matches!(&err, SandboxError::NodeEvictionInProgress { node } if node == "worker-7"),
            "{err:?}"
        );

        let req = CreateSandboxRequest {
            node: Some("worker-7".into()),
            ..Default::default()
        };
        let err = service.place_sandbox(&req).await.expect_err("cordoned");
        assert!(
            matches!(&err, SandboxError::NodeNotReady { node, status }
                if node == "worker-7" && status.contains("evicted")),
            "{err:?}"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The per-user cap counts what the user created through the API and
    /// nothing else: not destroyed rows, not agent-run sandboxes, not managed
    /// application workspaces.
    #[tokio::test]
    async fn the_user_cap_counts_only_live_standalone_sandboxes() {
        let data_root = unique_data_root("cap-count");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![std::collections::BTreeMap::from([(
                "num_items".to_string(),
                sea_orm::Value::from(4i64),
            )])]])
            .into_connection();
        let db = Arc::new(db);
        let (service, registry) = build_service_shared(db.clone(), FakeProvider::new(), data_root);

        let live = SandboxService::live_standalone_sandboxes(service.db.as_ref(), 7)
            .await
            .expect("count query");

        assert_eq!(live, 4);
        drop((service, registry));
        let log = Arc::try_unwrap(db)
            .unwrap_or_else(|_| panic!("service still holds the connection"))
            .into_transaction_log();
        let sql = format!("{log:?}").replace("\\\"", "\"");
        assert!(sql.contains("\"sandboxes\".\"user_id\" = $1"), "{sql}");
        assert!(sql.contains("\"sandboxes\".\"status\" <> $2"), "{sql}");
        assert!(
            sql.contains("\"sandboxes\".\"metadata\"->>'source' = $3"),
            "{sql}"
        );
    }

    /// A malformed allow-list must stop placement, not read as "every node
    /// is allowed" the way a defaulted settings document would.
    #[tokio::test]
    async fn a_malformed_allow_list_fails_placement_closed() {
        let data_root = unique_data_root("malformed-allow-list");
        let row = temps_entities::settings::Model {
            id: 1,
            data: serde_json::json!({
                "preview_domain": false,
                "agent_sandbox": {"allowed_node_ids": "3,7"}
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let err = service
            .place_sandbox(&CreateSandboxRequest::default())
            .await
            .expect_err("placement must fail closed");
        assert!(
            matches!(
                &err,
                SandboxError::PlacementSettings {
                    operation: "load",
                    ..
                }
            ),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// Explicit placement asks the chosen worker whether it can run a
    /// sandbox, and refuses it by name when it cannot.
    #[tokio::test]
    async fn explicit_placement_refuses_a_worker_that_cannot_run_sandboxes() {
        struct NoDocker;
        #[async_trait::async_trait]
        impl placement::NodeProbe for NoDocker {
            async fn check(&self, _node_id: i32) -> Result<(), String> {
                Err("Docker is not available on the node".into())
            }
        }
        let db = Arc::new(
            settings_and_worker(MockDatabase::new(DatabaseBackend::Postgres)).into_connection(),
        );
        let mut service = super::tests::preview_test_service_with_db(db);
        service.node_probe = Arc::new(NoDocker);

        let req = CreateSandboxRequest {
            node: Some("7".into()),
            ..Default::default()
        };
        let err = service.place_sandbox(&req).await.expect_err("no Docker");
        assert!(
            matches!(&err, SandboxError::NodeNotReady { node, status }
                if node == "worker-7" && status.contains("Docker is not available")),
            "{err:?}"
        );
    }

    /// The single-node default (no node requested, control plane allowed)
    /// asks no worker and lists no nodes.
    #[tokio::test]
    async fn default_placement_on_the_control_plane_makes_no_node_calls() {
        struct Panics;
        #[async_trait::async_trait]
        impl placement::NodeProbe for Panics {
            async fn check(&self, node_id: i32) -> Result<(), String> {
                panic!("node {node_id} probed on the control-plane path")
            }
        }
        // Only the settings read is queued: a node query would fail.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                .into_connection(),
        );
        let mut service = super::tests::preview_test_service_with_db(db);
        service.node_probe = Arc::new(Panics);

        let placed = service
            .place_sandbox(&CreateSandboxRequest::default())
            .await
            .expect("control plane");
        assert_eq!(placed, None);
    }

    /// ADR 063: on a control plane that runs no workloads, a create with no
    /// node goes to a worker (the control plane is never a candidate).
    #[tokio::test]
    async fn without_local_workloads_default_placement_picks_a_worker() {
        let db = Arc::new(
            settings_and_worker(MockDatabase::new(DatabaseBackend::Postgres))
                .append_query_results([Vec::<sandboxes::Model>::new()])
                .into_connection(),
        );
        let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
        assert!(!service.runs_local_sandboxes());

        let placed = service
            .place_sandbox(&CreateSandboxRequest::default())
            .await
            .expect("a worker");
        assert_eq!(placed, Some(7));

        // Naming the control plane is refused rather than honoured.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                .into_connection(),
        );
        let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
        let req = CreateSandboxRequest {
            node: Some("control-plane".into()),
            ..Default::default()
        };
        let err = service
            .place_sandbox(&req)
            .await
            .expect_err("no local host");
        assert!(matches!(err, SandboxError::NoLocalHost { .. }), "{err:?}");
    }

    /// ADR 063: with no worker at all, the create fails with the
    /// worker-node-required error, not a 404 or a local fallback.
    #[tokio::test]
    async fn without_local_workloads_and_no_worker_placement_needs_a_worker_node() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                // only the control plane's own row
                .append_query_results([vec![crate::services::placement::tests::node_row(
                    "control-plane",
                    "active",
                    Some(1),
                )]])
                .append_query_results([Vec::<sandboxes::Model>::new()])
                .into_connection(),
        );
        let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
        let err = service
            .place_sandbox(&CreateSandboxRequest::default())
            .await
            .expect_err("no worker");
        assert!(
            matches!(&err, SandboxError::NoWorkerNodeForSandboxes { detail }
                if detail == "no worker node has joined it yet"),
            "{err:?}"
        );
    }

    /// ADR 063: an application workspace needs a directory on this host,
    /// which runs nothing; refused before any database work.
    #[tokio::test]
    async fn without_local_workloads_application_workspaces_are_refused() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = super::tests::preview_test_service_with_db(db).with_local_workloads(false);
        let req = CreateSandboxRequest {
            host_work_dir_override: Some(PathBuf::from("/tmp/app-workspace")),
            ..Default::default()
        };
        let err = service
            .place_sandbox(&req)
            .await
            .expect_err("no local host");
        let SandboxError::NoLocalHost { message } = &err else {
            panic!("expected NoLocalHost, got {err:?}");
        };
        assert!(
            message.starts_with("An application workspace needs a sandbox"),
            "{message}"
        );
    }

    /// The node page asks for the control plane by its `nodes` row id; that
    /// resolves to the control plane instead of "node does not exist".
    #[tokio::test]
    async fn the_control_planes_own_row_lists_the_control_planes_sandboxes() {
        let mut cp_row =
            crate::services::placement::tests::node_row("control-plane", "active", Some(1));
        cp_row.id = 1;
        cp_row.name = "cp-1".into();
        for local_workloads in [true, false] {
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                    .append_query_results([vec![cp_row.clone()]])
                    .append_query_results([Vec::<sandboxes::Model>::new()])
                    // the miss path: is node 1 the control plane's row?
                    .append_query_results([vec![cp_row.clone()]])
                    .append_query_results([[std::collections::BTreeMap::from([(
                        "num_items",
                        sea_orm::Value::BigInt(Some(0)),
                    )])]])
                    .append_query_results([Vec::<sandboxes::Model>::new()])
                    .into_connection(),
            );
            let service = super::tests::preview_test_service_with_db(db)
                .with_local_workloads(local_workloads);
            let listing = service
                .node_sandboxes("1", None, None)
                .await
                .expect("the control plane");
            assert!(listing.node.is_control_plane);
            assert_eq!(listing.node.eligible, local_workloads);
            if !local_workloads {
                assert_eq!(
                    listing.node.reason.as_deref(),
                    Some(placement::CONTROL_PLANE_RUNS_NO_WORKLOADS_REASON)
                );
            }
            assert_eq!(listing.total, 0);
        }
    }

    #[test]
    fn eviction_owners_are_the_destroyed_sandboxes_owners() {
        let owners = HashMap::from([
            ("sbx_a".to_string(), Some(4)),
            ("sbx_b".to_string(), Some(2)),
            ("sbx_c".to_string(), Some(4)),
            ("sbx_d".to_string(), None),
            ("sbx_failed".to_string(), Some(9)),
        ]);
        let destroyed = vec![
            "sbx_a".to_string(),
            "sbx_b".to_string(),
            "sbx_c".to_string(),
            "sbx_d".to_string(),
        ];
        assert_eq!(eviction_owners(&destroyed, &owners), vec![2, 4]);
    }

    #[test]
    fn eviction_outcomes_are_split_and_sorted() {
        let (destroyed, unconfirmed, failed) = split_eviction_outcomes(vec![
            ("sbx_c".into(), Ok(ContainerOutcome::Destroyed)),
            (
                "sbx_b".into(),
                Ok(ContainerOutcome::Unconfirmed("did not answer".into())),
            ),
            (
                "sbx_d".into(),
                Err(SandboxError::Validation {
                    message: "bad\u{1b}[31m".into(),
                }),
            ),
            ("sbx_a".into(), Ok(ContainerOutcome::Destroyed)),
        ]);

        assert_eq!(destroyed, vec!["sbx_a", "sbx_b", "sbx_c"]);
        assert_eq!(
            unconfirmed,
            vec![("sbx_b".to_string(), "did not answer".to_string())]
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "sbx_d");
        assert!(!failed[0].1.contains('\u{1b}'), "{:?}", failed[0].1);
    }

    #[test]
    fn node_sandboxes_page_is_clamped() {
        assert_eq!(
            node_sandboxes_page(None, None),
            (1, NODE_SANDBOXES_DEFAULT_PAGE_SIZE)
        );
        assert_eq!(
            node_sandboxes_page(Some(0), Some(1_000)),
            (1, NODE_SANDBOXES_MAX_PAGE_SIZE)
        );
        assert_eq!(node_sandboxes_page(Some(u64::MAX), Some(0)), (MAX_PAGE, 1));
    }

    #[test]
    fn reasons_from_a_node_are_bounded_and_printable() {
        let reason = bounded_reason("line one\n\u{1b}]0;owned\u{7}line two");
        assert!(!reason.chars().any(char::is_control), "{reason:?}");
        assert!(reason.contains("line one") && reason.contains("line two"));

        let long = "é".repeat(MAX_REASON_CHARS * 2);
        let reason = bounded_reason(&long);
        assert_eq!(reason.chars().count(), MAX_REASON_CHARS + 1);
        assert!(reason.ends_with('…'));
        assert_eq!(bounded_reason("short"), "short");
    }

    #[test]
    fn cleanup_command_targets_the_sandbox_and_its_proxy() {
        assert_eq!(
            node_cleanup_command("sbx_0b507bb997ea14d0"),
            "docker ps -aq --filter name=temps-sandbox-0b507bb997ea14d0 | xargs -r docker rm -f"
        );
    }

    /// Agent-run sandboxes take an early return through
    /// `release_for_agent_run`, and their work dir belongs to the executor,
    /// not to `data_root`. If that early return is ever lost, this test
    /// catches it before it recursively deletes a live run's workspace.
    #[tokio::test]
    async fn destroy_sandbox_for_agent_run_leaves_the_work_dir_alone() {
        let data_root = unique_data_root("agent-run");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id → row owned by agent run 42
            .append_query_results([vec![row(PUBLIC_ID, Some(42))]])
            // agent_runs lookup → empty, i.e. run is gone ⇒ terminal
            .append_query_results([Vec::<agent_runs::Model>::new()])
            // release_for_agent_run: rows for this run
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let _ = service.destroy_sandbox(PUBLIC_ID, 1).await;

        assert!(
            dir.exists(),
            "an agent-run sandbox must not have {} deleted — that directory \
             is owned by the executor, and this path is only for standalone \
             sandboxes",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn create_sandbox_provider_failure_leaves_no_work_dir() {
        let data_root = unique_data_root("create-fail");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // placement settings (ADR-048) → no row, i.e. defaults: every node allowed
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            // insert of the new row (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed update
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_create = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .create_sandbox(1, CreateSandboxRequest::default())
            .await
            .expect_err("provider create failed, so create_sandbox must fail");
        assert!(
            matches!(err, SandboxError::CreateFailed { .. }),
            "{:?}",
            err
        );

        let leftovers: Vec<_> = std::fs::read_dir(&data_root)
            .expect("read data root")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed create must not strand a work dir; found {:?}",
            leftovers
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A caller that stops waiting (a proxy timing out during a first image
    /// build) must not cancel the create: it still reaches its cleanup path,
    /// so no work dir or "running" row is stranded.
    #[tokio::test]
    async fn spawned_create_finishes_after_the_caller_stops_waiting() {
        let data_root = unique_data_root("create-detached");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed update, reached only if the create was not cancelled
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_create = true;
        provider.create_delay = Some(Duration::from_millis(200));
        let (service, _) = build_service(db, provider, data_root.clone());

        let (held, hold) = tokio::sync::oneshot::channel::<()>();
        let waiting = service.spawn_create(1, CreateSandboxRequest::default(), held);
        // The caller gives up while the provider is still creating.
        let gave_up = tokio::time::timeout(Duration::from_millis(20), waiting).await;
        assert!(gave_up.is_err(), "the create should still be running");
        // `hold` is released only when the create has ended.
        tokio::time::timeout(Duration::from_secs(5), hold)
            .await
            .expect("the create must finish without a caller")
            .expect_err("the held value is dropped, never sent");

        let leftovers: Vec<_> = std::fs::read_dir(&data_root)
            .expect("read data root")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a create nobody waited for must still clean up; found {:?}",
            leftovers
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// While a create runs, the row already says `running`. Reads must say
    /// `starting` instead, and the mark must go away when the create ends.
    #[tokio::test]
    async fn a_create_in_progress_reads_as_starting_until_it_ends() {
        let data_root = unique_data_root("create-starting");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_create = true;
        provider.create_delay = Some(Duration::from_millis(300));
        let (service, _) = build_service(db, provider, data_root.clone());

        let (held, hold) = tokio::sync::oneshot::channel::<()>();
        let _waiting = service.spawn_create(1, CreateSandboxRequest::default(), held);
        let creating = {
            let mut found = None;
            for _ in 0..100 {
                if let Some(id) = service.creates_in_progress.set().iter().next().cloned() {
                    found = Some(id);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            found.expect("the create should be marked in progress")
        };
        let mut summaries = [
            SandboxSummary::from(&row(&creating, None)),
            SandboxSummary::from(&row(PUBLIC_ID, None)),
        ];
        service.mark_creates_in_progress(&mut summaries);
        assert_eq!(summaries[0].status, "starting");
        assert_eq!(
            summaries[1].status, "running",
            "other sandboxes keep their status"
        );

        tokio::time::timeout(Duration::from_secs(5), hold)
            .await
            .expect("the create must finish")
            .expect_err("the held value is dropped, never sent");
        assert!(
            !service.creates_in_progress.contains(&creating),
            "the mark must be cleared when the create ends, even when it fails"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// Exec and filesystem calls on a sandbox still being created get a
    /// clear 409 instead of a wake attempt on a container that does not exist.
    #[tokio::test]
    async fn resolve_id_refuses_a_sandbox_whose_create_is_still_running() {
        let data_root = unique_data_root("resolve-starting");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let _mark = service.creates_in_progress.begin(PUBLIC_ID);

        let error = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect_err("a sandbox being created cannot run commands yet");
        match error {
            SandboxError::InvalidState {
                sandbox_id, state, ..
            } => {
                assert_eq!(sandbox_id, PUBLIC_ID);
                assert_eq!(state, "starting");
            }
            other => panic!("expected InvalidState(starting), got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The costly one: seeding runs after the clone, so by the time it fails
    /// the directory can hold an entire repository, and the row is already
    /// destroyed so no later `destroy_sandbox` can reach it.
    #[tokio::test]
    async fn create_sandbox_seed_failure_leaves_no_work_dir() {
        let data_root = unique_data_root("seed-fail");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // placement settings (ADR-048) → no row, i.e. defaults: every node allowed
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            // insert of the new row
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // update after create (status/metadata write-back)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_exec = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let req = CreateSandboxRequest {
            source: Some(SandboxSource::Tarball {
                // A public IP literal keeps this cleanup regression test
                // deterministic: source validation succeeds without DNS and
                // the fake provider remains the intended seed failure.
                url: "https://93.184.216.34/src.tar.gz".into(),
            }),
            ..Default::default()
        };
        let err = service
            .create_sandbox(1, req)
            .await
            .expect_err("seeding failed, so create_sandbox must fail");
        assert!(matches!(err, SandboxError::ExecFailed { .. }), "{:?}", err);

        let leftovers: Vec<_> = std::fs::read_dir(&data_root)
            .expect("read data root")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed seed must not strand a work dir holding a cloned \
             repository; found {:?}",
            leftovers
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── Major 5: from_snapshot routing ───────────────────────────────────────

    /// When `CreateSandboxRequest.from_snapshot_artifact` is `Some`, the
    /// service routes to `registry.create_from_snapshot` instead of
    /// `registry.create`. FakeProvider doesn't override `create_from_snapshot`
    /// so it returns the trait-default "not supported" error — the test
    /// confirms the routing by observing that error rather than a normal
    /// create-failure reason.
    #[tokio::test]
    async fn create_sandbox_from_snapshot_routes_to_create_from_snapshot() {
        let data_root = unique_data_root("from-snapshot-route");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // placement settings (ADR-048) → no row, i.e. defaults: every node allowed
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            // insert of the new row (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed update (RETURNING) — cleanup after create fails
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let artifact = temps_ai_agents::sandbox::SnapshotArtifact {
            content_path: std::path::PathBuf::from("/tmp/test.tar"),
            content_digest: "sha256fake".to_string(),
            primary_digest: "sha256fake".to_string(),
            size_bytes: 1024,
            backend: temps_ai_agents::sandbox::SandboxBackend::Docker,
            image_ref: Some("temps-snapshot/test:v1".to_string()),
            image_id: Some("sha256:fake".to_string()),
            workspace: None,
        };

        let req = CreateSandboxRequest {
            from_snapshot_artifact: Some(artifact),
            project_id: Some(42),
            ..Default::default()
        };

        let err = service
            .create_sandbox(1, req)
            .await
            .expect_err("FakeProvider::create_from_snapshot returns NotSupported, must fail");

        // The error message proves we went through the from-snapshot path, not
        // the normal create path.
        let msg = err.to_string();
        assert!(
            msg.contains("not supported") || msg.contains("from_snapshot"),
            "error must reference the snapshot create path, got: {:?}",
            err
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── Major 5: non-Docker provider trait defaults ───────────────────────────

    /// Providers that don't override `take_snapshot` return a typed "not
    /// supported" error rather than silently succeeding. Regression guard for
    /// any future provider that inherits the trait without implementing it.
    #[tokio::test]
    async fn non_docker_provider_take_snapshot_default_returns_not_supported() {
        let handle = handle_for("test");
        let result = FakeProvider::new().take_snapshot(&handle, None, 1024).await;
        assert!(
            result.is_err(),
            "default take_snapshot must return an error, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not supported"),
            "error must say 'not supported', got: {}",
            msg
        );
    }

    /// Providers that don't override `create_from_snapshot` return a typed
    /// "not supported" error. Ensures the from-snapshot routing fails loudly
    /// on backends that don't implement it, rather than silently no-oping.
    #[tokio::test]
    async fn non_docker_provider_create_from_snapshot_default_returns_not_supported() {
        let artifact = temps_ai_agents::sandbox::SnapshotArtifact {
            content_path: std::path::PathBuf::from("/tmp/test.tar"),
            content_digest: "sha256fake".to_string(),
            primary_digest: "sha256fake".to_string(),
            size_bytes: 0,
            backend: temps_ai_agents::sandbox::SandboxBackend::Docker,
            image_ref: None,
            image_id: None,
            workspace: None,
        };
        let config = SandboxCreateConfig {
            node_id: None,
            run_id: 0,
            container_name_override: None,
            host_work_dir: std::path::PathBuf::from("/tmp"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            allow_docker: false,
            egress_policy: Default::default(),
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: std::time::Duration::from_secs(3600),
            backend: None,
            owner_user_id: None,
        };
        let result = FakeProvider::new()
            .create_from_snapshot(&artifact, config)
            .await;
        assert!(
            result.is_err(),
            "default create_from_snapshot must return an error, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not supported"),
            "error must say 'not supported', got: {}",
            msg
        );
    }

    /// Providers that don't override `delete_image` should no-op successfully.
    /// The default is intentionally a no-op for backends (Local, Firecracker)
    /// that have no image store concept.
    #[tokio::test]
    async fn non_docker_provider_delete_image_default_is_noop() {
        let result = FakeProvider::new()
            .delete_image("temps-snapshot/test:v1")
            .await;
        assert!(
            result.is_ok(),
            "default delete_image must succeed (no-op), got: {:?}",
            result
        );
    }

    // ── Major 5: nullify_source_sandbox failure isolation ─────────────────────

    /// `destroy_sandbox` calls `nullify_source_sandbox` best-effort. When the
    /// snapshot DB fails (e.g. transient network error), the destroy must still
    /// complete and return `Ok(())`. A DB error in nullify must never strand the
    /// user with an un-destroyable sandbox.
    ///
    /// We attach a SnapshotService backed by a MockDatabase that errors on the
    /// `update_many` query, then verify destroy_sandbox still returns Ok.
    #[tokio::test]
    async fn destroy_sandbox_nullify_failure_does_not_fail_destroy() {
        use crate::services::snapshot_service::SnapshotService;
        use temps_ai_agents::sandbox::local::LocalSandboxProvider;

        let data_root = unique_data_root("nullify-fail");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        // SandboxService DB: three queries for destroy path
        let sandbox_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        // SnapshotService DB: update_many for nullify fails with a DB error
        let snap_db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "simulated nullify transient failure".to_string(),
                )])
                .into_connection(),
        );

        let snap_provider = Arc::new(LocalSandboxProvider::new()) as Arc<dyn SandboxProvider>;
        let snap_registry = Arc::new(StandaloneSandboxRegistry::local_only(snap_provider.clone()));
        let snapshot_service =
            Arc::new(SnapshotService::new(snap_db, snap_registry, snap_provider));

        // Build the full SandboxService with the snapshot_service injected,
        // then wrap in Arc (note: with_snapshot_service must be called before Arc::new).
        let db = Arc::new(sandbox_db);
        let registry = Arc::new(StandaloneSandboxRegistry::local_only(
            Arc::new(FakeProvider::new()) as Arc<dyn SandboxProvider>,
        ));
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(test_server_config(data_root.clone())),
            db.clone(),
        ));
        let encryption = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key"),
        );
        let git = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption,
            Arc::new(PanicOnSendJobQueue) as Arc<dyn temps_core::JobQueue>,
            config.clone(),
        ));
        let cookie_crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test cookie key"),
        );
        let service = Arc::new(
            SandboxService::new(
                db,
                registry,
                Arc::new(JobTracker::new()),
                config,
                cookie_crypto,
                git,
                data_root.clone(),
                Arc::new(crate::services::placement::EveryNodeReady),
            )
            .with_snapshot_service(snapshot_service),
        );

        // destroy_sandbox must succeed even though nullify_source_sandbox failed.
        let result = service.destroy_sandbox(PUBLIC_ID, 1).await;
        assert!(
            result.is_ok(),
            "destroy_sandbox must succeed even when nullify_source_sandbox fails, got: {:?}",
            result
        );

        // Work dir still cleaned up
        assert!(
            !dir.exists(),
            "work dir must be removed even when nullify failed"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[test]
    fn application_workspace_usage_parses_atomic_metrics_and_ports() {
        let usage = parse_application_workspace_usage(
            "memory=1048576\npids=17\ncpu=332401\ndisk=26376\nports=3000,8080\n",
        );

        assert_eq!(usage.memory_used_bytes, Some(1_048_576));
        assert_eq!(usage.pids_used, Some(17));
        assert_eq!(usage.cpu_usage_usec, Some(332_401));
        assert_eq!(usage.disk_used_bytes, Some(26_376));
        assert_eq!(usage.open_ports, vec![3000, 8080]);
    }

    #[test]
    fn application_workspace_usage_command_has_proc_socket_fallback() {
        assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("/proc/net/tcp"));
        assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("/proc/net/tcp6"));
        assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("du -sb /home/temps/workspace"));
    }

    #[test]
    fn runtime_variables_merge_without_losing_identical_shared_values() {
        let mut variables = HashMap::from([("SHARED_HOST".to_string(), "database".to_string())]);

        merge_runtime_variables(
            &mut variables,
            HashMap::from([
                ("SHARED_HOST".to_string(), "database".to_string()),
                ("REDIS_URL".to_string(), "redis://database:6379".to_string()),
            ]),
        )
        .expect("identical shared values should be accepted");

        assert_eq!(variables.len(), 2);
        assert_eq!(variables["REDIS_URL"], "redis://database:6379");
    }

    #[test]
    fn runtime_variables_reject_ambiguous_service_credentials() {
        let mut variables =
            HashMap::from([("DATABASE_URL".to_string(), "postgres://one".to_string())]);

        let conflict = merge_runtime_variables(
            &mut variables,
            HashMap::from([("DATABASE_URL".to_string(), "postgres://two".to_string())]),
        )
        .expect_err("different values for one runtime name must be rejected");

        assert_eq!(conflict, "DATABASE_URL");
        assert_eq!(variables["DATABASE_URL"], "postgres://one");
    }

    fn link_row(id: i32, service_id: i32, project_id: Option<i32>) -> sandbox_services::Model {
        sandbox_services::Model {
            id,
            sandbox_id: 7,
            service_id,
            project_id,
            created_by: Some(1),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn linked_services_lists_the_sandboxs_links_with_their_projects() {
        let data_root = unique_data_root("linked-services");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![link_row(1, 4, Some(9)), link_row(2, 6, None)]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let links = service
            .linked_services(1, PUBLIC_ID)
            .await
            .expect("owner reads the links");

        assert_eq!(
            links,
            vec![
                LinkedService {
                    service_id: 4,
                    project_id: Some(9)
                },
                LinkedService {
                    service_id: 6,
                    project_id: None
                },
            ]
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn another_users_sandbox_has_no_links_to_read() {
        let data_root = unique_data_root("linked-services-foreign");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let error = service
            .linked_services(2, PUBLIC_ID)
            .await
            .expect_err("a non-owner must not see the sandbox");

        assert!(
            matches!(error, SandboxError::NotFound { ref sandbox_id } if sandbox_id == PUBLIC_ID),
            "got {error:?}"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn a_link_that_cannot_be_given_credentials_is_removed_again() {
        let data_root = unique_data_root("link-rollback");
        // No credential provider is registered, so building the sandbox's
        // access fails after the row was added; the row must be taken back
        // (the delete's exec result is consumed) rather than left linked.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([Vec::<sandbox_services::Model>::new()])
            .append_query_results([vec![link_row(1, 4, Some(9))]])
            .append_query_results([vec![link_row(1, 4, Some(9))]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let error = service
            .link_services(
                1,
                PUBLIC_ID,
                &[LinkedService {
                    service_id: 4,
                    project_id: Some(9),
                }],
            )
            .await
            .expect_err("no credential provider means no scoped access");

        assert!(
            matches!(error, SandboxError::Unavailable { .. }),
            "got {error:?}"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn service_support_needs_a_credential_provider() {
        let data_root = unique_data_root("service-support");
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let error = service
            .service_supports_sandbox(4)
            .await
            .expect_err("without a provider the answer is an error, not a quiet false");

        assert!(
            matches!(error, SandboxError::Unavailable { .. }),
            "got {error:?}"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The control plane offers every selectable backend, says which it can
    /// run, and names the setup step for each one it cannot, so the console
    /// can show the choice instead of hiding it.
    #[test]
    fn the_control_plane_lists_its_backends_with_the_fix_for_missing_ones() {
        let mut provider = FakeProvider::new();
        provider.backends = Some(vec![SandboxBackend::Docker]);
        let (service, _) = build_service(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            provider,
            unique_data_root("cp-backends"),
        );

        let backends = service.control_plane_backends();

        let offered: Vec<_> = backends.iter().map(|b| b.backend).collect();
        assert_eq!(
            offered,
            temps_ai_agents::sandbox::SELECTABLE_SANDBOX_BACKENDS.to_vec()
        );
        let docker = &backends[0];
        assert!(docker.available && docker.reason.is_none(), "{docker:?}");
        let firecracker = &backends[1];
        assert!(!firecracker.available);
        assert!(
            firecracker
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("temps firecracker setup")),
            "an unavailable backend says how to provision it: {firecracker:?}"
        );
        assert!(!backends[2].available && backends[2].reason.is_some());
    }
}
