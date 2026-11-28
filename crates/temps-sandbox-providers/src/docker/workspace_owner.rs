// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Handing a sandbox's workspace back to Temps when the sandbox goes away.
//!
//! The sandbox user owns its workspace: every start chowns it to that user,
//! so the tools inside can write. Temps itself then works on the same
//! directory from outside: it removes it when a sandbox is destroyed, and
//! replaces it when a workspace is restored. Running as root, Temps can do
//! that whatever the owner. Running unprivileged (its own container image
//! runs as a non-root user without `CAP_CHOWN`), it cannot even delete a file
//! the sandbox wrote, and every destroyed sandbox would leave its working
//! tree on disk for good.
//!
//! So a destroy hands the files back first. The sandbox's own image chowns
//! the workspace to Temps' user, in a short-lived container that mounts the
//! workspace exactly as the sandbox did, has no network, and holds only the
//! capabilities a recursive chown needs. Temps' own process never needs a
//! privilege for it.

use super::*;
use std::time::Duration;

/// Label on the short-lived container, so a leftover one is recognisable.
pub(super) const RECLAIM_LABEL: &str = "sh.temps.sandbox.workspace-reclaim";

/// How long a hand-back may take before it is abandoned. A chown over a
/// large `node_modules` takes seconds; the destroy must not hang on it.
const RECLAIM_TIMEOUT: Duration = Duration::from_secs(120);

/// The user and group this process runs as, from the owner of `/proc/self`.
/// `None` where that does not exist (not Linux).
pub(super) fn process_owner() -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .ok()
        .map(|metadata| (metadata.uid(), metadata.gid()))
}

/// Whether a process running as `owner` needs the workspace handed back. Root
/// can already remove and replace anything there.
pub(super) fn needs_hand_back(owner: Option<(u32, u32)>) -> Option<(u32, u32)> {
    owner.filter(|(uid, _)| *uid != 0)
}

/// How a sandbox container got its workspace: a `Binds` entry or a `Mounts`
/// entry (a volume narrowed to the sandbox's directory).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum WorkspaceSource {
    Bind(String),
    Mount(Box<bollard::models::Mount>),
}

/// The workspace source of an inspected sandbox container, if it has one.
pub(super) fn workspace_source(
    info: &bollard::models::ContainerInspectResponse,
) -> Option<WorkspaceSource> {
    let host = info.host_config.as_ref()?;
    if let Some(mount) = host
        .mounts
        .iter()
        .flatten()
        .find(|mount| mount.target.as_deref() == Some(CONTAINER_WORK_DIR))
    {
        return Some(WorkspaceSource::Mount(Box::new(mount.clone())));
    }
    host.binds
        .iter()
        .flatten()
        .find(|bind| bind.split(':').nth(1) == Some(CONTAINER_WORK_DIR))
        .map(|bind| WorkspaceSource::Bind(bind.clone()))
}

/// The container that chowns `source` to `owner`, run from `image`.
pub(super) fn reclaim_container(
    image: &str,
    source: WorkspaceSource,
    owner: (u32, u32),
) -> bollard::models::ContainerCreateBody {
    let (uid, gid) = owner;
    let mut host_config = bollard::models::HostConfig {
        network_mode: Some("none".into()),
        readonly_rootfs: Some(true),
        cap_drop: Some(vec!["ALL".into()]),
        // CHOWN to change owners; DAC_READ_SEARCH to descend into
        // directories the sandbox user made private.
        cap_add: Some(vec!["CHOWN".into(), "DAC_READ_SEARCH".into()]),
        security_opt: Some(vec!["no-new-privileges:true".into()]),
        pids_limit: Some(16),
        ..Default::default()
    };
    match source {
        WorkspaceSource::Bind(bind) => host_config.binds = Some(vec![bind]),
        WorkspaceSource::Mount(mut mount) => {
            mount.read_only = Some(false);
            host_config.mounts = Some(vec![*mount]);
        }
    }
    bollard::models::ContainerCreateBody {
        image: Some(image.into()),
        user: Some("0:0".into()),
        // `-h`: a link the sandbox left is re-owned itself, never followed.
        entrypoint: Some(vec!["chown".into()]),
        cmd: Some(vec![
            "-hR".into(),
            format!("{uid}:{gid}"),
            "--".into(),
            CONTAINER_WORK_DIR.into(),
        ]),
        network_disabled: Some(true),
        labels: Some(HashMap::from([(RECLAIM_LABEL.into(), "true".into())])),
        host_config: Some(host_config),
        ..Default::default()
    }
}

impl DockerSandboxProvider {
    /// Hand `handle`'s workspace back to this process's user before its
    /// container is removed, when this process is not root.
    ///
    /// Best effort and bounded: a failure is logged with what is left behind,
    /// and the destroy goes on, since the sandbox is going either way.
    pub(super) async fn hand_back_workspace(&self, handle: &SandboxHandle) {
        let Some(owner) = needs_hand_back(process_owner()) else {
            return;
        };
        let info = match self
            .docker
            .inspect_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => info,
            Err(error) => {
                tracing::debug!(
                    sandbox = %handle.sandbox_name,
                    %error,
                    "sandbox container not inspectable; its workspace is not handed back"
                );
                return;
            }
        };
        let (Some(source), Some(image)) = (
            workspace_source(&info),
            info.config.as_ref().and_then(|config| config.image.clone()),
        ) else {
            return;
        };

        let name = format!("temps-workspace-reclaim-{}", uuid::Uuid::new_v4().simple());
        let body = reclaim_container(&image, source, owner);
        let outcome = tokio::time::timeout(RECLAIM_TIMEOUT, self.run_to_completion(&name, body))
            .await
            .unwrap_or_else(|_| {
                Err(format!(
                    "did not finish within {}s",
                    RECLAIM_TIMEOUT.as_secs()
                ))
            });
        let _ = self
            .docker
            .remove_container(
                &name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;
        match outcome {
            Ok(()) => tracing::debug!(
                sandbox = %handle.sandbox_name,
                "handed the sandbox workspace back to uid {}:{}", owner.0, owner.1
            ),
            Err(reason) => tracing::warn!(
                sandbox = %handle.sandbox_name,
                "Could not hand the sandbox workspace back to uid {}:{} ({}); Temps may be \
                 unable to remove or replace files the sandbox wrote, and they stay on disk",
                owner.0,
                owner.1,
                reason
            ),
        }
    }

    /// Create, start and wait for container `name`; `Err` says which step
    /// failed, or the exit code.
    async fn run_to_completion(
        &self,
        name: &str,
        body: bollard::models::ContainerCreateBody,
    ) -> Result<(), String> {
        use futures::StreamExt;

        self.docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(name)
                        .build(),
                ),
                body,
            )
            .await
            .map_err(|error| format!("create {name}: {error}"))?;
        self.docker
            .start_container(
                name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|error| format!("start {name}: {error}"))?;
        let mut wait = self.docker.wait_container(
            name,
            None::<bollard::query_parameters::WaitContainerOptions>,
        );
        match wait.next().await {
            Some(Ok(response)) if response.status_code == 0 => Ok(()),
            Some(Ok(response)) => Err(format!("chown exited {}", response.status_code)),
            Some(Err(bollard::errors::Error::DockerContainerWaitError { code, error })) => {
                Err(format!("chown exited {code}: {error}"))
            }
            Some(Err(error)) => Err(format!("wait for {name}: {error}")),
            None => Err(format!("wait for {name}: no exit status")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspected(
        binds: Option<Vec<String>>,
        mounts: Option<Vec<bollard::models::Mount>>,
    ) -> bollard::models::ContainerInspectResponse {
        bollard::models::ContainerInspectResponse {
            host_config: Some(bollard::models::HostConfig {
                binds,
                mounts,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn volume_mount() -> bollard::models::Mount {
        bollard::models::Mount {
            typ: Some(bollard::models::MountType::VOLUME),
            source: Some("temps_data".into()),
            target: Some(CONTAINER_WORK_DIR.into()),
            volume_options: Some(bollard::models::MountVolumeOptions {
                subpath: Some("standalone-sandboxes/sbx_7120".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn root_never_needs_the_workspace_handed_back() {
        assert_eq!(needs_hand_back(Some((0, 0))), None);
        assert_eq!(needs_hand_back(None), None);
        assert_eq!(needs_hand_back(Some((1001, 1001))), Some((1001, 1001)));
    }

    #[test]
    fn process_owner_is_this_process_on_linux() {
        if cfg!(target_os = "linux") {
            assert!(process_owner().is_some());
        }
    }

    #[test]
    fn workspace_from_a_volume_mount_is_found_with_its_subpath() {
        let info = inspected(
            Some(vec![format!("home-vol:{SANDBOX_HOME}")]),
            Some(vec![volume_mount()]),
        );

        assert_eq!(
            workspace_source(&info),
            Some(WorkspaceSource::Mount(Box::new(volume_mount())))
        );
    }

    #[test]
    fn workspace_from_a_host_bind_is_found() {
        let bind = format!("/srv/temps/standalone-sandboxes/sbx_7120:{CONTAINER_WORK_DIR}");
        let info = inspected(
            Some(vec![format!("home-vol:{SANDBOX_HOME}"), bind.clone()]),
            None,
        );

        assert_eq!(workspace_source(&info), Some(WorkspaceSource::Bind(bind)));
    }

    #[test]
    fn a_container_without_a_workspace_has_nothing_to_hand_back() {
        let info = inspected(Some(vec![format!("home-vol:{SANDBOX_HOME}")]), None);

        assert_eq!(workspace_source(&info), None);
        assert_eq!(
            workspace_source(&bollard::models::ContainerInspectResponse::default()),
            None
        );
    }

    #[test]
    fn reclaim_container_chowns_only_the_workspace_with_minimal_privilege() {
        let body = reclaim_container(
            "sandbox-image:1",
            WorkspaceSource::Mount(Box::new(volume_mount())),
            (1001, 1001),
        );

        assert_eq!(body.image.as_deref(), Some("sandbox-image:1"));
        assert_eq!(body.user.as_deref(), Some("0:0"));
        assert_eq!(body.entrypoint, Some(vec!["chown".to_string()]));
        assert_eq!(
            body.cmd,
            Some(vec![
                "-hR".to_string(),
                "1001:1001".to_string(),
                "--".to_string(),
                CONTAINER_WORK_DIR.to_string(),
            ])
        );
        assert_eq!(body.network_disabled, Some(true));
        let host = body.host_config.expect("host config");
        assert_eq!(host.network_mode.as_deref(), Some("none"));
        assert_eq!(host.readonly_rootfs, Some(true));
        assert_eq!(host.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(
            host.cap_add,
            Some(vec!["CHOWN".to_string(), "DAC_READ_SEARCH".to_string()])
        );
        assert_eq!(host.privileged, None);
        assert_eq!(host.binds, None);
        let mounts = host.mounts.expect("the workspace mount");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].target.as_deref(), Some(CONTAINER_WORK_DIR));
        assert_eq!(
            mounts[0]
                .volume_options
                .as_ref()
                .and_then(|options| options.subpath.as_deref()),
            Some("standalone-sandboxes/sbx_7120")
        );
    }

    #[test]
    fn reclaim_container_reuses_a_host_bind_as_is() {
        let bind = format!("/srv/w:{CONTAINER_WORK_DIR}");

        let body = reclaim_container("img", WorkspaceSource::Bind(bind.clone()), (1001, 1002));

        let host = body.host_config.expect("host config");
        assert_eq!(host.binds, Some(vec![bind]));
        assert_eq!(host.mounts, None);
        assert_eq!(body.cmd.expect("cmd")[1], "1001:1002");
    }
}
