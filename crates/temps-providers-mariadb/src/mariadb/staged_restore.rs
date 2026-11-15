// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node half of a MariaDB in-place restore (ADR-099): everything a
//! restore does to the container, the same function on the control plane and
//! on a worker's agent.
//!
//! A WAL-G physical base is fetched and prepared by a helper that shares the
//! stopped service's volume, into a staging directory next to the live data.
//! Only once the base is complete is it swapped in, with the original data
//! moved aside on the same volume (a rename, never a copy). The restored
//! server is started and must answer; for a point-in-time recovery the
//! archived binary logs are replayed into it. Only then is the original
//! deleted. When any step after the swap fails, the restored data is removed,
//! the original moved back and the server started on it as it was, with the
//! restart policy it had.
//!
//! The original and the restored data share the volume until the restore
//! ends, so a restore needs room for both.

use super::*;
use crate::S3Credentials;

/// The data directory: the root of the service's volume.
const DATADIR: &str = "/var/lib/mysql";
/// Where the base is fetched and prepared, next to the live data.
const STAGING_DIR: &str = "/var/lib/mysql/.temps-restore-staging";
/// Where the original data waits while the restored server is verified.
const ORIGINAL_DIR: &str = "/var/lib/mysql/.temps-restore-original";
/// How long the restored (or put back) server may take to answer.
const READY_DEADLINE: Duration = Duration::from_secs(180);
/// Exit code of a helper that found the data of an interrupted restore.
const INTERRUPTED_EXIT: i64 = 64;
/// Exit code of a rollback helper that found no original to put back.
const NOTHING_TO_PUT_BACK_EXIT: i64 = 65;

/// Shell that puts the original data back over whatever is in the data
/// directory. `set +e`: it runs on a failure path and does what it can.
const PUT_BACK_FUNCTION: &str = r#"put_back() {
  set +e
  rm -rf "$S"
  if [ -d "$O" ]; then
    for e in "$D"/* "$D"/.[!.]* "$D"/..?*; do
      [ -e "$e" ] || [ -L "$e" ] || continue
      [ "$e" = "$O" ] && continue
      rm -rf -- "$e"
    done
    for e in "$O"/* "$O"/.[!.]* "$O"/..?*; do
      [ -e "$e" ] || [ -L "$e" ] || continue
      mv -- "$e" "$D"/
    done
    rmdir "$O" && echo "temps-mariadb-restore: original data put back" >&2
  fi
}"#;

/// What an in-place restore restores, and to which point.
pub struct MariaDbInPlaceRestore<'a> {
    pub s3_client: &'a aws_sdk_s3::Client,
    /// The S3 source: its bucket holds the backup, its base path the binary
    /// logs when the location does not name them.
    pub s3_credentials: &'a S3Credentials,
    /// A WAL-G repository (`s3://<bucket>/…/walg`), or the key of a logical
    /// dump or legacy physical base, in the source's bucket.
    pub location: &'a str,
    /// The sentinel user data that selects the WAL-G base; `None` restores
    /// the latest.
    pub target_user_data: Option<&'a str>,
    /// The point to recover to; `None` restores the backup as it was.
    pub recovery_target: Option<&'a RecoveryTarget>,
    /// Entered at the restore's last safe point: before the server is
    /// stopped for a WAL-G base, after a legacy base or a dump is downloaded
    /// and before it is applied. A worker's agent passes its deadline here,
    /// so a restore past it before it began writing never touches the
    /// server, and one that began is let finish or put the original back.
    pub gate: &'a dyn crate::RestoreGate,
}

/// A WAL-G base swapped into a container, the original kept aside until
/// [`MariaDbService::keep_restored`] or [`MariaDbService::put_original_back`].
pub(super) struct StagedSwap {
    container: String,
    image: String,
    /// The policy the container had before the restore, given back to it.
    restart_policy: Option<bollard::models::RestartPolicy>,
    /// The base's binary-log position, from `xtrabackup_binlog_info`.
    pub(super) coordinate: Option<MariaDbBinlogCoordinate>,
}

/// `(base path, service)` whose `binlog/` prefix holds the binary logs that
/// continue the backup at `key`: the service the backup was taken of, which
/// its layout names (`…/external_services/mariadb/<service>/…`). Another
/// service's backup replays that service's logs, never the target's own.
pub(super) fn binlog_archive_of(key: &str) -> Option<(String, String)> {
    let (before, after) = key.rsplit_once("external_services/mariadb/")?;
    let service = after.split('/').next().filter(|name| !name.is_empty())?;
    Some((before.trim_matches('/').to_string(), service.to_string()))
}

/// The object-key prefix, with a trailing slash, of the binary logs a
/// point-in-time recovery of the backup at `location` replays: those of the
/// service the backup was taken of ([`binlog_archive_of`]), or, for a
/// location whose layout names no service, `target_service`'s under
/// `bucket_path`. The same derivation the restore itself makes, so a
/// credential scoped to it reaches exactly the logs the replay reads.
pub fn binlog_read_prefix(
    location: &str,
    bucket: &str,
    bucket_path: &str,
    target_service: &str,
) -> String {
    let key = MariaDbService::backup_key_from_location(location, bucket);
    let (prefix, service) = binlog_archive_of(&key).unwrap_or_else(|| {
        (
            bucket_path.trim_matches('/').to_string(),
            target_service.to_string(),
        )
    });
    let manifest = MariaDbService::binlog_manifest_key(&prefix, &service);
    match manifest.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    }
}

impl MariaDbService {
    /// Restore `config`'s container in place (see the module docs). The
    /// control plane's `restore_in_place` and in-place `restore_pitr` are
    /// this, and so is a worker agent's MariaDB restore.
    pub async fn restore_in_place_on_this_node(
        &self,
        config: &MariaDbConfig,
        restore: MariaDbInPlaceRestore<'_>,
    ) -> Result<()> {
        let bucket = restore.s3_credentials.bucket_name.as_str();
        let key = Self::backup_key_from_location(restore.location, bucket);
        let Some(target) = restore.recovery_target else {
            if Self::is_walg_repository_location(&key) {
                restore.gate.begin_target_writes().await?;
                let swap = self
                    .stage_walg_base(
                        config,
                        restore.s3_credentials,
                        restore.location,
                        restore.target_user_data,
                    )
                    .await?;
                self.keep_restored(swap).await;
            } else if Self::is_physical_base_location(&key) {
                let temp_dir = tempfile::tempdir()?;
                let mbstream_path = temp_dir.path().join("base.mbstream");
                self.download_and_gunzip_base(restore.s3_client, bucket, &key, &mbstream_path)
                    .await?;
                restore.gate.begin_target_writes().await?;
                self.physical_restore_into_container(config, &mbstream_path)
                    .await?;
            } else {
                self.restore_logical_from_s3(
                    restore.s3_client,
                    bucket,
                    restore.location,
                    config,
                    restore.gate,
                )
                .await?;
            }
            info!(service = %self.name, "MariaDB in-place restore completed");
            return Ok(());
        };

        let legacy_coordinate = self
            .pitr_base_coordinate(restore.s3_client, bucket, restore.location)
            .await?;
        // Fail on a target MariaDB cannot honor before any data is touched.
        Self::recovery_target_to_stop_flag(target)?;
        let (prefix, archived_as) = binlog_archive_of(&key).unwrap_or_else(|| {
            (
                restore
                    .s3_credentials
                    .bucket_path
                    .trim_matches('/')
                    .to_string(),
                self.name.clone(),
            )
        });

        if Self::is_walg_repository_location(&key) {
            restore.gate.begin_target_writes().await?;
            let swap = self
                .stage_walg_base(
                    config,
                    restore.s3_credentials,
                    restore.location,
                    restore.target_user_data,
                )
                .await?;
            let replayed = async {
                let coordinate = swap.coordinate.clone().ok_or_else(|| {
                    EngineError::failed(
                        &self.name,
                        format!(
                            "the WAL-G base {} records no binary-log position, so it cannot \
                             start a point-in-time recovery",
                            restore.location
                        ),
                    )
                })?;
                self.replay_archived_binlogs(
                    restore.s3_client,
                    bucket,
                    &prefix,
                    &archived_as,
                    config,
                    &coordinate,
                    target,
                )
                .await
            }
            .await;
            match replayed {
                Ok(()) => {
                    self.keep_restored(swap).await;
                    info!(service = %self.name, "MariaDB point-in-time recovery completed");
                    Ok(())
                }
                Err(error) => match self.put_original_back(&swap, config).await {
                    Ok(()) => Err(EngineError::failed(
                        &self.name,
                        format!(
                            "point-in-time recovery failed and the original data was put back \
                             unchanged: {error}"
                        ),
                    )),
                    Err(rollback) => Err(EngineError::failed(
                        &self.name,
                        format!(
                            "point-in-time recovery failed ({error}), and putting the original \
                             data back failed too: {rollback}. The original is kept at \
                             {ORIGINAL_DIR} in container '{}'",
                            swap.container
                        ),
                    )),
                },
            }
        } else {
            // A legacy single-object base: the copy-back path, whose
            // coordinate comes from its metadata companion.
            let coordinate = legacy_coordinate.ok_or_else(|| {
                EngineError::failed(
                    &self.name,
                    "Legacy physical MariaDB backup has no binlog coordinate",
                )
            })?;
            let temp_dir = tempfile::tempdir()?;
            let mbstream_path = temp_dir.path().join("base.mbstream");
            self.download_and_gunzip_base(restore.s3_client, bucket, &key, &mbstream_path)
                .await?;
            restore.gate.begin_target_writes().await?;
            self.physical_restore_into_container(config, &mbstream_path)
                .await?;
            self.replay_archived_binlogs(
                restore.s3_client,
                bucket,
                &prefix,
                &archived_as,
                config,
                &coordinate,
                target,
            )
            .await
        }
    }

    /// The legacy base's binary-log position for a point-in-time recovery
    /// from `location`, `None` for a WAL-G repository (whose position is in
    /// the fetched base). Refuses a logical dump, or a legacy base taken
    /// without binary logging or with a position that is not a number.
    pub(super) async fn pitr_base_coordinate(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        location: &str,
    ) -> Result<Option<MariaDbBinlogCoordinate>> {
        let base_key = Self::backup_key_from_location(location, bucket);
        if Self::is_walg_repository_location(&base_key) {
            return Ok(None);
        }
        if !Self::is_physical_base_location(&base_key) {
            return Err(EngineError::invalid_input(
                &self.name,
                format!(
                    "PITR requires a physical (mariadb-backup) base backup; '{location}' is a \
                     logical dump and cannot be used for point-in-time recovery"
                ),
            ));
        }
        let metadata = self
            .fetch_base_metadata(s3_client, bucket, &base_key)
            .await?;
        let text = |field: &str| {
            metadata
                .get(field)
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string()
        };
        let engine = text("engine");
        let pitr_enabled = metadata
            .get("pitr")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let file = text("binlog_file");
        if engine != "mariadb_physical" || !pitr_enabled || file.is_empty() {
            return Err(EngineError::invalid_input(
                &self.name,
                format!(
                    "PITR requires a physical (mariadb-backup) base with binlog coordinates; \
                     base metadata has engine='{engine}', pitr={pitr_enabled}, \
                     binlog_file='{file}' — not usable for point-in-time recovery"
                ),
            ));
        }
        // `binlog_position` is free-form text from an S3-hosted
        // `metadata.json` that ends up in the replay command, so a tampered
        // base fails here, before any container work.
        let position = metadata
            .get("binlog_position")
            .and_then(|value| value.as_str())
            .unwrap_or("4");
        Ok(Some(MariaDbBinlogCoordinate {
            file: Self::normalize_binlog_filename(&file)?,
            position: Self::parse_binlog_position(
                Self::validate_binlog_position(position)?.as_str(),
                "physical base metadata",
            )?,
            gtid: text("gtid"),
        }))
    }

    /// Fetch the binary logs archived as `archived_as` from `coordinate` on
    /// and replay them into `config`'s running server, up to `target`.
    #[allow(
        clippy::too_many_arguments,
        reason = "where the logs are, where they go and how far: one argument each"
    )]
    pub(super) async fn replay_archived_binlogs(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        prefix: &str,
        archived_as: &str,
        config: &MariaDbConfig,
        coordinate: &MariaDbBinlogCoordinate,
        target: &RecoveryTarget,
    ) -> Result<()> {
        info!(
            service = %self.name,
            archived_as,
            binlog_file = %coordinate.file,
            binlog_position = coordinate.position,
            gtid = %coordinate.gtid,
            "Replaying archived MariaDB binary logs from the restored base"
        );
        let binlog_temp = tempfile::tempdir()?;
        let segments = self
            .fetch_binlogs_for_replay(
                s3_client,
                bucket,
                prefix,
                archived_as,
                &coordinate.file,
                binlog_temp.path(),
            )
            .await?;
        self.replay_binlogs(config, &segments, coordinate.position, target)
            .await
    }

    /// Fetch and prepare the WAL-G base `repository` into a staging
    /// directory of `config`'s stopped container, swap it in keeping the
    /// original aside, and start the server on it. On any failure the
    /// original is in place and the server started on it again.
    pub(super) async fn stage_walg_base(
        &self,
        config: &MariaDbConfig,
        s3_credentials: &S3Credentials,
        repository: &str,
        target_user_data: Option<&str>,
    ) -> Result<StagedSwap> {
        let container = self.get_live_container_name(config);
        let inspect = self
            .docker
            .inspect_container(&container, None::<InspectContainerOptions>)
            .await
            .map_err(|error| {
                EngineError::failed(
                    &self.name,
                    format!(
                        "Failed to inspect MariaDB container '{container}' before a WAL-G \
                         restore: {error}"
                    ),
                )
            })?;
        let image = inspect
            .config
            .as_ref()
            .and_then(|container_config| container_config.image.clone())
            .ok_or_else(|| {
                EngineError::failed(
                    &self.name,
                    format!(
                        "MariaDB container '{container}' does not report the image to restore \
                         with"
                    ),
                )
            })?;
        let restart_policy = inspect
            .host_config
            .as_ref()
            .and_then(|host| host.restart_policy.clone());

        let mut env = vec![
            format!("WALG_S3_PREFIX={repository}"),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            format!(
                "WALG_MYSQL_DATASOURCE_NAME=root:{}@tcp(127.0.0.1:3306)/mysql",
                config.root_password
            ),
            "WALG_STREAM_CREATE_COMMAND=echo noop".to_string(),
            format!("WALG_STREAM_RESTORE_COMMAND=mbstream -x -C {STAGING_DIR}"),
            format!("WALG_MYSQL_BACKUP_PREPARE_COMMAND=mariadb-backup --prepare --target-dir={STAGING_DIR}"),
        ];
        // Absent unless the source holds a temporary (STS-style) credential.
        env.extend(s3_credentials.session_token_env());
        if let Some(endpoint) = s3_credentials
            .resolve_endpoint_for_container(&self.docker, &container)
            .await
        {
            env.push(format!("AWS_ENDPOINT={endpoint}"));
        }
        if s3_credentials.uses_path_style() {
            env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }
        if let Some(target_user_data) = target_user_data {
            env.push(format!("WALG_FETCH_TARGET_USER_DATA={target_user_data}"));
        }
        let fetch_target = if target_user_data.is_some() {
            "--target-user-data \"$WALG_FETCH_TARGET_USER_DATA\""
        } else {
            "LATEST"
        };

        self.set_restart_policy(&container, Self::no_restart())
            .await?;
        self.stop_for_restore(&container).await;
        info!(
            service = %self.name,
            container = %container,
            repository,
            "Fetching a WAL-G MariaDB base into a staging directory"
        );
        let swap = StagedSwap {
            container: container.clone(),
            image,
            restart_policy,
            coordinate: None,
        };
        let script = format!(
            "set -eu\n\
             D={DATADIR}; S={STAGING_DIR}; O={ORIGINAL_DIR}\n\
             if [ -e \"$O\" ]; then\n\
               echo \"temps-mariadb-restore: $O is the original data of an earlier restore that \
             was interrupted after its swap; refusing to touch it\" >&2\n\
               exit {INTERRUPTED_EXIT}\n\
             fi\n\
             {PUT_BACK_FUNCTION}\n\
             trap 'status=$?; if [ \"$status\" -ne 0 ]; then put_back; fi' EXIT\n\
             rm -rf \"$S\"; mkdir -p \"$S\"\n\
             echo temps-mariadb-restore: fetch\n\
             wal-g backup-fetch {fetch_target}\n\
             if test -s \"$S/xtrabackup_binlog_info\"; then printf 'TEMPS_BINLOG_COORD='; cat \"$S/xtrabackup_binlog_info\"; fi\n\
             echo temps-mariadb-restore: swap\n\
             mkdir \"$O\"\n\
             for e in \"$D\"/* \"$D\"/.[!.]* \"$D\"/..?*; do\n\
               [ -e \"$e\" ] || [ -L \"$e\" ] || continue\n\
               case \"$e\" in \"$S\"|\"$O\") continue ;; esac\n\
               mv -- \"$e\" \"$O\"/\n\
             done\n\
             for e in \"$S\"/* \"$S\"/.[!.]* \"$S\"/..?*; do\n\
               [ -e \"$e\" ] || [ -L \"$e\" ] || continue\n\
               mv -- \"$e\" \"$D\"/\n\
             done\n\
             rmdir \"$S\"\n\
             chown -R mysql:mysql \"$D\"\n\
             echo temps-mariadb-restore: swapped"
        );
        let fetched = self
            .run_volume_helper(&swap, "walg-restore", script, env)
            .await
            .and_then(|logs| Self::parse_optional_walg_restore_coordinate(&logs));
        let coordinate = match fetched {
            Ok(coordinate) => coordinate,
            Err(error) => {
                // The helper put the original back (or never moved it).
                let restarted = self.start_as_it_was(&swap, config).await;
                return Err(match restarted {
                    Ok(()) => EngineError::failed(
                        &self.name,
                        format!(
                            "the WAL-G base {repository} could not be restored, and the original \
                             data was left in place and started again: {error}"
                        ),
                    ),
                    Err(start) => EngineError::failed(
                        &self.name,
                        format!(
                            "the WAL-G base {repository} could not be restored ({error}), and \
                             the original data could not be started again: {start}"
                        ),
                    ),
                });
            }
        };
        let swap = StagedSwap { coordinate, ..swap };

        // The restored server must answer before the original may go.
        let started = async {
            self.start_container_named(&swap.container).await?;
            self.wait_until_answering(&swap.container, config).await
        }
        .await;
        if let Err(error) = started {
            return Err(match self.put_original_back(&swap, config).await {
                Ok(()) => EngineError::failed(
                    &self.name,
                    format!(
                        "the restored MariaDB server did not come up ({error}); the original \
                         data was put back unchanged and started again"
                    ),
                ),
                Err(rollback) => EngineError::failed(
                    &self.name,
                    format!(
                        "the restored MariaDB server did not come up ({error}), and putting the \
                         original data back failed too: {rollback}. The original is kept at \
                         {ORIGINAL_DIR} in container '{}'",
                        swap.container
                    ),
                ),
            });
        }
        self.set_restart_policy(&swap.container, swap.restart_policy.clone())
            .await?;
        info!(
            service = %self.name,
            container = %swap.container,
            pitr_anchor = swap.coordinate.is_some(),
            "Restored MariaDB base is serving; the original data is kept aside until the \
             restore is confirmed"
        );
        Ok(swap)
    }

    /// The restore is confirmed: delete the original kept aside. A failure
    /// only costs space, and is logged: the restored data is in place (the
    /// next restore refuses to run until the directory is gone).
    pub(super) async fn keep_restored(&self, swap: StagedSwap) {
        if let Err(error) = self
            .run_container_command(
                &swap.container,
                vec!["rm".into(), "-rf".into(), ORIGINAL_DIR.into()],
                None,
                Duration::from_secs(600),
            )
            .await
        {
            error!(
                service = %self.name,
                container = %swap.container,
                %error,
                "The MariaDB restore succeeded, but deleting the original data kept at {} \
                 failed; delete it to free the space (a later restore refuses to run while it \
                 exists)",
                ORIGINAL_DIR
            );
        }
    }

    /// Undo a swap: stop the restored server, put the original data back,
    /// and start it with the restart policy it had.
    pub(super) async fn put_original_back(
        &self,
        swap: &StagedSwap,
        config: &MariaDbConfig,
    ) -> Result<()> {
        warn!(
            service = %self.name,
            container = %swap.container,
            "Putting the original MariaDB data back after a failed restore"
        );
        self.set_restart_policy(&swap.container, Self::no_restart())
            .await?;
        self.stop_for_restore(&swap.container).await;
        let script = format!(
            "D={DATADIR}; S={STAGING_DIR}; O={ORIGINAL_DIR}\n\
             {PUT_BACK_FUNCTION}\n\
             if [ ! -d \"$O\" ]; then echo \"temps-mariadb-restore: no original data at $O\" >&2; \
             exit {NOTHING_TO_PUT_BACK_EXIT}; fi\n\
             put_back\n\
             [ ! -e \"$O\" ]"
        );
        let put_back = self
            .run_volume_helper(swap, "restore-rollback", script, Vec::new())
            .await;
        let started = self.start_as_it_was(swap, config).await;
        put_back?;
        started
    }

    /// Give the container its restart policy back, start it, and wait until
    /// it answers.
    async fn start_as_it_was(&self, swap: &StagedSwap, config: &MariaDbConfig) -> Result<()> {
        let policy = self
            .set_restart_policy(&swap.container, swap.restart_policy.clone())
            .await;
        self.start_container_named(&swap.container).await?;
        self.wait_until_answering(&swap.container, config).await?;
        policy
    }

    fn no_restart() -> Option<bollard::models::RestartPolicy> {
        Some(bollard::models::RestartPolicy {
            name: Some(bollard::models::RestartPolicyNameEnum::NO),
            maximum_retry_count: None,
        })
    }

    async fn set_restart_policy(
        &self,
        container: &str,
        policy: Option<bollard::models::RestartPolicy>,
    ) -> Result<()> {
        let Some(policy) = policy else {
            return Ok(());
        };
        self.docker
            .update_container(
                container,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(policy.clone()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| {
                EngineError::failed(
                    &self.name,
                    format!(
                        "Failed to set the restart policy of MariaDB container '{container}' to \
                         {:?}: {error}",
                        policy.name
                    ),
                )
            })
    }

    async fn stop_for_restore(&self, container: &str) {
        if let Err(error) = self
            .docker
            .stop_container(
                container,
                Some(StopContainerOptions {
                    t: Some(30),
                    signal: None,
                }),
            )
            .await
        {
            // Already stopped is fine; anything else shows up when the
            // helper cannot get the volume.
            debug!(container, %error, "Stopping MariaDB for a restore");
        }
    }

    async fn start_container_named(&self, container: &str) -> Result<()> {
        self.docker
            .start_container(
                container,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|error| {
                EngineError::failed(
                    &self.name,
                    format!("Failed to start MariaDB container '{container}': {error}"),
                )
            })
    }

    /// Wait until the server in `container` answers a ping, bounded by
    /// [`READY_DEADLINE`]. A container without a healthcheck (a worker's)
    /// is running long before the server accepts connections.
    async fn wait_until_answering(&self, container: &str, config: &MariaDbConfig) -> Result<()> {
        self.wait_for_container_health(&self.docker, container)
            .await?;
        let deadline = std::time::Instant::now() + READY_DEADLINE;
        let mut config = config.clone();
        config.container_name = Some(container.to_string());
        loop {
            let answered = self.ping(&config).await;
            let running = self
                .docker
                .inspect_container(container, None::<InspectContainerOptions>)
                .await
                .ok()
                .and_then(|info| info.state)
                .and_then(|state| state.running)
                .unwrap_or(false);
            match answered {
                Ok(()) if running => return Ok(()),
                _ if !running => {
                    return Err(EngineError::failed(
                        &self.name,
                        format!(
                            "MariaDB container '{container}' stopped instead of starting: {}",
                            self.collect_container_logs(container).await.trim()
                        ),
                    ))
                }
                Err(error) if std::time::Instant::now() >= deadline => {
                    return Err(EngineError::failed(
                        &self.name,
                        format!(
                            "MariaDB in '{container}' did not answer within {}s: {error}",
                            READY_DEADLINE.as_secs()
                        ),
                    ))
                }
                _ => sleep(Duration::from_secs(1)).await,
            }
        }
    }

    /// Run `script` in a helper from the service's image that shares its
    /// stopped container's volumes, and return its output when it exits 0.
    /// The helper is removed on every path.
    async fn run_volume_helper(
        &self,
        swap: &StagedSwap,
        purpose: &str,
        script: String,
        env: Vec<String>,
    ) -> Result<String> {
        use bollard::models::{ContainerCreateBody, HostConfig};

        ensure_network_exists(&self.docker).await.map_err(|error| {
            EngineError::failed(
                &self.name,
                format!("Failed to ensure the restore network: {error:?}"),
            )
        })?;
        let container = &swap.container;
        let helper_name = format!("{container}-{purpose}-{}", uuid::Uuid::new_v4());
        let helper = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(swap.image.clone()),
                    cmd: Some(vec!["sh".to_string(), "-c".to_string(), script]),
                    env: Some(env),
                    user: Some("root".to_string()),
                    labels: Some(crate::externalsvc::restore_helper::restore_helper_labels(
                        container,
                    )),
                    host_config: Some(HostConfig {
                        volumes_from: Some(vec![container.clone()]),
                        ..Default::default()
                    }),
                    networking_config: Some(bollard::models::NetworkingConfig {
                        endpoints_config: Some(HashMap::from([(
                            temps_core::NETWORK_NAME.to_string(),
                            bollard::models::EndpointSettings::default(),
                        )])),
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| {
                EngineError::failed(
                    &self.name,
                    format!(
                        "Failed to create the MariaDB {purpose} helper for '{container}': {error}"
                    ),
                )
            })?;

        let outcome = async {
            self.docker
                .start_container(
                    &helper.id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|error| {
                    EngineError::failed(
                        &self.name,
                        format!(
                            "Failed to start the MariaDB {purpose} helper for '{container}': \
                             {error}"
                        ),
                    )
                })?;
            let wait = tokio::time::timeout(
                MARIADB_RESTORE_HELPER_TIMEOUT,
                self.docker
                    .wait_container(
                        &helper.id,
                        None::<bollard::query_parameters::WaitContainerOptions>,
                    )
                    .next(),
            )
            .await;
            let mut logs = String::new();
            let mut log_stream = self.docker.logs(
                &helper.id,
                Some(bollard::query_parameters::LogsOptions {
                    stdout: true,
                    stderr: true,
                    follow: false,
                    ..Default::default()
                }),
            );
            while let Some(chunk) = log_stream.next().await {
                match chunk {
                    Ok(output) => logs.push_str(&output.to_string()),
                    Err(error) => {
                        logs.push_str(&format!("\nunable to read the helper's logs: {error}"));
                        break;
                    }
                }
            }
            let exit_code = match wait {
                Err(_) => {
                    return Err(EngineError::failed(
                        &self.name,
                        format!(
                            "The MariaDB {purpose} helper for '{container}' timed out after \
                             {}s: {}",
                            MARIADB_RESTORE_HELPER_TIMEOUT.as_secs(),
                            logs.trim()
                        ),
                    ))
                }
                Ok(Some(Ok(response))) => response.status_code,
                Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError {
                    code, ..
                }))) => code,
                Ok(Some(Err(error))) => {
                    return Err(EngineError::failed(
                        &self.name,
                        format!(
                            "Failed waiting for the MariaDB {purpose} helper for '{container}': \
                             {error}. Logs: {}",
                            logs.trim()
                        ),
                    ))
                }
                Ok(None) => -1,
            };
            match exit_code {
                0 => Ok(logs),
                INTERRUPTED_EXIT => Err(EngineError::conflict(
                    &self.name,
                    format!(
                        "an earlier restore of container '{container}' was interrupted after \
                         swapping its data: the data it replaced is still at {ORIGINAL_DIR}. \
                         Check which data the service should keep, then remove that directory \
                         (or move it back) before restoring again"
                    ),
                )),
                code => Err(EngineError::failed(
                    &self.name,
                    format!(
                        "The MariaDB {purpose} helper for '{container}' exited with code \
                         {code}: {}",
                        logs.trim()
                    ),
                )),
            }
        }
        .await;

        if let Err(error) = self
            .docker
            .remove_container(
                &helper.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await
        {
            warn!(helper = %helper_name, %error, "Failed to remove a MariaDB restore helper");
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gate that refuses the write phase, as a node's deadline does once
    /// it expired before the restore began writing.
    struct Expired;

    #[async_trait::async_trait]
    impl crate::RestoreGate for Expired {
        fn is_cancelled(&self) -> bool {
            true
        }

        async fn begin_target_writes(&self) -> std::result::Result<(), crate::RestoreCancelled> {
            Err(crate::RestoreCancelled {
                target: "mariadb-shop".into(),
            })
        }
    }

    /// A WAL-G restore, in place or to a point in time, whose write phase is
    /// refused stops before the server is stopped: no Docker call is made.
    #[tokio::test]
    async fn a_refused_write_phase_stops_a_walg_restore_before_the_server_is_stopped() {
        // Never contacted: the restore must stop before any Docker call.
        let docker = Arc::new(
            Docker::connect_with_http("http://127.0.0.1:9", 1, bollard::API_DEFAULT_VERSION)
                .expect("lazy Docker client"),
        );
        let service = MariaDbService::new("shop".to_string(), docker);
        let config: MariaDbConfig = serde_json::from_value(serde_json::json!({
            "host": "localhost",
            "port": "3306",
            "database": "shop",
            "username": "shop",
            "password": "pw",
            "root_password": "root-pw",
            "docker_image": "mariadb:11"
        }))
        .expect("a MariaDB config");
        let s3_client = aws_sdk_s3::Client::from_conf(
            aws_sdk_s3::Config::builder()
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .region(aws_sdk_s3::config::Region::new("us-east-1"))
                .endpoint_url("http://127.0.0.1:9")
                .build(),
        );
        let credentials = S3Credentials {
            access_key_id: "AKID".to_string(),
            secret_key: "SECRET".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_name: "backups".to_string(),
            bucket_path: String::new(),
            force_path_style: true,
        };
        let point = RecoveryTarget::Time {
            time: chrono::Utc::now(),
        };
        for recovery_target in [None, Some(&point)] {
            let error = service
                .restore_in_place_on_this_node(
                    &config,
                    MariaDbInPlaceRestore {
                        s3_client: &s3_client,
                        s3_credentials: &credentials,
                        location: "s3://backups/external_services/mariadb/shop/walg",
                        target_user_data: None,
                        recovery_target,
                        gate: &Expired,
                    },
                )
                .await
                .expect_err("a refused write phase must stop the restore");
            assert!(
                matches!(error, EngineError::RestoreCancelled(_)),
                "pitr={}: expected a cancellation, got: {error}",
                recovery_target.is_some()
            );
        }
    }

    #[test]
    fn binlogs_are_those_of_the_service_the_backup_was_taken_of() {
        assert_eq!(
            binlog_archive_of("fleet/external_services/mariadb/shop/walg"),
            Some(("fleet".to_string(), "shop".to_string()))
        );
        assert_eq!(
            binlog_archive_of("a/b/external_services/mariadb/origin/walg"),
            Some(("a/b".to_string(), "origin".to_string()))
        );
        assert_eq!(
            binlog_archive_of("external_services/mariadb/shop/2026/11/08/u/base.mbstream.gz"),
            Some((String::new(), "shop".to_string()))
        );
        assert_eq!(binlog_archive_of("somewhere/else/walg"), None);
        assert_eq!(
            binlog_read_prefix(
                "s3://b/a/b/external_services/mariadb/origin/walg",
                "b",
                "x",
                "shop"
            ),
            "a/b/external_services/mariadb/origin/binlog/"
        );
        assert_eq!(
            binlog_read_prefix(
                "external_services/mariadb/shop/2026/11/08/u/base.mbstream.gz",
                "b",
                "",
                "t"
            ),
            "external_services/mariadb/shop/binlog/"
        );
        assert_eq!(
            binlog_read_prefix("somewhere/else/walg", "b", "/fleet/", "shop"),
            "fleet/external_services/mariadb/shop/binlog/"
        );
        assert_eq!(binlog_archive_of("external_services/mariadb//walg"), None);
    }

    /// The scripts carry no credential and name only the fixed directories.
    #[test]
    fn the_swap_scripts_are_fixed_and_keep_secrets_in_the_environment() {
        for text in [PUT_BACK_FUNCTION, STAGING_DIR, ORIGINAL_DIR] {
            assert!(!text.contains("PWD"), "{text}");
            assert!(!text.contains("SECRET"), "{text}");
        }
        assert!(STAGING_DIR.starts_with(DATADIR) && ORIGINAL_DIR.starts_with(DATADIR));
        assert!(PUT_BACK_FUNCTION.contains("set +e"));
    }
}
