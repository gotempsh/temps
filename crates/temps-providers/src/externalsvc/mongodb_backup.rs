// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node half of MongoDB backups and restores (ADR-099): everything they
//! do to the container. The control plane runs it with its own Docker client
//! for a service it hosts; a worker's agent runs the same functions with the
//! node's client for a service on that worker.
//!
//! **Backup** ([`mongodb_backup`]):
//!
//! 1. Read the root credentials from the container's environment, else the
//!    service's stored ones (the configured user may hold a narrower role).
//! 2. Check the image for WAL-G (`which wal-g` in the container). For a
//!    worker's service the control plane makes the same check through the
//!    agent first and sends its choice ([`MongodbBackupMode`]) with a
//!    credential scoped to that one repository or archive directory
//!    (ADR-102, ADR-099); the node takes exactly that backup.
//! 3. With WAL-G, `wal-g backup-push` in the container streams `mongodump
//!    --archive` into the service's WAL-G repository.
//! 4. Without it, a pinned `mongodump --archive --gzip` helper on the
//!    service network streams one archive object into the S3 source
//!    (`worker_data::dump_to_s3`).
//!
//! **Restore** ([`mongodb_restore_on_node`]): the provider's own in-place
//! restore (`MongodbService::restore_in_place`), which the control plane
//! runs for a local service: `wal-g backup-fetch` in the container for a
//! WAL-G repository, a `mongorestore --drop` sidecar for an archive.
//!
//! The plane half (S3 source, size, metadata companion, the backup row and
//! its WAL-G identity, admission and credential reconciliation) stays in the
//! backup engine and the restore service.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bollard::query_parameters::InspectContainerOptions;
use bollard::Docker;
use temps_service_protocol::{
    MongodbBackupMode, MongodbBackupRequest, MongodbBackupResult, MongodbRestoreRequest,
    MongodbRestoreResult, WorkerS3Destination,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::mongodb::MongodbService;
use super::naming::mongodb_container_name;
use super::postgres_walg_backup::{
    refuse_overlapping_backup_push, run_walg_exec, WalgBaseBackupError,
};
use super::worker_data::{dump_credentials, dump_to_s3, DumpEngine, WorkerDataError};
use super::{ExternalService, RestoreContext, S3Credentials, ServiceConfig, ServiceType};

/// The `mongodump` image of an archive backup (an image without WAL-G):
/// release- and digest-pinned, so the dump tool is the same on every node
/// whatever the service's own image ships.
pub const MONGODUMP_HELPER_IMAGE: &str =
    "mongo:7.0.39-jammy@sha256:04582c3a144d088f841c446abfc19f79adcefa8bd00ad4a7fb18e27b9585c5d6";

/// What WAL-G runs to produce and to replay a backup. The URI comes from the
/// exec's environment, never the command line.
pub const WALG_STREAM_CREATE_COMMAND: &str = "mongodump --archive --uri=\"$MONGODB_URI\"";
pub const WALG_STREAM_RESTORE_COMMAND: &str =
    "mongorestore --archive --drop --uri=\"$MONGODB_URI\"";

/// The file name of an archive backup: the one the provider's restore reads.
pub const ARCHIVE_FILE_NAME: &str = "dump.archive";

/// How long the WAL-G check may take, as the control plane's engine choice.
const WALG_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// `<bucket_path>/external_services/mongodb/<service>`: where a MongoDB
/// service's backups live inside its S3 source.
fn service_root(bucket_path: &str, service_name: &str) -> String {
    let root = format!("external_services/mongodb/{service_name}");
    match bucket_path.trim_matches('/') {
        "" => root,
        base => format!("{base}/{root}"),
    }
}

/// `WALG_S3_PREFIX` of a MongoDB service: the same on the control plane and
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

/// The key prefix every archive backup of a service is written under, with
/// a trailing slash.
pub fn archive_key_prefix(bucket_path: &str, service_name: &str) -> String {
    format!("{}/", service_root(bucket_path, service_name))
}

/// The key of an archive backup taken on `date`:
/// `<bucket_path>/external_services/mongodb/<service>/<Y/m/d>/<uuid>/dump.archive`.
pub fn archive_key(
    bucket_path: &str,
    service_name: &str,
    backup_uuid: &str,
    date: chrono::DateTime<chrono::Utc>,
) -> String {
    format!(
        "{}{}/{backup_uuid}/{ARCHIVE_FILE_NAME}",
        archive_key_prefix(bucket_path, service_name),
        date.format("%Y/%m/%d")
    )
}

/// Whether `key` is an archive key [`archive_key`] made for this service and
/// backup, on any date.
pub fn is_archive_key_of(
    key: &str,
    bucket_path: &str,
    service_name: &str,
    backup_uuid: &str,
) -> bool {
    let Some(rest) = key.strip_prefix(&archive_key_prefix(bucket_path, service_name)) else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    matches!(
        parts.as_slice(),
        [year, month, day, uuid, file]
            if *uuid == backup_uuid
                && *file == ARCHIVE_FILE_NAME
                && [year, month, day].iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    )
}

/// One backup of a MongoDB service.
#[derive(Debug, Clone)]
pub struct MongodbBackup {
    pub service_id: i32,
    /// The container it runs in.
    pub container_name: String,
    pub service_config: ServiceConfig,
    pub s3: S3Credentials,
    pub backup_uuid: String,
    /// The backup the caller's credential was granted for; `None` to check
    /// the image here (the control plane's own services, where it holds the
    /// source's credential anyway).
    pub mode: Option<MongodbBackupMode>,
}

/// What [`mongodb_backup`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MongodbBackupOutcome {
    /// WAL-G streamed the dump into [`walg_prefix`].
    WalgStream { stdout: String, stderr: String },
    /// An archive was uploaded to `key`.
    Archive { key: String, size_bytes: u64 },
}

#[derive(Debug, Error)]
pub enum MongodbBackupError {
    #[error("MongoDB service {service_id} ('{service}') cannot be backed up here: {reason}")]
    Target {
        service_id: i32,
        service: String,
        reason: String,
    },
    #[error(
        "MongoDB service {service_id}: its container '{container}' could not be inspected: \
         {reason}"
    )]
    Inspect {
        service_id: i32,
        container: String,
        reason: String,
    },
    #[error(
        "MongoDB wal-g backup-push of service {service_id} in '{container}' exited with code \
         {exit_code}. stderr: {stderr}"
    )]
    BackupPush {
        service_id: i32,
        container: String,
        exit_code: i64,
        stderr: String,
    },
    #[error("WAL-G exec in MongoDB container '{container}' failed to {operation}: {reason}")]
    Exec {
        container: String,
        operation: &'static str,
        reason: String,
    },
    #[error("MongoDB backup in container '{container}' was cancelled")]
    Cancelled { container: String },
    #[error(
        "A wal-g backup-push of MongoDB service {service_id} is still running in '{container}', \
         left by an earlier attempt whose agent restarted or passed its deadline. Nothing was \
         started; the backup can run once it ends"
    )]
    BackupPushRunning { service_id: i32, container: String },
    #[error("MongoDB service {service_id}: pulling the mongodump image {image} failed: {reason}")]
    HelperImage {
        service_id: i32,
        image: &'static str,
        reason: String,
    },
    #[error("MongoDB service {service_id}: the mongodump archive backup failed: {source}")]
    Archive {
        service_id: i32,
        #[source]
        source: Box<WorkerDataError>,
    },
    #[error(
        "MongoDB service {service_id} ('{service}'): archive key '{key}' is not this backup's          ({backup_uuid}) archive key under '{bucket_path}'; nothing was written"
    )]
    ArchiveKey {
        service_id: i32,
        service: String,
        key: String,
        backup_uuid: String,
        bucket_path: String,
    },
    #[error(
        "MongoDB service {service_id}: the control plane granted a WAL-G stream backup, but          wal-g does not run in container '{container}' (its image changed?); nothing was          written, and the next attempt checks the image again"
    )]
    WalgMissing { service_id: i32, container: String },
}

impl MongodbBackupError {
    /// Whether a retry can only repeat this error.
    pub fn is_refusal(&self) -> bool {
        match self {
            Self::Target { .. } | Self::ArchiveKey { .. } => true,
            Self::WalgMissing { .. }
            | Self::Inspect { .. }
            | Self::BackupPush { .. }
            | Self::Exec { .. }
            | Self::Cancelled { .. }
            | Self::BackupPushRunning { .. }
            | Self::HelperImage { .. }
            | Self::Archive { .. } => false,
        }
    }
}

/// The environment of `wal-g backup-push` (and, with the restore command, of
/// `backup-fetch`): the repository, the S3 source, the database's URI, the
/// stream commands and the backup's identity.
pub fn walg_stream_env(
    walg_prefix: &str,
    s3: &S3Credentials,
    mongodb_uri: &str,
    backup_uuid: &str,
    container_endpoint: Option<String>,
) -> Vec<String> {
    let mut env = vec![
        format!("WALG_S3_PREFIX={walg_prefix}"),
        format!("AWS_ACCESS_KEY_ID={}", s3.access_key_id),
        format!("AWS_SECRET_ACCESS_KEY={}", s3.secret_key),
        format!("AWS_REGION={}", s3.region),
        format!("MONGODB_URI={mongodb_uri}"),
        format!("WALG_STREAM_CREATE_COMMAND={WALG_STREAM_CREATE_COMMAND}"),
        format!("WALG_STREAM_RESTORE_COMMAND={WALG_STREAM_RESTORE_COMMAND}"),
    ];
    // Present only for a temporary credential.
    env.extend(s3.session_token_env());
    env.extend(super::postgres_walg_backup::walg_identity_env(backup_uuid));
    if let Some(endpoint) = container_endpoint {
        env.push(format!(
            "AWS_ENDPOINT={}",
            if endpoint.starts_with("http") {
                endpoint
            } else {
                format!("http://{endpoint}")
            }
        ));
    }
    if s3.force_path_style {
        env.push("AWS_S3_FORCE_PATH_STYLE=true".into());
    }
    env
}

/// A WAL-G exec's error as this engine's: `operation` names the step for
/// an error that has no variant here.
fn walg_error(
    error: WalgBaseBackupError,
    container_name: &str,
    operation: &'static str,
) -> MongodbBackupError {
    match error {
        WalgBaseBackupError::Cancelled { container } => MongodbBackupError::Cancelled { container },
        WalgBaseBackupError::Exec {
            container,
            operation,
            reason,
        } => MongodbBackupError::Exec {
            container,
            operation,
            reason,
        },
        WalgBaseBackupError::BackupPushRunning {
            service_id,
            container,
        } => MongodbBackupError::BackupPushRunning {
            service_id,
            container,
        },
        other => MongodbBackupError::Exec {
            container: container_name.to_string(),
            operation,
            reason: other.to_string(),
        },
    }
}

/// The root URI of the database inside its own container.
fn local_uri(username: &str, password: &str) -> String {
    format!(
        "mongodb://{}:{}@127.0.0.1:27017/?authSource=admin",
        urlencoding::encode(username),
        urlencoding::encode(password),
    )
}

/// Whether `wal-g` runs in `container_name`: the provider's own check,
/// bounded. `false` on any error or timeout, which selects the archive.
async fn container_has_walg(docker: &Arc<Docker>, service: &str, container_name: &str) -> bool {
    let probe = MongodbService::new(service.to_string(), Arc::clone(docker));
    tokio::time::timeout(WALG_PROBE_TIMEOUT, probe.container_has_walg(container_name))
        .await
        .unwrap_or_else(|_| {
            warn!(
                container = container_name,
                "The WAL-G check of a MongoDB container did not answer in time; taking an archive"
            );
            false
        })
}

/// The S3 source as the archive helper's upload takes it.
fn destination(s3: &S3Credentials) -> WorkerS3Destination {
    WorkerS3Destination {
        bucket: s3.bucket_name.clone(),
        region: s3.region.clone(),
        endpoint: s3.endpoint.clone(),
        force_path_style: s3.force_path_style,
        access_key_id: s3.access_key_id.clone(),
        secret_access_key: s3.secret_key.clone(),
        session_token: s3.session_token.clone(),
    }
}

/// Back up `request`'s service in its container on `docker` (see the module
/// documentation). Returns on `cancel`; dropping the future stops it too.
pub async fn mongodb_backup(
    docker: Arc<Docker>,
    request: MongodbBackup,
    cancel: &CancellationToken,
) -> Result<MongodbBackupOutcome, MongodbBackupError> {
    let MongodbBackup {
        service_id,
        container_name,
        service_config,
        s3,
        backup_uuid,
        mode,
    } = request;
    let service = service_config.name.clone();
    // An archive key the caller chose is checked before anything runs: its
    // credential covers that key's directory and nothing else.
    if let Some(MongodbBackupMode::Archive { key }) = &mode {
        if !is_archive_key_of(key, &s3.bucket_path, &service, &backup_uuid) {
            return Err(MongodbBackupError::ArchiveKey {
                service_id,
                service,
                key: key.clone(),
                backup_uuid,
                bucket_path: s3.bucket_path.clone(),
            });
        }
    }
    let inspect = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await
        .map_err(|error| MongodbBackupError::Inspect {
            service_id,
            container: container_name.clone(),
            reason: error.to_string(),
        })?;
    let container_env = inspect
        .config
        .and_then(|config| config.env)
        .unwrap_or_default();
    let credentials = dump_credentials(
        DumpEngine::Mongodb,
        &service_config.parameters,
        &container_env,
    );
    info!(
        service_id,
        container = %container_name,
        username = %credentials.username,
        password_set = !credentials.password.is_empty(),
        "MongoDB backup credentials resolved"
    );

    let walg = match &mode {
        None => container_has_walg(&docker, &service, &container_name).await,
        Some(MongodbBackupMode::WalgStream) => {
            if !container_has_walg(&docker, &service, &container_name).await {
                return Err(MongodbBackupError::WalgMissing {
                    service_id,
                    container: container_name,
                });
            }
            true
        }
        Some(MongodbBackupMode::Archive { .. }) => false,
    };
    if walg {
        refuse_overlapping_backup_push(&docker, service_id, &container_name, "MongoDB", cancel)
            .await
            .map_err(|error| {
                walg_error(
                    error,
                    &container_name,
                    "check for a running wal-g backup-push",
                )
            })?;
        let prefix = walg_prefix(&s3.bucket_name, &s3.bucket_path, &service);
        let endpoint = s3
            .resolve_endpoint_for_container(&docker, &container_name)
            .await;
        let env = walg_stream_env(
            &prefix,
            &s3,
            &local_uri(&credentials.username, &credentials.password),
            &backup_uuid,
            endpoint,
        );
        info!(service_id, repository = %prefix, "MongoDB WAL-G stream backup starting");
        let output = run_walg_exec(&docker, &container_name, "wal-g backup-push", &env, cancel)
            .await
            .map_err(|error| walg_error(error, &container_name, "run wal-g backup-push"))?;
        if output.exit_code != 0 {
            return Err(MongodbBackupError::BackupPush {
                service_id,
                container: container_name,
                exit_code: output.exit_code,
                stderr: bounded_tail(&output.stderr),
            });
        }
        return Ok(MongodbBackupOutcome::WalgStream {
            stdout: output.stdout,
            stderr: output.stderr,
        });
    }

    warn!(
        service_id,
        container = %container_name,
        "MongoDB image has no WAL-G; taking a mongodump archive. It is usable for restores but \
         not eligible for managed Cloud restore verification"
    );
    crate::utils::pull_image_with_retry(&docker, MONGODUMP_HELPER_IMAGE, None)
        .await
        .map_err(|error| MongodbBackupError::HelperImage {
            service_id,
            image: MONGODUMP_HELPER_IMAGE,
            reason: error.to_string(),
        })?;
    let key = match mode {
        Some(MongodbBackupMode::Archive { key }) => key,
        _ => archive_key(&s3.bucket_path, &service, &backup_uuid, chrono::Utc::now()),
    };
    let upload_to = destination(&s3);
    let dump = dump_to_s3(
        Arc::clone(&docker),
        DumpEngine::Mongodb,
        &service_config,
        &container_name,
        &upload_to,
        &key,
        Some(MONGODUMP_HELPER_IMAGE),
    );
    let size_bytes = tokio::select! {
        // Dropping the dump removes its helper and aborts the upload.
        _ = cancel.cancelled() => {
            return Err(MongodbBackupError::Cancelled { container: container_name })
        }
        size = dump => size.map_err(|source| MongodbBackupError::Archive {
            service_id,
            source: Box::new(source),
        })?,
    };
    Ok(MongodbBackupOutcome::Archive { key, size_bytes })
}

/// The last 2,000 bytes of `value`, on a character boundary.
pub fn bounded_tail(value: &str) -> String {
    const MAX_BYTES: usize = 2_000;
    let trimmed = value.trim();
    if trimmed.len() <= MAX_BYTES {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - MAX_BYTES;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

/// A worker service's configuration as the node runs it: a MongoDB in the
/// container Temps created for it, whose stored host names this node.
/// `Err` names why it cannot be handled here; nothing was touched.
fn node_service_config(
    service_id: i32,
    mut service_config: ServiceConfig,
    published_address: Option<IpAddr>,
) -> Result<(ServiceConfig, String), String> {
    if service_config.service_type != ServiceType::Mongodb {
        return Err(format!(
            "service {service_id} ('{}') is {}, not MongoDB",
            service_config.name, service_config.service_type
        ));
    }
    if let Err(error) = super::resolve_managed_probe_target(&mut service_config, published_address)
    {
        return Err(error.to_string());
    }
    let container = mongodb_container_name(&service_config.name);
    if let Some(recorded) = service_config
        .parameters
        .get("container_name")
        .and_then(|v| v.as_str())
        .filter(|name| !name.is_empty())
    {
        if recorded != container {
            return Err(format!(
                "it is recorded in container '{recorded}', and backups and restores on a worker \
                 node apply only to the container Temps created ('{container}')"
            ));
        }
    }
    Ok((service_config, container))
}

/// The S3 source of a node request.
fn node_s3(destination: WorkerS3Destination, bucket_path: String) -> S3Credentials {
    S3Credentials {
        access_key_id: destination.access_key_id,
        secret_key: destination.secret_access_key,
        session_token: destination.session_token.filter(|t| !t.is_empty()),
        region: destination.region,
        endpoint: destination.endpoint.filter(|e| !e.trim().is_empty()),
        bucket_name: destination.bucket,
        bucket_path,
        force_path_style: destination.force_path_style,
    }
}

/// The agent's side of `POST /agent/services/mongodb/backup`: the request as
/// [`mongodb_backup`] takes it, against this node's Docker.
pub async fn mongodb_backup_on_node(
    request: MongodbBackupRequest,
    docker: Arc<Docker>,
    published_address: Option<IpAddr>,
) -> MongodbBackupResult {
    let MongodbBackupRequest {
        service_id,
        node_id: _,
        service_config,
        destination,
        bucket_path,
        backup_uuid,
        mode,
    } = request;
    let name = service_config.name.clone();
    let (service_config, container_name) =
        match node_service_config(service_id, service_config, published_address) {
            Ok(target) => target,
            Err(reason) => {
                return MongodbBackupResult::Refused {
                    reason: format!(
                        "MongoDB service {service_id} ('{name}') cannot be backed up on this \
                         node: {reason}"
                    ),
                }
            }
        };
    // Dropping the request (the control plane gave up) drops this future,
    // and the exec or helper with it; the token only serves the shared
    // signature.
    let cancel = CancellationToken::new();
    match mongodb_backup(
        docker,
        MongodbBackup {
            service_id,
            container_name,
            service_config,
            s3: node_s3(destination, bucket_path),
            backup_uuid,
            mode: Some(mode),
        },
        &cancel,
    )
    .await
    {
        Ok(MongodbBackupOutcome::WalgStream { stdout, stderr }) => {
            MongodbBackupResult::WalgStream { stdout, stderr }
        }
        Ok(MongodbBackupOutcome::Archive { key, size_bytes }) => {
            MongodbBackupResult::Archive { key, size_bytes }
        }
        Err(error) if error.is_refusal() => MongodbBackupResult::Refused {
            reason: error.to_string(),
        },
        Err(error) => MongodbBackupResult::Failed {
            reason: error.to_string(),
        },
    }
}

/// Where a restore reads its backup, checked against the source's bucket:
/// a WAL-G repository inside it, or an object key.
fn restore_location(location: &str, bucket: &str) -> Result<String, String> {
    let location = location.trim();
    if location.is_empty() {
        return Err("the backup location is empty".to_string());
    }
    if location.starts_with("s3://") {
        let inside = format!("s3://{bucket}/");
        if !location.starts_with(&inside) || location.len() == inside.len() {
            return Err(format!(
                "the WAL-G repository '{location}' is not inside bucket '{bucket}'"
            ));
        }
        return Ok(location.to_string());
    }
    let key = location.trim_start_matches('/');
    if key.split('/').any(|part| part == "..") {
        return Err(format!(
            "the backup key '{location}' is not a plain object key"
        ));
    }
    Ok(key.to_string())
}

/// A row the provider's restore context carries, holding only what a node
/// knows: never stored.
fn backup_row(location: &str) -> temps_entities::backups::Model {
    temps_entities::backups::Model {
        id: 0,
        name: "worker restore".to_string(),
        backup_id: String::new(),
        schedule_id: None,
        schedule_run_id: None,
        backup_type: "full".to_string(),
        state: "completed".to_string(),
        started_at: chrono::Utc::now(),
        finished_at: None,
        size_bytes: None,
        file_count: None,
        s3_source_id: 0,
        s3_location: location.to_string(),
        error_message: None,
        metadata: "{}".to_string(),
        checksum: None,
        compression_type: String::new(),
        created_by: 0,
        expires_at: None,
        tags: "[]".to_string(),
    }
}

/// The target service as the provider's restore context carries it: never
/// stored.
fn service_row(service_id: i32, name: &str) -> temps_entities::external_services::Model {
    let now = chrono::Utc::now();
    temps_entities::external_services::Model {
        id: service_id,
        name: name.to_string(),
        service_type: ServiceType::Mongodb.to_string(),
        version: None,
        status: "running".to_string(),
        created_at: now,
        updated_at: now,
        slug: None,
        config: None,
        node_id: None,
        topology: "standalone".to_string(),
        error_message: None,
        health_status: None,
        last_health_check_at: None,
        last_health_error: None,
        consecutive_health_failures: 0,
        health_metadata: None,
        metrics_enabled: false,
        default_backup_provisioned: false,
        container_name: None,
        created_by_user_id: None,
        continuous_archive_s3_source_id: None,
        continuous_archive_pinned_at: None,
        workload_mode: Default::default(),
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
    }
}

/// The agent's side of `POST /agent/services/mongodb/restore`: the
/// provider's own in-place restore, as the control plane runs it for a local
/// service, against this node's Docker. The control plane has already
/// passed its last cancellable point when it sends this; what is left to
/// stop it is the node's deadline, `writes`, which the restore passes before
/// `mongorestore` runs (a WAL-G stream restore writes as it fetches, so it
/// passes it before it starts).
pub async fn mongodb_restore_on_node(
    request: MongodbRestoreRequest,
    docker: Arc<Docker>,
    published_address: Option<IpAddr>,
    writes: Arc<temps_service_protocol::TargetWriteBoundary>,
) -> MongodbRestoreResult {
    let service_id = request.service_id;
    let name = request.service_config.name.clone();
    let refused = |reason: String| MongodbRestoreResult::Refused {
        reason: format!(
            "MongoDB service {service_id} ('{name}') cannot be restored on this node: {reason}"
        ),
    };
    let engine_config = request.engine_config();
    let (engine_config, container) =
        match node_service_config(service_id, engine_config, published_address) {
            Ok(target) => target,
            Err(reason) => return refused(reason),
        };
    let location = match restore_location(&request.location, &request.source.bucket) {
        Ok(location) => location,
        Err(reason) => return refused(reason),
    };
    let s3 = node_s3(request.source, String::new());
    let client = s3.build_s3_client().await;
    let source_row = super::worker_data::s3_source_row(&destination(&s3));
    let backup = backup_row(&location);
    let target = service_row(service_id, &name);
    let pool = sea_orm::DatabaseConnection::Disconnected;
    info!(
        service_id,
        container = %container,
        location = %location,
        "Restoring a MongoDB service in place with the provider's restore"
    );
    let instance = MongodbService::new(name.clone(), docker);
    if !instance.defers_target_writes(&location) && !writes.begin_target_writes() {
        return MongodbRestoreResult::Failed {
            reason: format!(
                "MongoDB restore of service {service_id} ('{name}') from '{location}' reached \
                 this node's deadline before it started; nothing was restored"
            ),
        };
    }
    let result = instance
        .restore_in_place(RestoreContext {
            s3_client: &client,
            s3_credentials: &s3,
            s3_source: &source_row,
            backup: &backup,
            backup_location: &location,
            source_service: &target,
            source_config: engine_config,
            live_target_config: request.service_config,
            pool: &pool,
            gate: writes.as_ref(),
        })
        .await;
    match result {
        Ok(()) => MongodbRestoreResult::Completed,
        Err(error) => MongodbRestoreResult::Failed {
            reason: format!(
                "MongoDB restore of service {service_id} ('{name}') from '{location}' failed: \
                 {error}"
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(session_token: Option<&str>, force_path_style: bool) -> S3Credentials {
        S3Credentials {
            access_key_id: "AKID".to_string(),
            secret_key: "SECRET".to_string(),
            session_token: session_token.map(str::to_string),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_name: "backups".to_string(),
            bucket_path: "base".to_string(),
            force_path_style,
        }
    }

    #[test]
    fn the_layout_is_derived_from_the_source_the_service_and_the_backup_alone() {
        assert_eq!(
            walg_prefix("backups", "", "orders"),
            "s3://backups/external_services/mongodb/orders/walg"
        );
        assert_eq!(
            walg_prefix("backups", "/team/a/", "orders"),
            "s3://backups/team/a/external_services/mongodb/orders/walg"
        );
        assert_eq!(
            walg_key_prefix("team/a", "orders"),
            "team/a/external_services/mongodb/orders/walg/"
        );
        let date = chrono::DateTime::parse_from_rfc3339("2026-11-08T10:00:00Z")
            .expect("a date")
            .with_timezone(&chrono::Utc);
        let key = archive_key("team/a", "orders", "u-1", date);
        assert_eq!(
            key,
            "team/a/external_services/mongodb/orders/2026/11/08/u-1/dump.archive"
        );
        assert!(is_archive_key_of(&key, "team/a", "orders", "u-1"));
        assert!(!is_archive_key_of(&key, "team/a", "orders", "u-2"));
        assert!(!is_archive_key_of(&key, "", "orders", "u-1"));
        assert!(!is_archive_key_of(
            "team/a/external_services/mongodb/orders/x/../../u-1/dump.archive",
            "team/a",
            "orders",
            "u-1"
        ));
    }

    /// The exact environment the control plane's engine always ran WAL-G
    /// with: the URI only in the environment, the identity in the sentinel.
    #[test]
    fn walg_runs_with_the_uri_in_its_environment_and_the_backup_identity() {
        let env = walg_stream_env(
            "s3://backups/base/external_services/mongodb/orders/walg",
            &credentials(Some("TOKEN"), true),
            &local_uri("root", "p@ss:-w"),
            "uuid-1",
            Some("rustfs:9000".to_string()),
        );
        assert_eq!(
            env,
            vec![
                "WALG_S3_PREFIX=s3://backups/base/external_services/mongodb/orders/walg",
                "AWS_ACCESS_KEY_ID=AKID",
                "AWS_SECRET_ACCESS_KEY=SECRET",
                "AWS_REGION=us-east-1",
                "MONGODB_URI=mongodb://root:p%40ss%3A-w@127.0.0.1:27017/?authSource=admin",
                "WALG_STREAM_CREATE_COMMAND=mongodump --archive --uri=\"$MONGODB_URI\"",
                "WALG_STREAM_RESTORE_COMMAND=mongorestore --archive --drop --uri=\"$MONGODB_URI\"",
                "AWS_SESSION_TOKEN=TOKEN",
                "WALG_DELTA_MAX_STEPS=0",
                "WALG_SENTINEL_USER_DATA={\"temps_backup_id\":\"uuid-1\"}",
                "AWS_ENDPOINT=http://rustfs:9000",
                "AWS_S3_FORCE_PATH_STYLE=true",
            ]
        );
        assert!(!WALG_STREAM_CREATE_COMMAND.contains("mongodb://"));
        assert!(!WALG_STREAM_RESTORE_COMMAND.contains("mongodb://"));
    }

    #[test]
    fn the_mongodump_image_is_release_and_digest_pinned() {
        assert!(MONGODUMP_HELPER_IMAGE.starts_with("mongo:7.0.39-jammy@sha256:"));
    }

    fn config(parameters: serde_json::Value) -> ServiceConfig {
        ServiceConfig {
            name: "orders".to_string(),
            service_type: ServiceType::Mongodb,
            version: None,
            parameters,
        }
    }

    /// The node runs only its own managed MongoDB: another engine, a
    /// non-loopback stored host or another container is refused.
    #[test]
    fn the_node_runs_only_the_container_temps_created() {
        let (_, container) = node_service_config(
            3,
            config(serde_json::json!({
                "host": "localhost",
                "container_name": "temps-mongodb-orders"
            })),
            None,
        )
        .expect("the managed container");
        assert_eq!(container, "temps-mongodb-orders");
        let other = node_service_config(
            3,
            config(serde_json::json!({"host": "localhost", "container_name": "app-db"})),
            None,
        )
        .expect_err("another container");
        assert!(other.contains("app-db"), "{other}");
        let host = node_service_config(3, config(serde_json::json!({"host": "10.0.0.9"})), None)
            .expect_err("a non-loopback host");
        assert!(host.contains("10.0.0.9"), "{host}");
        let mut redis = config(serde_json::json!({}));
        redis.service_type = ServiceType::Redis;
        assert!(node_service_config(3, redis, None).is_err());
    }

    /// A restore reads a WAL-G repository inside the source's bucket, or an
    /// object key; nothing else.
    #[test]
    fn a_restore_location_stays_inside_the_source_bucket() {
        assert_eq!(
            restore_location("s3://backups/x/external_services/mongodb/o/walg", "backups"),
            Ok("s3://backups/x/external_services/mongodb/o/walg".to_string())
        );
        assert!(restore_location("s3://other/x/walg", "backups").is_err());
        assert!(restore_location("s3://backups/", "backups").is_err());
        assert!(restore_location("s3://backupsx/walg", "backups").is_err());
        assert_eq!(
            restore_location("/x/2026/11/08/u/dump.archive", "backups"),
            Ok("x/2026/11/08/u/dump.archive".to_string())
        );
        assert!(restore_location("x/../../etc/dump.archive", "backups").is_err());
        assert!(restore_location("  ", "backups").is_err());
    }

    /// Refusals happen before Docker is reached: a request naming another
    /// container, or a location outside the bucket, changes nothing.
    #[tokio::test]
    async fn refused_requests_never_reach_docker() {
        let docker = Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("an unused client"),
        );
        let destination = WorkerS3Destination {
            bucket: "backups".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: true,
            access_key_id: "a".to_string(),
            secret_access_key: "s".to_string(),
            session_token: None,
        };
        let backup = mongodb_backup_on_node(
            MongodbBackupRequest {
                service_id: 4,
                node_id: 2,
                service_config: config(serde_json::json!({"container_name": "app-db"})),
                destination: destination.clone(),
                bucket_path: String::new(),
                backup_uuid: "u".to_string(),
                mode: MongodbBackupMode::WalgStream,
            },
            Arc::clone(&docker),
            None,
        )
        .await;
        assert!(
            matches!(&backup, MongodbBackupResult::Refused { reason } if reason.contains("service 4")),
            "{backup:?}"
        );
        // An archive key other than this backup's (another service's, another
        // backup's, outside the source's path) is refused before the
        // container is looked at: the credential covers only the granted key.
        for key in [
            "base/external_services/mongodb/billing/2026/11/09/u/dump.archive",
            "base/external_services/mongodb/orders/2026/11/09/other/dump.archive",
            "elsewhere/dump.archive",
        ] {
            let backup = mongodb_backup_on_node(
                MongodbBackupRequest {
                    service_id: 4,
                    node_id: 2,
                    service_config: config(serde_json::json!({})),
                    destination: destination.clone(),
                    bucket_path: "base".to_string(),
                    backup_uuid: "u".to_string(),
                    mode: MongodbBackupMode::Archive {
                        key: key.to_string(),
                    },
                },
                Arc::clone(&docker),
                None,
            )
            .await;
            assert!(
                matches!(&backup, MongodbBackupResult::Refused { reason }
                    if reason.contains(key) && reason.contains("nothing was written")),
                "{key}: {backup:?}"
            );
        }
        let restore = mongodb_restore_on_node(
            MongodbRestoreRequest {
                service_id: 4,
                node_id: 2,
                service_config: config(serde_json::json!({})),
                origin_password: None,
                source: destination,
                location: "s3://elsewhere/walg".to_string(),
                operation_id: None,
            },
            docker,
            None,
            Arc::new(temps_service_protocol::TargetWriteBoundary::new("orders")),
        )
        .await;
        assert!(
            matches!(&restore, MongodbRestoreResult::Refused { reason } if reason.contains("elsewhere")),
            "{restore:?}"
        );
    }

    /// The origin's password reaches the provider as its `_alt_password`
    /// fallback, exactly as the control plane hands it over locally.
    #[test]
    fn the_origin_password_is_the_providers_fallback() {
        let request = MongodbRestoreRequest {
            service_id: 4,
            node_id: 2,
            service_config: config(serde_json::json!({"password": "target"})),
            origin_password: Some("origin".to_string()),
            source: WorkerS3Destination {
                bucket: "b".to_string(),
                region: "r".to_string(),
                endpoint: None,
                force_path_style: true,
                access_key_id: "a".to_string(),
                secret_access_key: "s".to_string(),
                session_token: None,
            },
            location: "k".to_string(),
            operation_id: None,
        };
        let engine = request.engine_config();
        assert_eq!(engine.parameters["password"], "target");
        assert_eq!(engine.parameters["_alt_password"], "origin");
    }

    #[test]
    fn bounded_tail_preserves_utf8_boundaries() {
        let value = format!("{}END", "é".repeat(1_100));
        let tail = bounded_tail(&value);
        assert!(tail.ends_with("END"));
        assert!(tail.starts_with('…'));
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
    /// deadline, must not start a second `backup-push` while the first one
    /// still streams the dump.
    #[tokio::test]
    async fn a_surviving_backup_push_is_not_overlapped() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        let name = container(&docker, SURVIVING_PUSH).await;
        let_the_push_start().await;
        let result = mongodb_backup(
            Arc::clone(&docker),
            MongodbBackup {
                service_id: 42,
                container_name: name.clone(),
                service_config: ServiceConfig {
                    name: "orders".to_string(),
                    service_type: ServiceType::Mongodb,
                    version: None,
                    parameters: serde_json::json!({ "username": "root", "password": "pw" }),
                },
                s3: unreachable_s3(),
                backup_uuid: "b-2".to_string(),
                mode: Some(MongodbBackupMode::WalgStream),
            },
            &CancellationToken::new(),
        )
        .await;
        remove(&docker, &name).await;
        let error = result.expect_err("a second backup-push must not start");
        assert!(
            matches!(
                &error,
                MongodbBackupError::BackupPushRunning { service_id: 42, container } if container == &name
            ),
            "unexpected error: {error}"
        );
        assert!(error.to_string().contains("still running"), "{error}");
        assert!(!error.is_refusal(), "a retry can succeed once it ends");
    }
}
