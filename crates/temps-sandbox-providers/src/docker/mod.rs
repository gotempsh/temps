// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::Docker;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

use super::user::{
    SANDBOX_CHOWN, SANDBOX_GID, SANDBOX_HOME, SANDBOX_UID, SANDBOX_USER, SANDBOX_WORK_DIR,
};
use super::{
    ExecStream, OnStreamEventCallback, PtyAttachment, RuntimeCompatibility, SandboxCreateConfig,
    SandboxEgressPolicy, SandboxExecResult, SandboxHandle, SandboxProvider, PTY_AGENT_SOCKET,
};
use crate::docker_network_isolation::{
    create_host_isolated_network, has_host_isolation, with_host_isolation,
};
#[cfg(test)]
use crate::docker_network_isolation::{
    BRIDGE_GATEWAY_MODE_IPV4_OPTION, BRIDGE_INHIBIT_IPV4_OPTION,
};
use temps_agent_core::error::{
    is_network_pool_exhausted, network_pool_exhausted_reason, AgentError,
};
use temps_agent_core::OnEventCallback;

// Items the submodules name as `super::X`, which before the split resolved
// in `crate::sandbox`.
use super::{
    file_too_large, git_credential_bundle, home_volume_name_for_label, pty_agent_bundle,
    DefaultResourceLimits, KillSignal, SandboxBackend, SnapshotArtifact, SnapshotCompanionArtifact,
};

mod create;
mod egress;
mod exec;
mod files;
mod image;
mod network;
mod provider;
mod recover;
mod snapshot;

pub use egress::SANDBOX_MODEL_RELAY_BASE_URL;
pub use files::{
    directory_upload_files, stage_directory_upload, DirectoryUploadPlan, SkippedUploadEntry,
    UploadSkipReason,
};
pub use image::{dockerfile_for_runtime, image_name_for_runtime, SANDBOX_IMAGE_VERSION};

pub(crate) use create::{HOME_VOLUME_PREFIX, HOME_VOLUME_SCHEME};
pub use network::{sandbox_network_name, SANDBOX_PREVIEW_GATEWAY_LABEL};

use egress::{
    recovered_container_matches_egress_policy, sandbox_container_environment,
    sandbox_egress_proxy_name, sandbox_mcp_relay_url, SANDBOX_EGRESS_NETWORK,
    SANDBOX_EGRESS_POLICY_LABEL, SANDBOX_EGRESS_POLICY_VERSION,
};
use exec::{
    agent_runtime_exec_config, has_harness_recovery_capability, runtime_health_exec_config,
    ROOT_EXEC_PATH,
};
use files::{is_turn_secret_path, runtime_user_owns_uploaded_file};
use image::{ensure_mutable_rebuild_target, selected_preset_runtime};
use network::{
    application_network_connection_order, container_is_parked, docker_network_matches_policy,
    effective_sandbox_network, is_managed_sandbox_network, network_labelled_for_another_gateway,
    ORPHANED_NETWORK_MIN_AGE, SANDBOX_NETWORK_OWNER_LABEL,
};

#[cfg(test)]
use create::HOME_VOLUME_LABEL;
#[cfg(test)]
use egress::{
    container_has_extra_host, egress_policy_of_sidecar, sandbox_egress_proxy_extra_hosts,
    SidecarEgressPolicy, SANDBOX_EGRESS_PROXY_ALIAS, SANDBOX_EGRESS_PROXY_PORT,
    SANDBOX_EGRESS_PROXY_SCRIPT, SANDBOX_EGRESS_RULES_ENV, SANDBOX_EGRESS_RULES_LABEL,
    SANDBOX_HOST_GATEWAY, SANDBOX_MCP_RELAY_BASE_URL, SANDBOX_NAME_ENV,
};
#[cfg(test)]
use exec::{
    completed_exec_lines, exec_runs_as_root, ownership_fix_command, write_probe_script,
    ExecOutputBuffer, OwnershipScope,
};
#[cfg(test)]
use image::{image_name_for_runtime_in_channel, runtime_from_image_name, SandboxChannel};
#[cfg(test)]
use network::{
    is_managed_preview_gateway, isolated_sandbox_network_request, managed_sandbox_network,
    network_has_foreign_endpoints, orphan_candidate_owner,
    sandbox_network_matches_isolation_policy, SANDBOX_NETWORK_PREFIX,
};
#[cfg(test)]
use snapshot::{
    archive_workspace, build_env_scrub_changes, combined_snapshot_digest,
    find_surviving_sensitive_keys, hash_file, is_sensitive_env_key, publish_content_addressed_file,
    restore_workspace_archive, SENSITIVE_ENV_PATTERNS,
};

/// Container naming prefix — used for recovery after server restarts.
pub const SANDBOX_NAME_PREFIX: &str = "temps-sandbox-";

/// Label every sandbox container carries (value `"true"`), set by `create`
/// since sandboxes were introduced. The name prefix alone is not proof that
/// a container is a sandbox — the egress proxy sidecar
/// (`temps-sandbox-egress-proxy-v2-…`) and any project whose slug starts with
/// `temps-sandbox` share it — so code that must only touch sandboxes (the
/// worker sandbox API, ADR-048) also requires this label.
pub const SANDBOX_CONTAINER_LABEL: &str = "sh.temps.sandbox";

/// Single-quote a string for safe embedding in a `sh -c` command line.
/// Handles embedded single quotes via the `'\''` idiom.
fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

fn docker_error_is_not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

/// What a failed `inspect_container` says about liveness. The daemon
/// answering "no such container" proves it is gone (`Ok(false)`). Anything
/// else (socket down, daemon restarting, timeout) says nothing about the
/// container, and `Ok(false)` is a verdict callers act on by rewriting the
/// sandbox row, so it is an error and stays "unknown".
fn inspect_failure_verdict(
    handle: &SandboxHandle,
    error: bollard::errors::Error,
) -> Result<bool, AgentError> {
    if docker_error_is_not_found(&error) {
        return Ok(false);
    }
    Err(AgentError::SandboxExecFailed {
        run_id: 0,
        sandbox_id: handle.sandbox_id.clone(),
        reason: format!(
            "Failed to inspect container {}: {}",
            handle.sandbox_name, error
        ),
    })
}

/// Path inside the container where the repository is mounted. Aliased to the
/// shared `SANDBOX_WORK_DIR` constant so a future image with a different
/// non-root user (and therefore a different home dir) only requires editing
/// `sandbox::user`.
const CONTAINER_WORK_DIR: &str = SANDBOX_WORK_DIR;

/// Configuration for the Docker sandbox provider.
#[derive(Debug, Clone)]
pub struct DockerSandboxConfig {
    /// Runtime preset: "node", "bun", "python", "rust", "go", "full", or "custom"
    pub runtime: String,
    /// Custom Docker image (only used when runtime is "custom")
    pub custom_image: String,
    /// Default CPU limit in cores
    pub default_cpu_limit: f64,
    /// Default memory limit in MB
    pub default_memory_limit_mb: u64,
    /// Network mode: "none" for full isolation, or a bridge name
    pub network_mode: String,
    /// Host control-plane origin used only by the egress sidecar's
    /// capability-scoped model-relay reverse route.
    pub control_plane_url: String,
    /// Exact preview-gateway container owned by this Temps instance. This
    /// scopes sandbox network attachment when multiple instances share a
    /// Docker daemon. Shared with the gateway's settings endpoint, so a
    /// rename applies to the sandboxes created after it.
    pub preview_gateway_container_name: crate::preview_gateway::GatewayName,
    /// Most bytes of each exec stream (stdout, stderr) kept for the result,
    /// enforced while Docker's output is read. `None` keeps all of it, which
    /// is what callers on the control plane expect; a worker node sets it so
    /// a chatty command cannot exhaust the node's memory before the response
    /// is truncated. Streamed lines are still delivered in full.
    pub exec_output_limit: Option<usize>,
}

impl Default for DockerSandboxConfig {
    fn default() -> Self {
        Self {
            runtime: "node".to_string(),
            custom_image: String::new(),
            default_cpu_limit: 4.0,
            default_memory_limit_mb: 8192,
            network_mode: "full".to_string(),
            control_plane_url: "http://host.docker.internal:8080".to_string(),
            preview_gateway_container_name: crate::preview_gateway::GatewayName::default(),
            exec_output_limit: None,
        }
    }
}

impl DockerSandboxConfig {
    /// Resolve the image name for the current configuration.
    /// For presets, returns `temps-sandbox-{runtime}:{SANDBOX_IMAGE_VERSION}`.
    /// For custom, returns the user-provided image.
    pub fn resolved_image(&self) -> String {
        if self.runtime == "custom" && !self.custom_image.is_empty() {
            self.custom_image.clone()
        } else {
            image_name_for_runtime(&self.runtime)
        }
    }
}

/// Docker-based sandbox provider. Each agent run gets its own container with
/// bind-mounted work directory, resource limits, and security hardening.
/// PID limit applied when a create request does not set one.
pub const DEFAULT_PIDS_LIMIT: i64 = 512;

pub struct DockerSandboxProvider {
    docker: Arc<Docker>,
    config: DockerSandboxConfig,
    network_lock: tokio::sync::Mutex<()>,
    /// Which sandboxes are being created and which are being reaped, so the
    /// two can never overlap. A creating sandbox's isolated network may be
    /// reused from an earlier incarnation and so be arbitrarily old, yet its
    /// container does not exist until the very end of `create`; the orphan
    /// reaper must not mistake that gap for an abandoned network, and a
    /// `create` must not start reusing a network the reaper is tearing down.
    creates_in_flight: std::sync::Mutex<SandboxLifecycle>,
}

/// Per-sandbox-name bookkeeping behind
/// [`DockerSandboxProvider::creates_in_flight`]. One lock covers both sets so
/// "no create is running, so claim it for reaping" is a single atomic step.
#[derive(Default)]
struct SandboxLifecycle {
    /// Running creates, with a count per name (overlapping creates of one
    /// sandbox are possible).
    creating: HashMap<String, usize>,
    /// Names whose leftovers the reaper is currently removing.
    reaping: std::collections::HashSet<String>,
}

type LifecycleRegistry = std::sync::Mutex<SandboxLifecycle>;

fn lock_lifecycle(registry: &LifecycleRegistry) -> std::sync::MutexGuard<'_, SandboxLifecycle> {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Marks a sandbox as being created for as long as it lives. See
/// [`DockerSandboxProvider::creates_in_flight`].
struct CreateInFlight<'a> {
    registry: &'a LifecycleRegistry,
    name: String,
}

impl<'a> CreateInFlight<'a> {
    /// Register a create, or `None` while the reaper holds this name.
    fn try_new(registry: &'a LifecycleRegistry, name: &str) -> Option<Self> {
        let mut state = lock_lifecycle(registry);
        if state.reaping.contains(name) {
            return None;
        }
        *state.creating.entry(name.to_string()).or_insert(0) += 1;
        Some(Self {
            registry,
            name: name.to_string(),
        })
    }
}

impl Drop for CreateInFlight<'_> {
    fn drop(&mut self) {
        let mut state = lock_lifecycle(self.registry);
        if let Some(count) = state.creating.get_mut(&self.name) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.creating.remove(&self.name);
            }
        }
    }
}

/// Held by the reaper while it removes a sandbox's leftovers; creates of the
/// same name wait until it drops.
struct ReapClaim<'a> {
    registry: &'a LifecycleRegistry,
    name: String,
}

impl<'a> ReapClaim<'a> {
    /// Claim a name for reaping, or `None` if a create (or another reap) of
    /// it is running.
    fn try_new(registry: &'a LifecycleRegistry, name: &str) -> Option<Self> {
        let mut state = lock_lifecycle(registry);
        if state.creating.contains_key(name) || !state.reaping.insert(name.to_string()) {
            return None;
        }
        Some(Self {
            registry,
            name: name.to_string(),
        })
    }
}

impl Drop for ReapClaim<'_> {
    fn drop(&mut self) {
        lock_lifecycle(self.registry).reaping.remove(&self.name);
    }
}

impl DockerSandboxProvider {
    pub fn new(docker: Arc<Docker>, config: DockerSandboxConfig) -> Self {
        Self {
            docker,
            config,
            network_lock: tokio::sync::Mutex::new(()),
            creates_in_flight: std::sync::Mutex::new(SandboxLifecycle::default()),
        }
    }
}

#[cfg(test)]
mod tests;

#[test]
fn docker_not_found_is_safe_for_idempotent_cleanup() {
    let not_found = bollard::errors::Error::DockerResponseServerError {
        status_code: 404,
        message: "No such container".to_string(),
    };
    let conflict = bollard::errors::Error::DockerResponseServerError {
        status_code: 409,
        message: "container is still running".to_string(),
    };

    assert!(docker_error_is_not_found(&not_found));
    assert!(!docker_error_is_not_found(&conflict));
}

#[test]
fn a_failed_inspection_is_unknown_unless_the_container_is_gone() {
    let handle = SandboxHandle {
        node_id: None,
        sandbox_id: "abc123".to_string(),
        sandbox_name: "temps-sandbox-test".to_string(),
        work_dir: std::path::PathBuf::from("/workspace"),
        backend: crate::SandboxBackend::Docker,
        image: String::new(),
        conditions: Vec::new(),
    };
    let gone = bollard::errors::Error::DockerResponseServerError {
        status_code: 404,
        message: "No such container".to_string(),
    };
    let daemon_down = bollard::errors::Error::DockerResponseServerError {
        status_code: 500,
        message: "daemon is restarting".to_string(),
    };

    assert!(matches!(inspect_failure_verdict(&handle, gone), Ok(false)));
    let unknown = inspect_failure_verdict(&handle, daemon_down);
    assert!(
        matches!(&unknown, Err(AgentError::SandboxExecFailed { reason, .. })
            if reason.contains("temps-sandbox-test") && reason.contains("daemon is restarting")),
        "{unknown:?}"
    );
}

/// ADR-055 §10, phase 0: does Docker's embedded resolver forward
/// external names for a container whose only network is a sandbox's
/// isolated bridge? If it did, a sandbox could leak data through DNS
/// queries past the egress proxy. Docker Engine 26 stopped forwarding
/// for containers attached only to internal networks; this pins that
/// down on the exact network request sandboxes are created with.
#[cfg(test)]
mod external_name_forwarding;
