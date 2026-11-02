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

mod agent_run;
mod application_workspace;
mod create;
mod data_network;
mod destroy;
mod egress;
mod lifecycle;
mod links;
mod node_placement;
mod preview;
mod reconcile;
mod records;
mod source_import;

use agent_run::{ensure_not_agent_run, run_status_is_terminal};
pub use application_workspace::{
    is_managed_application_workspace_image, managed_application_workspace_image,
    ApplicationWorkspaceConfig, ApplicationWorkspaceSandbox, ApplicationWorkspaceUsage,
};
use application_workspace::{managed_application_id_from_name, APPLICATION_WORKSPACE_NAME_PREFIX};
use create::{backend_unavailable_message, API_SANDBOX_SOURCE};
pub use destroy::node_cleanup_command;
use destroy::{container_label_for, ContainerOutcome, DestroyMode, EvictionGate};
pub use egress::EGRESS_POLICY_REAPPLY_FAILED;
pub use links::{LinkedService, ScopedRuntime};
pub(crate) use node_placement::bounded_reason;
pub use node_placement::{
    NodeEviction, NodeSandbox, NodeSandboxes, NODE_SANDBOXES_DEFAULT_PAGE_SIZE,
    NODE_SANDBOXES_MAX_PAGE_SIZE,
};
use node_placement::{EVICTION_CONCURRENCY, EVICTION_DESTROY_DEADLINE, MAX_PAGE};
pub use preview::PreviewAccessChange;
pub use reconcile::{reconcilable_at_startup, VmReconcileReport};
use source_import::{
    bounded_command_error, source_kind, validate_resolved_source, SOURCE_IMPORT_SYSTEM_PATH,
};
pub(crate) use source_import::{url_has_embedded_credentials, validate_source_destination};

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
}

#[cfg(test)]
pub(crate) mod tests;

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
mod storage_cleanup_tests;
