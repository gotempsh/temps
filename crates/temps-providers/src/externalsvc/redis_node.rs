// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node half of Redis backups and restores (ADR-099): everything a
//! backup or restore does to the container. The control plane runs these
//! functions with its own Docker client for a Redis it hosts; a worker's
//! agent runs the same functions with the node's client for a Redis on that
//! worker.
//!
//! - **WAL-G stream backup** ([`redis_walg_backup`]): `wal-g backup-push` in
//!   the container, streaming `redis-cli --rdb -` straight to the S3 source.
//! - **RDB backup** ([`redis_rdb_backup`]), for an image without WAL-G: a
//!   pinned `redis-cli` helper on the service network takes an RDB snapshot,
//!   and its gzipped output is streamed into one object, a part at a time.
//! - **In-place restore** ([`restore_on_node`]): the provider's own
//!   `restore_from_s3`, which stages the snapshot beside the data, swaps it
//!   in keeping the original aside, and puts the original back on failure.
//!
//! The plane half (which engine, the S3 source, prefix and key, size,
//! metadata, the backup row and its WAL-G identity, restore admission) stays
//! in the backup engine. Neither half stops Redis for a backup.

use std::sync::Arc;

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, InspectContainerOptions, RemoveContainerOptionsBuilder,
    StartContainerOptions, WaitContainerOptionsBuilder,
};
use bollard::Docker;
use futures::StreamExt;
use temps_service_protocol::{
    RedisRdbBackupRequest, RedisRdbBackupResult, RedisRestoreRequest, RedisRestoreResult,
    RedisWalgBackupRequest, RedisWalgBackupResult, WorkerS3Destination,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::naming::{redis_container_name, shell_escape};
use super::postgres_walg_backup::{
    refuse_overlapping_backup_push, run_walg_exec, walg_identity_env, WalgBaseBackupError,
};
use super::redis::restore_swap::keep_target_bookkeeping;
use super::redis::{classify_redis_backup_location, RedisBackupLocationKind};
use super::worker_data::{
    helper_host_config, s3_credentials, s3_source_row, stream_helper_to_s3, StreamedHelper,
    WorkerDataError,
};
use super::{
    managed_instance_name, EngineError, RedisService, S3Credentials, SensitiveValues,
    ServiceConfig, ServiceType,
};

/// What `wal-g backup-push` streams. `redis-cli --rdb -` negotiates
/// diskless replication and writes the RDB followed by Redis's 40-byte
/// hexadecimal EOF marker, which `redis-check-rdb` accepts but Redis rejects
/// when the snapshot is loaded as an AOF base file on restore. `head -c -40`
/// keeps only a 40-byte tail buffer, so this stays a true stream from Redis
/// to WAL-G with no database-sized file.
pub const WALG_STREAM_CREATE_COMMAND: &str = "bash -c 'error=$(mktemp); redis-cli --rdb - 2>$error | head -c -40; statuses=(\"${PIPESTATUS[@]}\"); code=${statuses[0]}; if [ ${statuses[1]} -ne 0 ]; then code=${statuses[1]}; fi; cat $error >&2; if [ $code -ne 0 ] && grep -q \"Fail to fsync.*Invalid argument\" $error; then code=0; fi; rm -f $error; exit $code'";

/// How a fetch writes the stream back, as Temps Cloud's verified restores
/// expect it; Temps' own restore stages it instead (see `restore_swap`).
pub const WALG_STREAM_RESTORE_COMMAND: &str = "cat > /data/dump.rdb";

/// The `redis-cli` helper of an RDB backup: release and digest pinned, so
/// the helper that receives the password is always the same image.
pub const RDB_HELPER_IMAGE: &str =
    "redis:7.4.10-alpine@sha256:e7723ff73d963f5cc6d9c4643ea3d989527a402a319239054e9472a7fb9219a2";

/// The object name of an RDB backup.
pub const RDB_DUMP_FILE: &str = "dump.rdb.gz";

/// How much of `wal-g`'s stderr an error keeps.
const STDERR_TAIL_BYTES: usize = 2_000;

/// How long removing an abandoned Dragonfly snapshot waits for the `SAVE`
/// writing it: Dragonfly finishes a `SAVE` whose client was killed, and
/// renames its `.tmp` file into place when it ends.
const SNAPSHOT_SAVE_WAIT_SECS: u64 = 3_600;
/// How often that wait asks Dragonfly whether it is still saving.
const SNAPSHOT_POLL_SECS: u64 = 2;
/// The cleanup helper's exit code when it gave up waiting.
const SNAPSHOT_STILL_SAVING_EXIT: i64 = 3;

/// `<bucket_path>/external_services/redis/<service>`: where a Redis
/// service's backups live inside its S3 source.
fn service_root(bucket_path: &str, service_name: &str) -> String {
    let root = format!("external_services/redis/{service_name}");
    match bucket_path.trim_matches('/') {
        "" => root,
        base => format!("{base}/{root}"),
    }
}

/// `WALG_S3_PREFIX` of a Redis service: the same on the control plane and
/// on every worker, so either restores the other's backups.
pub fn walg_prefix(bucket: &str, bucket_path: &str, service_name: &str) -> String {
    format!(
        "s3://{bucket}/{}/walg",
        service_root(bucket_path, service_name)
    )
}

/// The object-key prefix of [`walg_prefix`] inside its bucket, with a
/// trailing slash.
pub fn walg_key_prefix(bucket_path: &str, service_name: &str) -> String {
    format!("{}/walg/", service_root(bucket_path, service_name))
}

/// The environment of `wal-g backup-push`: the repository, the source's
/// credentials, the stream commands, and the backup's identity in the
/// sentinel. The password travels in `REDISCLI_AUTH`, never argv.
pub fn walg_backup_env(
    walg_prefix: &str,
    s3: &S3Credentials,
    password: &str,
    backup_uuid: &str,
    container_endpoint: Option<String>,
) -> Vec<String> {
    let mut env = vec![
        format!("WALG_S3_PREFIX={walg_prefix}"),
        format!("AWS_ACCESS_KEY_ID={}", s3.access_key_id),
        format!("AWS_SECRET_ACCESS_KEY={}", s3.secret_key),
        format!("AWS_REGION={}", s3.region),
    ];
    if !password.is_empty() {
        env.push(format!("REDISCLI_AUTH={password}"));
    }
    env.extend([
        format!("WALG_STREAM_CREATE_COMMAND={WALG_STREAM_CREATE_COMMAND}"),
        format!("WALG_STREAM_RESTORE_COMMAND={WALG_STREAM_RESTORE_COMMAND}"),
    ]);
    // Absent unless the source holds a temporary credential.
    env.extend(s3.session_token_env());
    env.extend(walg_identity_env(backup_uuid));
    if let Some(endpoint) = container_endpoint {
        let url = temps_core::s3_endpoint::endpoint_url(&endpoint);
        env.push(format!("AWS_ENDPOINT={url}"));
    }
    if s3.uses_path_style() {
        env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
    }
    env
}

/// The RDB helper's script, gzipping one snapshot to stdout. The
/// container is reached by name on the service network.
///
/// - Redis (and Valkey, KeyDB) serve `redis-cli --rdb`, which takes the
///   snapshot over a replication handshake into a file in the helper (the
///   stream form carries a marker Redis rejects on restore). A replica
///   serves it like a primary.
/// - Dragonfly refuses that handshake: a replica with "Replicating a replica
///   is unsupported", a primary with an unknown `SYNC`. It is asked to
///   `SAVE RDB <snapshot>` into its own `dir` instead, which works in either
///   role; the helper reads the file through the container's volumes
///   (`workdir` is the container's, where a relative `dir` points) and
///   removes it afterwards.
/// - That `dir` is read from Dragonfly (a failed read stops the backup, it
///   is never taken for an empty `dir`) and normalized lexically (`.`, `..`,
///   repeated slashes; symlinks inside the container are not resolved). One
///   outside every one of the container's `mounts` is refused before the
///   `SAVE`: the snapshot would land in the container's own filesystem,
///   where neither this helper nor the cleanup can read or remove it, and
///   each attempt would leave another one there.
pub fn rdb_dump_script(
    container: &str,
    workdir: &str,
    mounts: &[String],
    snapshot: &str,
) -> String {
    let mounts = mount_roots(mounts);
    let in_volume = if mounts.is_empty() {
        String::new()
    } else {
        let patterns: Vec<String> = mounts
            .iter()
            .flat_map(|mount| {
                let mount = shell_escape(mount);
                [mount.clone(), format!("{mount}/*")]
            })
            .collect();
        format!("\n    {}) ;;", patterns.join("|"))
    };
    let listed = if mounts.is_empty() {
        "none".to_string()
    } else {
        mounts.join(", ")
    };
    format!(
        r#"set -e
host={host}
if redis-cli -h "$host" -p 6379 INFO server | grep -q '^dragonfly_version:'; then
  reply=$(redis-cli -h "$host" -p 6379 --raw CONFIG GET dir 2>&1) || true
  if [ "$(printf '%s\n' "$reply" | sed -n 1p | tr -d '\r')" != dir ]; then
    echo "Reading the snapshot dir of Dragonfly $host failed: $reply" >&2
    exit 1
  fi
  dir=$(printf '%s\n' "$reply" | sed -n 2p | tr -d '\r')
  case "$dir" in
    /*) ;;
    *) dir={workdir}/"$dir" ;;
  esac
  dir=$(printf '%s\n' "$dir" | awk -F/ '{{n = 0; for (i = 1; i <= NF; i++) {{ c = $i; if (c == "" || c == ".") continue; if (c == "..") {{ if (n > 0) n--; continue }} s[++n] = c }} o = ""; for (i = 1; i <= n; i++) o = o "/" s[i]; if (o == "") o = "/"; print o}}')
  case "$dir" in{in_volume}
    *)
      echo "Dragonfly $host saves snapshots to $dir, which is not on a volume of its container (volumes: "{listed}"), so a backup could neither read nor remove one there; point its dir at a volume to back it up" >&2
      exit 1 ;;
  esac
  f="$dir"/{snapshot}.rdb
  trap 'rm -f "$f" "$f.tmp"' EXIT
  echo "$host runs Dragonfly, which does not serve redis-cli --rdb: saving a snapshot to $f instead" >&2
  reply=$(redis-cli -h "$host" -p 6379 SAVE RDB {snapshot} 2>&1)
  if [ "$reply" != OK ]; then
    echo "SAVE RDB on Dragonfly $host failed: $reply" >&2
    exit 1
  fi
  if [ ! -f "$f" ]; then
    echo "Dragonfly $host reported saving $f, which this backup cannot find on its volumes" >&2
    exit 1
  fi
  gzip -c "$f"
else
  f="${{TMPDIR:-/tmp}}/temps-backup.rdb"
  redis-cli -h "$host" -p 6379 --rdb "$f" >&2
  gzip -c "$f"
  rm -f "$f"
fi"#,
        host = shell_escape(container),
        workdir = shell_escape(workdir),
        listed = shell_escape(&listed),
        snapshot = shell_escape(snapshot),
    )
}

/// The container's mount destinations without trailing slashes, as the
/// scripts match a snapshot's directory against them.
fn mount_roots(mounts: &[String]) -> Vec<&str> {
    mounts
        .iter()
        .map(|mount| mount.trim_end_matches('/'))
        .filter(|mount| !mount.is_empty())
        .collect()
}

/// The script removing snapshot `snapshot` after its backup's helper was
/// killed (an upload failure, a cancellation) before its own trap could.
///
/// It looks for the file by its unique name in the container's `mounts`,
/// the only place the backup lets Dragonfly save it, rather than asking
/// Dragonfly for its `dir` again: an unreachable Dragonfly then cannot point
/// it at the wrong directory. Dragonfly finishes a `SAVE` whose client was
/// killed, writing `<snapshot>.rdb.tmp` and renaming it when done, so the
/// script waits, bounded, until neither that file exists nor Dragonfly
/// reports `saving:1` on two checks in a row: a `SAVE` the helper sent just
/// before it was killed shows up between them. On a server that is not
/// Dragonfly no such file exists and nothing waits.
pub fn rdb_snapshot_cleanup_script(container: &str, mounts: &[String], snapshot: &str) -> String {
    let polls = SNAPSHOT_SAVE_WAIT_SECS / SNAPSHOT_POLL_SECS;
    let roots: Vec<String> = mount_roots(mounts).into_iter().map(shell_escape).collect();
    format!(
        r#"host={host}
name={snapshot}
snapshot_files() {{
  find {roots} -xdev -type f \( -name "$name.rdb" -o -name "$name.rdb.tmp" \) "$@" 2>/dev/null
}}
saving() {{
  [ -n "$(find {roots} -xdev -type f -name "$name.rdb.tmp" 2>/dev/null)" ] ||
    redis-cli -h "$host" -p 6379 INFO persistence 2>/dev/null | grep -q '^saving:1'
}}
i=0
quiet=0
while [ "$quiet" -lt 2 ]; do
  if saving; then quiet=0; else quiet=$((quiet + 1)); fi
  i=$((i + 1))
  if [ "$i" -gt {polls} ]; then
    snapshot_files -delete
    echo "Dragonfly $host was still saving after {wait}s; $name.rdb is left behind if that save completes" >&2
    exit {still_saving}
  fi
  if [ "$quiet" -lt 2 ]; then sleep {poll}; fi
done
snapshot_files -delete"#,
        host = shell_escape(container),
        snapshot = shell_escape(snapshot),
        roots = roots.join(" "),
        wait = SNAPSHOT_SAVE_WAIT_SECS,
        poll = SNAPSHOT_POLL_SECS,
        still_saving = SNAPSHOT_STILL_SAVING_EXIT,
    )
}

/// Why a Redis backup or restore did not happen.
#[derive(Debug, Error)]
pub enum RedisNodeError {
    #[error("Redis service {service_id} ('{service}') cannot be {operation} here: {reason}")]
    Refused {
        service_id: i32,
        service: String,
        operation: &'static str,
        reason: String,
    },
    #[error(
        "wal-g backup-push of Redis service {service_id} in '{container}' exited with code \
         {exit_code}. stderr: {stderr}"
    )]
    BackupPush {
        service_id: i32,
        container: String,
        exit_code: i64,
        stderr: String,
    },
    #[error("WAL-G exec in Redis container '{container}' failed: {reason}")]
    Exec { container: String, reason: String },
    #[error("The backup of Redis container '{container}' was cancelled")]
    Cancelled { container: String },
    #[error(
        "A wal-g backup-push of Redis service {service_id} is still running in '{container}', \
         left by an earlier attempt whose agent restarted or passed its deadline. Nothing was \
         started; the backup can run once it ends"
    )]
    BackupPushRunning { service_id: i32, container: String },
    #[error("RDB backup of Redis service {service_id} ('{service}') failed: {source}")]
    Rdb {
        service_id: i32,
        service: String,
        #[source]
        source: Box<WorkerDataError>,
    },
    #[error("Inspecting Redis container '{container}' failed: {reason}")]
    Inspect { container: String, reason: String },
    #[error("Pulling the Redis backup helper image {image} failed: {reason}")]
    HelperImage { image: String, reason: String },
    #[error(
        "Removing Dragonfly snapshot '{snapshot}' from the volume of Redis container \
         '{container}' failed: {reason}"
    )]
    SnapshotCleanup {
        container: String,
        snapshot: String,
        reason: String,
    },
    #[error("Restore of Redis service {service_id} ('{service}') failed: {source}")]
    Restore {
        service_id: i32,
        service: String,
        #[source]
        source: Box<EngineError>,
    },
}

impl RedisNodeError {
    /// Whether a retry can only repeat this error.
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

fn tail(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= STDERR_TAIL_BYTES {
        return if trimmed.is_empty() {
            "<empty>".to_string()
        } else {
            trimmed.to_string()
        };
    }
    let mut start = trimmed.len() - STDERR_TAIL_BYTES;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

/// A WAL-G exec's error as this engine's.
fn walg_error(error: WalgBaseBackupError, container_name: &str) -> RedisNodeError {
    match error {
        WalgBaseBackupError::Cancelled { container } => RedisNodeError::Cancelled { container },
        WalgBaseBackupError::BackupPushRunning {
            service_id,
            container,
        } => RedisNodeError::BackupPushRunning {
            service_id,
            container,
        },
        other => RedisNodeError::Exec {
            container: container_name.to_string(),
            reason: other.to_string(),
        },
    }
}

/// One WAL-G stream backup of a Redis service.
/// `Debug` redacts the password.
#[derive(Clone)]
pub struct RedisWalgBackup {
    pub service_id: i32,
    pub service: String,
    /// The container it runs in.
    pub container_name: String,
    pub password: String,
    pub s3: S3Credentials,
    pub backup_uuid: String,
}

impl std::fmt::Debug for RedisWalgBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisWalgBackup")
            .field("service_id", &self.service_id)
            .field("service", &self.service)
            .field("container_name", &self.container_name)
            .field("password", &"[REDACTED]")
            .field("s3", &self.s3)
            .field("backup_uuid", &self.backup_uuid)
            .finish()
    }
}

/// Push a WAL-G stream backup of `backup`'s Redis from its container on
/// `docker`. Returns on `cancel`.
pub async fn redis_walg_backup(
    docker: &Docker,
    backup: RedisWalgBackup,
    cancel: &CancellationToken,
) -> Result<(), RedisNodeError> {
    let RedisWalgBackup {
        service_id,
        service,
        container_name,
        password,
        s3,
        backup_uuid,
    } = backup;
    refuse_overlapping_backup_push(docker, service_id, &container_name, "Redis", cancel)
        .await
        .map_err(|error| walg_error(error, &container_name))?;
    let prefix = walg_prefix(&s3.bucket_name, &s3.bucket_path, &service);
    let endpoint = s3
        .resolve_endpoint_for_container(docker, &container_name)
        .await;
    let env = walg_backup_env(&prefix, &s3, &password, &backup_uuid, endpoint);
    info!(
        service_id,
        container = %container_name,
        repository = %prefix,
        "Pushing a Redis WAL-G stream backup"
    );
    let output = run_walg_exec(docker, &container_name, "wal-g backup-push", &env, cancel)
        .await
        .map_err(|error| walg_error(error, &container_name))?;
    if output.exit_code != 0 {
        return Err(RedisNodeError::BackupPush {
            service_id,
            container: container_name,
            exit_code: output.exit_code,
            stderr: SensitiveValues::new()
                .credential("", &password, None)
                .s3_credentials(&s3)
                .redact(&tail(&output.stderr)),
        });
    }
    Ok(())
}

/// One RDB backup of a Redis service.
/// `Debug` redacts the password.
#[derive(Clone)]
pub struct RedisRdbBackup {
    pub service_id: i32,
    pub service: String,
    /// The container the helper takes the snapshot of.
    pub container_name: String,
    pub password: String,
    pub destination: WorkerS3Destination,
    /// The `…/dump.rdb.gz` object it writes.
    pub key: String,
}

impl std::fmt::Debug for RedisRdbBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisRdbBackup")
            .field("service_id", &self.service_id)
            .field("service", &self.service)
            .field("container_name", &self.container_name)
            .field("password", &"[REDACTED]")
            .field("destination", &self.destination)
            .field("key", &self.key)
            .finish()
    }
}

/// The snapshot file one RDB backup asks Dragonfly to save into its
/// container's volume. `Debug` redacts the password.
#[derive(Clone)]
pub struct RdbSnapshot {
    pub container_name: String,
    /// The container's mount destinations, where the snapshot may be.
    pub mounts: Vec<String>,
    /// The file's name without `.rdb`, unique to the backup.
    pub name: String,
    pub password: String,
}

impl std::fmt::Debug for RdbSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RdbSnapshot")
            .field("container_name", &self.container_name)
            .field("mounts", &self.mounts)
            .field("name", &self.name)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Remove `snapshot` from its container's volume with a one-shot helper,
/// once Dragonfly has finished any `SAVE` still writing it. A no-op when
/// there is no such file, and when the container has no volume (the backup
/// refuses to save a snapshot then).
pub async fn remove_rdb_snapshot(
    docker: &Docker,
    snapshot: &RdbSnapshot,
) -> Result<(), RedisNodeError> {
    if mount_roots(&snapshot.mounts).is_empty() {
        return Ok(());
    }
    let failed = |reason: String| RedisNodeError::SnapshotCleanup {
        container: snapshot.container_name.clone(),
        snapshot: snapshot.name.clone(),
        reason,
    };
    let helper_name = format!(
        "temps-backup-cleanup-{}-{:08x}",
        snapshot.container_name.trim_start_matches('/'),
        rand::random::<u32>()
    );
    let env = if snapshot.password.is_empty() {
        vec![]
    } else {
        vec![format!("REDISCLI_AUTH={}", snapshot.password)]
    };
    let created = docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name(&helper_name)
                    .build(),
            ),
            ContainerCreateBody {
                image: Some(RDB_HELPER_IMAGE.to_string()),
                entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
                cmd: Some(vec![rdb_snapshot_cleanup_script(
                    &snapshot.container_name,
                    &snapshot.mounts,
                    &snapshot.name,
                )]),
                env: Some(env),
                labels: Some(
                    [
                        ("sh.temps.kind".to_string(), "backup".to_string()),
                        (
                            "sh.temps.backup_of".to_string(),
                            snapshot.container_name.clone(),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
                host_config: Some(helper_host_config(Some(&snapshot.container_name))),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| failed(format!("creating helper '{helper_name}': {error}")))?;
    let outcome = async {
        docker
            .start_container(&created.id, None::<StartContainerOptions>)
            .await
            .map_err(|error| failed(format!("starting helper '{helper_name}': {error}")))?;
        let deadline = std::time::Duration::from_secs(SNAPSHOT_SAVE_WAIT_SECS + 300);
        let waited = tokio::time::timeout(
            deadline,
            docker
                .wait_container(
                    &created.id,
                    Some(WaitContainerOptionsBuilder::new().build()),
                )
                .next(),
        )
        .await
        .map_err(|_| {
            failed(format!(
                "helper '{helper_name}' did not finish within {}s",
                deadline.as_secs()
            ))
        })?;
        let exit_code = match waited {
            Some(Ok(response)) => response.status_code,
            Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
            Some(Err(error)) => {
                return Err(failed(format!(
                    "waiting for helper '{helper_name}': {error}"
                )))
            }
            None => -1,
        };
        match exit_code {
            0 => Ok(()),
            SNAPSHOT_STILL_SAVING_EXIT => Err(failed(format!(
                "Dragonfly was still saving after {SNAPSHOT_SAVE_WAIT_SECS}s; the file is left \
                 behind if that save completes"
            ))),
            code => Err(failed(format!(
                "helper '{helper_name}' exited with code {code}"
            ))),
        }
    }
    .await;
    if let Err(error) = docker
        .remove_container(
            &created.id,
            Some(RemoveContainerOptionsBuilder::new().force(true).build()),
        )
        .await
    {
        warn!(helper = %helper_name, %error, "removing the snapshot cleanup helper failed");
    }
    outcome
}

/// Removes an RDB backup's Dragonfly snapshot when the backup ends any way
/// but success. The helper's own trap removes it when its script ends, but
/// a helper killed midway (a failed upload, a cancelled backup) never runs
/// it, and the `SAVE` it started carries on in Dragonfly: without this, each
/// such attempt would leave a dataset-sized file on the live data volume.
///
/// The cleanup starts only once the dump helper has stopped, so the helper
/// cannot send its `SAVE` after the cleanup looked.
struct SnapshotGuard {
    docker: Arc<Docker>,
    /// The dump helper's container name.
    helper: String,
    snapshot: Option<RdbSnapshot>,
}

impl SnapshotGuard {
    /// The backup succeeded, so the helper's trap already removed the file.
    fn disarm(mut self) {
        self.snapshot = None;
    }
}

impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        let Some(snapshot) = self.snapshot.take() else {
            return;
        };
        let docker = Arc::clone(&self.docker);
        let helper = std::mem::take(&mut self.helper);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    wait_until_stopped(&docker, &helper).await;
                    if let Err(error) = remove_rdb_snapshot(&docker, &snapshot).await {
                        warn!(
                            container = %snapshot.container_name,
                            snapshot = %snapshot.name,
                            %error,
                            "an abandoned Redis backup snapshot could not be removed"
                        );
                    }
                });
            }
            Err(_) => warn!(
                container = %snapshot.container_name,
                snapshot = %snapshot.name,
                "an abandoned Redis backup snapshot is left behind: no runtime to remove it"
            ),
        }
    }
}

/// How long the snapshot cleanup waits for the dump helper to stop.
const HELPER_STOP_WAIT_SECS: u64 = 120;

/// Wait, bounded, until container `name` is gone or no longer running: the
/// dump helper's own guard is removing it.
async fn wait_until_stopped(docker: &Docker, name: &str) {
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(HELPER_STOP_WAIT_SECS);
    loop {
        let running = match docker
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
        {
            Ok(inspect) => inspect
                .state
                .and_then(|state| state.running)
                .unwrap_or(false),
            Err(_) => false,
        };
        if !running {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(
                helper = %name,
                "the Redis dump helper is still running after {HELPER_STOP_WAIT_SECS}s; \
                 removing its snapshot anyway"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Whether `key` is an RDB backup of `service` (`…/external_services/redis/
/// <service>/…/dump.rdb.gz`), the only object the RDB backup writes.
fn is_rdb_key_of(key: &str, service: &str) -> bool {
    let root = format!("external_services/redis/{service}/");
    key.ends_with(&format!("/{RDB_DUMP_FILE}"))
        && (key.starts_with(&root) || key.contains(&format!("/{root}")))
        && !key.split('/').any(|segment| segment == "..")
}

/// Take an RDB snapshot of `backup`'s Redis with the pinned helper and
/// stream it, gzipped, into its key. Returns the bytes uploaded.
pub async fn redis_rdb_backup(
    docker: Arc<Docker>,
    backup: RedisRdbBackup,
) -> Result<u64, RedisNodeError> {
    let RedisRdbBackup {
        service_id,
        service,
        container_name,
        password,
        destination,
        key,
    } = backup;
    if !is_rdb_key_of(&key, &service) {
        return Err(RedisNodeError::Refused {
            service_id,
            service,
            operation: "backed up",
            reason: format!(
                "the object key '{key}' is not a {RDB_DUMP_FILE} under the service's own \
                 external_services/redis/ prefix"
            ),
        });
    }
    let inspect = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await
        .map_err(|error| RedisNodeError::Inspect {
            container: container_name.clone(),
            reason: error.to_string(),
        })?;
    let running = inspect
        .state
        .and_then(|state| state.running)
        .unwrap_or(false);
    // Where Dragonfly may save a snapshot the helper can read and remove.
    let mounts: Vec<String> = inspect
        .mounts
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mount| mount.destination)
        .collect();
    // Where a relative snapshot `dir` of Dragonfly points; Docker
    // starts a process in `/` when the image sets no working directory.
    let workdir = inspect
        .config
        .and_then(|config| config.working_dir)
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| "/".to_string());
    if !running {
        return Err(RedisNodeError::Refused {
            service_id,
            service,
            operation: "backed up",
            reason: format!(
                "its container '{container_name}' is not running; start the service before \
                 backing it up"
            ),
        });
    }
    if docker.inspect_image(RDB_HELPER_IMAGE).await.is_err() {
        temps_docker_util::pull_image(&docker, temps_docker_util::PullSpec::new(RDB_HELPER_IMAGE))
            .await
            .map_err(|error| RedisNodeError::HelperImage {
                image: RDB_HELPER_IMAGE.to_string(),
                reason: error.to_string(),
            })?;
    }
    let env = if password.is_empty() {
        vec![]
    } else {
        vec![format!("REDISCLI_AUTH={password}")]
    };
    info!(
        service_id,
        container = %container_name,
        bucket = %destination.bucket,
        key = %key,
        "Taking a Redis RDB backup"
    );
    let sensitive = SensitiveValues::new()
        .credential("", &password, None)
        .credential(
            &destination.access_key_id,
            &destination.secret_access_key,
            destination.session_token.as_deref(),
        );
    let snapshot = RdbSnapshot {
        container_name: container_name.clone(),
        mounts,
        name: format!("temps-backup-{:08x}", rand::random::<u32>()),
        password: password.clone(),
    };
    let script = rdb_dump_script(&container_name, &workdir, &snapshot.mounts, &snapshot.name);
    let helper_name = format!(
        "temps-worker-backup-{}-{:08x}",
        container_name.trim_start_matches('/'),
        rand::random::<u32>()
    );
    let guard = SnapshotGuard {
        docker: docker.clone(),
        helper: helper_name.clone(),
        snapshot: Some(snapshot),
    };
    let result = stream_helper_to_s3(
        docker.clone(),
        StreamedHelper {
            service: service.clone(),
            container: container_name.clone(),
            tool: "redis-cli --rdb",
            image: RDB_HELPER_IMAGE.to_string(),
            script,
            env,
            // Dragonfly's snapshot is a file in its volume.
            volumes_from: Some(container_name.clone()),
            name: Some(helper_name),
            sensitive,
        },
        &destination,
        &key,
    )
    .await;
    if result.is_ok() {
        guard.disarm();
    }
    result.map_err(|source| RedisNodeError::Rdb {
        service_id,
        service,
        source: Box::new(source),
    })
}

/// The managed container of `config`'s Redis on this node and its password,
/// or why this node does not back it up or restore it. An imported
/// container, under a name of its own, is refused: the restore writes to the
/// container the provider derives.
fn node_target(
    service_id: i32,
    config: &ServiceConfig,
    operation: &'static str,
) -> Result<(String, String), RedisNodeError> {
    let refused = |reason: String| RedisNodeError::Refused {
        service_id,
        service: config.name.clone(),
        operation,
        reason,
    };
    if !matches!(config.service_type, ServiceType::Redis | ServiceType::Kv) {
        return Err(refused(format!("it is a {} service", config.service_type)));
    }
    let container = redis_container_name(&managed_instance_name(&config.name, config.service_type));
    if let Some(recorded) = config
        .parameters
        .get("container_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty() && *name != container)
    {
        return Err(refused(format!(
            "it runs in imported container '{recorded}', and this node backs up and restores \
             only the container Temps created ('{container}')"
        )));
    }
    let password = config
        .parameters
        .get("password")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((container, password))
}

fn node_s3(destination: WorkerS3Destination, bucket_path: String) -> S3Credentials {
    S3Credentials {
        bucket_path,
        ..s3_credentials(&destination)
    }
}

/// The agent's side of `POST /agent/services/redis/walg-backup`.
pub async fn walg_backup_on_node(
    request: RedisWalgBackupRequest,
    docker: Arc<Docker>,
) -> RedisWalgBackupResult {
    let RedisWalgBackupRequest {
        service_id,
        service_config,
        destination,
        bucket_path,
        backup_uuid,
    } = request;
    let (container_name, password) = match node_target(service_id, &service_config, "backed up") {
        Ok(target) => target,
        Err(error) => {
            return RedisWalgBackupResult::Refused {
                reason: error.to_string(),
            }
        }
    };
    // The agent runs this detached under its own deadline, so it is never
    // cancelled midway; the token only serves the shared signature.
    let cancel = CancellationToken::new();
    match redis_walg_backup(
        &docker,
        RedisWalgBackup {
            service_id,
            service: service_config.name,
            container_name,
            password,
            s3: node_s3(destination, bucket_path),
            backup_uuid,
        },
        &cancel,
    )
    .await
    {
        Ok(()) => RedisWalgBackupResult::Completed,
        Err(error) if error.is_refusal() => RedisWalgBackupResult::Refused {
            reason: error.to_string(),
        },
        Err(error) => RedisWalgBackupResult::Failed {
            reason: error.to_string(),
        },
    }
}

/// The agent's side of `POST /agent/services/redis/rdb-backup`.
pub async fn rdb_backup_on_node(
    request: RedisRdbBackupRequest,
    docker: Arc<Docker>,
) -> RedisRdbBackupResult {
    let RedisRdbBackupRequest {
        service_id,
        service_config,
        destination,
        key,
    } = request;
    let (container_name, password) = match node_target(service_id, &service_config, "backed up") {
        Ok(target) => target,
        Err(error) => {
            return RedisRdbBackupResult::Refused {
                reason: error.to_string(),
            }
        }
    };
    match redis_rdb_backup(
        docker,
        RedisRdbBackup {
            service_id,
            service: service_config.name,
            container_name,
            password,
            destination,
            key,
        },
    )
    .await
    {
        Ok(size_bytes) => RedisRdbBackupResult::Completed { size_bytes },
        Err(error) if error.is_refusal() => RedisRdbBackupResult::Refused {
            reason: error.to_string(),
        },
        Err(error) => RedisRdbBackupResult::Failed {
            reason: error.to_string(),
        },
    }
}

/// The agent's side of `POST /agent/services/redis/restore`: the
/// provider's own in-place restore, against this node's container.
pub async fn restore_on_node(
    request: RedisRestoreRequest,
    docker: Arc<Docker>,
    writes: Arc<temps_service_protocol::TargetWriteBoundary>,
) -> RedisRestoreResult {
    let RedisRestoreRequest {
        service_id,
        mut service_config,
        source,
        location,
        keep_target_bookkeeping: keep,
        operation_id: _,
    } = request;
    let service = service_config.name.clone();
    let refused = |reason: String| RedisRestoreResult::Refused {
        reason: format!(
            "Redis service {service_id} ('{service}') cannot be restored on this node: {reason}"
        ),
    };
    let (container, _) = match node_target(service_id, &service_config, "restored") {
        Ok(target) => target,
        Err(error) => {
            return RedisRestoreResult::Refused {
                reason: error.to_string(),
            }
        }
    };
    match classify_redis_backup_location(&location) {
        RedisBackupLocationKind::Walg => {
            if !location.starts_with(&format!("s3://{}/", source.bucket)) {
                return refused(format!(
                    "the WAL-G repository '{location}' is outside the source's bucket '{}'",
                    source.bucket
                ));
            }
        }
        RedisBackupLocationKind::RdbGzip => {}
        RedisBackupLocationKind::Unsupported => {
            return refused(format!(
                "'{location}' is neither a WAL-G repository nor a .rdb.gz snapshot"
            ))
        }
    }
    if let Err(error) = docker
        .inspect_container(&container, None::<InspectContainerOptions>)
        .await
    {
        return refused(format!(
            "its container '{container}' cannot be inspected: {error}"
        ));
    }
    if keep {
        keep_target_bookkeeping(&mut service_config);
    }
    let credentials = s3_credentials(&source);
    let client = credentials.build_s3_client().await;
    let s3_source = s3_source_row(&source);
    info!(
        service_id,
        container = %container,
        location = %location,
        keep_target_bookkeeping = keep,
        "Restoring a Redis service in place"
    );
    let instance = RedisService::new(
        managed_instance_name(&service_config.name, service_config.service_type),
        docker,
    );
    // The node's deadline is the restore's gate: entered before the
    // container is touched, so a restore past it before then never stops
    // Redis, and one that stopped it is let finish its swap or put the
    // original back.
    match instance
        .restore_from_s3_gated(
            &client,
            &credentials,
            &location,
            &s3_source,
            service_config,
            writes.as_ref(),
        )
        .await
    {
        Ok(()) => RedisRestoreResult::Completed,
        Err(error) => RedisRestoreResult::Failed {
            reason: RedisNodeError::Restore {
                service_id,
                service,
                source: Box::new(error),
            }
            .to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(session_token: Option<&str>) -> S3Credentials {
        S3Credentials {
            access_key_id: "AKID".to_string(),
            secret_key: "SECRET".to_string(),
            session_token: session_token.map(str::to_string),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_name: "backups".to_string(),
            bucket_path: "fleet".to_string(),
            force_path_style: true,
        }
    }

    /// A Redis backup printed for a log or an error carries neither the
    /// server's password nor the S3 secret.
    #[test]
    fn redis_backups_debug_print_no_secret() {
        let walg = RedisWalgBackup {
            service_id: 8,
            service: "cache".to_string(),
            container_name: "redis-cache".to_string(),
            password: "redis-secret".to_string(),
            s3: credentials(Some("token-secret")),
            backup_uuid: "u-1".to_string(),
        };
        let rdb = RedisRdbBackup {
            service_id: 8,
            service: "cache".to_string(),
            container_name: "redis-cache".to_string(),
            password: "redis-secret".to_string(),
            destination: WorkerS3Destination {
                bucket: "backups".to_string(),
                region: "us-east-1".to_string(),
                endpoint: None,
                force_path_style: true,
                access_key_id: "AKID".to_string(),
                secret_access_key: "SECRET".to_string(),
                session_token: Some("token-secret".to_string()),
            },
            key: "fleet/external_services/redis/cache/u-1/dump.rdb.gz".to_string(),
        };
        let snapshot = RdbSnapshot {
            container_name: "redis-cache".to_string(),
            mounts: vec!["/data".to_string()],
            name: "temps-backup-0000abcd".to_string(),
            password: "redis-secret".to_string(),
        };
        for printed in [
            format!("{walg:?}"),
            format!("{rdb:?}"),
            format!("{snapshot:?}"),
        ] {
            for secret in ["redis-secret", "SECRET", "token-secret"] {
                assert!(!printed.contains(secret), "{printed}");
            }
            assert!(printed.contains("redis-cache"), "{printed}");
        }
    }

    fn config(parameters: serde_json::Value) -> ServiceConfig {
        ServiceConfig {
            name: "cache".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters,
        }
    }

    #[test]
    fn the_prefix_is_derived_from_the_source_and_the_service_alone() {
        assert_eq!(
            walg_prefix("backups", "", "cache"),
            "s3://backups/external_services/redis/cache/walg"
        );
        assert_eq!(
            walg_prefix("backups", "/team/a/", "cache"),
            "s3://backups/team/a/external_services/redis/cache/walg"
        );
        assert_eq!(
            walg_key_prefix("team/a", "cache"),
            "team/a/external_services/redis/cache/walg/"
        );
    }

    /// The exact environment the stream backup runs with, on either plane.
    #[test]
    fn the_stream_backup_runs_with_its_identity_and_the_password_in_the_environment() {
        let env = walg_backup_env(
            "s3://backups/fleet/external_services/redis/cache/walg",
            &credentials(Some("TOKEN")),
            "p'w",
            "uuid-1",
            Some("rustfs:9000".to_string()),
        );
        assert_eq!(
            env,
            vec![
                "WALG_S3_PREFIX=s3://backups/fleet/external_services/redis/cache/walg".to_string(),
                "AWS_ACCESS_KEY_ID=AKID".to_string(),
                "AWS_SECRET_ACCESS_KEY=SECRET".to_string(),
                "AWS_REGION=us-east-1".to_string(),
                "REDISCLI_AUTH=p'w".to_string(),
                format!("WALG_STREAM_CREATE_COMMAND={WALG_STREAM_CREATE_COMMAND}"),
                "WALG_STREAM_RESTORE_COMMAND=cat > /data/dump.rdb".to_string(),
                "AWS_SESSION_TOKEN=TOKEN".to_string(),
                "WALG_DELTA_MAX_STEPS=0".to_string(),
                "WALG_SENTINEL_USER_DATA={\"temps_backup_id\":\"uuid-1\"}".to_string(),
                "AWS_ENDPOINT=http://rustfs:9000".to_string(),
                "AWS_S3_FORCE_PATH_STYLE=true".to_string(),
            ]
        );
        let open = walg_backup_env("s3://b/p", &credentials(None), "", "u", None);
        assert!(!open.iter().any(|e| e.starts_with("REDISCLI_AUTH")));
        assert!(!open.iter().any(|e| e.starts_with("AWS_SESSION_TOKEN")));
    }

    #[test]
    fn the_stream_commands_match_the_cloud_restore_contract() {
        assert!(WALG_STREAM_CREATE_COMMAND.contains("redis-cli --rdb -"));
        assert!(WALG_STREAM_CREATE_COMMAND.contains("head -c -40"));
        assert!(WALG_STREAM_CREATE_COMMAND.contains("PIPESTATUS"));
        assert_eq!(WALG_STREAM_RESTORE_COMMAND, "cat > /data/dump.rdb");
        assert!(RDB_HELPER_IMAGE.contains("redis:7.4.10-alpine@sha256:"));
    }

    #[test]
    fn the_rdb_helper_keeps_the_password_out_of_its_script() {
        let script = rdb_dump_script(
            "redis-cache",
            "/data",
            &["/data".to_string()],
            "temps-backup-0000abcd",
        );
        assert!(script.contains("host='redis-cache'\n"));
        assert!(script.contains("redis-cli -h \"$host\" -p 6379 --rdb \"$f\""));
        assert!(script.contains("gzip -c \"$f\""));
        assert!(!script.contains("REDISCLI_AUTH"));
    }

    /// Run `script` under `sh` with a stub `redis-cli` playing Dragonfly:
    /// `CONFIG GET dir` answers `dir`, or an error reply when `None`; `INFO
    /// persistence` reports `saving:0`; `SAVE` answers `OK`. Returns whether
    /// the script succeeded, its stderr, and the commands it sent.
    fn run_against_stub_dragonfly(script: &str, dir: Option<&str>) -> (bool, String, String) {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "temps-redis-node-{}-{:08x}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("stub dir");
        let stub = bin.join("redis-cli");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             echo \"$*\" >> \"$STUB_LOG\"\n\
             case \"$*\" in\n\
             *'INFO server'*) printf 'dragonfly_version:df-test\\r\\n' ;;\n\
             *'INFO persistence'*) printf 'saving:0\\r\\n' ;;\n\
             *'CONFIG GET dir'*) if [ -n \"$STUB_NO_DIR\" ]; then echo 'ERR no such option'; \
             else printf 'dir\\r\\n%s\\r\\n' \"$STUB_DIR\"; fi ;;\n\
             *'SAVE RDB'*) echo OK ;;\n\
             esac\n",
        )
        .expect("stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let log = root.join("calls");
        let path = format!(
            "{}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            bin.display()
        );
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .env("PATH", path)
            .env("STUB_LOG", &log)
            .env("STUB_DIR", dir.unwrap_or_default());
        if dir.is_none() {
            command.env("STUB_NO_DIR", "1");
        }
        let output = command.output().expect("run sh");
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&root);
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            calls,
        )
    }

    /// Dragonfly refuses `--rdb` ("Replicating a replica is unsupported" on
    /// a replica), so it saves its own snapshot, which is read from its
    /// volume and removed on every path.
    #[test]
    fn dragonfly_saves_a_snapshot_instead_of_replicating() {
        let script = rdb_dump_script(
            "redis-cache",
            "/data",
            &["/data".to_string()],
            "temps-backup-0000abcd",
        );
        assert!(script.contains("trap 'rm -f \"$f\" \"$f.tmp\"' EXIT"));
        // redis-cli exits 0 on an error reply, so the reply itself is checked.
        assert!(script.contains("[ \"$reply\" != OK ]"));
        let (_, stderr, calls) = run_against_stub_dragonfly(&script, Some(""));
        assert!(
            calls.contains("SAVE RDB temps-backup-0000abcd"),
            "{calls}\n{stderr}"
        );
        assert!(!calls.contains("--rdb"), "{calls}");
        assert!(
            stderr.contains("saving a snapshot to /data/temps-backup-0000abcd.rdb"),
            "{stderr}"
        );
    }

    /// `dir` is normalized before it is matched against the volumes: a
    /// relative one joins the working directory with one separator, `.`,
    /// `..` and repeated slashes collapse, and a volume matches itself or
    /// what lies under it at a `/` boundary.
    #[test]
    fn a_snapshot_dir_is_normalized_before_it_is_matched_to_a_volume() {
        let mounts = ["/data/".to_string(), "/srv/cache".to_string()];
        for (workdir, dir, saved_to) in [
            ("/", "data", "/data"),
            ("/data", "", "/data"),
            ("/data", ".", "/data"),
            ("/", "//srv//cache/./snaps/", "/srv/cache/snaps"),
            ("/data", "../srv/cache", "/srv/cache"),
        ] {
            let script = rdb_dump_script("redis-cache", workdir, &mounts, "temps-backup-0000abcd");
            let (_, stderr, calls) = run_against_stub_dragonfly(&script, Some(dir));
            assert!(
                calls.contains("SAVE RDB"),
                "{workdir} + {dir:?} is on a volume\n{stderr}"
            );
            assert!(
                stderr.contains(&format!(
                    "saving a snapshot to {saved_to}/temps-backup-0000abcd.rdb"
                )),
                "{workdir} + {dir:?}\n{stderr}"
            );
        }
    }

    /// A snapshot outside every volume could be neither read nor removed, so
    /// such a `dir` is refused before Dragonfly is asked to save, however
    /// it is spelled.
    #[test]
    fn a_snapshot_dir_outside_the_volumes_is_refused_before_saving() {
        let mounts = ["/data".to_string(), "/srv/cache".to_string()];
        for (workdir, dir, resolved) in [
            ("/data", "/tmp", "/tmp"),
            ("/data", "/data/../tmp", "/tmp"),
            ("/data", "..", "/"),
            ("/", "data2", "/data2"),
            ("/srv", "cache2", "/srv/cache2"),
        ] {
            let script = rdb_dump_script("redis-cache", workdir, &mounts, "temps-backup-0000abcd");
            let (ok, stderr, calls) = run_against_stub_dragonfly(&script, Some(dir));
            assert!(!ok, "{workdir} + {dir:?} is refused");
            assert!(!calls.contains("SAVE"), "{workdir} + {dir:?}: {calls}");
            assert!(
                stderr.contains(&format!(
                    "saves snapshots to {resolved}, which is not on a volume of its container \
                     (volumes: /data, /srv/cache)"
                )),
                "{stderr}"
            );
        }

        // No volume at all: every dir is refused.
        let script = rdb_dump_script("redis-cache", "/", &[], "temps-backup-0000abcd");
        let (ok, stderr, calls) = run_against_stub_dragonfly(&script, Some("/data"));
        assert!(!ok && !calls.contains("SAVE"), "{calls}");
        assert!(stderr.contains("(volumes: none)"), "{stderr}");
    }

    /// A `dir` Dragonfly did not report is never taken for an empty one.
    #[test]
    fn a_failed_dir_lookup_stops_the_backup_before_saving() {
        let script = rdb_dump_script(
            "redis-cache",
            "/data",
            &["/data".to_string()],
            "temps-backup-0000abcd",
        );
        let (ok, stderr, calls) = run_against_stub_dragonfly(&script, None);
        assert!(!ok);
        assert!(!calls.contains("SAVE"), "{calls}");
        assert!(
            stderr.contains("Reading the snapshot dir of Dragonfly redis-cache failed: ERR"),
            "{stderr}"
        );
    }

    /// The cleanup finds the abandoned snapshot by its unique name in the
    /// container's volumes, wherever Dragonfly's `dir` put it, without asking
    /// Dragonfly for that `dir`, and leaves every other file alone.
    #[test]
    fn an_abandoned_snapshot_is_found_in_the_volumes_and_removed() {
        let volume = std::env::temp_dir().join(format!(
            "temps-redis-volume-{}-{:08x}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let nested = volume.join("snaps");
        std::fs::create_dir_all(&nested).expect("volume");
        // No `.tmp`: that is a save still running, which the Docker test
        // covers against a real Dragonfly.
        for file in [
            nested.join("temps-backup-0000abcd.rdb"),
            volume.join("dump.rdb"),
            volume.join("temps-backup-other.rdb"),
        ] {
            std::fs::write(&file, b"x").expect("file");
        }
        let script = rdb_snapshot_cleanup_script(
            "redis-cache",
            &[volume.display().to_string()],
            "temps-backup-0000abcd",
        );
        assert!(script.contains(&format!(
            "if [ \"$i\" -gt {} ]",
            SNAPSHOT_SAVE_WAIT_SECS / SNAPSHOT_POLL_SECS
        )));
        assert!(script.contains(&format!("exit {SNAPSHOT_STILL_SAVING_EXIT}")));
        assert!(!script.contains("REDISCLI_AUTH"));

        let (ok, stderr, calls) = run_against_stub_dragonfly(&script, None);
        let left: Vec<bool> = [
            nested.join("temps-backup-0000abcd.rdb"),
            volume.join("dump.rdb"),
            volume.join("temps-backup-other.rdb"),
        ]
        .iter()
        .map(|file| file.exists())
        .collect();
        let _ = std::fs::remove_dir_all(&volume);
        assert!(ok, "{stderr}");
        assert!(!calls.contains("CONFIG GET"), "{calls}");
        // Done only after two quiet checks of Dragonfly in a row.
        assert_eq!(calls.matches("INFO persistence").count(), 2, "{calls}");
        assert_eq!(left, [false, true, true]);
    }

    #[test]
    fn an_rdb_backup_writes_only_its_own_dump() {
        assert!(is_rdb_key_of(
            "external_services/redis/cache/2026/11/08/u/dump.rdb.gz",
            "cache"
        ));
        assert!(is_rdb_key_of(
            "fleet/external_services/redis/cache/2026/11/08/u/dump.rdb.gz",
            "cache"
        ));
        for key in [
            "external_services/redis/other/2026/u/dump.rdb.gz",
            "external_services/redis/cache/2026/u/dump.sql.gz",
            "external_services/redis/cache/../other/u/dump.rdb.gz",
            "elsewhere/dump.rdb.gz",
        ] {
            assert!(!is_rdb_key_of(key, "cache"), "{key}");
        }
    }

    /// Only the managed container of a Redis is backed up or restored here.
    #[test]
    fn the_node_target_is_the_managed_redis_container() {
        let (container, password) = node_target(
            4,
            &config(serde_json::json!({ "password": "pw" })),
            "backed up",
        )
        .expect("a managed Redis");
        assert_eq!(container, "redis-cache");
        assert_eq!(password, "pw");
        assert!(node_target(
            4,
            &config(serde_json::json!({ "container_name": "redis-cache" })),
            "backed up"
        )
        .is_ok());
        let imported = node_target(
            4,
            &config(serde_json::json!({ "container_name": "legacy-cache" })),
            "restored",
        )
        .expect_err("an imported container");
        assert!(imported.is_refusal());
        assert!(imported.to_string().contains("legacy-cache"), "{imported}");
        let mut mongo = config(serde_json::json!({}));
        mongo.service_type = ServiceType::Mongodb;
        assert!(node_target(4, &mongo, "backed up").is_err());
    }

    fn docker() -> Arc<Docker> {
        #[allow(clippy::unwrap_used, reason = "test helper with a constant address")]
        Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .unwrap(),
        )
    }

    fn destination() -> WorkerS3Destination {
        WorkerS3Destination {
            bucket: "backups".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: true,
            access_key_id: "a".to_string(),
            secret_access_key: "s".to_string(),
            session_token: None,
        }
    }

    /// A restore of something that is not a backup of this format, or of a
    /// repository in another bucket, is refused before any container call.
    #[tokio::test]
    async fn a_restore_outside_the_source_or_format_is_refused_first() {
        for location in [
            "s3://other-bucket/external_services/redis/cache/walg",
            "external_services/redis/cache/2026/u/dump.sql.gz",
        ] {
            let writes = Arc::new(temps_service_protocol::TargetWriteBoundary::new("cache"));
            let result = restore_on_node(
                RedisRestoreRequest {
                    service_id: 4,
                    service_config: config(serde_json::json!({})),
                    source: destination(),
                    location: location.to_string(),
                    keep_target_bookkeeping: false,
                    operation_id: None,
                },
                docker(),
                Arc::clone(&writes),
            )
            .await;
            assert!(
                matches!(&result, RedisRestoreResult::Refused { reason } if reason.contains("cannot be restored on this node")),
                "{location}: {result:?}"
            );
            assert!(
                !writes.writes_began(),
                "{location}: a refused restore never enters its write phase"
            );
        }
    }
}

/// Against a real Docker daemon. Skips without Docker.
#[cfg(test)]
#[cfg(feature = "docker-tests")]
mod docker_tests {
    use super::*;
    use crate::externalsvc::postgres_walg_backup::docker_tests::{
        container, docker_or_skip, let_the_push_start, remove, unreachable_s3, SURVIVING_PUSH,
    };

    /// As for PostgreSQL: a retry after the agent restarted, or after the
    /// deadline, must not start a second `backup-push` (a second RDB
    /// stream) while the first one still runs.
    #[tokio::test]
    async fn a_surviving_backup_push_is_not_overlapped() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        let name = container(&docker, SURVIVING_PUSH).await;
        let_the_push_start().await;
        let result = redis_walg_backup(
            &docker,
            RedisWalgBackup {
                service_id: 44,
                service: "cache".to_string(),
                container_name: name.clone(),
                password: "redis-secret".to_string(),
                s3: unreachable_s3(),
                backup_uuid: "b-2".to_string(),
            },
            &CancellationToken::new(),
        )
        .await;
        remove(&docker, &name).await;
        let error = result.expect_err("a second backup-push must not start");
        assert!(
            matches!(
                &error,
                RedisNodeError::BackupPushRunning { service_id: 44, container } if container == &name
            ),
            "unexpected error: {error}"
        );
        assert!(error.to_string().contains("still running"), "{error}");
        assert!(!error.is_refusal(), "a retry can succeed once it ends");
    }
}
