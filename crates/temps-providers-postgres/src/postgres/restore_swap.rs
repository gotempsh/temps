// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The data-directory swap that ends a WAL-G restore, staged so a restore
//! that fails leaves the database it found.
//!
//! The fetched backup is moved into `PGDATA` and the original data directory
//! (with the archive credentials beside it) is moved aside to
//! [`WALG_PRE_RESTORE`] on the same volume: two renames, no copy. The
//! restored server is then started and must finish recovery and promote.
//! Only then is the kept original deleted. When it does not, the original is
//! moved back and its container started again as it was.
//!
//! How the container is started again depends on where PostgreSQL runs (see
//! [`SwapRecreate`]); the swap itself is the same everywhere.

use bollard::models::{ContainerCreateBody, HostConfig};
use tracing::{error, info, warn};

use super::node_recreate::{NodeArchiveMode, NodeContainerPlan};
use super::*;
use crate::externalsvc::{ExternalService, ServiceConfig, ServiceResourceLimits};

/// Where a restore keeps the original data directory (`data`) and archive
/// credentials (`walg.env`) until the restored server has promoted.
pub(super) const WALG_PRE_RESTORE: &str = "/var/lib/postgresql/pre_restore";

/// The write-capable WAL-G credentials `archive_command` reads.
const WALG_ENV: &str = "/var/lib/postgresql/walg.env";

/// Exit code of [`swap_in_script`] when an earlier restore's kept original is
/// still on the volume: nothing was moved.
const SWAP_REFUSED: i64 = 4;

/// Move the original `pgdata` (and `walg_env`) aside to `keep`, then the
/// fetched `restore_temp` into `pgdata`. Refuses with [`SWAP_REFUSED`],
/// moving nothing, while `keep` exists: it may hold the only copy of a
/// database an earlier restore could not put back.
pub(super) fn swap_in_script(
    pgdata: &str,
    restore_temp: &str,
    keep: &str,
    walg_env: &str,
) -> String {
    format!(
        "set -e; \
         if [ -e {keep} ]; then echo \"{keep} already exists: an earlier restore kept the original \
         data there\" >&2; exit {SWAP_REFUSED}; fi; \
         mkdir {keep}; \
         mv {pgdata} {keep}/data; \
         if [ -f {walg_env} ]; then mv {walg_env} {keep}/walg.env; fi; \
         mv {restore_temp} {pgdata}; \
         chmod 700 {pgdata}"
    )
}

/// Put the original kept by [`swap_in_script`] back: the restored `pgdata`
/// is deleted, the original and its archive credentials moved back, `keep`
/// removed. Fails without deleting anything when `keep` holds no data.
pub(super) fn swap_back_script(pgdata: &str, keep: &str, walg_env: &str) -> String {
    format!(
        "set -e; \
         if [ ! -d {keep}/data ]; then echo \"{keep}/data does not exist: there is no original \
         to put back\" >&2; exit 3; fi; \
         rm -rf {pgdata}; \
         mv {keep}/data {pgdata}; \
         if [ -f {keep}/walg.env ]; then mv {keep}/walg.env {walg_env}; fi; \
         rm -rf {keep}"
    )
}

/// How the container comes back after the swap, read before the restore
/// changes it.
pub(super) enum SwapRecreate {
    /// An imported container: its lifecycle is the operator's, so it is
    /// started again (`force_recreate`), never rebuilt.
    Imported {
        restored: ServiceConfig,
        original: ServiceConfig,
    },
    /// A control-plane container, rebuilt from the provider's specification.
    Managed {
        limits: ServiceResourceLimits,
        /// The original's `archive_mode`, from its command.
        original_archiving: bool,
        original: Box<PostgresConfig>,
    },
    /// A worker's container, rebuilt from its own inspected specification.
    Node {
        node: NodeRecreate,
        restored: Box<NodeContainerPlan>,
        original: Box<NodeContainerPlan>,
    },
}

/// The volumes of the container, as binds a helper container mounts. Read
/// up front: the container may be gone when a rollback needs them.
pub(super) type VolumeBinds = Vec<String>;

/// A swap that could not run, and whether it moved anything.
pub(super) struct SwapFailed {
    pub(super) moved: bool,
    pub(super) reason: String,
}

impl PostgresService {
    /// How the restore will start the container again, and the volumes it
    /// mounts. Read-only: called before the container is stopped.
    pub(super) async fn plan_swap_recreate(
        &self,
        container_name: &str,
        service_config: &ServiceConfig,
        postgres_config: &PostgresConfig,
        live_config: &PostgresConfig,
        live_service_config: &ServiceConfig,
    ) -> Result<(SwapRecreate, VolumeBinds)> {
        let inspect = self
            .docker
            .inspect_container(
                container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                EngineError::failed(
                    &self.name,
                    format!(
                        "could not inspect the container '{container_name}' to restore into: {e}"
                    ),
                )
            })?;
        let binds: VolumeBinds = inspect
            .mounts
            .iter()
            .flatten()
            .filter_map(|mount| {
                let source = mount.name.clone().or_else(|| mount.source.clone())?;
                let target = mount.destination.clone()?;
                Some(format!("{source}:{target}"))
            })
            .collect();
        if binds.is_empty() {
            return Err(EngineError::failed(
                &self.name,
                format!(
                    "the container '{container_name}' mounts no volume to restore into; nothing \
                     was changed"
                ),
            ));
        }
        let recreate = if let Some(node) = &self.node {
            SwapRecreate::Node {
                node: node.clone(),
                restored: Box::new(
                    self.node_container_plan(node, container_name, NodeArchiveMode::Off)
                        .await?,
                ),
                original: Box::new(
                    self.node_container_plan(node, container_name, NodeArchiveMode::Keep)
                        .await?,
                ),
            }
        } else if postgres_config.container_name.is_some() {
            SwapRecreate::Imported {
                restored: service_config.clone(),
                original: live_service_config.clone(),
            }
        } else {
            let command = inspect
                .config
                .as_ref()
                .and_then(|config| config.cmd.clone())
                .unwrap_or_default();
            SwapRecreate::Managed {
                limits: ServiceResourceLimits::from_parameters(&service_config.parameters),
                original_archiving: postgres_cmd_pins_archive_mode_on(&command),
                original: Box::new(live_config.clone()),
            }
        };
        Ok((recreate, binds))
    }

    /// Run `script` as root in a helper container of `image` mounting
    /// `binds`, and return its exit code. The helper is always removed.
    ///
    /// Root, because `PGDATA`'s parent belongs to root in the official
    /// images: `postgres` can change what is inside the data directory, not
    /// rename it. `mv` keeps every file's owner.
    pub(super) async fn run_volume_helper(
        &self,
        container_name: &str,
        image: &str,
        binds: &VolumeBinds,
        script: String,
    ) -> std::result::Result<i64, String> {
        use futures::StreamExt;
        let helper_name = format!("{container_name}-restore-helper");
        // A helper an interrupted restore left behind holds the name.
        let _ = self
            .docker
            .remove_container(
                &helper_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        let helper = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
                    cmd: Some(vec![script]),
                    host_config: Some(HostConfig {
                        binds: Some(binds.clone()),
                        ..Default::default()
                    }),
                    user: Some("0".to_string()),
                    labels: Some(crate::externalsvc::restore_helper::restore_helper_labels(
                        container_name,
                    )),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("could not create the helper container '{helper_name}': {e}"))?;
        let outcome = async {
            self.docker
                .start_container(
                    &helper.id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| {
                    format!("could not start the helper container '{helper_name}': {e}")
                })?;
            match self
                .docker
                .wait_container(
                    &helper.id,
                    None::<bollard::query_parameters::WaitContainerOptions>,
                )
                .next()
                .await
            {
                Some(Ok(done)) => Ok(done.status_code),
                // A non-zero exit is reported as this error by bollard.
                Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => {
                    Ok(code)
                }
                Some(Err(e)) => Err(format!(
                    "could not wait for the helper container '{helper_name}': {e}"
                )),
                None => Err(format!(
                    "the helper container '{helper_name}' ended without an exit code"
                )),
            }
        }
        .await;
        let logs = if matches!(outcome, Ok(code) if code != 0) {
            self.helper_logs(&helper.id).await
        } else {
            String::new()
        };
        let _ = self
            .docker
            .remove_container(
                &helper.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await;
        match outcome {
            Ok(code) if code != 0 && !logs.trim().is_empty() => {
                warn!(container = container_name, code, logs = %logs.trim(), "Restore helper failed");
                Ok(code)
            }
            other => other,
        }
    }

    async fn helper_logs(&self, id: &str) -> String {
        use futures::StreamExt;
        let mut logs = self.docker.logs(
            id,
            Some(bollard::query_parameters::LogsOptions {
                stdout: true,
                stderr: true,
                tail: "20".to_string(),
                ..Default::default()
            }),
        );
        let mut text = String::new();
        while let Some(Ok(line)) = logs.next().await {
            text.push_str(&line.to_string());
        }
        text
    }

    /// Refuse a restore, before it fetches anything, while an earlier one's
    /// kept original is still on the volume (see [`swap_in_script`]).
    pub(super) async fn ensure_no_kept_original(&self, container_name: &str) -> Result<()> {
        use temps_docker_util::{exec_capture, ExecCaptureOptions, ExitCodePolicy, OutputLimit};
        let probe = exec_capture(
            &self.docker,
            container_name,
            bollard::exec::CreateExecOptions {
                cmd: Some(vec!["test", "-e", WALG_PRE_RESTORE]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
            ExecCaptureOptions::combined(OutputLimit::Head(1024))
                .exit_code(ExitCodePolicy::BestEffort),
        )
        .await
        .map_err(|e| {
            EngineError::failed(
                &self.name,
                format!(
                    "could not check '{container_name}' for data an earlier restore kept: {}",
                    e.cause_text()
                ),
            )
        })?;
        match probe.exit_code {
            Some(1) => Ok(()),
            Some(0) => Err(EngineError::conflict(
                &self.name,
                format!(
                    "{WALG_PRE_RESTORE} exists on the data volume of '{container_name}': an \
                     earlier restore that could not finish kept the original data there. Check \
                     it, and remove it once it is no longer needed, before restoring again; \
                     nothing was changed"
                ),
            )),
            other => Err(EngineError::failed(
                &self.name,
                format!(
                    "could not check '{container_name}' for data an earlier restore kept: the \
                     check exited with {other:?}"
                ),
            )),
        }
    }

    /// Move the fetched backup into `PGDATA`, keeping the original aside.
    pub(super) async fn swap_in_restored(
        &self,
        container_name: &str,
        image: &str,
        binds: &VolumeBinds,
        pgdata: &str,
        restore_temp: &str,
    ) -> std::result::Result<(), SwapFailed> {
        let script = swap_in_script(pgdata, restore_temp, WALG_PRE_RESTORE, WALG_ENV);
        match self
            .run_volume_helper(container_name, image, binds, script)
            .await
        {
            Ok(0) => Ok(()),
            Ok(SWAP_REFUSED) => Err(SwapFailed {
                moved: false,
                reason: format!(
                    "{WALG_PRE_RESTORE} already exists on the data volume of '{container_name}': \
                     an earlier restore kept the original data there. Check it, and remove it \
                     once it is no longer needed"
                ),
            }),
            Ok(code) => Err(SwapFailed {
                moved: true,
                reason: format!("the PGDATA swap exited with code {code}"),
            }),
            Err(reason) => Err(SwapFailed {
                moved: false,
                reason,
            }),
        }
    }

    /// Start the restored data directory and verify it: recovery finished,
    /// promoted, archiving suspended.
    pub(super) async fn start_restored(
        &self,
        container_name: &str,
        recreate: &SwapRecreate,
        postgres_config: &PostgresConfig,
    ) -> Result<()> {
        match recreate {
            SwapRecreate::Imported { restored, .. } => {
                // Imported containers keep their external lifecycle; start()
                // will not recreate them, so restore the policy disabled for
                // the swap.
                self.set_restart_policy(
                    container_name,
                    bollard::models::RestartPolicyNameEnum::ALWAYS,
                )
                .await
                .map_err(|reason| PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: format!(
                        "could not restore the imported container's restart policy: {reason}"
                    ),
                })?;
                self.force_recreate(restored.clone()).await?;
            }
            SwapRecreate::Managed { limits, .. } => {
                // Restore orchestration does not persist a replacement host
                // port. Recreate at the registered endpoint, retaining PGDATA
                // if that endpoint cannot bind, instead of silently changing
                // its address.
                limits
                    .validate()
                    .map_err(|reason| PostgresArchiveVerificationError {
                        service_name: self.name.clone(),
                        reason: format!("invalid restored service resource limits: {reason}"),
                    })?;
                self.remove_restore_target(container_name).await?;
                self.create_container_once(&self.docker, postgres_config, limits, false)
                    .await?;
            }
            SwapRecreate::Node { node, restored, .. } => {
                self.remove_restore_target(container_name).await?;
                self.node_create_from_plan(node, restored).await?;
            }
        }
        // Docker's running state alone is not SQL readiness for an imported
        // container without a healthcheck. Authenticate with the restored
        // login and wait for recovery before checking policy or reporting
        // success.
        self.wait_for_restored_database(postgres_config).await?;
        let (mode, _) = self
            .active_archive_settings(postgres_config, "/bin/true")
            .await?;
        if mode != "off" {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the restored database did not suspend WAL archiving. Check the container's startup command and PostgreSQL configuration before retrying; restore into a new managed service if the imported container's policy cannot be changed safely".to_string(),
            }
            .into());
        }
        Ok(())
    }

    /// Delete the original kept by the swap, once the restored server is up.
    /// A failure leaves it on the volume, which the next restore reports.
    pub(super) async fn drop_kept_original(&self, container_name: &str) {
        let script = format!("rm -rf {WALG_PRE_RESTORE}");
        let exec = async {
            let exec = self
                .docker
                .create_exec(
                    container_name,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(vec!["sh", "-c", script.as_str()]),
                        // The helper that kept it ran as root.
                        user: Some("0"),
                        ..Default::default()
                    },
                )
                .await?;
            self.docker
                .start_exec(
                    &exec.id,
                    Some(bollard::exec::StartExecOptions {
                        detach: false,
                        ..Default::default()
                    }),
                )
                .await?;
            for _ in 0..600 {
                let inspect = self.docker.inspect_exec(&exec.id).await?;
                if inspect.running != Some(true) {
                    return Ok(inspect.exit_code);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Ok(None)
        };
        let result: std::result::Result<Option<i64>, bollard::errors::Error> = exec.await;
        match result {
            Ok(Some(0)) => info!(
                container = container_name,
                "Removed the original data kept during the restore"
            ),
            other => warn!(
                container = container_name,
                result = ?other,
                "Could not remove the original data kept during the restore at {}; remove it \
                 before the next restore",
                WALG_PRE_RESTORE
            ),
        }
    }

    /// Put the original data back after a restore that failed, and start its
    /// container as it was. `moved` is whether the swap moved anything.
    /// Returns what happened, for the restore's error.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call site, each a distinct input"
    )]
    pub(super) async fn roll_back_swap(
        &self,
        container_name: &str,
        image: &str,
        binds: &VolumeBinds,
        pgdata: &str,
        recreate: &SwapRecreate,
        moved: bool,
        live_config: &PostgresConfig,
    ) -> std::result::Result<(), String> {
        // Whatever runs under the name (the restored server, recovering or
        // restarting) must not hold the volume while it is changed.
        if let Err(reason) = self
            .set_restart_policy(container_name, bollard::models::RestartPolicyNameEnum::NO)
            .await
        {
            warn!(container = container_name, %reason, "Could not disable the restart policy before the rollback");
        }
        let _ = self
            .docker
            .stop_container(
                container_name,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(30),
                    signal: None,
                }),
            )
            .await;
        if moved {
            let script = swap_back_script(pgdata, WALG_PRE_RESTORE, WALG_ENV);
            match self
                .run_volume_helper(container_name, image, binds, script)
                .await
            {
                Ok(0) => {}
                Ok(code) => {
                    return Err(format!(
                        "putting the original data back exited with code {code}; it is kept at \
                         {WALG_PRE_RESTORE}/data on the data volume of '{container_name}'"
                    ))
                }
                Err(reason) => {
                    return Err(format!(
                        "putting the original data back failed ({reason}); it is kept at \
                         {WALG_PRE_RESTORE}/data on the data volume of '{container_name}'"
                    ))
                }
            }
        }
        let started = match recreate {
            SwapRecreate::Imported { original, .. } => {
                if let Err(reason) = self
                    .set_restart_policy(
                        container_name,
                        bollard::models::RestartPolicyNameEnum::ALWAYS,
                    )
                    .await
                {
                    warn!(container = container_name, %reason, "Could not restore the imported container's restart policy");
                }
                self.force_recreate(original.clone()).await
            }
            SwapRecreate::Managed {
                limits,
                original_archiving,
                original,
            } => match self.remove_restore_target(container_name).await {
                Ok(()) => {
                    self.create_container_once(&self.docker, original, limits, *original_archiving)
                        .await
                }
                Err(e) => Err(e),
            },
            SwapRecreate::Node { node, original, .. } => {
                match self.remove_restore_target(container_name).await {
                    Ok(()) => self.node_create_from_plan(node, original).await,
                    Err(e) => Err(e),
                }
            }
        };
        if let Err(e) = started {
            return Err(format!(
                "the original data is back in place, but its container could not be started: {e}"
            ));
        }
        if let Err(e) = self.wait_for_restored_database(live_config).await {
            error!(container = container_name, error = %e, "The original database did not come back after a failed restore");
            return Err(format!(
                "the original data is back in place, but the database did not become ready: {e}"
            ));
        }
        Ok(())
    }

    async fn remove_restore_target(&self, container_name: &str) -> Result<()> {
        match self
            .docker
            .remove_container(
                container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(error) => Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: format!("could not recreate the restored managed container: {error}"),
            }
            .into()),
        }
    }

    pub(super) async fn set_restart_policy(
        &self,
        container_name: &str,
        name: bollard::models::RestartPolicyNameEnum,
    ) -> std::result::Result<(), String> {
        self.docker
            .update_container(
                container_name,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(name),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    struct Volume {
        root: std::path::PathBuf,
    }

    impl Volume {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "pg-swap-{tag}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            std::fs::create_dir_all(root.join("18/docker")).unwrap();
            std::fs::write(root.join("18/docker/PG_VERSION"), "original").unwrap();
            std::fs::write(root.join("walg.env"), "WALG_S3_PREFIX=s3://b/p").unwrap();
            std::fs::create_dir_all(root.join("restore_temp")).unwrap();
            std::fs::write(root.join("restore_temp/PG_VERSION"), "restored").unwrap();
            Self { root }
        }
        fn path(&self, rel: &str) -> String {
            self.root.join(rel).display().to_string()
        }
        fn run(&self, script: &str) -> i32 {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .status()
                .unwrap()
                .code()
                .unwrap_or(-1)
        }
        fn read(&self, rel: &str) -> Option<String> {
            std::fs::read_to_string(self.root.join(rel)).ok()
        }
        fn swap_in(&self) -> i32 {
            self.run(&swap_in_script(
                &self.path("18/docker"),
                &self.path("restore_temp"),
                &self.path("pre_restore"),
                &self.path("walg.env"),
            ))
        }
        fn swap_back(&self) -> i32 {
            self.run(&swap_back_script(
                &self.path("18/docker"),
                &self.path("pre_restore"),
                &self.path("walg.env"),
            ))
        }
    }

    impl Drop for Volume {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The swap keeps the original and its archive credentials aside, and
    /// the restored directory gets the mode PostgreSQL requires.
    #[test]
    fn the_swap_keeps_the_original_beside_the_restored_data() {
        let volume = Volume::new("in");
        assert_eq!(volume.swap_in(), 0);
        assert_eq!(
            volume.read("18/docker/PG_VERSION").as_deref(),
            Some("restored")
        );
        assert_eq!(
            volume.read("pre_restore/data/PG_VERSION").as_deref(),
            Some("original")
        );
        assert!(volume.read("walg.env").is_none(), "archiving is suspended");
        assert!(volume.read("pre_restore/walg.env").is_some());
        assert!(!Path::new(&volume.path("restore_temp")).exists());
        let mode = std::fs::metadata(volume.path("18/docker"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    /// A failed restore puts back exactly what it found.
    #[test]
    fn swapping_back_restores_the_original_and_its_credentials() {
        let volume = Volume::new("back");
        assert_eq!(volume.swap_in(), 0);
        assert_eq!(volume.swap_back(), 0);
        assert_eq!(
            volume.read("18/docker/PG_VERSION").as_deref(),
            Some("original")
        );
        assert_eq!(
            volume.read("walg.env").as_deref(),
            Some("WALG_S3_PREFIX=s3://b/p")
        );
        assert!(!Path::new(&volume.path("pre_restore")).exists());
    }

    /// An original an earlier restore kept is never overwritten or deleted.
    #[test]
    fn a_kept_original_blocks_the_next_swap() {
        let volume = Volume::new("kept");
        std::fs::create_dir_all(volume.root.join("pre_restore/data")).unwrap();
        std::fs::write(volume.root.join("pre_restore/data/PG_VERSION"), "older").unwrap();
        assert_eq!(volume.swap_in(), SWAP_REFUSED as i32);
        assert_eq!(
            volume.read("18/docker/PG_VERSION").as_deref(),
            Some("original")
        );
        assert_eq!(
            volume.read("pre_restore/data/PG_VERSION").as_deref(),
            Some("older")
        );
        assert_eq!(
            volume.read("restore_temp/PG_VERSION").as_deref(),
            Some("restored")
        );
    }

    /// Without a kept original there is nothing to put back, and the live
    /// data directory is not touched.
    #[test]
    fn swapping_back_without_a_kept_original_deletes_nothing() {
        let volume = Volume::new("none");
        assert_eq!(volume.swap_back(), 3);
        assert_eq!(
            volume.read("18/docker/PG_VERSION").as_deref(),
            Some("original")
        );
    }
}
