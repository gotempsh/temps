// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node half of a PostgreSQL WAL-G base backup (ADR-099): everything the
//! backup does to the container. The control plane runs it with its own
//! Docker client for a service it hosts; a worker's agent runs the same
//! function with the node's client for a service on that worker.
//!
//! 1. Refuse an unconfirmed restart: with `allow_restart` false, a service
//!    whose `archive_mode` is off is left exactly as it is.
//! 2. Turn continuous archiving on, or refresh it: `walg.env` on the data
//!    volume, `archive_command` reading it, and, when `archive_mode` is off,
//!    the guarded container recreate (the provider's own on the control
//!    plane, the running container's own on a worker).
//! 3. `wal-g backup-push $PGDATA` in the container, unless one already runs
//!    there (checked first, before anything is changed).
//! 4. Read `pg_current_wal_lsn()`.
//!
//! The plane half (S3 source and its pin, size, metadata, the backup row)
//! stays in the backup engine.

use std::net::IpAddr;
use std::sync::Arc;

use bollard::Docker;
use temps_docker_util::{
    exec_capture, DockerUtilError, ExecCaptureOptions, ExitCodePolicy, OutputLimit,
    StreamErrorPolicy,
};
use temps_service_protocol::{
    PostgresWalgArchiveRequest, PostgresWalgArchiveResult, PostgresWalgBackupRequest,
    PostgresWalgBackupResult, PostgresWalgCredentialsRequest, PostgresWalgCredentialsResult,
    WorkerS3Destination,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::postgres::{NodeRecreate, PostgresService};
use super::{EngineError, S3Credentials, ServiceConfig, ServiceType};

/// How much of each output stream a WAL-G exec keeps: the end, where a failure
/// explains itself.
pub const WALG_EXEC_CAPTURE_BYTES: usize = 64 * 1024;

/// `<bucket_path>/external_services/postgres/<service>`: where a PostgreSQL
/// service's backups live inside its S3 source.
fn service_root(bucket_path: &str, service_name: &str) -> String {
    let root = format!("external_services/postgres/{service_name}");
    match bucket_path.trim_matches('/') {
        "" => root,
        base => format!("{base}/{root}"),
    }
}

/// `WALG_S3_PREFIX` of a PostgreSQL service: the same on the control plane
/// and on every worker, so either restores the other's backups.
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

/// The WAL-G variables that tie a base backup to its Temps backup: always a
/// full backup, and the backup's id in the sentinel's user data.
pub fn walg_identity_env(backup_uuid: &str) -> [String; 2] {
    [
        "WALG_DELTA_MAX_STEPS=0".to_string(),
        format!(
            "WALG_SENTINEL_USER_DATA={}",
            serde_json::json!({ "temps_backup_id": backup_uuid })
        ),
    ]
}

/// Where the container a recreate builds comes from.
#[derive(Debug, Clone)]
pub enum ArchiveRecreate {
    /// The provider's control-plane specification.
    Provider,
    /// The running container's own, on a worker (see [`NodeRecreate`]).
    OnNode(NodeRecreate),
}

/// One WAL-G base backup of a PostgreSQL service.
#[derive(Debug, Clone)]
pub struct WalgBaseBackup {
    pub service_id: i32,
    /// The container it runs in.
    pub container_name: String,
    pub service_config: ServiceConfig,
    pub s3: S3Credentials,
    pub backup_uuid: String,
    pub allow_restart: bool,
}

/// What [`walg_base_backup`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalgBaseBackupOutcome {
    /// PostgreSQL was restarted in a recreated container to turn archiving
    /// on; its data was verified to be the same cluster first.
    pub restarted: bool,
    pub stdout: String,
    pub stderr: String,
    pub lsn: Option<String>,
}

#[derive(Debug, Error)]
pub enum WalgBaseBackupError {
    #[error(
        "Backup of service {service_id} ('{service}') would restart PostgreSQL to turn WAL \
         archiving on, and that was not confirmed when the backup was requested (archiving was \
         on then). Nothing was changed. Run the backup again and confirm the restart"
    )]
    RestartNotConfirmed { service_id: i32, service: String },
    #[error(
        "Could not check whether backing up service {service_id} ('{service}') would restart \
         PostgreSQL: {reason}"
    )]
    RestartProbe {
        service_id: i32,
        service: String,
        reason: String,
    },
    #[error(
        "Cannot create a recoverable WAL-G backup for service {service_id} ('{service}'): \
         continuous archiving could not be enabled and verified: {source}"
    )]
    Archiving {
        service_id: i32,
        service: String,
        #[source]
        source: Box<EngineError>,
    },
    #[error(
        "wal-g backup-push of service {service_id} in '{container}' exited with code \
         {exit_code}. stderr: {stderr}. stdout: {stdout}"
    )]
    BackupPush {
        service_id: i32,
        container: String,
        exit_code: i64,
        stderr: String,
        stdout: String,
    },
    #[error("WAL-G exec in container '{container}' failed to {operation}: {reason}")]
    Exec {
        container: String,
        operation: &'static str,
        reason: String,
    },
    #[error(
        "A wal-g backup-push of service {service_id} is still running in '{container}', left by \
         an earlier attempt whose agent restarted or passed its deadline. Nothing was started; \
         the backup can run once it ends"
    )]
    BackupPushRunning { service_id: i32, container: String },
    #[error("WAL-G exec in container '{container}' was cancelled")]
    Cancelled { container: String },
    #[error("PostgreSQL service {service_id} ('{service}') cannot be backed up here: {reason}")]
    Target {
        service_id: i32,
        service: String,
        reason: String,
    },
}

impl WalgBaseBackupError {
    /// Whether a retry can only repeat this error: an unconfirmed restart, or
    /// a recreate the guard refuses because of the container's storage.
    pub fn is_refusal(&self) -> bool {
        match self {
            Self::RestartNotConfirmed { .. } | Self::Target { .. } => true,
            Self::Archiving { source, .. } => match source.as_ref() {
                EngineError::RecreateGuard(guard) => guard.is_refusal(),
                EngineError::RestartNotConfirmed(_) => true,
                _ => false,
            },
            Self::RestartProbe { .. }
            | Self::BackupPush { .. }
            | Self::BackupPushRunning { .. }
            | Self::Exec { .. }
            | Self::Cancelled { .. } => false,
        }
    }
}

/// The output of `sh -c <cmd>` in a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalgExecOutput {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Run `sh -c <cmd>` inside `container_name` with `env`, keeping the last
/// [`WALG_EXEC_CAPTURE_BYTES`] of each stream. Returns on `cancel`.
pub async fn run_walg_exec(
    docker: &Docker,
    container_name: &str,
    cmd: &str,
    env: &[String],
    cancel: &CancellationToken,
) -> Result<WalgExecOutput, WalgBaseBackupError> {
    let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    let output = exec_capture(
        docker,
        container_name,
        bollard::exec::CreateExecOptions {
            cmd: Some(vec!["sh", "-c", cmd]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            env: Some(env_refs),
            ..Default::default()
        },
        ExecCaptureOptions::separate(
            OutputLimit::Tail(WALG_EXEC_CAPTURE_BYTES),
            OutputLimit::Tail(WALG_EXEC_CAPTURE_BYTES),
        )
        .stream_errors(StreamErrorPolicy::Stop)
        .exit_code(ExitCodePolicy::BestEffort)
        .cancel(cancel),
    )
    .await
    .map_err(|error| match error {
        DockerUtilError::ExecCancelled { .. } => WalgBaseBackupError::Cancelled {
            container: container_name.to_string(),
        },
        DockerUtilError::ExecCreate { source, .. } => WalgBaseBackupError::Exec {
            container: container_name.to_string(),
            operation: "create exec",
            reason: source.to_string(),
        },
        other => WalgBaseBackupError::Exec {
            container: container_name.to_string(),
            operation: "start exec",
            reason: other.cause_text(),
        },
    })?;
    if let Some(error) = output.stream_error {
        warn!(container = container_name, %error, "WAL-G exec stream error");
    }
    if let Some(error) = output.inspect_error {
        return Err(WalgBaseBackupError::Exec {
            container: container_name.to_string(),
            operation: "inspect exec",
            reason: error.to_string(),
        });
    }
    Ok(WalgExecOutput {
        exit_code: output.exit_code.unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Login of the backup's `psql` and `wal-g` calls, from the service's
/// stored configuration, with PostgreSQL's own defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresLogin {
    pub username: String,
    pub password: String,
    pub database: String,
}

impl PostgresLogin {
    pub fn from_parameters(parameters: &serde_json::Value) -> Self {
        let text = |keys: &[&str], default: &str| {
            keys.iter()
                .find_map(|key| parameters.get(*key).and_then(|v| v.as_str()))
                .unwrap_or(default)
                .to_string()
        };
        Self {
            username: text(&["username"], "postgres"),
            password: text(&["password"], ""),
            database: text(&["database", "db_name"], "postgres"),
        }
    }

    /// `PGUSER`, `PGPASSWORD`, `PGDATABASE`: credentials go through the
    /// environment, never the command line.
    pub fn env(&self) -> [String; 3] {
        [
            format!("PGUSER={}", self.username),
            format!("PGPASSWORD={}", self.password),
            format!("PGDATABASE={}", self.database),
        ]
    }
}

/// The environment of `wal-g backup-push`. WAL-G's memory is capped at
/// roughly 4 × 128 MiB: its defaults hold every in-flight tar buffer in RAM
/// and can exhaust a small container.
pub fn backup_push_env(
    walg_prefix: &str,
    s3: &S3Credentials,
    login: &PostgresLogin,
    backup_uuid: &str,
    container_endpoint: Option<String>,
) -> Vec<String> {
    let mut env = vec![
        format!("WALG_S3_PREFIX={walg_prefix}"),
        format!("AWS_ACCESS_KEY_ID={}", s3.access_key_id),
        format!("AWS_SECRET_ACCESS_KEY={}", s3.secret_key),
        format!("AWS_REGION={}", s3.region),
    ];
    env.extend(login.env());
    env.extend([
        "PGHOST=localhost".to_string(),
        "PGPORT=5432".to_string(),
        "WALG_UPLOAD_CONCURRENCY=4".to_string(),
        "WALG_UPLOAD_DISK_CONCURRENCY=1".to_string(),
        "WALG_UPLOAD_QUEUE=2".to_string(),
        "WALG_TAR_SIZE_THRESHOLD=134217728".to_string(),
    ]);
    // Absent unless the source holds a temporary credential.
    env.extend(s3.session_token_env());
    env.extend(walg_identity_env(backup_uuid));
    if let Some(endpoint) = container_endpoint {
        let url = if endpoint.starts_with("http") {
            endpoint
        } else {
            format!("http://{endpoint}")
        };
        env.push(format!("AWS_ENDPOINT={url}"));
    }
    if s3.force_path_style {
        env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
    }
    env
}

/// Take a WAL-G base backup of `request`'s service in its container on
/// `docker` (see the module documentation).
pub async fn walg_base_backup(
    docker: Arc<Docker>,
    request: WalgBaseBackup,
    recreate: ArchiveRecreate,
    cancel: &CancellationToken,
) -> Result<WalgBaseBackupOutcome, WalgBaseBackupError> {
    let WalgBaseBackup {
        service_id,
        container_name,
        service_config,
        s3,
        backup_uuid,
        allow_restart,
    } = request;
    let service = service_config.name.clone();
    let prefix = walg_prefix(&s3.bucket_name, &s3.bucket_path, &service);
    let mut postgres = PostgresService::new(service.clone(), Arc::clone(&docker));
    if let ArchiveRecreate::OnNode(node) = recreate {
        postgres = postgres.on_node(node);
    }

    // Before anything is changed: a `backup-push` survives the task that
    // started it (stopped at its deadline, or its agent restarted), which
    // also loses the in-memory record that would refuse this one.
    match probe_backup_push(&docker, &container_name, cancel).await? {
        BackupPushProbe::Idle => {}
        BackupPushProbe::Running => {
            return Err(WalgBaseBackupError::BackupPushRunning {
                service_id,
                container: container_name,
            })
        }
        BackupPushProbe::Unavailable => warn!(
            service_id,
            container = %container_name,
            "Cannot tell whether a wal-g backup-push already runs in the container (no tr or \
             grep in it); starting the base backup"
        ),
    }

    if !allow_restart {
        match postgres
            .backup_requires_restart(service_config.clone())
            .await
        {
            Ok(false) => {}
            Ok(true) => {
                return Err(WalgBaseBackupError::RestartNotConfirmed {
                    service_id,
                    service,
                })
            }
            Err(error) => {
                return Err(WalgBaseBackupError::RestartProbe {
                    service_id,
                    service,
                    reason: error.to_string(),
                })
            }
        }
    }
    // Before the base backup, never after: `backup-push` ends by completing
    // the current WAL segment, which PostgreSQL marks for archiving only if
    // archiving is already on. A base backup taken first could not be
    // recovered. Archiving that is already on costs one settings check.
    let archiving = postgres
        .enable_continuous_archiving_reporting(
            service_config.clone(),
            &s3,
            &prefix,
            // Enforced where the restart is decided too, not only probed
            // above: archiving can be turned off in between (an in-place
            // restore finishing).
            allow_restart,
        )
        .await
        .map_err(|source| WalgBaseBackupError::Archiving {
            service_id,
            service: service.clone(),
            source: Box::new(source),
        })?;
    if archiving.restarted {
        warn!(
            service_id,
            service = %service,
            "PostgreSQL was restarted to enable WAL archiving (archive_mode was off); its data \
             was verified intact before the base backup"
        );
    }

    let login = PostgresLogin::from_parameters(&service_config.parameters);
    let endpoint = s3
        .resolve_endpoint_for_container(&docker, &container_name)
        .await;
    let env = backup_push_env(&prefix, &s3, &login, &backup_uuid, endpoint);
    let output = run_walg_exec(
        &docker,
        &container_name,
        "wal-g backup-push $PGDATA",
        &env,
        cancel,
    )
    .await?;
    if output.exit_code != 0 {
        let or_empty = |text: &str| {
            let text = text.trim();
            if text.is_empty() {
                "<empty>".to_string()
            } else {
                text.to_string()
            }
        };
        return Err(WalgBaseBackupError::BackupPush {
            service_id,
            container: container_name,
            exit_code: output.exit_code,
            stderr: or_empty(&output.stderr),
            stdout: or_empty(&output.stdout),
        });
    }
    if !output.stderr.trim().is_empty() {
        info!(
            service_id,
            container = %container_name,
            "wal-g stderr (warnings): {}",
            output.stderr.trim()
        );
    }
    let lsn = match current_wal_lsn(&docker, &container_name, &login).await {
        Ok(lsn) => Some(lsn),
        Err(error) => {
            warn!(service_id, %error, "Could not read the WAL LSN after a WAL-G base backup");
            None
        }
    };
    Ok(WalgBaseBackupOutcome {
        restarted: archiving.restarted,
        stdout: output.stdout,
        stderr: output.stderr,
        lsn,
    })
}

/// Whether a `wal-g backup-push` runs in a container, from its process
/// table. The `pus[h]` keeps the probe's own command line from matching.
const BACKUP_PUSH_PROBE: &str = "command -v tr >/dev/null && command -v grep >/dev/null \
     || { echo unavailable; exit 0; }; \
     for f in /proc/[0-9]*/cmdline; do tr '\\000' ' ' < \"$f\" 2>/dev/null; echo; done \
     | grep -Eq '(^|[ /])wal-g backup-pus[h]( |$)' && echo running || echo idle";

/// What [`probe_backup_push`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupPushProbe {
    Running,
    Idle,
    /// The container lacks the tools to tell.
    Unavailable,
}

impl BackupPushProbe {
    fn parse(stdout: &str) -> Option<Self> {
        match stdout.trim() {
            "running" => Some(Self::Running),
            "idle" => Some(Self::Idle),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Whether a `wal-g backup-push` already runs in `container_name`.
pub async fn probe_backup_push(
    docker: &Docker,
    container_name: &str,
    cancel: &CancellationToken,
) -> Result<BackupPushProbe, WalgBaseBackupError> {
    let output = run_walg_exec(docker, container_name, BACKUP_PUSH_PROBE, &[], cancel).await?;
    BackupPushProbe::parse(&output.stdout).ok_or_else(|| WalgBaseBackupError::Exec {
        container: container_name.to_string(),
        operation: "check for a running wal-g backup-push",
        reason: format!(
            "exit code {}, stdout: {:?}, stderr: {:?}",
            output.exit_code,
            output.stdout.trim(),
            output.stderr.trim()
        ),
    })
}

/// `pg_current_wal_lsn()` in the container, credentials in the environment.
async fn current_wal_lsn(
    docker: &Docker,
    container_name: &str,
    login: &PostgresLogin,
) -> Result<String, WalgBaseBackupError> {
    let env = login.env();
    let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    let output = exec_capture(
        docker,
        container_name,
        bollard::exec::CreateExecOptions {
            cmd: Some(vec!["psql", "-t", "-c", "SELECT pg_current_wal_lsn()"]),
            env: Some(env_refs),
            attach_stdout: Some(true),
            attach_stderr: Some(false),
            ..Default::default()
        },
        ExecCaptureOptions::separate(OutputLimit::Head(4096), OutputLimit::Head(0))
            .stream_errors(StreamErrorPolicy::Stop)
            .exit_code(ExitCodePolicy::NotRead),
    )
    .await
    .map_err(|error| WalgBaseBackupError::Exec {
        container: container_name.to_string(),
        operation: "read the WAL LSN",
        reason: error.cause_text(),
    })?;
    Ok(output.stdout.trim().to_string())
}

/// A worker service's configuration as the node's checks and recreate need
/// it: the stored loopback host retargeted to where the node publishes its
/// ports, and the managed container's recorded name dropped (kept, the
/// provider would treat the container as imported and never turn archiving
/// on). `Err` names why the service cannot be handled on this node.
fn node_service_config(
    service_id: i32,
    mut service_config: ServiceConfig,
    published_address: Option<IpAddr>,
    operation: &str,
) -> Result<(ServiceConfig, String), String> {
    if service_config.service_type != ServiceType::Postgres {
        return Err(format!(
            "{operation} is for PostgreSQL services; service {service_id} ('{}') is {}",
            service_config.name, service_config.service_type
        ));
    }
    if let Err(error) = super::resolve_managed_probe_target(&mut service_config, published_address)
    {
        return Err(format!(
            "PostgreSQL service {service_id} ('{}') cannot be handled on this node: {error}",
            service_config.name
        ));
    }
    let container_name = super::naming::postgres_container_name(&service_config.name);
    if let Some(parameters) = service_config.parameters.as_object_mut() {
        if parameters.get("container_name").and_then(|v| v.as_str())
            == Some(container_name.as_str())
        {
            parameters.remove("container_name");
        }
    }
    Ok((service_config, container_name))
}

fn node_s3(destination: WorkerS3Destination, bucket_path: String) -> S3Credentials {
    S3Credentials {
        access_key_id: destination.access_key_id,
        secret_key: destination.secret_access_key,
        session_token: destination.session_token,
        region: destination.region,
        endpoint: destination.endpoint,
        bucket_name: destination.bucket,
        bucket_path,
        force_path_style: destination.force_path_style,
    }
}

/// The agent's side of `POST /agent/services/postgres/walg-archive`: point
/// continuous archiving at the request's destination, with the control
/// plane's own `enable_continuous_archiving` and this node's recreate. The
/// re-point is an operator's explicit decision, which accepts the restart
/// turning archiving on needs, as on the control plane.
pub async fn walg_archive_on_node(
    request: PostgresWalgArchiveRequest,
    docker: Arc<Docker>,
    node: NodeRecreate,
    published_address: Option<IpAddr>,
) -> PostgresWalgArchiveResult {
    let PostgresWalgArchiveRequest {
        service_id,
        node_id: _,
        service_config,
        destination,
        bucket_path,
    } = request;
    let (service_config, _container) = match node_service_config(
        service_id,
        service_config,
        published_address,
        "WAL-G archiving",
    ) {
        Ok(target) => target,
        Err(reason) => return PostgresWalgArchiveResult::Refused { reason },
    };
    let service = service_config.name.clone();
    let s3 = node_s3(destination, bucket_path);
    let prefix = walg_prefix(&s3.bucket_name, &s3.bucket_path, &service);
    info!(service_id, service = %service, prefix = %prefix, "Pointing WAL-G archiving at a destination");
    match PostgresService::new(service.clone(), docker)
        .on_node(node)
        .enable_continuous_archiving_reporting(service_config, &s3, &prefix, true)
        .await
    {
        Ok(outcome) => PostgresWalgArchiveResult::Completed {
            restarted: outcome.restarted,
        },
        Err(EngineError::RecreateGuard(guard)) if guard.is_refusal() => {
            PostgresWalgArchiveResult::Refused {
                reason: guard.to_string(),
            }
        }
        Err(error) => PostgresWalgArchiveResult::Failed {
            reason: format!(
                "WAL-G archiving of service {service_id} ('{service}') could not be pointed at \
                 {prefix}: {error}"
            ),
        },
    }
}

/// The agent's side of `POST /agent/services/postgres/walg-credentials`:
/// rewrite `walg.env` with the request's credential for the service's own
/// WAL-G repository, and nothing else (ADR-102).
pub async fn walg_credentials_on_node(
    request: PostgresWalgCredentialsRequest,
    docker: Arc<Docker>,
    published_address: Option<IpAddr>,
) -> PostgresWalgCredentialsResult {
    let PostgresWalgCredentialsRequest {
        service_id,
        service_config,
        destination,
        bucket_path,
    } = request;
    let (service_config, _container) = match node_service_config(
        service_id,
        service_config,
        published_address,
        "refreshing WAL-G archiving credentials",
    ) {
        Ok(target) => target,
        Err(reason) => return PostgresWalgCredentialsResult::Refused { reason },
    };
    let service = service_config.name.clone();
    let s3 = node_s3(destination, bucket_path);
    let prefix = walg_prefix(&s3.bucket_name, &s3.bucket_path, &service);
    match PostgresService::new(service.clone(), docker)
        .refresh_archive_credentials(service_config, &s3, &prefix)
        .await
    {
        Ok(true) => {
            info!(service_id, service = %service, prefix = %prefix, "Refreshed the WAL-G archiving credential");
            PostgresWalgCredentialsResult::Refreshed
        }
        Ok(false) => PostgresWalgCredentialsResult::Refused {
            reason: format!(
                "continuous archiving is not set up for service {service_id} ('{service}') on \
                 this node (no walg.env); a base backup sets it up"
            ),
        },
        Err(error) => PostgresWalgCredentialsResult::Failed {
            reason: format!(
                "the WAL-G archiving credential of service {service_id} ('{service}') could not \
                 be refreshed: {error}"
            ),
        },
    }
}

/// The agent's side of `POST /agent/services/postgres/walg-backup`: the
/// request as [`walg_base_backup`] takes it, run against this node's Docker,
/// recreating with `node`.
///
/// The stored host of a worker service is loopback, which is not where the
/// node publishes its ports; the settings checks connect through
/// `published_address`, like the health and WAL probes.
pub async fn walg_base_backup_on_node(
    request: PostgresWalgBackupRequest,
    docker: Arc<Docker>,
    node: NodeRecreate,
    published_address: Option<IpAddr>,
) -> PostgresWalgBackupResult {
    let PostgresWalgBackupRequest {
        service_id,
        node_id: _,
        service_config,
        destination,
        bucket_path,
        backup_uuid,
        allow_restart,
    } = request;
    let (service_config, container_name) = match node_service_config(
        service_id,
        service_config,
        published_address,
        "WAL-G base backups",
    ) {
        Ok(target) => target,
        Err(reason) => return PostgresWalgBackupResult::Refused { reason },
    };
    let s3 = node_s3(destination, bucket_path);
    // Dropping the request (the control plane gave up) drops this future,
    // and the exec with it; the token only serves the shared signature.
    let cancel = CancellationToken::new();
    match walg_base_backup(
        docker,
        WalgBaseBackup {
            service_id,
            container_name,
            service_config,
            s3,
            backup_uuid,
            allow_restart,
        },
        ArchiveRecreate::OnNode(node),
        &cancel,
    )
    .await
    {
        Ok(outcome) => PostgresWalgBackupResult::Completed {
            restarted: outcome.restarted,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            lsn: outcome.lsn,
        },
        Err(error) if error.is_refusal() => PostgresWalgBackupResult::Refused {
            reason: error.to_string(),
        },
        Err(error) => PostgresWalgBackupResult::Failed {
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_push_probe_reads_only_its_own_answers() {
        assert_eq!(
            BackupPushProbe::parse("running\n"),
            Some(BackupPushProbe::Running)
        );
        assert_eq!(BackupPushProbe::parse("idle"), Some(BackupPushProbe::Idle));
        assert_eq!(
            BackupPushProbe::parse("unavailable\n"),
            Some(BackupPushProbe::Unavailable)
        );
        assert_eq!(BackupPushProbe::parse(""), None);
        assert_eq!(BackupPushProbe::parse("sh: /proc: denied"), None);
    }

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
    fn the_prefix_is_derived_from_the_source_and_the_service_alone() {
        assert_eq!(
            walg_prefix("backups", "", "orders"),
            "s3://backups/external_services/postgres/orders/walg"
        );
        assert_eq!(
            walg_prefix("backups", "/team/a/", "orders"),
            "s3://backups/team/a/external_services/postgres/orders/walg"
        );
        assert_eq!(
            walg_key_prefix("", "orders"),
            "external_services/postgres/orders/walg/"
        );
        assert_eq!(
            walg_key_prefix("team/a", "orders"),
            "team/a/external_services/postgres/orders/walg/"
        );
    }

    #[test]
    fn the_login_falls_back_to_postgres_defaults() {
        let login = PostgresLogin::from_parameters(&serde_json::json!({
            "db_name": "app", "password": "pw"
        }));
        assert_eq!(
            login,
            PostgresLogin {
                username: "postgres".to_string(),
                password: "pw".to_string(),
                database: "app".to_string(),
            }
        );
        assert_eq!(
            PostgresLogin::from_parameters(&serde_json::json!({})).database,
            "postgres"
        );
    }

    /// The exact environment `backup-push` has always run with; the worker
    /// runs this same function.
    #[test]
    fn backup_push_runs_with_the_memory_caps_identity_and_endpoint() {
        let login = PostgresLogin {
            username: "app".to_string(),
            password: "p'w".to_string(),
            database: "appdb".to_string(),
        };
        let env = backup_push_env(
            "s3://backups/base/external_services/postgres/orders/walg",
            &credentials(Some("TOKEN"), true),
            &login,
            "uuid-1",
            Some("minio:9000".to_string()),
        );
        assert_eq!(
            env,
            vec![
                "WALG_S3_PREFIX=s3://backups/base/external_services/postgres/orders/walg",
                "AWS_ACCESS_KEY_ID=AKID",
                "AWS_SECRET_ACCESS_KEY=SECRET",
                "AWS_REGION=us-east-1",
                "PGUSER=app",
                "PGPASSWORD=p'w",
                "PGDATABASE=appdb",
                "PGHOST=localhost",
                "PGPORT=5432",
                "WALG_UPLOAD_CONCURRENCY=4",
                "WALG_UPLOAD_DISK_CONCURRENCY=1",
                "WALG_UPLOAD_QUEUE=2",
                "WALG_TAR_SIZE_THRESHOLD=134217728",
                "AWS_SESSION_TOKEN=TOKEN",
                "WALG_DELTA_MAX_STEPS=0",
                "WALG_SENTINEL_USER_DATA={\"temps_backup_id\":\"uuid-1\"}",
                "AWS_ENDPOINT=http://minio:9000",
                "AWS_S3_FORCE_PATH_STYLE=true",
            ]
        );
        let plain = backup_push_env("s3://b/p", &credentials(None, false), &login, "u", None);
        assert!(!plain.iter().any(|e| e.starts_with("AWS_SESSION_TOKEN")));
        assert!(!plain.iter().any(|e| e.starts_with("AWS_ENDPOINT")));
        assert!(!plain
            .iter()
            .any(|e| e.starts_with("AWS_S3_FORCE_PATH_STYLE")));
    }

    #[test]
    fn only_refusals_are_final() {
        let refused = WalgBaseBackupError::RestartNotConfirmed {
            service_id: 1,
            service: "orders".to_string(),
        };
        assert!(refused.is_refusal());
        let probe = WalgBaseBackupError::RestartProbe {
            service_id: 1,
            service: "orders".to_string(),
            reason: "connection refused".to_string(),
        };
        assert!(!probe.is_refusal());
        let not_confirmed = WalgBaseBackupError::Archiving {
            service_id: 1,
            service: "orders".to_string(),
            source: Box::new(EngineError::RestartNotConfirmed(
                super::super::postgres::PostgresRestartNotConfirmed {
                    service_name: "orders".to_string(),
                },
            )),
        };
        assert!(not_confirmed.is_refusal());
        let push = WalgBaseBackupError::BackupPush {
            service_id: 1,
            container: "postgres-orders".to_string(),
            exit_code: 1,
            stderr: "x".to_string(),
            stdout: "<empty>".to_string(),
        };
        assert!(!push.is_refusal());
        let surviving = WalgBaseBackupError::BackupPushRunning {
            service_id: 1,
            container: "postgres-orders".to_string(),
        };
        assert!(!surviving.is_refusal(), "it can run once the other ends");
    }

    /// A non-PostgreSQL service or a stored host that is not loopback is
    /// refused before the node's Docker is touched.
    #[tokio::test]
    async fn the_node_refuses_what_is_not_its_managed_postgres() {
        let docker = Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("an HTTP Docker client is built without connecting"),
        );
        let node = NodeRecreate {
            service_id: 3,
            node_id: 2,
            attachment: Default::default(),
            after_create: Arc::new(|_| Box::pin(async {})),
        };
        let request = |service_type, host: &str| PostgresWalgBackupRequest {
            service_id: 3,
            node_id: 2,
            service_config: ServiceConfig {
                name: "orders".to_string(),
                service_type,
                version: None,
                parameters: serde_json::json!({"host": host, "port": "5432"}),
            },
            destination: temps_service_protocol::WorkerS3Destination {
                bucket: "backups".to_string(),
                region: "us-east-1".to_string(),
                endpoint: None,
                force_path_style: true,
                access_key_id: "a".to_string(),
                secret_access_key: "s".to_string(),
                session_token: None,
            },
            bucket_path: String::new(),
            backup_uuid: "u".to_string(),
            allow_restart: false,
        };
        let redis = walg_base_backup_on_node(
            request(ServiceType::Redis, "localhost"),
            docker.clone(),
            node.clone(),
            None,
        )
        .await;
        assert!(
            matches!(&redis, PostgresWalgBackupResult::Refused { reason } if reason.contains("is redis")),
            "{redis:?}"
        );
        let elsewhere = walg_base_backup_on_node(
            request(ServiceType::Postgres, "10.9.8.7"),
            docker.clone(),
            node.clone(),
            None,
        )
        .await;
        assert!(
            matches!(&elsewhere, PostgresWalgBackupResult::Refused { reason } if reason.contains("10.9.8.7")),
            "{elsewhere:?}"
        );

        // Re-pointing archiving checks the same before anything runs.
        let archive = |service_type, host: &str| {
            let backup = request(service_type, host);
            PostgresWalgArchiveRequest {
                service_id: backup.service_id,
                node_id: backup.node_id,
                service_config: backup.service_config,
                destination: backup.destination,
                bucket_path: backup.bucket_path,
            }
        };
        for (request, expected) in [
            (archive(ServiceType::Redis, "localhost"), "is redis"),
            (archive(ServiceType::Postgres, "10.9.8.7"), "10.9.8.7"),
        ] {
            let result = walg_archive_on_node(request, docker.clone(), node.clone(), None).await;
            assert!(
                matches!(&result, PostgresWalgArchiveResult::Refused { reason } if reason.contains(expected)),
                "{expected}: {result:?}"
            );
        }
    }
}

/// Against a real Docker daemon. Skips without Docker.
#[cfg(test)]
#[cfg(feature = "docker-tests")]
mod docker_tests {
    use super::*;
    use bollard::models::ContainerCreateBody;
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    };
    use futures::StreamExt;

    const IMAGE: &str = "postgres:18-alpine";

    async fn docker_or_skip() -> Option<Arc<Docker>> {
        let docker = Docker::connect_with_local_defaults().ok()?;
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable, skipping");
            return None;
        }
        let mut pull = docker.create_image(
            Some(CreateImageOptionsBuilder::new().from_image(IMAGE).build()),
            None,
            None,
        );
        while let Some(item) = pull.next().await {
            if let Err(error) = item {
                eprintln!("could not pull {IMAGE}, skipping: {error}");
                return None;
            }
        }
        Some(Arc::new(docker))
    }

    /// A started container running `script`, removed on drop of the test.
    async fn container(docker: &Docker, script: &str) -> String {
        let name = format!("walg-probe-{:08x}", rand::random::<u32>());
        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(IMAGE.to_string()),
                    entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
                    cmd: Some(vec![script.to_string()]),
                    ..Default::default()
                },
            )
            .await
            .expect("create container");
        docker
            .start_container(
                &name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start container");
        name
    }

    async fn remove(docker: &Docker, name: &str) {
        let _ = docker
            .remove_container(
                name,
                Some(
                    RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await;
    }

    /// A `wal-g backup-push` of an earlier attempt, still uploading: the
    /// agent's task that ran it stopped at its deadline, or the agent
    /// restarted, but the process kept running in the container.
    const SURVIVING_PUSH: &str = "printf '#!/bin/sh\\nsleep 600\\n' > /tmp/wal-g; \
         chmod +x /tmp/wal-g; /tmp/wal-g backup-push /var/lib/postgresql/data & \
         exec sleep 600";

    fn request(container_name: &str) -> WalgBaseBackup {
        WalgBaseBackup {
            service_id: 41,
            container_name: container_name.to_string(),
            service_config: ServiceConfig {
                name: "orders-db".to_string(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: serde_json::json!({}),
            },
            s3: S3Credentials {
                access_key_id: "AKID".to_string(),
                secret_key: "SECRET".to_string(),
                session_token: None,
                region: "us-east-1".to_string(),
                endpoint: Some("http://127.0.0.1:1".to_string()),
                bucket_name: "backups".to_string(),
                bucket_path: String::new(),
                force_path_style: true,
            },
            backup_uuid: "b-2".to_string(),
            allow_restart: true,
        }
    }

    /// The reported failure: a retry started a second `backup-push` while
    /// the first still uploaded. Nothing else is done to the service.
    #[tokio::test]
    async fn a_surviving_backup_push_is_not_overlapped() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        let name = container(&docker, SURVIVING_PUSH).await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let result = walg_base_backup(
            Arc::clone(&docker),
            request(&name),
            ArchiveRecreate::Provider,
            &CancellationToken::new(),
        )
        .await;
        remove(&docker, &name).await;
        let error = result.expect_err("a second backup-push must not start");
        assert!(
            error.to_string().contains("still running"),
            "unexpected error: {error}"
        );
        assert!(!error.is_refusal(), "a retry can succeed once it ends");
    }

    /// No false alarm on processes whose command lines merely mention
    /// WAL-G (archiving's `wal-g wal-push`), nor on the probe's own shell.
    #[tokio::test]
    async fn an_idle_container_has_no_backup_push() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        let name = container(
            &docker,
            "sh -c 'sleep 600; : wal-g wal-push pg_wal/000000010000000000000001' & \
             sh -c 'sleep 600; : wal-g backup-pushed' & exec sleep 601",
        )
        .await;
        let probe = probe_backup_push(&docker, &name, &CancellationToken::new()).await;
        remove(&docker, &name).await;
        assert_eq!(probe.expect("probe"), BackupPushProbe::Idle);
    }

    #[tokio::test]
    async fn a_surviving_backup_push_is_found() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        let name = container(&docker, SURVIVING_PUSH).await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let probe = probe_backup_push(&docker, &name, &CancellationToken::new()).await;
        remove(&docker, &name).await;
        assert_eq!(probe.expect("probe"), BackupPushProbe::Running);
    }
}
