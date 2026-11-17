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

use bollard::query_parameters::InspectContainerOptions;
use bollard::Docker;
use temps_service_protocol::{
    RedisRdbBackupRequest, RedisRdbBackupResult, RedisRestoreRequest, RedisRestoreResult,
    RedisWalgBackupRequest, RedisWalgBackupResult, WorkerS3Destination,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::naming::{redis_container_name, shell_escape};
use super::postgres_walg_backup::{
    refuse_overlapping_backup_push, run_walg_exec, walg_identity_env, WalgBaseBackupError,
};
use super::redis::restore_swap::keep_target_bookkeeping;
use super::redis::{classify_redis_backup_location, RedisBackupLocationKind};
use super::worker_data::{
    s3_credentials, s3_source_row, stream_helper_to_s3, StreamedHelper, WorkerDataError,
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

/// The RDB helper's script: the snapshot goes to a file in the helper (the
/// stream form carries a marker Redis rejects on restore), then gzipped to
/// stdout. The container is reached by name on the service network.
pub fn rdb_dump_script(container: &str) -> String {
    format!(
        "set -e\n\
         f=\"${{TMPDIR:-/tmp}}/temps-backup.rdb\"\n\
         redis-cli -h {host} -p 6379 --rdb \"$f\" >&2\n\
         gzip -c \"$f\"\n\
         rm -f \"$f\"",
        host = shell_escape(container)
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
    let running = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await
        .map_err(|error| RedisNodeError::Inspect {
            container: container_name.clone(),
            reason: error.to_string(),
        })?
        .state
        .and_then(|state| state.running)
        .unwrap_or(false);
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
    stream_helper_to_s3(
        docker.clone(),
        StreamedHelper {
            service: service.clone(),
            container: container_name.clone(),
            tool: "redis-cli --rdb",
            image: RDB_HELPER_IMAGE.to_string(),
            script: rdb_dump_script(&container_name),
            env,
            sensitive,
        },
        &destination,
        &key,
    )
    .await
    .map_err(|source| RedisNodeError::Rdb {
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
        for printed in [format!("{walg:?}"), format!("{rdb:?}")] {
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
        let script = rdb_dump_script("redis-cache");
        assert!(script.contains("redis-cli -h 'redis-cache' -p 6379 --rdb \"$f\""));
        assert!(script.contains("gzip -c \"$f\""));
        assert!(!script.contains("REDISCLI_AUTH"));
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
