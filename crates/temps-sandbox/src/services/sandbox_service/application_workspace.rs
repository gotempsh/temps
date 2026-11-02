// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable AI application workspaces: the managed daemon images, workspace
//! realization, rebuild and restore, usage, and the in-sandbox Git bootstrap.

use super::*;

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
pub(super) fn pinned_or_fallback(
    pinned: Option<&'static str>,
    fallback: &'static str,
) -> &'static str {
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

pub(super) const APPLICATION_WORKSPACE_NAME_PREFIX: &str = "ai-application:";

pub(super) fn managed_application_id_from_name(name: &str) -> Option<&str> {
    name.strip_prefix(APPLICATION_WORKSPACE_NAME_PREFIX)
        .filter(|application_id| !application_id.is_empty())
}

pub(super) fn application_workspace_identity_is_attested(
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

pub(super) fn application_workspace_config_has_drift(
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

pub(super) const APPLICATION_WORKSPACE_USAGE_COMMAND: &str = "memory=$(cat /sys/fs/cgroup/memory.current 2>/dev/null || true); pids=$(cat /sys/fs/cgroup/pids.current 2>/dev/null || true); cpu=$(sed -n 's/^usage_usec //p' /sys/fs/cgroup/cpu.stat 2>/dev/null || true); disk=$(du -sb /home/temps/workspace 2>/dev/null | awk 'NR==1 {print $1}'); ports=$(if command -v ss >/dev/null 2>&1; then ss -ltnH 2>/dev/null | awk '{print $4}' | sed 's/.*://'; else for socket_table in /proc/net/tcp /proc/net/tcp6; do test -r \"$socket_table\" || continue; awk 'NR > 1 && $4 == \"0A\" {split($2, address, \":\"); print address[2]}' \"$socket_table\"; done | while IFS= read -r hex_port; do printf '%d\\n' \"0x$hex_port\"; done; fi | sort -nu | paste -sd, -); printf 'memory=%s\\n' \"$memory\"; printf 'pids=%s\\n' \"$pids\"; printf 'cpu=%s\\n' \"$cpu\"; printf 'disk=%s\\n' \"$disk\"; printf 'ports=%s\\n' \"$ports\"";

pub(super) fn parse_application_workspace_usage(stdout: &str) -> ApplicationWorkspaceUsage {
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

// Application workspaces are repositories, not disposable command scratch
// space. This bootstrap runs *inside* the sandbox after create/recovery, so
// both repository metadata and commits live on the same durable bind mount as
// the files the development harness edits.
pub(super) const APPLICATION_GIT_BOOTSTRAP: &str = r#"set -eu
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

pub(super) fn application_workspace_mutex(
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

impl SandboxService {
    pub(super) async fn lock_application_workspace(
        &self,
        application_id: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.application_workspace_locks.lock().await;
            application_workspace_mutex(&mut locks, application_id)
        };
        lock.lock_owned().await
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
