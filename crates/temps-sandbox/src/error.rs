// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed errors for the standalone sandbox API.
//!
//! Every variant includes the identifiers needed to understand the error
//! in isolation (sandbox_id, job_id, path, reason). This follows the
//! codebase-wide rule: error messages must be greppable and include the
//! IDs of the resources involved.

use thiserror::Error;

use temps_agents::error::AgentError;

/// Typed errors for the snapshot sub-API (ADR-037).
///
/// HTTP mappings:
/// - `NotFound` → 404
/// - `NotReady` | `CrossBackendRestore` | `QuotaExceeded` | `InvalidState` |
///   `LegacyArtifactUnsupported` → 422
/// - `NotSupported` → 501
/// - `ScrubFailed` | `DigestMismatch` | `ArtifactMissing` | `SandboxNotFound` → 500
/// - `Database` | `Io` | `Provider` → 500
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum SandboxSnapshotError {
    /// The requested snapshot does not exist (or the caller does not own it).
    #[error("Snapshot {snapshot_id} not found")]
    NotFound { snapshot_id: String },

    /// The snapshot exists but is not yet `ready` (e.g. still `creating`).
    #[error("Snapshot {snapshot_id} is not ready (status: {status})")]
    NotReady { snapshot_id: String, status: String },

    /// Attempt to restore a Docker snapshot onto a Firecracker backend (or v.v.).
    #[error("Cannot restore a '{snapshot_backend}' snapshot on a '{target_backend}' backend")]
    CrossBackendRestore {
        snapshot_backend: String,
        target_backend: String,
    },

    /// The backend does not support snapshots (for example, Local or
    /// microsandbox).
    #[error(
        "Snapshot is not supported by backend '{backend}'; \
         use a 'firecracker' or 'docker' sandbox to take snapshots"
    )]
    NotSupported { backend: String },

    /// Snapshots are not available yet for sandboxes on worker nodes
    /// (ADR-048 phase 2). Refused before anything touches the sandbox.
    #[error(
        "Snapshots are not available yet for sandboxes on worker nodes (this sandbox runs on node '{node}'). \
         Create the sandbox on the control plane (`--node control-plane`) to snapshot it, on an install whose control plane runs workloads itself."
    )]
    NotOnWorkerNode { node: String },

    /// A snapshot for this user is already in progress (`creating` status).
    ///
    /// Only one snapshot per user may be in flight at a time to prevent the
    /// TOCTOU race in the quota check (creating rows have size_bytes = 0 until
    /// they finalize, so multiple concurrent creates could bypass the byte cap).
    /// The caller should wait for the in-flight snapshot to reach `ready` or
    /// `failed` before retrying.
    #[error(
        "A snapshot for user {user_id} is already in progress; \
         wait for it to reach 'ready' or 'failed' before creating another"
    )]
    SnapshotInProgress { user_id: i32 },

    /// The user's total snapshot storage would exceed their quota.
    #[error(
        "Snapshot quota exceeded for user {user_id}: used {used_bytes} bytes, \
         quota {quota_bytes} bytes"
    )]
    QuotaExceeded {
        user_id: i32,
        used_bytes: u64,
        quota_bytes: u64,
    },

    /// The credential-scrubbing step failed or the verification rejected the
    /// committed image because a sensitive key survived.
    #[error("Snapshot scrub failed for sandbox {sandbox_id}: {reason}")]
    ScrubFailed { sandbox_id: String, reason: String },

    /// The artifact's content digest doesn't match what was expected.
    #[error("Digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    /// The artifact file is missing from disk (stale or never written).
    #[error("Snapshot artifact missing at path: {path}")]
    ArtifactMissing { path: String },

    /// The persisted companion-artifact metadata is malformed.
    #[error("Snapshot {snapshot_id} has invalid artifact metadata: {reason}")]
    InvalidArtifactMetadata { snapshot_id: String, reason: String },

    /// A snapshot predates the integrity/workspace metadata required by the
    /// current restore implementation. It remains deletable but cannot be
    /// restored safely or without silently losing workspace state.
    #[error("Snapshot {snapshot_id} uses an unsupported legacy artifact format: {reason}")]
    LegacyArtifactUnsupported { snapshot_id: String, reason: String },

    /// The source sandbox doesn't exist, was already destroyed, or its
    /// provider handle could not be recovered (for example the VM is not
    /// running). `reason` carries the registry's own explanation.
    #[error("Sandbox {sandbox_id} not found: {reason}")]
    SandboxNotFound { sandbox_id: String, reason: String },

    /// The sandbox is not running — snapshots require a live container.
    ///
    /// v1 only supports snapshotting running sandboxes. Resume the sandbox
    /// first (PUT /v1/sandboxes/{id}/resume), then retry the snapshot.
    #[error(
        "Sandbox {sandbox_id} is not running; snapshots require a running sandbox \
         — resume it first"
    )]
    SandboxNotRunning { sandbox_id: String },

    /// The sandbox is in a state that doesn't allow snapshotting.
    #[error("Sandbox {sandbox_id} is in state '{state}' — cannot {operation}")]
    InvalidState {
        sandbox_id: String,
        state: String,
        operation: String,
    },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Provider error: {0}")]
    Provider(#[from] AgentError),
}

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum SandboxError {
    /// The requested sandbox does not exist or has been destroyed.
    #[error("Sandbox {sandbox_id} not found")]
    NotFound { sandbox_id: String },

    /// The sandbox row exists (it is listed, and owned by the caller) but its
    /// VM or container is not running and the provider cannot serve the
    /// request: the VMM exited, was killed together with the Temps service,
    /// or the host rebooted, or the provider no longer has the VM's files.
    /// Distinct from [`SandboxError::NotFound`], which is only for ids that do
    /// not exist, so the caller is told what happened and how to recover
    /// instead of being sent looking for a sandbox that is still listed.
    /// `backend` is `None` when the provider lost the VM entirely and
    /// there is no handle to read it from. Mapped to HTTP 409.
    #[error(
        "Sandbox {sandbox_id} exists, but its {} is not running: {reason}. {}",
        vm_label(backend.as_deref()),
        recovery.advice(sandbox_id)
    )]
    VmNotRunning {
        sandbox_id: String,
        backend: Option<String>,
        reason: String,
        recovery: VmRecovery,
    },

    /// A background job tracked by [`JobTracker`] was not found in the
    /// requested sandbox. Separate from `NotFound` so callers can tell the
    /// difference between "wrong sandbox id" and "wrong job id".
    #[error("Job {job_id} not found in sandbox {sandbox_id}")]
    JobNotFound { sandbox_id: String, job_id: String },

    /// The underlying provider failed to create the container.
    #[error("Failed to create sandbox for user {user_id}: {reason}")]
    CreateFailed { user_id: i32, reason: String },

    /// Command execution failed inside the sandbox (spawn/attach failure,
    /// not a non-zero exit — a non-zero exit returns `ExecResult` with the
    /// code, it is not an error).
    #[error("Exec failed in sandbox {sandbox_id}: {reason}")]
    ExecFailed { sandbox_id: String, reason: String },

    /// A filesystem operation failed against a path inside the sandbox.
    /// `op` is one of "read" | "write" | "stat" | "mkdir".
    #[error("FS {op} failed for '{path}' in sandbox {sandbox_id}: {reason}")]
    FileOp {
        sandbox_id: String,
        op: String,
        path: String,
        reason: String,
    },

    /// Input validation failed (empty name, invalid timeout, non-absolute
    /// path, etc.). Mapped to HTTP 400.
    #[error("Validation error: {message}")]
    Validation { message: String },

    /// The container runtime could not pull the sandbox image: the tag is
    /// not published, the registry is unreachable or access was denied.
    /// Mapped to HTTP 503: the request is valid and retrying can succeed once
    /// the image is available, but nothing about the request itself can fix it.
    #[error("Sandbox image '{image}' could not be pulled for sandbox {sandbox_id}: {reason}")]
    ImageUnavailable {
        sandbox_id: String,
        image: String,
        reason: String,
    },

    /// The container runtime has no address space left for the sandbox's
    /// isolated network (Docker: every `default-address-pools` range is
    /// subnetted). A capacity limit of the host, not a fault of the request
    /// and not a server bug: nothing about the request can fix it, but
    /// deleting unused sandboxes or widening the pools does. Mapped to HTTP
    /// 503 with that guidance in `reason`.
    #[error("Sandbox {sandbox_id} could not get an isolated network: {reason}")]
    NetworkCapacityExhausted { sandbox_id: String, reason: String },

    /// The sandbox exists but is not in a state that allows the requested
    /// operation (e.g. exec on a stopped sandbox). Mapped to HTTP 409.
    #[error("Sandbox {sandbox_id} is in state '{state}' — cannot {operation}")]
    InvalidState {
        sandbox_id: String,
        state: String,
        operation: String,
    },

    /// The sandbox row is attributed to an agent run (`agent_run_id` is
    /// set). Its container is named `temps-sandbox-<run_id>` — not by the
    /// row's public id — and the run owns the lifecycle, so standalone
    /// lifecycle ops (pause/resume/restart/resize, and destroy while the
    /// run is active) must not act on it: the registry would miss the real
    /// container and the DB row would drift from reality. Mapped to
    /// HTTP 409 telling the user to stop/cancel the run instead.
    #[error(
        "Sandbox {sandbox_id} belongs to agent run {run_id} and its lifecycle is managed by that run — stop or cancel agent run {run_id} instead"
    )]
    ManagedByAgentRun { sandbox_id: String, run_id: i32 },

    /// A workspace was requested for a project that doesn't exist (or was
    /// soft-deleted). Mapped to HTTP 404.
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    /// The project exists but has no git repository attached, so there is
    /// nothing to clone into the workspace. Mapped to HTTP 400 — the fix
    /// is to connect a repo to the project, or to pass an explicit
    /// `source`, and the message says so.
    #[error(
        "Project {project_id} ('{name}') has no git repository connected — connect a repo to the project, or pass an explicit `source` to seed the sandbox from a different URL"
    )]
    ProjectHasNoRepo { project_id: i32, name: String },

    /// An operation exceeded the per-sandbox timeout.
    #[error("Operation timed out in sandbox {sandbox_id} after {timeout_secs}s")]
    Timeout {
        sandbox_id: String,
        timeout_secs: u64,
    },

    /// Required plumbing is missing at runtime. Indicates a deployment
    /// misconfiguration (e.g. no Docker, no SandboxProvider registered).
    /// Mapped to HTTP 503.
    #[error("Sandbox subsystem unavailable: {reason}")]
    Unavailable { reason: String },

    /// A sandbox's worker node could not destroy it: unreachable, or it did
    /// not answer in time. The sandbox is kept (ADR-048 §7), so its
    /// container is not left running with nothing tracking it. Mapped to 503.
    #[error("Sandbox {sandbox_id} was not destroyed: its node '{node}' is unreachable ({reason}). It is kept so its container is not left running untracked. Try again once the node is back. If the node is gone for good, an administrator can destroy every sandbox on it from the node's Sandboxes tab or with `bunx @temps-sdk/cli sandbox nodes evict {node}`.")]
    NodeUnreachable {
        sandbox_id: String,
        node: String,
        reason: String,
    },

    /// The sandbox runs on a worker node, which cannot serve this feature
    /// yet (ADR-048 phase 1). The request is well-formed; it just cannot be
    /// served where the sandbox lives. Mapped to HTTP 422.
    #[error(
        "{feature} is not available yet for sandboxes on worker nodes (sandbox {sandbox_id} runs on node '{node_name}'). Create the sandbox on the control plane (`--node control-plane`) to use it, on an install whose control plane runs workloads itself."
    )]
    UnsupportedOnWorkerNode {
        sandbox_id: String,
        node_name: String,
        feature: String,
    },

    /// The sandbox's backend cannot enforce a `restricted` egress policy
    /// (only Docker sandboxes can). The request is well-formed; it just
    /// cannot be honoured where the sandbox runs. Mapped to HTTP 422.
    #[error(
        "Sandbox {sandbox_id} runs on the {backend} backend, which cannot restrict egress to an allow-list of hosts and CIDR blocks. Use egress mode 'internet' for it, or create the sandbox on the docker backend."
    )]
    EgressPolicyUnsupported { sandbox_id: String, backend: String },

    /// The sandbox's worker node refused the operation because it would
    /// replace a live sandbox there (ADR-048). Mapped to HTTP 409.
    #[error("Sandbox {sandbox_id} conflicts with existing state on its worker node: {reason}")]
    NodeConflict { sandbox_id: String, reason: String },

    /// The requested node does not exist (ADR-048).
    #[error("Node '{node}' does not exist. List the nodes that can run sandboxes with `bunx @temps-sdk/cli sandbox nodes`.")]
    NodeNotFound { node: String },

    /// The requested node exists but the operator excluded it from sandbox
    /// placement (ADR-048).
    #[error("Node '{node}' is not allowed to run sandboxes. An administrator can allow it under AI Workflows → Sandbox (/agent-sandbox/sandbox) or with `bunx @temps-sdk/cli sandbox nodes allow`.")]
    NodeNotAllowed { node: String },

    /// The requested node is allowed but not accepting new sandboxes
    /// (offline, draining, pending enrollment).
    #[error("Node '{node}' is {status} and cannot take new sandboxes right now. Pick another node or wait until it is active.")]
    NodeNotReady { node: String, status: String },

    /// This server is a control plane that runs no workloads, and what was
    /// asked for needs its own host (ADR 063): a sandbox explicitly on the
    /// control plane, an application workspace, or an operation on a row
    /// that has no worker node. Mapped to 409.
    #[error("{message}")]
    NoLocalHost { message: String },

    /// Automatic placement on a control plane that runs no workloads found
    /// no worker that can take the sandbox (ADR 063). `detail` says whether
    /// none has joined or names each worker and why it was passed over.
    /// Mapped to the worker-node-required problem (409, Settings → Nodes).
    #[error(
        "No node can run this sandbox: this control plane runs no workloads, so sandboxes run \
         only on worker nodes, and {detail}"
    )]
    NoWorkerNodeForSandboxes { detail: String },

    /// No node is eligible for automatic placement (ADR-048).
    #[error("No node can run sandboxes: every allowed node is offline or the allow-list is empty. Allow a node under AI Workflows → Sandbox (/agent-sandbox/sandbox) or with `bunx @temps-sdk/cli sandbox nodes allow`.")]
    NoPlacementNode,

    /// Automatic placement found only dedicated nodes
    /// (`temps.sh/role=dedicated`), which take a sandbox only when it asks
    /// for them by id or name.
    #[error("No node can take this sandbox automatically: {nodes}. Request a dedicated node explicitly by passing its id or name as `node`, or allow a node without the temps.sh/role=dedicated label under AI Workflows → Sandbox (/agent-sandbox/sandbox) or with `bunx @temps-sdk/cli sandbox nodes allow`.")]
    DedicatedNodesOnly {
        /// The dedicated nodes passed over, formatted as
        /// `node <id> (<name>) is dedicated …`.
        nodes: String,
    },

    /// The sandbox is attached to a project but that project has no active
    /// environment from which scoped service credentials can be issued.
    #[error("Project {project_id} attached to sandbox {sandbox_id} has no active environment")]
    RuntimeEnvironmentNotFound { sandbox_id: String, project_id: i32 },

    /// Provider credential resolution failed. The reason must never contain
    /// credential values; provider implementations return contextual metadata
    /// only.
    #[error("Failed to prepare runtime variables for sandbox {sandbox_id}, service {service_id}: {reason}")]
    RuntimeCredentialsFailed {
        sandbox_id: String,
        service_id: i32,
        reason: String,
    },

    /// A service a sandbox was asked to link to does not exist.
    #[error("Cannot link sandbox to service {service_id}: no such service")]
    ServiceNotFound { service_id: i32 },

    /// Two linked services exposed the same variable name with different
    /// values. Silently choosing one would connect application code to the
    /// wrong database.
    #[error("Sandbox {sandbox_id} has ambiguous runtime variable '{variable}' from multiple linked services")]
    RuntimeVariableConflict {
        sandbox_id: String,
        variable: String,
    },

    /// Hashing the preview password failed. Argon2 returns errors only for
    /// catastrophic platform issues (OS RNG failure etc.), so this maps to
    /// HTTP 500.
    #[error("Failed to hash preview password for sandbox {sandbox_id}: {reason}")]
    PasswordHashFailed { sandbox_id: String, reason: String },

    /// Minting the encrypted preview credential failed. This is an internal
    /// crypto/clock failure, never a client validation error.
    #[error("Failed to mint preview grant for sandbox {sandbox_id}: {source}")]
    PreviewGrantFailed {
        sandbox_id: String,
        #[source]
        source: temps_core::PreviewGrantError,
    },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Automatic placement found allowed, active nodes, but none of them
    /// could run a sandbox when asked (ADR-048). `reasons` names each node
    /// tried and why it failed. Mapped to 422.
    #[error("No node can run sandboxes right now: {reasons}. Fix the node (or allow another one under AI Workflows → Sandbox (/agent-sandbox/sandbox) or with `bunx @temps-sdk/cli sandbox nodes allow`), then try again.")]
    NoReadyPlacementNode { reasons: String },

    /// Another eviction of the same node is still running (ADR-048).
    /// Mapped to 409.
    #[error("Sandboxes on node '{node}' are already being evicted. Wait for that eviction to finish, then check the node's Sandboxes tab and run the eviction again if any are left.")]
    NodeEvictionInProgress { node: String },

    /// The platform settings holding the sandbox placement allow-list could
    /// not be read or written. Mapped to 500.
    #[error("Could not {operation} the sandbox placement settings: {source}")]
    PlacementSettings {
        operation: &'static str,
        #[source]
        source: temps_config::ConfigServiceError,
    },

    /// The platform settings holding the sandbox limits could not be read, so
    /// a create request cannot be checked against them. Fails closed rather
    /// than creating a sandbox no limit was applied to. Mapped to 500.
    #[error("Could not load the sandbox limit settings to check a create request from user {user_id}: {source}")]
    LimitSettings {
        user_id: i32,
        #[source]
        source: temps_config::ConfigServiceError,
    },

    /// The user already has as many sandboxes as the operator allows
    /// (`agent_sandbox.max_sandboxes_per_user`). Destroying one frees a slot.
    /// Mapped to 422.
    #[error("User {user_id} already has {live} sandbox(es) and the limit is {limit}; destroy one to create another")]
    UserSandboxLimitReached { user_id: i32, limit: u32, live: u64 },
}

impl SandboxError {
    /// An explicit request for the control plane on a server that runs no
    /// workloads (ADR 063).
    pub fn control_plane_requested_without_workloads() -> Self {
        SandboxError::NoLocalHost {
            message: "This control plane runs no workloads, so a sandbox cannot run on it \
                      (node 'control-plane'). Leave `node` empty to place the sandbox on a \
                      worker node, or name one (list them with `bunx @temps-sdk/cli sandbox \
                      nodes`)."
                .to_string(),
        }
    }

    /// `operation` needs this server's own host, which runs nothing (ADR 063).
    pub fn no_local_host(operation: &str) -> Self {
        SandboxError::NoLocalHost {
            message: AgentError::NoLocalSandboxHost {
                operation: operation.to_string(),
            }
            .to_string(),
        }
    }
}

/// How to get a sandbox whose VM is not running back, which depends on what
/// the provider reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmRecovery {
    /// The provider found the VM and it is down: its disk is intact and
    /// `sandbox resume` boots it again.
    Resume,
    /// The provider could not find the VM, or could not check it. Whether the
    /// VM's files still exist is unknown, so the advice does not promise a
    /// resume or a restart (a failed `resume` also lands here, and `restart`
    /// refuses a stopped row), and does not tell the caller to delete it: a
    /// guest that is merely slow to answer lands here too. Retry, then look at
    /// the server logs; delete only if the sandbox is not needed.
    RestartOrRecreate,
}

impl VmRecovery {
    fn advice(self, sandbox_id: &str) -> String {
        match self {
            VmRecovery::Resume => format!(
                "Run `bunx @temps-sdk/cli sandbox resume {sandbox_id}` to boot it again with its disk intact. \
                 If that fails because the VM's files are gone, delete it with \
                 `bunx @temps-sdk/cli sandbox rm {sandbox_id}` and create a new one."
            ),
            VmRecovery::RestartOrRecreate => format!(
                "Retry in a moment. If it keeps failing, check the Temps server logs for sandbox \
                 {sandbox_id}: its VM may be gone, or its guest may not be answering. If you no \
                 longer need its disk, delete it with `bunx @temps-sdk/cli sandbox rm {sandbox_id}` \
                 and create a new one."
            ),
        }
    }
}

/// What the error message calls the thing that is not running.
fn vm_label(backend: Option<&str>) -> String {
    match backend {
        Some(backend) => format!("{backend} VM"),
        None => "VM".to_string(),
    }
}

/// Translate a lower-level `AgentError` from the shared `SandboxProvider`
/// into a `SandboxError` with the standalone sandbox ID attached. The
/// provider's errors are keyed by `run_id` (an internal numeric id) — the
/// caller is responsible for passing the public `sandbox_id` so users see
/// the opaque ID, not the internal integer.
pub fn from_agent_error(sandbox_id: &str, err: AgentError) -> SandboxError {
    if let Some((image, reason)) = err.image_pull_failure() {
        return SandboxError::ImageUnavailable {
            sandbox_id: sandbox_id.to_string(),
            image: image.to_string(),
            reason: reason.to_string(),
        };
    }
    if let Some(reason) = err.network_pool_exhaustion() {
        let reason = if reason.contains("default-address-pools") {
            reason.to_string()
        } else {
            temps_agents::error::network_pool_exhausted_reason(sandbox_id, reason)
        };
        return SandboxError::NetworkCapacityExhausted {
            sandbox_id: sandbox_id.to_string(),
            reason,
        };
    }
    match err {
        // Every caller reaches the provider with a sandbox row already
        // loaded (an id with no row is `NotFound` before the provider is
        // asked), so a provider that cannot find the VM means the sandbox
        // exists but has nothing running behind it: a 409 with the way back,
        // not a 404 for an id that is still listed.
        AgentError::SandboxNotFound { sandbox, .. } => SandboxError::VmNotRunning {
            sandbox_id: sandbox_id.to_string(),
            backend: None,
            reason: format!("the provider cannot find it ({sandbox})"),
            recovery: VmRecovery::RestartOrRecreate,
        },
        AgentError::SandboxNotRunning {
            backend, reason, ..
        } => SandboxError::VmNotRunning {
            sandbox_id: sandbox_id.to_string(),
            backend: Some(backend),
            reason,
            recovery: VmRecovery::Resume,
        },
        AgentError::SandboxExecFailed { reason, .. } => SandboxError::ExecFailed {
            sandbox_id: sandbox_id.to_string(),
            reason,
        },
        limit_error @ AgentError::SnapshotSizeLimitExceeded { .. } => SandboxError::Validation {
            message: limit_error.to_string(),
        },
        // A request the provider (or a worker node, ADR-048) rejected as
        // invalid is the caller's error, not a failed exec.
        AgentError::Validation { message } => SandboxError::Validation { message },
        AgentError::Io(e) => SandboxError::Io(e),
        // ADR-048: a feature sandboxes on worker nodes cannot serve yet — a
        // 422 naming the feature and node, not a failed exec.
        AgentError::SandboxUnsupportedOnNode {
            node_name, feature, ..
        } => SandboxError::UnsupportedOnWorkerNode {
            sandbox_id: sandbox_id.to_string(),
            node_name,
            feature,
        },
        // A restricted egress policy on a backend that cannot enforce one: a
        // 422 naming the backend, never a silently wider policy.
        AgentError::SandboxEgressPolicyUnsupported { backend, .. } => {
            SandboxError::EgressPolicyUnsupported {
                sandbox_id: sandbox_id.to_string(),
                backend,
            }
        }
        // ADR 063: this host runs nothing; a 409 with what is possible instead.
        no_host @ AgentError::NoLocalSandboxHost { .. } => SandboxError::NoLocalHost {
            message: no_host.to_string(),
        },
        // ADR-048: the worker refused to replace a live sandbox — a 409, and
        // nothing is torn down for it.
        conflict @ AgentError::SandboxConflictOnNode { .. } => SandboxError::NodeConflict {
            sandbox_id: sandbox_id.to_string(),
            reason: conflict.to_string(),
        },
        // ADR-048: the worker hosting the sandbox is offline or unreachable —
        // a 503 with the node named, not a generic exec failure.
        node_error @ (AgentError::SandboxNodeUnavailable { .. }
        | AgentError::SandboxProviderUnavailable { .. }) => SandboxError::Unavailable {
            reason: node_error.to_string(),
        },
        other => SandboxError::ExecFailed {
            sandbox_id: sandbox_id.to_string(),
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unenforceable_egress_policy_is_a_422_naming_the_backend() {
        let err = from_agent_error(
            "sbx_abc",
            AgentError::SandboxEgressPolicyUnsupported {
                sandbox: "temps-sandbox-abc".into(),
                backend: "firecracker".into(),
            },
        );
        assert!(
            matches!(&err, SandboxError::EgressPolicyUnsupported { sandbox_id, backend }
                if sandbox_id == "sbx_abc" && backend == "firecracker"),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(
            text.contains("sbx_abc") && text.contains("firecracker"),
            "{text}"
        );
        let problem = temps_core::problemdetails::Problem::from(err);
        assert_eq!(
            problem.status_code,
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn worker_conflicts_are_409s_not_failures() {
        let err = from_agent_error(
            "sbx_abc",
            AgentError::SandboxConflictOnNode {
                sandbox: "temps-sandbox-abc".into(),
                node_name: "worker-1".into(),
                reason: "already exists and is running".into(),
            },
        );
        assert!(
            matches!(&err, SandboxError::NodeConflict { sandbox_id, reason }
                if sandbox_id == "sbx_abc" && reason.contains("worker-1")),
            "{err:?}"
        );
        let problem = temps_core::problemdetails::Problem::from(err);
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn image_pull_failures_become_image_unavailable_with_image_and_cause() {
        let err = from_agent_error(
            "sbx_abc",
            AgentError::SandboxCreationFailed {
                run_id: 3,
                provider: "docker".into(),
                reason: temps_agents::error::image_pull_failure_reason(
                    "registry.example.test/team/img:9.9.9",
                    "manifest unknown",
                ),
            },
        );
        match &err {
            SandboxError::ImageUnavailable {
                sandbox_id,
                image,
                reason,
            } => {
                assert_eq!(sandbox_id, "sbx_abc");
                assert_eq!(image, "registry.example.test/team/img:9.9.9");
                assert_eq!(reason, "manifest unknown");
            }
            other => panic!("expected ImageUnavailable, got {other:?}"),
        }
        let message = err.to_string();
        assert!(message.contains("registry.example.test/team/img:9.9.9"));
        assert!(message.contains("sbx_abc"));
        assert!(message.contains("manifest unknown"));
    }

    #[test]
    fn other_creation_failures_stay_generic_exec_failures() {
        let err = from_agent_error(
            "sbx_abc",
            AgentError::SandboxCreationFailed {
                run_id: 3,
                provider: "docker".into(),
                reason: "no space left on device".into(),
            },
        );
        assert!(matches!(err, SandboxError::ExecFailed { .. }), "{err:?}");
    }

    #[test]
    fn provider_validation_errors_stay_validation_errors() {
        let err = from_agent_error(
            "sbx_abc",
            AgentError::Validation {
                message: "bad path".into(),
            },
        );
        assert!(matches!(err, SandboxError::Validation { ref message } if message == "bad path"));
    }

    #[test]
    fn not_found_message_includes_id() {
        let err = SandboxError::NotFound {
            sandbox_id: "sbx_abc123".into(),
        };
        assert_eq!(err.to_string(), "Sandbox sbx_abc123 not found");
    }

    #[test]
    fn job_not_found_distinguishes_sandbox_from_job() {
        let err = SandboxError::JobNotFound {
            sandbox_id: "sbx_abc".into(),
            job_id: "job_xyz".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("sbx_abc"), "msg: {}", msg);
        assert!(msg.contains("job_xyz"), "msg: {}", msg);
    }

    #[test]
    fn file_op_message_includes_op_and_path() {
        let err = SandboxError::FileOp {
            sandbox_id: "sbx_a".into(),
            op: "read".into(),
            path: "/etc/hosts".into(),
            reason: "no such file".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("read"), "missing op: {}", msg);
        assert!(msg.contains("/etc/hosts"), "missing path: {}", msg);
        assert!(msg.contains("no such file"), "missing reason: {}", msg);
    }

    #[test]
    fn invalid_state_names_state_and_operation() {
        let err = SandboxError::InvalidState {
            sandbox_id: "sbx_a".into(),
            state: "stopped".into(),
            operation: "exec".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("stopped"));
        assert!(msg.contains("exec"));
    }

    #[test]
    fn managed_by_agent_run_names_sandbox_and_run() {
        let err = SandboxError::ManagedByAgentRun {
            sandbox_id: "sbx_abc".into(),
            run_id: 42,
        };
        let msg = err.to_string();
        assert!(msg.contains("sbx_abc"), "msg: {}", msg);
        assert!(msg.contains("42"), "msg: {}", msg);
        // The whole point of this variant: tell the user what to do instead.
        assert!(msg.contains("stop or cancel"), "msg: {}", msg);
    }

    /// A provider that cannot find the VM of a sandbox whose row exists is
    /// not "not found": the id is valid and listed. It is a 409 that names
    /// the public id (never the internal run id) and how to recover.
    #[test]
    fn provider_not_found_for_an_existing_row_is_vm_not_running() {
        let agent = AgentError::SandboxNotFound {
            run_id: 42,
            sandbox: "temps-sandbox-test".into(),
        };
        let err = from_agent_error("sbx_public", agent);
        assert!(
            matches!(&err, SandboxError::VmNotRunning { sandbox_id, backend: None, .. }
                if sandbox_id == "sbx_public"),
            "{err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("sbx_public"), "{msg}");
        assert!(msg.contains("temps-sandbox-test"), "{msg}");
        // A failed `resume` lands here too, so it must not send the caller
        // back to `resume`, nor to `restart` (which refuses a stopped row).
        assert!(!msg.contains("sandbox resume"), "{msg}");
        assert!(!msg.contains("sandbox restart"), "{msg}");
        assert!(msg.contains("sandbox rm sbx_public"), "{msg}");
        // Unknown liveness lands here too: the advice must not claim the
        // VM's files are gone.
        assert!(msg.contains("server logs"), "{msg}");
        assert!(!msg.contains("files are gone"), "{msg}");
        assert!(!msg.contains("42"), "internal run id leaked: {msg}");
    }

    #[test]
    fn not_running_vm_keeps_the_backend_and_the_liveness_reason() {
        let err = from_agent_error(
            "sbx_public",
            AgentError::SandboxNotRunning {
                run_id: 42,
                sandbox_id: "sbx_internal".into(),
                container: "temps-sandbox-public".into(),
                backend: "firecracker".into(),
                reason: "its firecracker process exited".into(),
            },
        );
        match &err {
            SandboxError::VmNotRunning {
                sandbox_id,
                backend,
                reason,
                recovery,
            } => {
                assert_eq!(sandbox_id, "sbx_public");
                assert_eq!(backend.as_deref(), Some("firecracker"));
                assert_eq!(reason, "its firecracker process exited");
                assert_eq!(*recovery, VmRecovery::Resume);
            }
            other => panic!("expected VmNotRunning, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("sbx_public exists"), "{msg}");
        assert!(msg.contains("firecracker VM is not running"), "{msg}");
        assert!(msg.contains("its firecracker process exited"), "{msg}");
        assert!(msg.contains("sandbox resume sbx_public"), "{msg}");
    }

    #[test]
    fn vm_not_running_is_a_409_problem_and_unknown_ids_stay_404() {
        let not_running = temps_core::problemdetails::Problem::from(SandboxError::VmNotRunning {
            sandbox_id: "sbx_a".into(),
            backend: Some("microsandbox".into()),
            reason: "its microsandbox process exited".into(),
            recovery: VmRecovery::Resume,
        });
        assert_eq!(not_running.status_code, axum::http::StatusCode::CONFLICT);
        assert_eq!(
            not_running.body.get("type").and_then(|v| v.as_str()),
            Some("https://temps.sh/probs/sandbox-vm-not-running")
        );
        assert_eq!(
            not_running.body.get("title").and_then(|v| v.as_str()),
            Some("Sandbox VM Not Running")
        );
        let detail = not_running
            .body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            detail.contains("sbx_a") && detail.contains("microsandbox VM"),
            "{detail}"
        );

        let missing = temps_core::problemdetails::Problem::from(SandboxError::NotFound {
            sandbox_id: "sbx_nope".into(),
        });
        assert_eq!(missing.status_code, axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn from_agent_error_preserves_exec_failure() {
        let agent = AgentError::SandboxExecFailed {
            run_id: 1,
            sandbox_id: "internal".into(),
            reason: "container died".into(),
        };
        let err = from_agent_error("sbx_pub", agent);
        match err {
            SandboxError::ExecFailed {
                sandbox_id, reason, ..
            } => {
                assert_eq!(sandbox_id, "sbx_pub");
                assert!(reason.contains("container died"));
            }
            other => panic!("expected ExecFailed, got {:?}", other),
        }
    }

    #[test]
    fn unsupported_on_node_keeps_the_public_id_node_and_feature() {
        let err = from_agent_error(
            "sbx_pub",
            AgentError::SandboxUnsupportedOnNode {
                sandbox_id: "temps-sandbox-abc".into(),
                node_name: "worker-2".into(),
                feature: "The interactive terminal".into(),
            },
        );
        match &err {
            SandboxError::UnsupportedOnWorkerNode {
                sandbox_id,
                node_name,
                feature,
            } => {
                assert_eq!(sandbox_id, "sbx_pub");
                assert_eq!(node_name, "worker-2");
                assert_eq!(feature, "The interactive terminal");
            }
            other => panic!("expected UnsupportedOnWorkerNode, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("sbx_pub"), "{msg}");
        assert!(msg.contains("worker-2"), "{msg}");
        assert!(!msg.contains("temps-sandbox-abc"), "{msg}");
    }

    #[test]
    fn unsupported_on_worker_node_is_a_422_problem_with_its_own_type() {
        let problem =
            temps_core::problemdetails::Problem::from(SandboxError::UnsupportedOnWorkerNode {
                sandbox_id: "sbx_pub".into(),
                node_name: "worker-2".into(),
                feature: "Disk resize".into(),
            });
        assert_eq!(
            problem.status_code,
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            problem.body.get("type").and_then(|v| v.as_str()),
            Some("https://temps.sh/probs/sandbox-unsupported-on-worker-node")
        );
        let detail = problem
            .body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(detail.contains("Disk resize") && detail.contains("worker-2"));
    }

    #[test]
    fn no_local_host_errors_are_typed_conflicts() {
        let err = from_agent_error(
            "sbx_x",
            AgentError::NoLocalSandboxHost {
                operation: "An application workspace".into(),
            },
        );
        assert!(matches!(err, SandboxError::NoLocalHost { .. }), "{err:?}");
        let problem = temps_core::problemdetails::Problem::from(err);
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
        assert_eq!(
            problem.body["error_code"],
            temps_agents::error::NO_LOCAL_SANDBOX_HOST_ERROR_CODE
        );
        assert!(!problem.body.contains_key("setup_path"));

        let problem = temps_core::problemdetails::Problem::from(
            SandboxError::control_plane_requested_without_workloads(),
        );
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn no_worker_node_is_the_worker_node_required_problem() {
        let problem =
            temps_core::problemdetails::Problem::from(SandboxError::NoWorkerNodeForSandboxes {
                detail: "no worker node has joined it yet".into(),
            });
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
        assert_eq!(
            problem.body["error_code"],
            temps_core::WORKER_NODE_REQUIRED_ERROR_CODE
        );
        assert_eq!(
            problem.body["setup_path"],
            temps_core::WORKER_NODE_SETUP_PATH
        );
        let detail = problem.body["detail"].as_str().unwrap();
        assert!(
            detail.contains("runs no workloads, so sandboxes run only on worker nodes"),
            "{detail}"
        );
        assert!(
            detail.contains("no worker node has joined it yet"),
            "{detail}"
        );
        assert!(
            detail.contains(temps_core::WORKER_NODE_REQUIRED_REMEDY),
            "{detail}"
        );
    }

    #[test]
    fn from_agent_error_catchall_becomes_exec_failed() {
        let agent = AgentError::AiCliNotInstalled {
            provider: "claude".into(),
        };
        let err = from_agent_error("sbx_x", agent);
        assert!(matches!(err, SandboxError::ExecFailed { .. }));
    }

    #[test]
    fn exhausted_address_pools_map_to_a_capacity_error_with_guidance() {
        let err = from_agent_error(
            "sbx_abc",
            AgentError::SandboxProviderUnavailable {
                provider: "docker".into(),
                reason: "create isolated network for sandbox 'temps-sandbox-abc': Docker \
                         responded with status code 400: all predefined address pools have \
                         been fully subnetted"
                    .into(),
            },
        );
        match &err {
            SandboxError::NetworkCapacityExhausted { sandbox_id, reason } => {
                assert_eq!(sandbox_id, "sbx_abc");
                assert!(reason.contains("default-address-pools"), "{reason}");
                assert!(reason.contains("Stop sandboxes"), "{reason}");
            }
            other => panic!("expected NetworkCapacityExhausted, got {other:?}"),
        }
    }

    #[test]
    fn provider_reason_that_already_carries_guidance_is_not_wrapped_twice() {
        let reason = temps_agents::error::network_pool_exhausted_reason(
            "temps-sandbox-abc",
            "all predefined address pools have been fully subnetted",
        );
        let err = from_agent_error(
            "sbx_abc",
            AgentError::SandboxCreationFailed {
                run_id: 7,
                provider: "docker".into(),
                reason: reason.clone(),
            },
        );
        match err {
            SandboxError::NetworkCapacityExhausted { reason: got, .. } => assert_eq!(got, reason),
            other => panic!("expected NetworkCapacityExhausted, got {other:?}"),
        }
    }

    #[test]
    fn other_provider_unavailability_stays_a_generic_unavailable() {
        let err = from_agent_error(
            "sbx_abc",
            AgentError::SandboxProviderUnavailable {
                provider: "docker".into(),
                reason: "daemon unreachable".into(),
            },
        );
        assert!(matches!(err, SandboxError::Unavailable { .. }), "{err:?}");
    }
}
