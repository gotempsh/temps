// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// Dropped after the attachment, so cleanup can reacquire its filesystem lease.
/// A cancellation keeps its capacity reservation until bounded cleanup ends.
pub(super) struct MountCleanup {
    pub root: PathBuf,
    pub id: String,
    pub docker: Option<(Arc<Docker>, String)>,
    pub permit: Option<OwnedSemaphorePermit>,
}

impl MountCleanup {
    pub fn disarm(&mut self) -> Result<OwnedSemaphorePermit, AgentError> {
        let permit = self.permit.take().ok_or_else(|| {
            failure(
                &self.id,
                "adopt proxy mount",
                "capacity reservation missing",
            )
        })?;
        self.docker = None;
        Ok(permit)
    }
}

impl Drop for MountCleanup {
    fn drop(&mut self) {
        let Some((docker, name)) = self.docker.take() else {
            return;
        };
        let root = self.root.clone();
        let id = self.id.clone();
        let permit = self.permit.take();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(mount_id = %id, "Proxy mount cleanup requires a live runtime; retained for recovery");
            return;
        };
        runtime.spawn(async move {
            let _permit = permit;
            match tokio::time::timeout(Duration::from_secs(15), remove_owned(&docker, &name, &id, &root)).await {
                Ok(Ok(())) => {},
                Ok(Err(error)) => tracing::error!(mount_id = %id, error = %error, "Proxy mount rollback incomplete; retained for recovery"),
                Err(_) => tracing::error!(mount_id = %id, "Proxy mount rollback timed out; retained for recovery"),
            }
        });
    }
}

async fn remove_owned(
    docker: &Docker,
    name: &str,
    id: &str,
    root: &Path,
) -> Result<(), AgentError> {
    match docker
        .inspect_container(
            name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        Ok(info) if mount_id(&info) == Some(id) => {
            let container_id = info.id.ok_or_else(|| {
                failure(
                    id,
                    "rollback container",
                    "inspect omitted immutable container ID",
                )
            })?;
            match docker
                .remove_container(
                    &container_id,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                Ok(()) => {}
                Err(e) if docker_error_is_not_found(&e) => {}
                Err(e) => return Err(failure(id, "remove owned container", e)),
            }
        }
        Ok(_) => {} // A replacement container is not owned by this invocation.
        Err(e) if docker_error_is_not_found(&e) => {}
        Err(e) => return Err(failure(id, "inspect rollback ownership", e)),
    }
    MountDirectory::recover(root, id)
        .and_then(MountDirectory::remove)
        .map_err(|e| failure(id, "remove abandoned mount", e))
}
