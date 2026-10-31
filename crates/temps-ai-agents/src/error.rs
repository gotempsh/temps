// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Autopilot config not found for project {project_id}")]
    ConfigNotFound { project_id: i32 },

    #[error("Agent '{slug}' not found in project {project_id}")]
    AgentNotFound { project_id: i32, slug: String },

    #[error("Autopilot run {run_id} not found")]
    RunNotFound { run_id: i32 },

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Daily budget exceeded for project {project_id}: spent {spent_cents} of {daily_limit_cents} cents")]
    BudgetExceeded {
        project_id: i32,
        daily_limit_cents: i32,
        spent_cents: i32,
    },

    #[error("Cooldown active for project {project_id}: {minutes_remaining} minutes remaining")]
    CooldownActive {
        project_id: i32,
        minutes_remaining: i32,
    },

    #[error("AI CLI '{provider}' is not installed or not found in PATH")]
    AiCliNotInstalled { provider: String },

    #[error("AI CLI '{provider}' failed with exit code {exit_code}: {stderr}")]
    AiCliFailed {
        provider: String,
        exit_code: i32,
        stderr: String,
    },

    #[error("AI CLI '{provider}' reported an error: {message}")]
    AiCliReportedError { provider: String, message: String },

    #[error("AI CLI '{provider}' timed out after {timeout_secs} seconds")]
    AiCliTimeout { provider: String, timeout_secs: u64 },

    #[error("Git operation failed: {message}")]
    GitError { message: String },

    #[error("Encryption error: {message}")]
    EncryptionError { message: String },

    #[error("Validation error: {message}")]
    Validation { message: String },

    /// An `error_group_id` that does not resolve inside the project the
    /// request names: either no such group exists, or it belongs to another
    /// project. The two cases are deliberately indistinguishable, in the
    /// variant and in its message alike — error group ids are sequential, so
    /// a distinguishable "it exists, elsewhere" answer would let a caller
    /// with ProjectsWrite on any one project enumerate another project's
    /// error groups.
    #[error("Error group {error_group_id} not found in project {project_id}")]
    InvalidErrorGroup {
        error_group_id: i32,
        project_id: i32,
    },

    #[error("Cannot locally rebuild immutable sandbox image '{image}'; choose a mutable tag before rebuilding")]
    ImmutableSandboxImageRebuild { image: String },

    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Sandbox creation failed for run {run_id} ({provider}): {reason}")]
    SandboxCreationFailed {
        run_id: i32,
        provider: String,
        reason: String,
    },

    /// `sandbox` names what was looked up (a container name, a sandbox id,
    /// or the run), so the error says which sandbox is missing even where
    /// there is no run id (`run_id` is 0 for sandboxes outside agent runs).
    #[error("Sandbox not found: {sandbox}")]
    SandboxNotFound { run_id: i32, sandbox: String },

    /// The sandbox's provider state is known (its VM or container exists on
    /// disk or in the runtime) but the VM is not running: the VMM process
    /// exited, was killed with the Temps service, or the host rebooted. Kept
    /// apart from [`AgentError::SandboxNotFound`] so callers can say "this
    /// sandbox exists but is not running" (a 409 with the way back) instead
    /// of "not found". `sandbox_id` is the public id when the caller has one
    /// and `backend` names the isolation backend that lost the VM.
    #[error(
        "Sandbox {sandbox_id} ({backend} backend, container {container}) is not running: {reason}"
    )]
    SandboxNotRunning {
        run_id: i32,
        sandbox_id: String,
        container: String,
        backend: String,
        reason: String,
    },

    /// A guest did not answer a terminal request in time. Kept apart from
    /// [`AgentError::SandboxExecFailed`] so callers can tell "the guest is
    /// stalled" (retry, restart the sandbox: a 504) from "this sandbox has no
    /// terminal support" (a setup problem: a 409 with the fix).
    #[error("Terminal {operation} for sandbox {sandbox_id} did not finish within {timeout_secs}s")]
    SandboxTerminalTimeout {
        sandbox_id: String,
        operation: String,
        timeout_secs: u64,
    },

    #[error("Sandbox exec failed for run {run_id} in sandbox {sandbox_id}: {reason}")]
    SandboxExecFailed {
        run_id: i32,
        sandbox_id: String,
        reason: String,
    },

    #[error(
        "Sandbox snapshot for {sandbox_id} exceeded the {max_size_bytes} byte limit while {stage}"
    )]
    SnapshotSizeLimitExceeded {
        sandbox_id: String,
        stage: String,
        max_size_bytes: u64,
    },

    #[error("Sandbox provider '{provider}' unavailable: {reason}")]
    SandboxProviderUnavailable { provider: String, reason: String },

    #[error("Sandbox node '{node_name}' (id {node_id}) is unavailable: {reason}")]
    SandboxNodeUnavailable {
        node_id: i32,
        node_name: String,
        reason: String,
    },

    /// A sandbox on a worker node was asked for a feature that only works
    /// for sandboxes on the control plane yet (ADR-048 phase 1): the request
    /// is valid, it just cannot be served where this sandbox lives.
    #[error(
        "{feature} is not available yet for sandboxes on worker nodes ({}). \
         Create the sandbox on the control plane to use it (on an install whose \
         control plane runs workloads itself).",
        unsupported_scope(sandbox_id, node_name)
    )]
    SandboxUnsupportedOnNode {
        sandbox_id: String,
        node_name: String,
        feature: String,
    },

    /// Something asked for a sandbox on this server's own host, and this
    /// server is a control plane that runs no workloads (ADR 063). Sandboxes
    /// with a worker node are unaffected; this is what the host half of the
    /// node router answers instead of falling back to an unsandboxed
    /// provider. `operation` names what was attempted.
    #[error(
        "{operation} needs a sandbox on this server, but this control plane runs no \
         workloads (serve profile 'control-plane'), so nothing runs on its own host. \
         Standalone sandboxes run on worker nodes (Settings → Nodes, /settings/nodes); \
         AI agent runs, autofix and application workspaces cannot run on worker nodes \
         yet, so run them on an install whose control plane runs workloads."
    )]
    NoLocalSandboxHost { operation: String },

    /// A worker node refused to create a sandbox because it would replace a
    /// live one (a running sandbox or a create in flight under the same
    /// label). Nothing must be torn down in response: the conflicting
    /// sandbox is not this request's.
    #[error(
        "Sandbox '{sandbox}' conflicts with existing state on worker node '{node_name}': {reason}"
    )]
    SandboxConflictOnNode {
        sandbox: String,
        node_name: String,
        reason: String,
    },

    /// A sandbox asked for a `restricted` egress policy (an allow-list of
    /// hosts and CIDR blocks) on a backend that cannot enforce one. The
    /// default `internet` policy works on every backend.
    #[error(
        "Sandbox '{sandbox}' runs on the {backend} backend, which cannot enforce a restricted \
         egress policy (an allow-list of hosts and CIDR blocks); use the docker backend, or \
         egress mode 'internet'"
    )]
    SandboxEgressPolicyUnsupported { sandbox: String, backend: String },

    #[error("Agent secret '{name}' not found in {scope}")]
    SecretNotFound { name: String, scope: String },

    #[error("Skill definition '{slug}' not found in project {project_id}")]
    SkillDefinitionNotFound { project_id: i32, slug: String },

    #[error("MCP definition '{slug}' not found in project {project_id}")]
    McpDefinitionNotFound { project_id: i32, slug: String },

    #[error("MCP config field '{field}' was not found for server '{slug}'")]
    McpConfigFieldNotFound { slug: String, field: String },

    #[error("A skill with slug '{slug}' already exists{}", scope_label(*project_id))]
    SkillDefinitionAlreadyExists {
        project_id: Option<i32>,
        slug: String,
    },

    #[error("An MCP server with slug '{slug}' already exists{}", scope_label(*project_id))]
    McpDefinitionAlreadyExists {
        project_id: Option<i32>,
        slug: String,
    },

    /// ADR 045: refused a push+PR the agent executor would otherwise make to
    /// a project this host declares (`TEMPS_DOCKER_SOCKET_PROJECTS`). Any
    /// commit landing on the tracked branch is what a later deploy — an
    /// admin's, or the platform's own failover reschedule — runs as host
    /// root, and nothing on this path can prove instance-admin authority the
    /// way an interactive session can, so it fails closed unconditionally
    /// rather than trusting whoever triggered the run.
    #[error(
        "Autopilot cannot push changes to project {slug}: it is granted host Docker access \
         (ADR 045) and this action is admin-only"
    )]
    DockerSocketWriteRequiresAdmin { slug: String },
}

/// RFC 7807 `type` of [`AgentError::NoLocalSandboxHost`] responses (ADR 063).
pub const NO_LOCAL_SANDBOX_HOST_TYPE: &str =
    "https://temps.sh/probs/control-plane-runs-no-workloads";
/// Title of [`AgentError::NoLocalSandboxHost`] responses.
pub const NO_LOCAL_SANDBOX_HOST_TITLE: &str = "This Control Plane Runs No Workloads";
/// Machine-readable `error_code` of [`AgentError::NoLocalSandboxHost`]
/// responses, so a client can branch without parsing prose.
pub const NO_LOCAL_SANDBOX_HOST_ERROR_CODE: &str = "CONTROL_PLANE_RUNS_NO_WORKLOADS";

fn unsupported_scope(sandbox_id: &str, node_name: &str) -> String {
    if sandbox_id.is_empty() {
        format!("node '{}'", node_name)
    } else {
        format!("sandbox {} runs on node '{}'", sandbox_id, node_name)
    }
}

fn scope_label(project_id: Option<i32>) -> String {
    match project_id {
        Some(id) => format!(" in project {}", id),
        None => " (global)".to_string(),
    }
}

impl From<sea_orm::DbErr> for AgentError {
    fn from(error: sea_orm::DbErr) -> Self {
        // Postgres unique-constraint violations surface as `DbErr::Exec` /
        // `DbErr::Query` carrying the raw libpq error string. Detect them so
        // race-condition duplicates (two concurrent inserts slip past the
        // service-layer existence check) don't leak as opaque 500s.
        let msg = error.to_string();
        if msg.contains("duplicate key value violates unique constraint") {
            if let Some(slug) = extract_dup_slug(&msg) {
                if msg.contains("skill_definitions") {
                    return AgentError::SkillDefinitionAlreadyExists {
                        project_id: None,
                        slug,
                    };
                }
                if msg.contains("mcp_definitions") {
                    return AgentError::McpDefinitionAlreadyExists {
                        project_id: None,
                        slug,
                    };
                }
            }
        }
        match &error {
            sea_orm::DbErr::RecordNotFound(_) => AgentError::RunNotFound { run_id: 0 },
            sea_orm::DbErr::RecordNotInserted => AgentError::Validation {
                message: format!("Duplicate record or constraint violation: {}", error),
            },
            _ => AgentError::Database(error),
        }
    }
}

/// Prefix of the [`AgentError::SandboxCreationFailed`] reason when the
/// container runtime could not pull the sandbox image. The reason is a string
/// because the error also crosses the worker-node HTTP boundary, so the pull
/// failure is encoded and decoded in one place: [`image_pull_failure_reason`]
/// builds it and [`AgentError::image_pull_failure`] reads it back.
const IMAGE_PULL_REASON_PREFIX: &str = "Failed to pull image ";

/// Build the `SandboxCreationFailed` reason for an image that could not be
/// pulled. `cause` is the container runtime's own message.
pub fn image_pull_failure_reason(image: &str, cause: &str) -> String {
    format!("{IMAGE_PULL_REASON_PREFIX}{image}: {cause}")
}

/// Substrings (lower case) that identify Docker having no subnet left to hand
/// to a new network. The first is the daemon's message when every
/// `default-address-pools` range is carved up; the second is the same
/// condition on older engines; the third is our own reworded message.
const NETWORK_POOL_EXHAUSTED_MARKERS: [&str; 3] = [
    "all predefined address pools have been fully subnetted",
    "could not find an available, non-overlapping ipv4 address pool",
    // What `network_pool_exhausted_reason` itself says, so the reworded
    // message is still recognised once the daemon's text is not in it.
    "docker has no free address pool left",
];

/// Whether `message` is the Docker daemon refusing to create a network
/// because its address pools are exhausted.
///
/// The error also crosses the worker-node HTTP boundary as a string, so it
/// is recognised by its text in one place, like the image pull failure.
pub fn is_network_pool_exhausted(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    NETWORK_POOL_EXHAUSTED_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

/// Build the reason for a sandbox whose isolated network could not be created
/// because Docker has no free address pool left. `cause` is the daemon's own
/// message. By the time this is returned, stopped sandboxes have already
/// given their networks back, so it states what is left: running sandboxes
/// and other containers' networks, or pools that are too small.
pub fn network_pool_exhausted_reason(sandbox: &str, cause: &str) -> String {
    format!(
        "Docker has no free address pool left for the isolated network of sandbox '{sandbox}' \
         ({cause}). Stopped sandboxes have already released their networks, so the address \
         space is held by running sandboxes and other Docker networks on this host. Stop \
         sandboxes you are not using, or raise `default-address-pools` in the Docker daemon \
         configuration (daemon.json, for example a larger `base` range with a `size` of 24) and \
         restart Docker."
    )
}

impl AgentError {
    /// The reason, when this error is the Docker daemon running out of
    /// address pools for the per-sandbox network. Callers answer it with a
    /// capacity error instead of a server fault.
    pub fn network_pool_exhaustion(&self) -> Option<&str> {
        match self {
            AgentError::SandboxProviderUnavailable { reason, .. }
            | AgentError::SandboxCreationFailed { reason, .. } => {
                is_network_pool_exhausted(reason).then_some(reason.as_str())
            }
            _ => None,
        }
    }

    /// A terminal request to the guest of `sandbox_id` that did not finish
    /// within `timeout`; `operation` says which step ("agent handshake on
    /// vsock port 5000", "agent bootstrap", …).
    pub fn terminal_timeout(
        sandbox_id: &str,
        operation: impl Into<String>,
        timeout: std::time::Duration,
    ) -> Self {
        AgentError::SandboxTerminalTimeout {
            sandbox_id: sandbox_id.to_string(),
            operation: operation.into(),
            timeout_secs: timeout.as_secs().max(1),
        }
    }

    /// When this error is a sandbox creation that failed because the image
    /// could not be pulled, return `(image, cause)`.
    pub fn image_pull_failure(&self) -> Option<(&str, &str)> {
        match self {
            AgentError::SandboxCreationFailed { reason, .. } => reason
                .strip_prefix(IMAGE_PULL_REASON_PREFIX)
                .and_then(|rest| rest.split_once(": "))
                .filter(|(image, _)| !image.is_empty() && !image.contains(char::is_whitespace)),
            _ => None,
        }
    }
}

/// Try to pull the slug out of a Postgres unique-violation message.
/// Postgres includes `DETAIL: Key (slug)=(foo) already exists.` in its
/// error body. Missing DETAIL just means we fall back to `None`.
fn extract_dup_slug(msg: &str) -> Option<String> {
    let key_idx = msg.find("Key (")?;
    let rest = &msg[key_idx + 5..];
    let val_start = rest.find(")=(")? + 3;
    let val_end = rest[val_start..].find(')')?;
    Some(rest[val_start..val_start + val_end].to_string())
}

#[cfg(test)]
mod image_pull_tests {
    use super::*;

    fn creation_failed(reason: String) -> AgentError {
        AgentError::SandboxCreationFailed {
            run_id: 7,
            provider: "docker".into(),
            reason,
        }
    }

    #[test]
    fn pull_failure_round_trips_image_and_cause() {
        let error = creation_failed(image_pull_failure_reason(
            "registry.example.test/team/img:1.2.3",
            "Docker responded with status code 404: manifest unknown",
        ));
        assert_eq!(
            error.image_pull_failure(),
            Some((
                "registry.example.test/team/img:1.2.3",
                "Docker responded with status code 404: manifest unknown"
            ))
        );
    }

    #[test]
    fn other_creation_failures_are_not_pull_failures() {
        let error = creation_failed("inspect runtime image x:1: no such image".into());
        assert_eq!(error.image_pull_failure(), None);
        let error = creation_failed("Failed to pull image : empty".into());
        assert_eq!(error.image_pull_failure(), None);
    }

    #[test]
    fn terminal_timeout_names_the_sandbox_step_and_limit() {
        let error = AgentError::terminal_timeout(
            "sbx_test",
            "agent handshake on vsock port 5000",
            std::time::Duration::from_secs(10),
        );
        let message = error.to_string();
        assert!(message.contains("sbx_test"), "{message}");
        assert!(message.contains("vsock port 5000"), "{message}");
        assert!(message.contains("10s"), "{message}");
        // Kept apart from exec failures so a handler can answer 504, not 409.
        assert!(!matches!(error, AgentError::SandboxExecFailed { .. }));
    }

    #[test]
    fn terminal_timeout_never_reports_a_zero_second_limit() {
        let error = AgentError::terminal_timeout(
            "sbx_test",
            "handshake",
            std::time::Duration::from_millis(50),
        );
        assert!(error.to_string().contains("within 1s"), "{error}");
    }

    #[test]
    fn non_creation_errors_are_not_pull_failures() {
        let error = AgentError::SandboxProviderUnavailable {
            provider: "docker".into(),
            reason: "Failed to pull image a:1: boom".into(),
        };
        assert_eq!(error.image_pull_failure(), None);
    }

    #[test]
    fn docker_address_pool_exhaustion_is_recognised() {
        assert!(is_network_pool_exhausted(
            "Docker responded with status code 400: all predefined address pools have been fully subnetted"
        ));
        assert!(is_network_pool_exhausted(
            "Could not find an available, non-overlapping IPv4 address pool among the defaults to assign to the network"
        ));
        assert!(!is_network_pool_exhausted(
            "network temps-sandbox-x already exists"
        ));
    }

    #[test]
    fn pool_exhaustion_reason_names_sandbox_cause_and_remedy() {
        let reason = network_pool_exhausted_reason("temps-sandbox-abc", "pools fully subnetted");
        assert!(reason.contains("temps-sandbox-abc"), "{reason}");
        assert!(reason.contains("pools fully subnetted"), "{reason}");
        assert!(reason.contains("default-address-pools"), "{reason}");
        assert!(
            reason.contains("Stopped sandboxes have already released"),
            "{reason}"
        );
        assert!(
            reason.contains("Stop sandboxes you are not using"),
            "{reason}"
        );
        let error = AgentError::SandboxProviderUnavailable {
            provider: "docker".into(),
            reason,
        };
        assert_eq!(
            error
                .network_pool_exhaustion()
                .map(|r| r.contains("temps-sandbox-abc")),
            Some(true)
        );
    }

    #[test]
    fn unrelated_errors_are_not_pool_exhaustion() {
        let error = creation_failed("create container: no such image".into());
        assert_eq!(error.network_pool_exhaustion(), None);
        assert_eq!(
            AgentError::RunNotFound { run_id: 1 }.network_pool_exhaustion(),
            None
        );
    }
}
