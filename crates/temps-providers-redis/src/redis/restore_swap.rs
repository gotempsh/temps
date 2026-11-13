// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The in-place restore of a Redis service, as the control plane and a
//! worker's agent both run it (ADR-099): one implementation, whichever
//! Docker daemon hosts the container.
//!
//! 1. With [`KEEP_TARGET_BOOKKEEPING`] set (another service's backup), read
//!    Temps' own bookkeeping keys from database 0 of the running server.
//! 2. Stop the container (its restart policy off) and, in a helper sharing
//!    its volume, stage the snapshot beside the data: WAL-G's
//!    `backup-fetch`, or the uploaded RDB. Nothing is replaced until it is
//!    whole and starts with an RDB header.
//! 3. Move the original `dump.rdb` and `appendonlydir` aside on the same
//!    volume (a rename), move the snapshot in, and write the multi-part AOF
//!    manifest that makes Redis 7+ load it whatever its `appendonly` setting.
//! 4. Start the container with its own restart policy and wait for it.
//! 5. Put the target's bookkeeping back when asked.
//! 6. Only then delete the kept original. When any step after the swap
//!    fails, the original is moved back and started as it was.
//!
//! The kept original is named after its restore, so a restore never
//! touches another's; one left behind by an interrupted restore makes the
//! next one refuse before it stops anything.

use std::collections::HashMap;
use std::path::Path;

use bollard::models::{
    ContainerCreateBody, ContainerUpdateBody, EndpointSettings, HostConfig, NetworkingConfig,
    RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, InspectContainerOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions, WaitContainerOptions,
};
use futures::StreamExt;
use tracing::{info, warn};

use super::super::error::{EngineError, Result};
use super::super::ServiceConfig;
use super::{RedisService, REDIS_BACKUP_EXEC_TIMEOUT};

/// Parameter of a restore's service configuration that asks for Temps'
/// bookkeeping keys to be put back to the target's after the restore. Set by
/// the orchestrator (with [`keep_target_bookkeeping`]) for another service's
/// backup, never stored.
pub const KEEP_TARGET_BOOKKEEPING: &str = "_restore_keeps_target_bookkeeping";

/// Mark `config` as a restore of another service's backup (see
/// [`KEEP_TARGET_BOOKKEEPING`]).
pub fn keep_target_bookkeeping(config: &mut ServiceConfig) {
    if let Some(parameters) = config.parameters.as_object_mut() {
        parameters.insert(
            KEEP_TARGET_BOOKKEEPING.to_string(),
            serde_json::Value::Bool(true),
        );
    }
}

/// Whether `config` asks for the target's bookkeeping to be kept.
pub(super) fn keeps_target_bookkeeping(config: &ServiceConfig) -> bool {
    config
        .parameters
        .get(KEEP_TARGET_BOOKKEEPING)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The keys Temps keeps in database 0 about the logical databases it hands
/// out (`_temps:redis_db_mapping:*`, `_temps:redis_db_owner:*`,
/// `_temps:redis_import_claim:*`).
pub(super) const BOOKKEEPING_PATTERN: &str = "_temps:*";

/// Where a snapshot is staged on the data volume before it replaces
/// anything.
pub(super) const STAGED_SNAPSHOT: &str = "/data/.temps-restore.rdb";

/// Prefix of the directory a restore keeps the original data in, on the
/// same volume; the restore's own token follows it.
const KEPT_ORIGINAL_PREFIX: &str = "/data/.temps-original-";

/// Where the snapshot comes from.
pub(super) enum Stage<'a> {
    /// `wal-g backup-fetch LATEST` from this repository, writing
    /// [`STAGED_SNAPSHOT`] through `WALG_STREAM_RESTORE_COMMAND`.
    Walg {
        prefix: &'a str,
        s3: &'a super::super::S3Credentials,
    },
    /// An RDB file on this host, uploaded into the helper.
    Rdb { host_path: &'a Path },
}

/// One in-place restore.
pub(super) struct SwapRestore<'a> {
    pub container: &'a str,
    pub password: &'a str,
    pub keep_target_bookkeeping: bool,
    pub stage: Stage<'a>,
    /// Entered before the container is touched, ahead of the restore's
    /// first write (stopping Redis): a restore cancelled (or, on a node,
    /// past its deadline) before then never stops the server, and one past
    /// it is let finish its swap or put the original back.
    pub gate: &'a dyn super::super::RestoreGate,
}

/// The token naming a restore's kept original: hex only, so it is safe in
/// the helper's script as it is.
fn restore_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

fn kept_original(token: &str) -> String {
    format!("{KEPT_ORIGINAL_PREFIX}{token}")
}

/// Refuse while an earlier restore's kept original is on the volume.
fn refuse_leftover_script() -> String {
    format!(
        "for kept in {KEPT_ORIGINAL_PREFIX}*; do\n\
         \x20 if [ -e \"$kept\" ]; then\n\
         \x20   echo \"an earlier restore's original data is still kept at $kept on the data \
         volume; move its dump.rdb and appendonlydir back or delete it, then restore again\" >&2\n\
         \x20   exit 4\n\
         \x20 fi\n\
         done\n"
    )
}

/// The swap, once the snapshot is staged: refuse anything that is not a whole
/// RDB, keep the original aside under `token`, move the snapshot in, write
/// the AOF manifest.
pub(super) fn swap_in_script(token: &str) -> String {
    let kept = kept_original(token);
    format!(
        "[ -s {STAGED_SNAPSHOT} ] || {{ echo 'the restore produced no snapshot' >&2; rm -f {STAGED_SNAPSHOT}; exit 3; }}\n\
         [ \"$(head -c 5 {STAGED_SNAPSHOT})\" = REDIS ] || {{ echo 'the restored file is not an RDB snapshot' >&2; rm -f {STAGED_SNAPSHOT}; exit 3; }}\n\
         mkdir {kept}\n\
         if [ -e /data/dump.rdb ]; then mv /data/dump.rdb {kept}/; fi\n\
         if [ -e /data/appendonlydir ]; then mv /data/appendonlydir {kept}/; fi\n\
         mv {STAGED_SNAPSHOT} /data/dump.rdb\n\
         mkdir /data/appendonlydir\n\
         cp /data/dump.rdb /data/appendonlydir/appendonly.aof.1.base.rdb\n\
         printf 'file appendonly.aof.1.base.rdb seq 1 type b\\n' > /data/appendonlydir/appendonly.aof.manifest\n\
         chown -R redis:redis /data/dump.rdb /data/appendonlydir\n\
         echo 'restored snapshot swapped in; original kept at {kept}'\n"
    )
}

/// Put the original kept under `token` back. A no-op when the swap never
/// happened, so it runs after any failure.
pub(super) fn roll_back_script(token: &str) -> String {
    let kept = kept_original(token);
    format!(
        "set -e\n\
         [ -d {kept} ] || exit 0\n\
         rm -rf /data/dump.rdb /data/appendonlydir {STAGED_SNAPSHOT}\n\
         if [ -e {kept}/dump.rdb ]; then mv {kept}/dump.rdb /data/; fi\n\
         if [ -e {kept}/appendonlydir ]; then mv {kept}/appendonlydir /data/; fi\n\
         rmdir {kept}\n\
         echo 'original data put back'\n"
    )
}

/// The helper script of `stage`.
fn stage_script(stage: &Stage<'_>, token: &str) -> String {
    let fetch = match stage {
        Stage::Walg { .. } => format!(
            "rm -f {STAGED_SNAPSHOT}\n\
             wal-g backup-fetch LATEST 2>&1 || {{ code=$?; rm -f {STAGED_SNAPSHOT}; exit $code; }}\n"
        ),
        Stage::Rdb { .. } => format!(
            "mv {dir}/{file} {STAGED_SNAPSHOT}\n",
            dir = super::RDB_RESTORE_DIR,
            file = super::RDB_RESTORE_FILE_NAME,
        ),
    };
    format!(
        "set -e\n{}{fetch}{}",
        refuse_leftover_script(),
        swap_in_script(token)
    )
}

/// The WAL-G helper's environment: the backup's repository, the source's
/// credentials, and the restore command that writes the staged snapshot.
async fn walg_env(
    docker: &bollard::Docker,
    container: &str,
    prefix: &str,
    s3: &super::super::S3Credentials,
) -> Vec<String> {
    let mut env = vec![
        format!("WALG_S3_PREFIX={prefix}"),
        format!("AWS_ACCESS_KEY_ID={}", s3.access_key_id),
        format!("AWS_SECRET_ACCESS_KEY={}", s3.secret_key),
        format!("AWS_REGION={}", s3.region),
        // Required by WAL-G even for a fetch.
        "WALG_STREAM_CREATE_COMMAND=echo noop".to_string(),
        format!("WALG_STREAM_RESTORE_COMMAND=cat > {STAGED_SNAPSHOT}"),
    ];
    env.extend(s3.session_token_env());
    if let Some(endpoint) = s3.resolve_endpoint_for_container(docker, container).await {
        env.push(format!("AWS_ENDPOINT={endpoint}"));
    }
    if s3.uses_path_style() {
        env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
    }
    env
}

impl RedisService {
    /// Run `restore` against its container (see the module documentation).
    pub(super) async fn restore_swapped(&self, restore: SwapRestore<'_>) -> Result<()> {
        // The last safe point: the container is neither read nor written
        // before it.
        restore.gate.begin_target_writes().await?;
        let container = restore.container;
        let inspect = self
            .docker
            .inspect_container(container, None::<InspectContainerOptions>)
            .await
            .map_err(|e| {
                EngineError::failed(
                    self.instance_name(),
                    format!("Failed to inspect Redis container '{container}' to restore it: {e}"),
                )
            })?;
        let image = inspect
            .config
            .as_ref()
            .and_then(|c| c.image.clone())
            .filter(|image| !image.is_empty())
            .ok_or_else(|| {
                EngineError::failed(
                    self.instance_name(),
                    format!("Redis container '{container}' reports no image to restore it with"),
                )
            })?;
        // The container's own policy (the agent's unless-stopped on a
        // worker, always on the control plane) is what it gets back.
        let restart_policy = inspect
            .host_config
            .as_ref()
            .and_then(|h| h.restart_policy.clone())
            .unwrap_or(RestartPolicy {
                name: Some(RestartPolicyNameEnum::ALWAYS),
                maximum_retry_count: None,
            });

        let bookkeeping = if restore.keep_target_bookkeeping {
            let keys = self.read_bookkeeping(container, restore.password).await?;
            info!(
                container,
                keys = keys.len(),
                "Read the target's Redis bookkeeping to keep it across another service's restore"
            );
            Some(keys)
        } else {
            None
        };

        let token = restore_token();
        info!(container, token = %token, "Stopping Redis to swap a restored snapshot in");
        self.set_restart_policy(
            container,
            RestartPolicy {
                name: Some(RestartPolicyNameEnum::NO),
                maximum_retry_count: None,
            },
        )
        .await?;
        if let Err(e) = self
            .docker
            .stop_container(container, None::<StopContainerOptions>)
            .await
        {
            let _ = self.set_restart_policy(container, restart_policy).await;
            return Err(EngineError::failed(
                self.instance_name(),
                format!("Failed to stop Redis container '{container}' for the restore: {e}"),
            ));
        }

        let env = match &restore.stage {
            Stage::Walg { prefix, s3 } => Some(walg_env(&self.docker, container, prefix, s3).await),
            Stage::Rdb { .. } => None,
        };
        let upload = match &restore.stage {
            Stage::Rdb { host_path } => Some(*host_path),
            Stage::Walg { .. } => None,
        };
        let swapped = self
            .run_volume_helper(
                container,
                &image,
                stage_script(&restore.stage, &token),
                env,
                upload,
            )
            .await;
        if let Err(error) = swapped {
            return Err(self
                .start_original_again(container, &image, &token, restart_policy, error)
                .await);
        }

        // From here the original is kept aside: every failure puts it back.
        if let Err(error) = self.start_and_wait(container, restart_policy.clone()).await {
            let _ = self
                .docker
                .stop_container(container, None::<StopContainerOptions>)
                .await;
            return Err(self
                .start_original_again(container, &image, &token, restart_policy, error)
                .await);
        }
        if let Some(keys) = &bookkeeping {
            if let Err(error) = self
                .write_bookkeeping(container, restore.password, keys)
                .await
            {
                let _ = self
                    .set_restart_policy(
                        container,
                        RestartPolicy {
                            name: Some(RestartPolicyNameEnum::NO),
                            maximum_retry_count: None,
                        },
                    )
                    .await;
                let _ = self
                    .docker
                    .stop_container(container, None::<StopContainerOptions>)
                    .await;
                return Err(self
                    .start_original_again(container, &image, &token, restart_policy, error)
                    .await);
            }
        }

        let kept = kept_original(&token);
        if let Err(error) = super::super::exec_util::run_exec(
            &self.docker,
            container,
            vec!["rm".to_string(), "-rf".to_string(), kept.clone()],
            None,
            REDIS_BACKUP_EXEC_TIMEOUT,
        )
        .await
        {
            warn!(
                container,
                kept = %kept,
                %error,
                "The restore succeeded, but its kept original could not be deleted; the next \
                 restore refuses until it is removed"
            );
        }
        info!(container, "Redis restored in place");
        Ok(())
    }

    /// After a failure that may follow the swap: put the original back (a
    /// no-op when nothing was swapped), give the container its policy back
    /// and start it. `cause` is the restore's own error, which this returns
    /// with what became of the original.
    async fn start_original_again(
        &self,
        container: &str,
        image: &str,
        token: &str,
        restart_policy: RestartPolicy,
        cause: EngineError,
    ) -> EngineError {
        let rolled_back = self
            .run_volume_helper(container, image, roll_back_script(token), None, None)
            .await;
        let started = match &rolled_back {
            Ok(()) => self.start_and_wait(container, restart_policy).await,
            Err(_) => {
                let _ = self.set_restart_policy(container, restart_policy).await;
                Ok(())
            }
        };
        let outcome = match (rolled_back, started) {
            (Ok(()), Ok(())) => "the original data was put back and is running again".to_string(),
            (Ok(()), Err(e)) => {
                format!("the original data was put back, but starting it again failed: {e}")
            }
            (Err(e), _) => format!(
                "putting the original data back failed ({e}); it is kept at {} on the data \
                 volume and the container is stopped",
                kept_original(token)
            ),
        };
        EngineError::failed(
            self.instance_name(),
            format!("Restore of Redis container '{container}' failed: {cause}; {outcome}"),
        )
    }

    async fn set_restart_policy(&self, container: &str, policy: RestartPolicy) -> Result<()> {
        self.docker
            .update_container(
                container,
                ContainerUpdateBody {
                    restart_policy: Some(policy),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                EngineError::failed(
                    self.instance_name(),
                    format!(
                        "Failed to set the restart policy of Redis container '{container}': {e}"
                    ),
                )
            })
    }

    async fn start_and_wait(&self, container: &str, policy: RestartPolicy) -> Result<()> {
        self.set_restart_policy(container, policy).await?;
        self.docker
            .start_container(container, None::<StartContainerOptions>)
            .await
            .map_err(|e| {
                EngineError::failed(
                    self.instance_name(),
                    format!("Failed to start Redis container '{container}': {e}"),
                )
            })?;
        self.wait_for_container_health(&self.docker, container)
            .await
    }

    /// Run `script` as root in a helper sharing `container`'s volumes, on the
    /// service network when it fetches (`env` set), on none otherwise;
    /// `upload` is copied into it first. Removed on every path.
    async fn run_volume_helper(
        &self,
        container: &str,
        image: &str,
        script: String,
        env: Option<Vec<String>>,
        upload: Option<&Path>,
    ) -> Result<()> {
        let fetches = env.is_some();
        if fetches {
            crate::utils::ensure_network_exists(&self.docker)
                .await
                .map_err(|e| {
                    EngineError::failed(
                        self.instance_name(),
                        format!("Failed to ensure the service network exists: {e:?}"),
                    )
                })?;
        }
        let helper_name = format!("{container}-restore-helper-{}", restore_token());
        let created = self
            .docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
                    cmd: Some(vec![script]),
                    env,
                    user: Some("root".to_string()),
                    labels: Some(super::super::restore_helper::restore_helper_labels(
                        container,
                    )),
                    host_config: Some(HostConfig {
                        volumes_from: Some(vec![container.to_string()]),
                        network_mode: (!fetches).then(|| "none".to_string()),
                        ..Default::default()
                    }),
                    networking_config: fetches.then(|| NetworkingConfig {
                        endpoints_config: Some(HashMap::from([(
                            temps_core::NETWORK_NAME.to_string(),
                            EndpointSettings::default(),
                        )])),
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                EngineError::failed(
                    self.instance_name(),
                    format!("Failed to create restore helper {helper_name} for '{container}': {e}"),
                )
            })?;
        let result = self
            .drive_helper(&created.id, &helper_name, container, upload)
            .await;
        let _ = self
            .docker
            .remove_container(
                &created.id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await;
        result
    }

    async fn drive_helper(
        &self,
        id: &str,
        helper_name: &str,
        container: &str,
        upload: Option<&Path>,
    ) -> Result<()> {
        if let Some(path) = upload {
            super::super::container_upload::upload_file_to_container(
                &self.docker,
                id,
                path,
                super::RDB_RESTORE_DIR,
                super::RDB_RESTORE_FILE_NAME,
                0o644,
            )
            .await
            .map_err(EngineError::from)?;
        }
        self.docker
            .start_container(id, None::<StartContainerOptions>)
            .await
            .map_err(|e| {
                EngineError::failed(
                    self.instance_name(),
                    format!("Failed to start restore helper {helper_name} for '{container}': {e}"),
                )
            })?;
        let waited = tokio::time::timeout(
            REDIS_BACKUP_EXEC_TIMEOUT,
            self.docker
                .wait_container(id, None::<WaitContainerOptions>)
                .next(),
        )
        .await;
        let mut logs = String::new();
        let mut stream = self.docker.logs(
            id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                follow: false,
                ..Default::default()
            }),
        );
        while let Some(Ok(chunk)) = stream.next().await {
            logs.push_str(&chunk.to_string());
        }
        let logs = logs.trim();
        let status = match waited {
            Err(_) => {
                return Err(EngineError::failed(
                    self.instance_name(),
                    format!(
                        "Restore helper {helper_name} for '{container}' did not exit within \
                         {REDIS_BACKUP_EXEC_TIMEOUT:?}. Logs: {logs}"
                    ),
                ))
            }
            Ok(Some(Ok(response))) => response.status_code,
            Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. }))) => code,
            Ok(Some(Err(e))) => {
                return Err(EngineError::failed(
                    self.instance_name(),
                    format!("Restore helper {helper_name} for '{container}' failed: {e}"),
                ))
            }
            Ok(None) => -1,
        };
        if status != 0 {
            return Err(EngineError::failed(
                self.instance_name(),
                format!(
                    "Restore helper {helper_name} for '{container}' exited with code {status}. \
                     Logs: {logs}"
                ),
            ));
        }
        info!(container, helper = %helper_name, "Restore helper: {logs}");
        Ok(())
    }

    /// `redis-cli` in `container` against database 0, the password in the
    /// environment: its output.
    async fn redis_cli(&self, container: &str, password: &str, args: &[&str]) -> Result<String> {
        let mut cmd = vec![
            "redis-cli".to_string(),
            "--no-auth-warning".to_string(),
            "-n".to_string(),
            "0".to_string(),
        ];
        cmd.extend(args.iter().map(|arg| arg.to_string()));
        let env = (!password.is_empty()).then(|| vec![format!("REDISCLI_AUTH={password}")]);
        let result = super::super::exec_util::run_exec(
            &self.docker,
            container,
            cmd,
            env,
            BOOKKEEPING_EXEC_TIMEOUT,
        )
        .await?;
        Ok(result.output)
    }

    /// The bookkeeping keys and their values, from the running server.
    pub(super) async fn read_bookkeeping(
        &self,
        container: &str,
        password: &str,
    ) -> Result<Vec<(String, String)>> {
        let listed = self
            .redis_cli(
                container,
                password,
                &["--scan", "--pattern", BOOKKEEPING_PATTERN],
            )
            .await?;
        let keys: Vec<&str> = listed.lines().filter(|key| !key.is_empty()).collect();
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["MGET"];
        args.extend(keys.iter().copied());
        let values = self.redis_cli(container, password, &args).await?;
        let values: Vec<&str> = values.lines().collect();
        if values.len() != keys.len() {
            return Err(EngineError::failed(
                self.instance_name(),
                format!(
                    "Reading the Redis bookkeeping of '{container}' returned {} values for {} keys",
                    values.len(),
                    keys.len()
                ),
            ));
        }
        Ok(keys
            .into_iter()
            .zip(values)
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect())
    }

    /// Replace the bookkeeping keys of the running server with `keys`.
    pub(super) async fn write_bookkeeping(
        &self,
        container: &str,
        password: &str,
        keys: &[(String, String)],
    ) -> Result<()> {
        let listed = self
            .redis_cli(
                container,
                password,
                &["--scan", "--pattern", BOOKKEEPING_PATTERN],
            )
            .await?;
        let restored: Vec<&str> = listed.lines().filter(|key| !key.is_empty()).collect();
        for batch in restored.chunks(256) {
            let mut args = vec!["DEL"];
            args.extend(batch.iter().copied());
            self.redis_cli(container, password, &args).await?;
        }
        if !keys.is_empty() {
            let mut args = vec!["MSET"];
            for (key, value) in keys {
                args.push(key);
                args.push(value);
            }
            let answer = self.redis_cli(container, password, &args).await?;
            if answer.trim() != "OK" {
                return Err(EngineError::failed(
                    self.instance_name(),
                    format!(
                        "Writing the target's Redis bookkeeping back into '{container}' was \
                         refused: {}",
                        answer.trim()
                    ),
                ));
            }
        }
        info!(
            container,
            removed = restored.len(),
            kept = keys.len(),
            "Put the target's Redis bookkeeping back after restoring another service's backup"
        );
        Ok(())
    }
}

/// How long one bookkeeping `redis-cli` call may take.
const BOOKKEEPING_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_is_read_from_the_restore_configuration_only() {
        let mut config = ServiceConfig {
            name: "cache".to_string(),
            service_type: super::super::super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({ "password": "pw" }),
        };
        assert!(!keeps_target_bookkeeping(&config));
        keep_target_bookkeeping(&mut config);
        assert!(keeps_target_bookkeeping(&config));
    }

    /// Nothing is replaced before the snapshot is whole, the original is
    /// kept under the restore's own name, and the roll-back touches only
    /// that one.
    #[test]
    fn the_swap_keeps_the_original_under_its_restore_and_refuses_a_leftover() {
        let swap = stage_script(
            &Stage::Rdb {
                host_path: Path::new("/x"),
            },
            "abc123",
        );
        let staged = swap.find("mv /tmp/temps-restore.rdb /data/.temps-restore.rdb");
        let leftover = swap.find("exit 4");
        let checked = swap.find("head -c 5");
        let kept = swap.find("mkdir /data/.temps-original-abc123");
        let replaced = swap.find("mv /data/.temps-restore.rdb /data/dump.rdb");
        assert!(leftover < staged && staged < checked && checked < kept && kept < replaced);
        assert!(swap.starts_with("set -e\n"), "{swap}");
        assert!(swap.contains("mv /data/dump.rdb /data/.temps-original-abc123/"));
        assert!(swap.contains("appendonly.aof.1.base.rdb seq 1 type b"));

        let back = roll_back_script("abc123");
        assert!(back.contains("[ -d /data/.temps-original-abc123 ] || exit 0"));
        assert!(back.contains("mv /data/.temps-original-abc123/dump.rdb /data/"));
        assert!(!back.contains("temps-original-*"));
    }

    #[test]
    fn a_walg_fetch_writes_only_the_staged_snapshot() {
        let s3 = super::super::super::S3Credentials {
            access_key_id: "a".into(),
            secret_key: "s".into(),
            session_token: None,
            region: "r".into(),
            endpoint: None,
            bucket_name: "b".into(),
            bucket_path: String::new(),
            force_path_style: false,
        };
        let script = stage_script(
            &Stage::Walg {
                prefix: "s3://b/p",
                s3: &s3,
            },
            "abc123",
        );
        assert!(script.contains("wal-g backup-fetch LATEST"));
        assert!(script.find("exit 4") < script.find("wal-g backup-fetch"));
        assert!(!script.contains("> /data/dump.rdb"), "{script}");
    }
}
