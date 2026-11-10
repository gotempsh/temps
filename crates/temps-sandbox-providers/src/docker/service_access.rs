// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker lifecycle ownership of credential-free proxy mounts. Only a trusted
//! provider may resolve an incarnation or activate its validated binding.
use super::*;
use std::{os::unix::fs::DirBuilderExt, sync::Mutex, time::Duration};
use temps_service_access_runtime::{
    HostAudit, MountDirectory, SandboxBinding, UnixAttachment, WorkerControl, MAX_WORKER_BINDINGS,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

pub(super) const MOUNT_LABEL: &str = "sh.temps.sandbox.service-access.mount";
pub(super) const GUEST_DIRECTORY: &str = "/run/temps-service-access";

pub use crate::service_access::ServiceAccessTarget;

struct Entry {
    target: ServiceAccessTarget,
    started_at: String,
    attachment: UnixAttachment,
    binding: Option<(Arc<SandboxBinding>, u64)>,
    _permit: OwnedSemaphorePermit,
}

pub(super) struct AccessMounts {
    control: WorkerControl,
    root: PathBuf,
    entries: Mutex<HashMap<String, Entry>>,
    permits: Arc<Semaphore>,
    operations: Mutex<std::collections::HashSet<String>>,
}

pub(super) struct Operation<'a> {
    mounts: &'a AccessMounts,
    id: String,
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.mounts
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

pub(super) struct PendingMount {
    target_id: String,
    attachment: UnixAttachment,
    cleanup: MountCleanup,
}

fn failure(id: &str, operation: &str, error: impl std::fmt::Display) -> AgentError {
    AgentError::SandboxExecFailed {
        run_id: 0,
        sandbox_id: id.into(),
        reason: format!("service access: {operation}: {error}"),
    }
}

impl AccessMounts {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            control: WorkerControl::new(Arc::new(HostAudit)),
            root,
            entries: Mutex::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(MAX_WORKER_BINDINGS)),
            operations: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Fail fast rather than allowing recovery/activation to race a stop.
    /// This is bounded control-plane bookkeeping, never a proxy request lock.
    pub(super) fn operation(&self, id: &str) -> Result<Operation<'_>, AgentError> {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if operations.len() >= MAX_WORKER_BINDINGS || !operations.insert(id.into()) {
            return Err(failure(
                id,
                "claim sandbox lifecycle",
                "another operation is in flight or worker capacity is exhausted",
            ));
        }
        Ok(Operation {
            mounts: self,
            id: id.into(),
        })
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn reserve(&self) -> Result<PendingMount, AgentError> {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|e| failure("new", "reserve worker capacity", e))?;
        match std::fs::DirBuilder::new().mode(0o700).create(&self.root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(failure("new", "create private mount root", e)),
        }
        // Retained mounts from crashes count against capacity too. Do not
        // accumulate unbounded directories when Docker cannot confirm cleanup.
        let retained = std::fs::read_dir(&self.root)
            .map_err(|e| failure("new", "count retained mounts", e))?
            .take(MAX_WORKER_BINDINGS)
            .count();
        if retained >= MAX_WORKER_BINDINGS {
            return Err(failure(
                "new",
                "reserve mount",
                "retained mount capacity exhausted; remove detached sandboxes before retrying",
            ));
        }
        let directory =
            MountDirectory::create(&self.root).map_err(|e| failure("new", "reserve mount", e))?;
        self.pending(directory, permit)
    }

    /// A Linux client can itself run in a container backed by a named volume.
    /// Verify that the daemon accepts this exact private bind source before
    /// adding it to a user sandbox. The probe never starts or joins a network.
    /// Unsupported topology leaves ordinary sandbox creation available.
    pub(super) async fn reserve_for_image(
        &self,
        docker: Arc<Docker>,
        image: &str,
    ) -> Result<Option<PendingMount>, AgentError> {
        let mut pending = match self.reserve() {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "Sandbox service access mount unavailable; ordinary sandbox creation will continue");
                return Ok(None);
            }
        };
        let name = format!("temps-service-access-probe-{}", Uuid::new_v4().simple());
        let mut body = bollard::models::ContainerCreateBody {
            image: Some(image.into()),
            cmd: Some(vec!["true".into()]),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("none".into()),
                readonly_rootfs: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        pending.configure(&mut body, Arc::clone(&docker), &name)?;
        let probe = async {
            let created = docker
                .create_container(
                    Some(
                        bollard::query_parameters::CreateContainerOptionsBuilder::new()
                            .name(&name)
                            .build(),
                    ),
                    body,
                )
                .await?;
            docker
                .remove_container(
                    &created.id,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await
        };
        match tokio::time::timeout(Duration::from_secs(20), probe).await {
            Ok(Ok(())) => Ok(Some(pending)),
            Ok(Err(_)) | Err(_) => {
                tracing::warn!("Sandbox service access disabled for this create: Docker could not verify a shared host mount; ordinary sandbox creation will continue");
                // Drop closes the denied socket and asynchronously cleans only
                // the unique probe. Its lease/capacity remain held by cleanup.
                Ok(None)
            }
        }
    }

    fn pending(
        &self,
        directory: MountDirectory,
        permit: OwnedSemaphorePermit,
    ) -> Result<PendingMount, AgentError> {
        let target_id = Uuid::new_v4().to_string();
        let attachment = UnixAttachment::reserve_mounted(directory, target_id.clone())
            .map_err(|e| failure(&target_id, "listen on mount", e))?;
        let attachment_id = directory_id(&attachment)?;
        Ok(PendingMount {
            target_id,
            attachment,
            cleanup: MountCleanup {
                root: self.root.clone(),
                id: attachment_id,
                docker: None,
                permit: Some(permit),
            },
        })
    }

    pub(super) fn adopt(
        &self,
        info: &bollard::models::ContainerInspectResponse,
        mut pending: PendingMount,
    ) -> Result<ServiceAccessTarget, AgentError> {
        let (container_id, started_at) = running_identity(info)?;
        verify_mount(
            info,
            pending.attachment.directory(),
            pending.attachment.mount_id(),
        )?;
        let target = ServiceAccessTarget {
            incarnation: pending.target_id.clone(),
            container_id: container_id.clone(),
        };
        let mut entries = self.entries();
        if entries.contains_key(&container_id) {
            return Err(failure(
                &container_id,
                "adopt mount",
                "an incarnation is already registered",
            ));
        }
        let permit = pending.cleanup.disarm()?;
        entries.insert(
            container_id,
            Entry {
                target: target.clone(),
                started_at,
                attachment: pending.attachment,
                binding: None,
                _permit: permit,
            },
        );
        Ok(target)
    }

    pub(super) fn target(
        &self,
        info: &bollard::models::ContainerInspectResponse,
    ) -> Result<ServiceAccessTarget, AgentError> {
        let (id, started_at) = running_identity(info)?;
        let entries = self.entries();
        let entry = entries.get(&id).ok_or_else(|| {
            failure(
                &id,
                "resolve incarnation",
                "attachment has not been recovered",
            )
        })?;
        if started_at != entry.started_at {
            return Err(failure(
                &id,
                "resolve incarnation",
                "container restarted; fresh consent is required",
            ));
        }
        if entry
            .binding
            .as_ref()
            .is_some_and(|(binding, _)| binding.status().revoked)
        {
            return Err(failure(
                &id,
                "resolve incarnation",
                "binding was retired or interrupted; fresh consent is required",
            ));
        }
        verify_mount(
            info,
            entry.attachment.directory(),
            entry.attachment.mount_id(),
        )?;
        Ok(entry.target.clone())
    }

    pub(super) fn activate(
        &self,
        info: &bollard::models::ContainerInspectResponse,
        target: &ServiceAccessTarget,
        binding: Arc<SandboxBinding>,
    ) -> Result<(), AgentError> {
        let (id, started_at) = running_identity(info)?;
        let entries = self.entries();
        let entry = entries
            .get(&id)
            .ok_or_else(|| failure(&id, "activate grant", "attachment is unavailable"))?;
        if entry.target != *target || entry.started_at != started_at {
            return Err(failure(
                &id,
                "activate grant",
                "sandbox incarnation changed",
            ));
        }
        verify_mount(
            info,
            entry.attachment.directory(),
            entry.attachment.mount_id(),
        )?;
        entry
            .attachment
            .activate(binding)
            .map_err(|e| failure(&id, "activate grant", e))
    }

    pub(super) async fn close(&self, id: &str) -> Result<(), AgentError> {
        let entry = self.entries().remove(id);
        if let Some(entry) = entry {
            entry
                .attachment
                .revoke_and_wait(Duration::from_secs(5))
                .await
                .map_err(|e| failure(id, "confirm proxy shutdown", e))?;
        }
        Ok(())
    }

    pub(super) fn recover(
        &self,
        info: &bollard::models::ContainerInspectResponse,
    ) -> Result<ServiceAccessTarget, AgentError> {
        let (id, _) = running_identity(info)?;
        let mount_id = mount_id(info).ok_or_else(|| {
            failure(
                &id,
                "recover proxy mount",
                "host proxy mount unavailable: use a compatible sandbox image and a worker sharing the Docker daemon filesystem",
            )
        })?;
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|e| failure(&id, "reserve recovery capacity", e))?;
        let directory = MountDirectory::recover(&self.root, mount_id)
            .map_err(|e| failure(&id, "recover proxy mount", e))?;
        verify_mount(info, directory.guest_path(), Some(mount_id))?;
        self.adopt(info, self.pending(directory, permit)?)
    }

    /// The caller has confirmed removal of the container, not only a stop.
    pub(super) fn remove_detached(&self, mount_id: &str) -> Result<(), AgentError> {
        MountDirectory::recover(&self.root, mount_id)
            .and_then(MountDirectory::remove)
            .map_err(|e| failure(mount_id, "remove detached proxy mount", e))
    }

    /// Called only after a complete successful Docker enumeration. Private
    /// lease validation prevents removing mounts owned by in-flight creates
    /// or another live worker, including mounts absent from that enumeration.
    pub(super) fn reap_detached(
        &self,
        referenced: &std::collections::HashSet<String>,
    ) -> Result<usize, AgentError> {
        let directories = match std::fs::read_dir(&self.root) {
            Ok(directories) => directories,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(failure("unassigned", "enumerate retained mounts", e)),
        };
        let mut removed = 0;
        for directory in directories.take(MAX_WORKER_BINDINGS * 2) {
            let directory =
                directory.map_err(|e| failure("unassigned", "inspect retained mount", e))?;
            let Some(id) = directory.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if referenced.contains(&id) {
                continue;
            }
            // Recovery validates the random ID, ownership, modes and private
            // lease. A busy lease or unknown entry is deliberately untouched.
            if let Ok(mount) = MountDirectory::recover(&self.root, &id) {
                mount
                    .remove()
                    .map_err(|e| failure(&id, "reap detached mount", e))?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

impl PendingMount {
    pub(super) fn configure(
        &mut self,
        body: &mut bollard::models::ContainerCreateBody,
        docker: Arc<Docker>,
        container_name: &str,
    ) -> Result<(), AgentError> {
        let id = self.attachment.mount_id().ok_or_else(|| {
            failure(
                &self.target_id,
                "configure mount",
                "persistent mount ID missing",
            )
        })?;
        let source = self.attachment.directory().to_str().ok_or_else(|| {
            failure(
                &self.target_id,
                "configure mount",
                "mount path is not UTF-8",
            )
        })?;
        body.labels
            .get_or_insert_with(HashMap::new)
            .insert(MOUNT_LABEL.into(), id.into());
        let host = body.host_config.get_or_insert_with(Default::default);
        host.mounts
            .get_or_insert_with(Vec::new)
            .push(bollard::models::Mount {
                typ: Some(bollard::models::MountType::BIND),
                source: Some(source.into()),
                target: Some(GUEST_DIRECTORY.into()),
                read_only: Some(true),
                ..Default::default()
            });
        // Automatic restarts bypass the provider's fresh-incarnation boundary.
        // Provider-managed starts/restarts always close the old attachment first.
        host.restart_policy = Some(bollard::models::RestartPolicy {
            name: Some(bollard::models::RestartPolicyNameEnum::NO),
            ..Default::default()
        });
        self.cleanup.docker = Some((docker, container_name.to_string()));
        self.attachment.retain_mount();
        Ok(())
    }
}

pub(super) fn mount_id(info: &bollard::models::ContainerInspectResponse) -> Option<&str> {
    info.config
        .as_ref()?
        .labels
        .as_ref()?
        .get(MOUNT_LABEL)
        .map(String::as_str)
}

fn running_identity(
    info: &bollard::models::ContainerInspectResponse,
) -> Result<(String, String), AgentError> {
    let id = info.id.as_deref().unwrap_or("unknown");
    let managed = info
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref())
        .and_then(|l| l.get(SANDBOX_CONTAINER_LABEL))
        .is_some_and(|v| v == "true");
    let state = info.state.as_ref().filter(|s| s.running == Some(true));
    let started_at = state
        .and_then(|s| s.started_at.as_ref())
        .filter(|s| !s.is_empty());
    if !managed || info.id.is_none() || started_at.is_none() {
        return Err(failure(
            id,
            "resolve running container",
            "expected a running managed sandbox with a start identity",
        ));
    }
    match started_at {
        Some(started_at) => Ok((id.into(), started_at.clone())),
        None => Err(failure(
            id,
            "resolve running container",
            "missing start identity",
        )),
    }
}

fn verify_mount(
    info: &bollard::models::ContainerInspectResponse,
    path: &Path,
    expected: Option<&str>,
) -> Result<(), AgentError> {
    let valid = expected.is_some()
        && mount_id(info) == expected
        && info.mounts.as_ref().is_some_and(|mounts| {
            let matches: Vec<_> = mounts
                .iter()
                .filter(|m| m.destination.as_deref() == Some(GUEST_DIRECTORY))
                .collect();
            matches.len() == 1
                && matches[0].typ.as_deref() == Some("bind")
                && matches[0].rw == Some(false)
                && matches[0].source.as_deref().map(Path::new) == Some(path)
        });
    if valid {
        Ok(())
    } else {
        Err(failure(
            info.id.as_deref().unwrap_or("unknown"),
            "verify proxy mount",
            "expected exactly this sandbox's read-only host attachment",
        ))
    }
}

impl DockerSandboxProvider {
    /// Cut this exact incarnation even when Docker is unreachable. A stale
    /// delivery must never revoke a newer incarnation of the same sandbox.
    pub async fn retire_service_access_target(
        &self,
        target: &ServiceAccessTarget,
    ) -> Result<(), AgentError> {
        let mounts = self.service_access.as_ref().ok_or_else(|| {
            failure(
                &target.container_id,
                "retire grant",
                "host transport unavailable",
            )
        })?;
        let _operation = mounts.operation(&target.container_id)?;
        {
            let entries = mounts.entries();
            if let Some(entry) = entries.get(&target.container_id) {
                if entry.target != *target {
                    // Adoption requires the previous attachment to be closed first.
                    // Under the per-container operation guard, a different current
                    // incarnation proves this saved target is already gone.
                    return Ok(());
                }
            }
        }
        mounts.close(&target.container_id).await
    }

    pub(super) async fn close_service_access_for_destroy(
        &self,
        handle: &SandboxHandle,
    ) -> Result<Option<String>, AgentError> {
        let Some(mounts) = &self.service_access else {
            return Ok(None);
        };
        // Close first even when Docker inspection fails: failure cannot extend access.
        mounts.close(&handle.sandbox_id).await?;
        match self
            .docker
            .inspect_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => Ok(mount_id(&info).map(str::to_owned)),
            Err(e) if docker_error_is_not_found(&e) => Ok(None),
            Err(e) => Err(failure(
                &handle.sandbox_id,
                "inspect mount before destroy",
                e,
            )),
        }
    }

    pub(super) async fn recover_service_access_after_start(
        &self,
        handle: &SandboxHandle,
    ) -> Result<(), AgentError> {
        let Some(mounts) = &self.service_access else {
            return Ok(());
        };
        let info = self.inspect_handle_policy(handle).await?;
        if mount_id(&info).is_some() && mounts.target(&info).is_err() {
            mounts.close(&handle.sandbox_id).await?;
            mounts.recover(&info)?;
        }
        Ok(())
    }

    pub async fn service_access_target(
        &self,
        handle: &SandboxHandle,
    ) -> Result<ServiceAccessTarget, AgentError> {
        let mounts = self.service_access.as_ref().ok_or_else(|| {
            failure(
                &handle.sandbox_id,
                "resolve proxy",
                "this host has no local Unix mount transport",
            )
        })?;
        let _operation = mounts.operation(&handle.sandbox_id)?;
        let info = self.inspect_handle_policy(handle).await?;
        match mounts.target(&info) {
            Ok(target) => Ok(target),
            Err(_) => {
                mounts.close(&handle.sandbox_id).await?;
                mounts.recover(&info)
            }
        }
    }

    pub async fn activate_service_access_binding(
        &self,
        handle: &SandboxHandle,
        target: &ServiceAccessTarget,
        binding: Arc<SandboxBinding>,
    ) -> Result<(), AgentError> {
        let mounts = self.service_access.as_ref().ok_or_else(|| {
            failure(
                &handle.sandbox_id,
                "activate grant",
                "host transport unavailable",
            )
        })?;
        let _operation = mounts.operation(&handle.sandbox_id)?;
        let info = self.inspect_handle_policy(handle).await?;
        mounts.activate(&info, target, binding)
    }
}

mod cleanup;
mod commands;
use cleanup::MountCleanup;

fn directory_id(attachment: &UnixAttachment) -> Result<String, AgentError> {
    attachment
        .mount_id()
        .map(str::to_owned)
        .ok_or_else(|| failure("new", "reserve mount", "persistent mount ID missing"))
}

#[cfg(test)]
mod tests;
