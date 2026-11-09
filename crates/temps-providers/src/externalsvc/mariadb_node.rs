// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node half of the MariaDB backup and restore engines (ADR-099):
//! everything they do to the container. The control plane runs these
//! functions with its own Docker client for a MariaDB it hosts; a worker's
//! agent runs the same functions with the node's client for a MariaDB on
//! that worker.
//!
//! - [`physical_backup`]: `wal-g backup-push` (a `mariadb-backup` mbstream
//!   streamed to S3 by WAL-G), then the binary-log position from
//!   `SHOW BINLOG STATUS`.
//! - [`dump_backup`]: `mariadb-dump` of every user database, gzipped in the
//!   container and streamed to S3 by multipart upload, without a host file.
//! - [`MariaDbService::restore_in_place_on_this_node`]: the staged WAL-G swap
//!   with rollback, point-in-time recovery by binary-log replay.
//! - [`MariaDbService::archive_binlogs`]: binary-log shipping.
//!
//! The plane half (engine choice, S3 source and its pin, layout, metadata,
//! records, credential reconciliation) stays in the backup engines.

use std::net::IpAddr;
use std::sync::Arc;

use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::Docker;
use futures::StreamExt;
use temps_docker_util::{
    exec_capture, DockerUtilError, ExecCaptureOptions, ExitCodePolicy, OutputLimit,
    StreamErrorPolicy,
};
use temps_service_protocol::{
    MariadbBinlogArchiveRequest, MariadbBinlogArchiveResult, MariadbBinlogCoordinate,
    MariadbDumpBackupRequest, MariadbDumpBackupResult, MariadbPhysicalBackupRequest,
    MariadbPhysicalBackupResult, MariadbRestoreRequest, MariadbRestoreResult, RecoveryTarget,
    WorkerS3Destination,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::mariadb::{MariaDbConfig, MariaDbInPlaceRestore, MariaDbService};
use super::postgres_walg_backup::{run_walg_exec, walg_identity_env, WalgBaseBackupError};
use super::worker_data::MultipartUpload;
use super::{EngineError, S3Credentials, ServiceConfig, ServiceType};

/// What WAL-G streams for a MariaDB base: `mariadb-backup --stream=mbstream`
/// as root over loopback. The password comes from `MYSQL_PWD` in the exec's
/// environment, never from this command.
pub const WALG_STREAM_CREATE_COMMAND: &str = "sh -ceu 'if command -v mariadb-backup >/dev/null 2>&1; then BK=mariadb-backup; else BK=mariabackup; fi; rm -rf /var/tmp/temps-mariadb-backup; mkdir -p /var/tmp/temps-mariadb-backup; exec \"$BK\" --backup --stream=mbstream --target-dir=/var/tmp/temps-mariadb-backup --user=root --host=127.0.0.1'";
const WALG_STREAM_RESTORE_COMMAND: &str = "mbstream -x -C /data";
const WALG_PREPARE_COMMAND: &str = "mariadb-backup --prepare --target-dir=/data";

/// In-container shell that dumps all user databases and gzips the result.
/// Credentials are not here: `-uroot` reads `MYSQL_PWD` from the exec's
/// environment.
///
/// Schema names are data a database user controls: each is one argument
/// (split on newlines only, never globbed) after `--`, so a schema named
/// like an option (`--host=...`) cannot redirect the dump, and the root
/// password with it.
pub const DUMP_SHELL: &str = "if command -v mariadb-dump >/dev/null 2>&1; then dump=mariadb-dump; else dump=mysqldump; fi; \
     if command -v mariadb >/dev/null 2>&1; then client=mariadb; else client=mysql; fi; \
     dbs=$($client -N -B -uroot -e \"SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME NOT IN ('information_schema','mysql','performance_schema','sys') ORDER BY SCHEMA_NAME\"); \
     if [ -z \"$dbs\" ]; then echo '-- No user databases to dump'; exit 0; fi; \
     set -f; IFS='\n'; set -- $dbs; unset IFS; \
     $dump --single-transaction --quick -uroot --databases -- \"$@\" | gzip";

/// The file name every MariaDB dump key ends with.
pub const DUMP_FILE_SUFFIX: &str = "dump.sql.gz";

/// How much stderr a dump keeps for its error: the end, where it explains.
const DUMP_STDERR_CAPTURE: usize = 16 * 1024;

/// `<bucket_path>/external_services/mariadb/<service>`: where a MariaDB
/// service's backups and binary logs live inside its S3 source.
pub fn service_root(bucket_path: &str, service_name: &str) -> String {
    let root = format!("external_services/mariadb/{service_name}");
    match bucket_path.trim_matches('/') {
        "" => root,
        base => format!("{base}/{root}"),
    }
}

/// `WALG_S3_PREFIX` of a MariaDB service: the same on the control plane and
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

/// `<service_root>/binlog/`: the only prefix the binary-log archiver
/// ([`MariaDbService::archive_binlogs`]) reads and writes, the segments and
/// their manifest.
pub fn binlog_key_prefix(bucket_path: &str, service_name: &str) -> String {
    format!("{}/binlog/", service_root(bucket_path, service_name))
}

/// The binary-log prefix a point-in-time recovery of `location` replays.
pub use temps_providers_mariadb::mariadb::binlog_read_prefix;

/// The environment of `wal-g backup-push` in a MariaDB container.
pub fn backup_push_env(
    prefix: &str,
    s3: &S3Credentials,
    root_password: &str,
    backup_uuid: &str,
) -> Vec<String> {
    let mut env = vec![
        format!("WALG_S3_PREFIX={prefix}"),
        format!("AWS_ACCESS_KEY_ID={}", s3.access_key_id),
        format!("AWS_SECRET_ACCESS_KEY={}", s3.secret_key),
        format!("AWS_REGION={}", s3.region),
        format!("MYSQL_PWD={root_password}"),
        format!("MARIADB_PWD={root_password}"),
        format!("WALG_MYSQL_DATASOURCE_NAME=root:{root_password}@tcp(127.0.0.1:3306)/mysql"),
        format!("WALG_STREAM_CREATE_COMMAND={WALG_STREAM_CREATE_COMMAND}"),
        format!("WALG_STREAM_RESTORE_COMMAND={WALG_STREAM_RESTORE_COMMAND}"),
        format!("WALG_MYSQL_BACKUP_PREPARE_COMMAND={WALG_PREPARE_COMMAND}"),
        "WALG_UPLOAD_CONCURRENCY=4".into(),
        "WALG_UPLOAD_DISK_CONCURRENCY=1".into(),
        "WALG_UPLOAD_QUEUE=2".into(),
        "WALG_TAR_SIZE_THRESHOLD=134217728".into(),
    ];
    // Only a temporary (STS-style) credential contributes an
    // `AWS_SESSION_TOKEN`; for a long-lived one the variable is absent.
    env.extend(super::aws_session_token_env(s3.session_token.as_deref()));
    env.extend(walg_identity_env(backup_uuid));
    env
}

/// Why a MariaDB node operation failed.
#[derive(Debug, Error)]
pub enum MariadbNodeError {
    #[error(
        "MariaDB service {service_id} ('{service}') cannot be {operation} on this node: {reason}"
    )]
    Refused {
        service_id: i32,
        service: String,
        operation: &'static str,
        reason: String,
    },
    #[error(
        "wal-g backup-push of MariaDB service {service_id} in '{container}' exited with code \
         {exit_code}. stderr: {stderr}"
    )]
    BackupPush {
        service_id: i32,
        container: String,
        exit_code: i64,
        stderr: String,
    },
    #[error(transparent)]
    Exec(#[from] WalgBaseBackupError),
    #[error("mariadb-dump of service {service_id} in '{container}' failed to {step}: {reason}")]
    DumpExec {
        service_id: i32,
        container: String,
        step: &'static str,
        reason: String,
    },
    #[error(
        "mariadb-dump of service {service_id} in '{container}' exited with code {exit_code}: \
         {stderr}"
    )]
    DumpFailed {
        service_id: i32,
        container: String,
        exit_code: i64,
        stderr: String,
    },
    #[error("mariadb-dump of service {service_id} in '{container}' produced no output")]
    EmptyDump { service_id: i32, container: String },
    #[error("mariadb-dump of service {service_id} in '{container}' was cancelled")]
    Cancelled { service_id: i32, container: String },
    #[error("uploading the dump of MariaDB service {service_id}: {source}")]
    Upload {
        service_id: i32,
        #[source]
        source: Box<super::worker_data::WorkerDataError>,
    },
}

impl MariadbNodeError {
    /// Whether a retry can only repeat this error.
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }

    /// Whether the operation stopped because it was cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            Self::Cancelled { .. } | Self::Exec(WalgBaseBackupError::Cancelled { .. })
        )
    }
}

/// One WAL-G physical base backup of a MariaDB service.
#[derive(Debug, Clone)]
pub struct MariadbPhysicalBackup {
    pub service_id: i32,
    /// The container it runs in.
    pub container_name: String,
    pub root_password: String,
    pub s3: S3Credentials,
    pub backup_uuid: String,
}

/// Push a WAL-G physical base of `backup`'s container to its repository
/// ([`walg_prefix`]) and return the binary-log position it was taken at
/// (`None` when binary logging is off: the base is valid, a point-in-time
/// recovery from it is not).
pub async fn physical_backup(
    docker: &Docker,
    backup: &MariadbPhysicalBackup,
    service_name: &str,
    cancel: &CancellationToken,
) -> Result<Option<MariadbBinlogCoordinate>, MariadbNodeError> {
    let prefix = walg_prefix(&backup.s3.bucket_name, &backup.s3.bucket_path, service_name);
    let mut env = backup_push_env(
        &prefix,
        &backup.s3,
        &backup.root_password,
        &backup.backup_uuid,
    );
    if let Some(endpoint) = backup
        .s3
        .resolve_endpoint_for_container(docker, &backup.container_name)
        .await
    {
        env.push(format!(
            "AWS_ENDPOINT={}",
            if endpoint.starts_with("http") {
                endpoint
            } else {
                format!("http://{endpoint}")
            }
        ));
    }
    if backup.s3.force_path_style {
        env.push("AWS_S3_FORCE_PATH_STYLE=true".into());
    }
    info!(
        service_id = backup.service_id,
        container = %backup.container_name,
        repository = %prefix,
        "Pushing a WAL-G MariaDB base backup"
    );
    let exec = run_walg_exec(
        docker,
        &backup.container_name,
        "wal-g backup-push",
        &env,
        cancel,
    )
    .await?;
    if exec.exit_code != 0 {
        return Err(MariadbNodeError::BackupPush {
            service_id: backup.service_id,
            container: backup.container_name.clone(),
            exit_code: exec.exit_code,
            stderr: tail(&exec.stderr, 2000),
        });
    }
    // The base is pushed; a missing position only costs its PITR.
    let coordinate = query_binlog_status(docker, &backup.container_name, &backup.root_password)
        .await
        .unwrap_or_else(|error| {
            warn!(
                service_id = backup.service_id,
                %error,
                "Could not read the MariaDB binary-log position after a base backup"
            );
            None
        });
    Ok(coordinate)
}

/// `SHOW BINLOG STATUS` and `@@global.gtid_binlog_pos` of the live server,
/// `None` when binary logging is off. The password goes through the exec's
/// environment, never its command.
async fn query_binlog_status(
    docker: &Docker,
    container_name: &str,
    root_password: &str,
) -> Result<Option<MariadbBinlogCoordinate>, String> {
    let mysql_pwd = format!("MYSQL_PWD={root_password}");
    let captured = exec_capture(
        docker,
        container_name,
        CreateExecOptions {
            cmd: Some(vec![
                "mariadb",
                "-N",
                "-B",
                "--user=root",
                "--host=127.0.0.1",
                "-e",
                "SHOW BINLOG STATUS; SELECT @@global.gtid_binlog_pos",
            ]),
            env: Some(vec![mysql_pwd.as_str()]),
            attach_stdout: Some(true),
            attach_stderr: Some(false),
            ..Default::default()
        },
        ExecCaptureOptions::separate(OutputLimit::Tail(64 * 1024), OutputLimit::Head(0))
            .stream_errors(StreamErrorPolicy::Stop)
            .exit_code(ExitCodePolicy::NotRead),
    )
    .await
    .map_err(|error| {
        let stage = match &error {
            DockerUtilError::ExecCreate { .. } => "create",
            _ => "start",
        };
        format!(
            "{stage} exec for binlog status on {container_name}: {}",
            error.cause_text()
        )
    })?;
    Ok(parse_binlog_status(&captured.stdout))
}

/// Line 1 of `-N -B` output is `<file>\t<position>\t…`, line 2 the GTID
/// position (empty when GTIDs are unused).
fn parse_binlog_status(output: &str) -> Option<MariadbBinlogCoordinate> {
    let mut lines = output.lines();
    let mut fields = lines.next()?.split('\t');
    let (file, position) = (fields.next()?, fields.next()?);
    if file.is_empty() || position.is_empty() {
        return None;
    }
    Some(MariadbBinlogCoordinate {
        file: file.to_string(),
        position: position.to_string(),
        gtid: lines.next().unwrap_or_default().trim().to_string(),
    })
}

/// One logical dump of a MariaDB service.
#[derive(Debug, Clone)]
pub struct MariadbDump {
    pub service_id: i32,
    pub service_name: String,
    pub container_name: String,
    pub root_password: String,
    pub bucket: String,
    /// The object it is written to.
    pub key: String,
}

/// Run [`DUMP_SHELL`] in `dump`'s container and stream its gzipped output
/// to `dump.key` by multipart upload; return the uploaded size. A failed,
/// empty or cancelled dump leaves no object behind.
pub async fn dump_backup(
    docker: &Docker,
    client: aws_sdk_s3::Client,
    dump: &MariadbDump,
    cancel: &CancellationToken,
) -> Result<u64, MariadbNodeError> {
    let container = dump.container_name.clone();
    let exec_error = |step: &'static str, reason: String| MariadbNodeError::DumpExec {
        service_id: dump.service_id,
        container: container.clone(),
        step,
        reason,
    };
    let env = [
        format!("MYSQL_PWD={}", dump.root_password),
        format!("MARIADB_PWD={}", dump.root_password),
    ];
    let exec = docker
        .create_exec(
            &container,
            CreateExecOptions {
                cmd: Some(vec!["sh", "-c", DUMP_SHELL]),
                env: Some(env.iter().map(String::as_str).collect()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| exec_error("create its exec", error.to_string()))?;
    let StartExecResults::Attached { mut output, .. } = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|error| exec_error("start its exec", error.to_string()))?
    else {
        return Err(exec_error(
            "attach to its exec",
            "the exec started detached".to_string(),
        ));
    };
    let mut upload = MultipartUpload::start_in(
        client,
        &dump.bucket,
        &dump.key,
        &dump.service_name,
        "application/x-gzip",
    )
    .await
    .map_err(|source| MariadbNodeError::Upload {
        service_id: dump.service_id,
        source: Box::new(source),
    })?;
    let mut stderr = String::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                upload.abort().await;
                return Err(MariadbNodeError::Cancelled {
                    service_id: dump.service_id,
                    container,
                });
            }
            chunk = output.next() => chunk,
        };
        match chunk {
            None => break,
            Some(Ok(LogOutput::StdOut { message })) => {
                if let Err(source) = upload.write(&message).await {
                    upload.abort().await;
                    return Err(MariadbNodeError::Upload {
                        service_id: dump.service_id,
                        source: Box::new(source),
                    });
                }
            }
            Some(Ok(LogOutput::StdErr { message })) => {
                stderr.push_str(&String::from_utf8_lossy(&message));
                if stderr.len() > 2 * DUMP_STDERR_CAPTURE {
                    stderr = tail(&stderr, DUMP_STDERR_CAPTURE);
                }
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                upload.abort().await;
                return Err(exec_error("read its output", error.to_string()));
            }
        }
    }
    let exit_code = match docker.inspect_exec(&exec.id).await {
        Ok(info) => info.exit_code.unwrap_or(-1),
        Err(error) => {
            upload.abort().await;
            return Err(exec_error("read its exit code", error.to_string()));
        }
    };
    if exit_code != 0 {
        upload.abort().await;
        return Err(MariadbNodeError::DumpFailed {
            service_id: dump.service_id,
            container,
            exit_code,
            stderr: tail(&stderr, DUMP_STDERR_CAPTURE),
        });
    }
    if upload.written() == 0 {
        upload.abort().await;
        return Err(MariadbNodeError::EmptyDump {
            service_id: dump.service_id,
            container,
        });
    }
    if !stderr.trim().is_empty() {
        debug!(container = %container, "mariadb-dump stderr: {}", stderr.trim());
    }
    upload
        .finish()
        .await
        .map_err(|source| MariadbNodeError::Upload {
            service_id: dump.service_id,
            source: Box::new(source),
        })
}

fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

// ── The agent's side ────────────────────────────────────────────────────

/// The MariaDB service a node request names, as this node runs it: the
/// container, the root password, and the provider's configuration. `Err`
/// says why the request cannot run here.
fn node_service(
    service_id: i32,
    service_config: &ServiceConfig,
    operation: &'static str,
) -> Result<(String, MariaDbConfig), MariadbNodeError> {
    let refused = |reason: String| MariadbNodeError::Refused {
        service_id,
        service: service_config.name.clone(),
        operation,
        reason,
    };
    if service_config.service_type != ServiceType::Mariadb {
        return Err(refused(format!(
            "it is a {} service, not MariaDB",
            service_config.service_type
        )));
    }
    // Parsing fills a missing (or too short) password with a generated one,
    // which is right for a service being created and wrong here: the server
    // only answers to the root password it was created with.
    let stored_root = service_config
        .parameters
        .get("root_password")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    let Some(stored_root) = stored_root else {
        return Err(refused(
            "its configuration has no root password".to_string(),
        ));
    };
    let config: MariaDbConfig = MariaDbService::parse_config(&service_config.name, service_config)
        .map_err(|error| refused(error.to_string()))?;
    if config.root_password != stored_root {
        return Err(refused(
            "its stored root password is not a usable MariaDB password".to_string(),
        ));
    }
    let managed = super::naming::mariadb_container_name(&service_config.name);
    // A worker records its container's name, which is the managed one; any
    // other is not this request's to touch.
    if let Some(recorded) = config.container_name.as_deref() {
        if recorded != managed {
            return Err(refused(format!(
                "its recorded container '{recorded}' is not the managed '{managed}'"
            )));
        }
    }
    Ok((managed, config))
}

fn node_s3(destination: WorkerS3Destination, bucket_path: String) -> S3Credentials {
    S3Credentials {
        access_key_id: destination.access_key_id,
        secret_key: destination.secret_access_key,
        session_token: destination.session_token.filter(|token| !token.is_empty()),
        region: destination.region,
        endpoint: destination
            .endpoint
            .filter(|endpoint| !endpoint.trim().is_empty()),
        bucket_name: destination.bucket,
        bucket_path,
        force_path_style: destination.force_path_style,
    }
}

/// Whether `key` is a dump of `service` under `bucket_path`: its own prefix,
/// ending in [`DUMP_FILE_SUFFIX`], nothing that leaves the prefix.
pub fn validate_dump_key(key: &str, bucket_path: &str, service: &str) -> Result<(), String> {
    let root = format!("{}/", service_root(bucket_path, service));
    if !key.starts_with(&root)
        || !key.ends_with(DUMP_FILE_SUFFIX)
        || key.split('/').any(|part| part == ".." || part == ".")
        || key.chars().any(|c| c.is_control())
    {
        return Err(format!(
            "the dump key '{key}' is not a MariaDB dump under '{root}'"
        ));
    }
    Ok(())
}

/// Whether `location` is a backup in `bucket` this restore can read: a
/// WAL-G repository `s3://<bucket>/…/walg`, or an object key.
pub fn validate_restore_location(location: &str, bucket: &str) -> Result<(), String> {
    if location.chars().any(|c| c.is_control()) || location.is_empty() {
        return Err("the backup location is empty or holds control characters".to_string());
    }
    let key = match location.strip_prefix("s3://") {
        Some(rest) => rest
            .strip_prefix(bucket)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(|| {
                format!("the backup '{location}' is not in the S3 source's bucket '{bucket}'")
            })?,
        None => location,
    };
    if key.starts_with('/') || key.split('/').any(|part| part == "..") {
        return Err(format!("'{location}' is not a backup location"));
    }
    if MariaDbService::is_walg_repository_location(key)
        && key
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '"')
    {
        return Err(format!("'{location}' is not a WAL-G repository"));
    }
    Ok(())
}

fn validate_recovery_target(target: &RecoveryTarget) -> Result<(), String> {
    MariaDbService::recovery_target_to_stop_flag(target)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The agent's side of `POST /agent/services/mariadb/physical-backup`.
pub async fn physical_backup_on_node(
    request: MariadbPhysicalBackupRequest,
    docker: Arc<Docker>,
    _published_address: Option<IpAddr>,
) -> MariadbPhysicalBackupResult {
    let MariadbPhysicalBackupRequest {
        service_id,
        node_id: _,
        service_config,
        destination,
        bucket_path,
        backup_uuid,
    } = request;
    let (container_name, config) =
        match node_service(service_id, &service_config, "backed up physically") {
            Ok(target) => target,
            Err(error) => {
                return MariadbPhysicalBackupResult::Refused {
                    reason: error.to_string(),
                }
            }
        };
    // Dropping the request drops this future, and the exec with it.
    let cancel = CancellationToken::new();
    match physical_backup(
        &docker,
        &MariadbPhysicalBackup {
            service_id,
            container_name,
            root_password: config.root_password,
            s3: node_s3(destination, bucket_path),
            backup_uuid,
        },
        &service_config.name,
        &cancel,
    )
    .await
    {
        Ok(binlog) => MariadbPhysicalBackupResult::Completed { binlog },
        Err(error) if error.is_refusal() => MariadbPhysicalBackupResult::Refused {
            reason: error.to_string(),
        },
        Err(error) => MariadbPhysicalBackupResult::Failed {
            reason: error.to_string(),
        },
    }
}

/// The agent's side of `POST /agent/services/mariadb/dump-backup`.
pub async fn dump_backup_on_node(
    request: MariadbDumpBackupRequest,
    docker: Arc<Docker>,
    _published_address: Option<IpAddr>,
) -> MariadbDumpBackupResult {
    let MariadbDumpBackupRequest {
        service_id,
        node_id: _,
        service_config,
        destination,
        bucket_path,
        key,
    } = request;
    let (container_name, config) = match node_service(service_id, &service_config, "dumped") {
        Ok(target) => target,
        Err(error) => {
            return MariadbDumpBackupResult::Refused {
                reason: error.to_string(),
            }
        }
    };
    if let Err(reason) = validate_dump_key(&key, &bucket_path, &service_config.name) {
        return MariadbDumpBackupResult::Refused { reason };
    }
    let s3 = node_s3(destination, bucket_path);
    let client = s3.build_s3_client().await;
    let cancel = CancellationToken::new();
    match dump_backup(
        &docker,
        client,
        &MariadbDump {
            service_id,
            service_name: service_config.name.clone(),
            container_name,
            root_password: config.root_password,
            bucket: s3.bucket_name.clone(),
            key,
        },
        &cancel,
    )
    .await
    {
        Ok(size_bytes) => MariadbDumpBackupResult::Completed { size_bytes },
        Err(error) if error.is_refusal() => MariadbDumpBackupResult::Refused {
            reason: error.to_string(),
        },
        Err(error) => MariadbDumpBackupResult::Failed {
            reason: error.to_string(),
        },
    }
}

/// The agent's side of `POST /agent/services/mariadb/restore`: the control
/// plane's own in-place restore, against this node's container.
pub async fn restore_on_node(
    request: MariadbRestoreRequest,
    docker: Arc<Docker>,
    _published_address: Option<IpAddr>,
) -> MariadbRestoreResult {
    let MariadbRestoreRequest {
        service_id,
        node_id: _,
        service_config,
        source,
        bucket_path,
        location,
        recovery_target,
        target_user_data,
    } = request;
    let refused = |reason: String| MariadbRestoreResult::Refused {
        reason: format!(
            "MariaDB service {service_id} ('{}') cannot be restored on this node: {reason}",
            service_config.name
        ),
    };
    let (_container, config) = match node_service(service_id, &service_config, "restored") {
        Ok(target) => target,
        Err(error) => {
            return MariadbRestoreResult::Refused {
                reason: error.to_string(),
            }
        }
    };
    if let Err(reason) = validate_restore_location(&location, &source.bucket) {
        return refused(reason);
    }
    if let Some(target) = &recovery_target {
        if let Err(reason) = validate_recovery_target(target) {
            return refused(reason);
        }
    }
    let s3 = node_s3(source, bucket_path);
    let client = s3.build_s3_client().await;
    info!(
        service_id,
        service = %service_config.name,
        pitr = recovery_target.is_some(),
        "Restoring a worker's MariaDB in place"
    );
    let service = MariaDbService::new(service_config.name.clone(), docker);
    match service
        .restore_in_place_on_this_node(
            &config,
            MariaDbInPlaceRestore {
                s3_client: &client,
                s3_credentials: &s3,
                location: &location,
                target_user_data: target_user_data.as_deref(),
                recovery_target: recovery_target.as_ref(),
            },
        )
        .await
    {
        Ok(()) => MariadbRestoreResult::Completed,
        Err(error @ (EngineError::Conflict { .. } | EngineError::InvalidInput { .. })) => {
            MariadbRestoreResult::Refused {
                reason: error.to_string(),
            }
        }
        Err(error) => MariadbRestoreResult::Failed {
            reason: error.to_string(),
        },
    }
}

/// The agent's side of `POST /agent/services/mariadb/binlog-archive`: the
/// control plane's own archiver, against this node's container.
pub async fn binlog_archive_on_node(
    request: MariadbBinlogArchiveRequest,
    docker: Arc<Docker>,
    _published_address: Option<IpAddr>,
) -> MariadbBinlogArchiveResult {
    let MariadbBinlogArchiveRequest {
        service_id,
        node_id: _,
        service_config,
        destination,
        bucket_path,
    } = request;
    let (_container, config) =
        match node_service(service_id, &service_config, "archiving binary logs") {
            Ok(target) => target,
            Err(error) => {
                return MariadbBinlogArchiveResult::Refused {
                    reason: error.to_string(),
                }
            }
        };
    let s3 = node_s3(destination, bucket_path);
    let client = s3.build_s3_client().await;
    let source = archive_source_row(&s3);
    let service = MariaDbService::new(service_config.name.clone(), docker);
    match service.archive_binlogs(&client, &source, &config).await {
        Ok(shipped) => MariadbBinlogArchiveResult::Completed { shipped },
        Err(error) => MariadbBinlogArchiveResult::Failed {
            reason: format!(
                "shipping the binary logs of MariaDB service {service_id} ('{}') failed: {error}",
                service_config.name
            ),
        },
    }
}

/// The S3 source row the archiver reads: bucket and base path, with the
/// decrypted credentials of the request. Never stored.
fn archive_source_row(s3: &S3Credentials) -> temps_entities::s3_sources::Model {
    let now = chrono::Utc::now();
    temps_entities::s3_sources::Model {
        id: 0,
        name: "worker-binlog-archive".to_string(),
        bucket_name: s3.bucket_name.clone(),
        region: s3.region.clone(),
        endpoint: s3.endpoint.clone(),
        bucket_path: s3.bucket_path.clone(),
        access_key_id: s3.access_key_id.clone(),
        secret_key: s3.secret_key.clone(),
        session_token: s3.session_token.clone(),
        credentials_expire_at: None,
        force_path_style: Some(s3.force_path_style),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        created_at: now,
        updated_at: now,
        backing_service_id: None,
        // Never read on the node: the credential above is already the one
        // the control plane scoped.
        worker_credentials: temps_entities::s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(service_type: ServiceType, parameters: serde_json::Value) -> ServiceConfig {
        ServiceConfig {
            name: "shop".to_string(),
            service_type,
            version: None,
            parameters,
        }
    }

    #[test]
    fn the_repository_is_the_control_planes_layout() {
        assert_eq!(
            walg_prefix("b", "fleet/", "shop"),
            "s3://b/fleet/external_services/mariadb/shop/walg"
        );
        assert_eq!(
            walg_prefix("b", "", "shop"),
            "s3://b/external_services/mariadb/shop/walg"
        );
        assert_eq!(
            walg_key_prefix("fleet", "shop"),
            "fleet/external_services/mariadb/shop/walg/"
        );
    }

    #[test]
    fn the_archiver_writes_where_a_recovery_reads() {
        assert_eq!(
            binlog_key_prefix("/fleet/", "shop"),
            "fleet/external_services/mariadb/shop/binlog/"
        );
        assert_eq!(
            binlog_key_prefix("", "shop"),
            "external_services/mariadb/shop/binlog/"
        );
        // A recovery of the service's own WAL-G base reads what it archived.
        assert_eq!(
            binlog_read_prefix(&walg_prefix("b", "fleet", "shop"), "b", "fleet", "shop"),
            binlog_key_prefix("fleet", "shop")
        );
    }

    /// PR #149 invariant: no credential in a command the node runs.
    #[test]
    fn credentials_stay_in_the_environment() {
        for command in [WALG_STREAM_CREATE_COMMAND, DUMP_SHELL] {
            assert!(!command.contains("MYSQL_PWD"), "{command}");
            assert!(!command.contains("--password"), "{command}");
            assert!(
                !command.contains("-p'") && !command.contains("-p\""),
                "{command}"
            );
        }
        assert!(DUMP_SHELL.contains("--databases -- \"$@\""));
        assert!(DUMP_SHELL.contains("set -f"));
        let s3 = S3Credentials {
            access_key_id: "access".into(),
            secret_key: "secret".into(),
            session_token: None,
            region: "auto".into(),
            endpoint: None,
            bucket_name: "b".into(),
            bucket_path: String::new(),
            force_path_style: true,
        };
        let env = backup_push_env("s3://b/p/walg", &s3, "p4ss", "id");
        let stream = env
            .iter()
            .find(|value| value.starts_with("WALG_STREAM_CREATE_COMMAND="))
            .expect("stream command");
        assert!(!stream.contains("p4ss"));
        assert!(env.iter().any(|value| value == "MYSQL_PWD=p4ss"));
        assert!(!env
            .iter()
            .any(|value| value.starts_with("AWS_SESSION_TOKEN")));
        let with_token = backup_push_env(
            "s3://b/p/walg",
            &S3Credentials {
                session_token: Some("sts".into()),
                ..s3
            },
            "p4ss",
            "id",
        );
        assert!(with_token
            .iter()
            .any(|value| value == "AWS_SESSION_TOKEN=sts"));
    }

    #[test]
    fn binlog_status_is_parsed_or_absent() {
        assert_eq!(
            parse_binlog_status("mysql-bin.000003\t342\t\t\n0-1-7\n"),
            Some(MariadbBinlogCoordinate {
                file: "mysql-bin.000003".into(),
                position: "342".into(),
                gtid: "0-1-7".into(),
            })
        );
        assert_eq!(parse_binlog_status(""), None);
        assert_eq!(parse_binlog_status("\t\t"), None);
    }

    #[test]
    fn a_dump_key_is_the_services_own() {
        let key = "fleet/external_services/mariadb/shop/2026/11/08/u/dump.sql.gz";
        assert!(validate_dump_key(key, "fleet", "shop").is_ok());
        assert!(validate_dump_key(key, "fleet", "other").is_err());
        assert!(validate_dump_key(
            "fleet/external_services/mariadb/shop/../other/dump.sql.gz",
            "fleet",
            "shop"
        )
        .is_err());
        assert!(validate_dump_key(
            "fleet/external_services/mariadb/shop/u/base.mbstream.gz",
            "fleet",
            "shop"
        )
        .is_err());
    }

    #[test]
    fn a_restore_reads_only_its_sources_bucket() {
        assert!(
            validate_restore_location("s3://b/fleet/external_services/mariadb/shop/walg", "b")
                .is_ok()
        );
        assert!(validate_restore_location(
            "fleet/external_services/mariadb/shop/x/dump.sql.gz",
            "b"
        )
        .is_ok());
        assert!(validate_restore_location(
            "s3://other/fleet/external_services/mariadb/shop/walg",
            "b"
        )
        .is_err());
        assert!(validate_restore_location("../etc/walg", "b").is_err());
        assert!(validate_restore_location("s3://b/a b'/walg", "b").is_err());
        assert!(validate_restore_location("", "b").is_err());
    }

    #[test]
    fn a_request_for_another_engine_or_container_is_refused() {
        let error = node_service(
            1,
            &config(
                ServiceType::Postgres,
                serde_json::json!({"root_password": "stored-root-password"}),
            ),
            "restored",
        )
        .expect_err("postgres");
        assert!(
            error.is_refusal() && error.to_string().contains("not MariaDB"),
            "{error}"
        );

        let error = node_service(
            1,
            &config(
                ServiceType::Mariadb,
                serde_json::json!({
                    "root_password": "stored-root-password",
                    "container_name": "postgres-other",
                }),
            ),
            "restored",
        )
        .expect_err("foreign container");
        assert!(error.to_string().contains("postgres-other"), "{error}");

        let (container, parsed) = node_service(
            1,
            &config(
                ServiceType::Mariadb,
                serde_json::json!({
                    "root_password": "stored-root-password",
                    "container_name": "mariadb-shop",
                }),
            ),
            "restored",
        )
        .expect("managed");
        assert_eq!(container, "mariadb-shop");
        assert_eq!(parsed.root_password, "stored-root-password");
    }

    /// The node uses the root password the server was created with, never
    /// one parsing would generate in its place: none, or one too short to be
    /// a MariaDB password, is refused before the container is touched.
    #[test]
    fn a_service_without_its_root_password_is_refused() {
        for parameters in [
            serde_json::json!({"container_name": "mariadb-shop"}),
            serde_json::json!({"root_password": "", "container_name": "mariadb-shop"}),
            serde_json::json!({"root_password": "short", "container_name": "mariadb-shop"}),
        ] {
            let error = node_service(
                1,
                &config(ServiceType::Mariadb, parameters.clone()),
                "dumped",
            )
            .expect_err("no usable root password");
            assert!(
                error.is_refusal() && error.to_string().contains("root password"),
                "{parameters}: {error}"
            );
        }
    }
}
