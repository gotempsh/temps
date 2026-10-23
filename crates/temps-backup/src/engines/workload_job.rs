// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backups as jobs (ADR-055 §12, Phase 9).
//!
//! A backup of a PostgreSQL database on a worker node is a
//! `RunToCompletion` workload owned by the database (`external_service/<id>`)
//! and pinned to the node holding its data volume (§8). The job runs the
//! database's own image (so `pg_dumpall` matches the server's major
//! version) on the database's network and streams
//!
//! ```text
//! pg_dumpall | gzip | wal-g st put --read-stdin <key>
//! ```
//!
//! straight into the S3 source: constant memory, no temp file, and no copy-out
//! channel back to the control plane. The object key and format are the
//! `postgres_pgdump` engine's (`.../dump.sql.gz`), so the restore planner and
//! a later local restore read it unchanged. A restore is the reverse job,
//! `wal-g st cat <key> | gunzip | psql`, on the same node.
//!
//! ## The gate
//!
//! [`takes_job_path`] selects this path only for a standalone PostgreSQL
//! database whose `external_services.workload_mode` is `enforce` **and**
//! that lives on a worker node. Every other database resolves exactly the
//! engine it did before (the default mode is `legacy`), control-plane
//! databases keep their local WAL-G/pg_dump chain, and the databases this
//! path serves are precisely the ones whose backups could not run at all
//! (the engines drive the control plane's local Docker). Rolling back is
//! setting the database's mode back to `legacy` through the existing audited
//! API: backup jobs stop being scheduled and the local path is unchanged.
//!
//! ## Secrets
//!
//! `PGPASSWORD` and the S3 credentials reach the job only through
//! [`WorkloadPlane::stage_env`]: they live in this process's memory until the
//! job's instance is created, are redacted from its output, and are never
//! written into the stored spec. A job recreated after a restart cannot read
//! them and fails; the executor's retry then starts a new one.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::{json, Value};
use tracing::{info, warn};

use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};
use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
use temps_workload::{
    Artifact, ConditionKind, EnvSpec, Isolation, Lifetime, PlacementSpec, Process, WorkloadKind,
    WorkloadPhase, WorkloadRef, WorkloadSpec,
};
use temps_workload_plane::{ApplyRequest, Owner, PlaneError, PlaneSlot, WorkloadPlane};
use temps_workload_volumes::VolumePin;

use super::v2_common;

/// Engine key stored in `backup_jobs.engine` for a backup taken as a job.
pub const ENGINE_KEY: &str = "postgres_workload_job";
/// Owner kind of the workloads a database's backups and restores run as.
pub const DATABASE_OWNER_KIND: &str = "external_service";
/// The node id of the control plane.
const CONTROL_PLANE_NODE_ID: i32 = 0;
const DUMP_FILE_SUFFIX: &str = "dump.sql.gz";
/// How long a finished job's row is kept for inspection.
const FINISHED_TTL_SECS: u32 = 3600;
/// Upper bound of one backup or restore job; the executor's own wall-clock
/// timeout normally fires first.
const JOB_TIMEOUT_SECS: u32 = 6 * 3600;

/// Exit codes of the job scripts, so a failure names the step that failed.
pub mod exit {
    /// `pg_dumpall` failed (the upload may hold a partial object).
    pub const DUMP_FAILED: i64 = 2;
    /// `wal-g st put` could not write the object.
    pub const UPLOAD_FAILED: i64 = 3;
    /// `wal-g st cat` could not read the object.
    pub const DOWNLOAD_FAILED: i64 = 4;
    /// `psql` could not apply the dump.
    pub const APPLY_FAILED: i64 = 5;
    /// The database's image has no `wal-g`.
    pub const NO_WALG: i64 = 6;
}

/// Whether a backup (and an in-place restore) of `service` runs as a job on
/// its node. See the module docs for why this is the gate.
pub fn takes_job_path(service: &temps_entities::external_services::Model) -> bool {
    is_postgres(&service.service_type)
        && service.topology.as_str() != "cluster"
        && service.workload_mode == ServiceWorkloadMode::Enforce
        && worker_node(service).is_some()
}

/// The worker node `service` lives on: `None` for the control plane (NULL,
/// or node 0).
pub fn worker_node(service: &temps_entities::external_services::Model) -> Option<i32> {
    service
        .node_id
        .filter(|node_id| *node_id != CONTROL_PLANE_NODE_ID)
}

fn is_postgres(service_type: &str) -> bool {
    matches!(
        service_type,
        "postgres" | "postgresql" | "timescale" | "timescaledb"
    )
}

/// Why a database job could not run.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseJobError {
    #[error(
        "{operation} of database {service_id} needs the workload plane, which this process does \
         not run (it starts with `temps serve` once the deployments plugin is up)"
    )]
    PlaneUnavailable {
        service_id: i32,
        operation: &'static str,
    },

    #[error(
        "{operation} of database {service_id} runs on node {node_id}, which cannot reach the S3 \
         endpoint '{endpoint}' of source {s3_source_id}: it names this host's loopback. \
         Configure the S3 source with an address the node can reach"
    )]
    EndpointUnreachable {
        service_id: i32,
        operation: &'static str,
        node_id: i32,
        s3_source_id: i32,
        endpoint: String,
    },

    #[error("{operation} job of database {service_id} was not started: {source}")]
    NotStarted {
        service_id: i32,
        operation: &'static str,
        #[source]
        source: Box<PlaneError>,
    },

    #[error(
        "{operation} job {workload_id} of database {service_id} on node {node_id} failed{}: {reason}",
        .exit_code.map(|code| format!(" (exit code {code})")).unwrap_or_default()
    )]
    Failed {
        service_id: i32,
        operation: &'static str,
        workload_id: i64,
        node_id: i32,
        exit_code: Option<i64>,
        reason: String,
    },

    #[error("{operation} job {workload_id} of database {service_id} was cancelled")]
    Cancelled {
        service_id: i32,
        operation: &'static str,
        workload_id: i64,
    },
}

impl DatabaseJobError {
    /// Whether retrying the same job cannot help.
    pub fn is_permanent(&self) -> bool {
        match self {
            DatabaseJobError::PlaneUnavailable { .. }
            | DatabaseJobError::EndpointUnreachable { .. } => true,
            DatabaseJobError::Failed { exit_code, .. } => *exit_code == Some(exit::NO_WALG),
            // Admission refusals are permanent; a store or scheduling error
            // may pass.
            DatabaseJobError::NotStarted { source, .. } => {
                matches!(**source, PlaneError::Admission(_))
            }
            DatabaseJobError::Cancelled { .. } => false,
        }
    }
}

impl From<DatabaseJobError> for BackupError {
    fn from(error: DatabaseJobError) -> Self {
        match error {
            DatabaseJobError::Cancelled { .. } => BackupError::Cancelled,
            other if other.is_permanent() => BackupError::PermanentFailure {
                reason: other.to_string(),
            },
            other => BackupError::Failed {
                reason: other.to_string(),
            },
        }
    }
}

/// The database a job runs against, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgJobTarget {
    pub service_id: i32,
    pub service_name: String,
    /// The node holding the database's data volume.
    pub node_id: i32,
    /// The database's own image.
    pub image: String,
    /// The database container's name on its network.
    pub host: String,
    pub port: u16,
    pub username: String,
    pub database: String,
}

impl PgJobTarget {
    /// Why the job is pinned where it is ("pinned to node 3 by volume
    /// `postgres-orders_data`").
    pub fn pin(&self) -> VolumePin {
        VolumePin {
            node_id: self.node_id,
            volumes: vec![format!("{}_data", self.host)],
        }
    }
}

/// The S3 source a job reads or writes. Holds credentials: never logged.
#[derive(Clone)]
pub struct S3JobTarget {
    pub s3_source_id: i32,
    pub bucket: String,
    pub region: String,
    /// `None`: AWS's own endpoint for the region.
    pub endpoint: Option<String>,
    pub force_path_style: bool,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for S3JobTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3JobTarget")
            .field("s3_source_id", &self.s3_source_id)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("force_path_style", &self.force_path_style)
            .finish_non_exhaustive()
    }
}

/// Whether `endpoint` names the control plane's own loopback, which a job on
/// another node can never reach.
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    let without_scheme = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    let authority = without_scheme.split('/').next().unwrap_or_default();
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// The variables a job gets through `stage_env`: the database password and
/// the S3 source for `wal-g st`.
pub fn job_env(password: &str, s3: &S3JobTarget) -> Vec<(String, String)> {
    let mut env = vec![
        ("PGPASSWORD".to_string(), password.to_string()),
        ("WALG_S3_PREFIX".to_string(), format!("s3://{}", s3.bucket)),
        ("AWS_ACCESS_KEY_ID".to_string(), s3.access_key_id.clone()),
        (
            "AWS_SECRET_ACCESS_KEY".to_string(),
            s3.secret_access_key.clone(),
        ),
        ("AWS_REGION".to_string(), s3.region.clone()),
    ];
    if let Some(token) = s3.session_token.as_ref().filter(|t| !t.is_empty()) {
        env.push(("AWS_SESSION_TOKEN".to_string(), token.clone()));
    }
    if let Some(endpoint) = s3.endpoint.as_ref().filter(|e| !e.trim().is_empty()) {
        let endpoint = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint.clone()
        } else {
            format!("https://{endpoint}")
        };
        env.push(("AWS_ENDPOINT".to_string(), endpoint));
    }
    if s3.force_path_style {
        env.push(("AWS_S3_FORCE_PATH_STYLE".to_string(), "true".to_string()));
    }
    env
}

use v2_common::shell_escape as q;

const REQUIRE_WALG: &str = "command -v wal-g >/dev/null 2>&1 || { echo \"temps: this image has \
     no wal-g; backups of a database on a worker node need one of Temps' PostgreSQL images\" >&2; \
     exit 6; }";

/// The backup script: the dump streams through gzip into the object. A dump
/// that fails still ends the upload, so the exit status of `pg_dumpall` is
/// carried out of the pipeline in a file (POSIX `sh` has no `pipefail`).
pub fn backup_script(target: &PgJobTarget, object_key: &str) -> String {
    format!(
        "{REQUIRE_WALG}\n\
         rc=/tmp/temps-backup-dump.rc\n\
         {{ pg_dumpall --clean --if-exists --no-password --host={host} --port={port} \
         --username={user} --database={db}; echo $? > \"$rc\"; }} \
         | gzip | wal-g st put --read-stdin --no-compress --no-encrypt --force {key} \
         || {{ echo 'temps: uploading the dump failed' >&2; exit 3; }}\n\
         [ \"$(cat \"$rc\")\" = 0 ] || {{ echo 'temps: pg_dumpall failed' >&2; exit 2; }}\n\
         echo 'temps: backup uploaded'",
        host = q(&target.host),
        port = target.port,
        user = q(&target.username),
        db = q(&target.database),
        key = q(object_key),
    )
}

/// The restore script: the object streams through gunzip into `psql`, as
/// the local pg_dump restore applies a `pg_dumpall --clean` dump (errors on
/// objects that already exist are reported, not fatal).
pub fn restore_script(target: &PgJobTarget, object_key: &str) -> String {
    format!(
        "{REQUIRE_WALG}\n\
         rc=/tmp/temps-restore-fetch.rc\n\
         {{ wal-g st cat {key}; echo $? > \"$rc\"; }} | gunzip \
         | psql -X -q --no-password --host={host} --port={port} --username={user} \
         --dbname=postgres >/dev/null \
         || {{ echo 'temps: psql could not apply the dump' >&2; exit 5; }}\n\
         [ \"$(cat \"$rc\")\" = 0 ] || {{ echo 'temps: downloading the dump failed' >&2; exit 4; }}\n\
         echo 'temps: restore applied'",
        host = q(&target.host),
        port = target.port,
        user = q(&target.username),
        key = q(object_key),
    )
}

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPurpose {
    Backup { backup_id: i32 },
    Restore { restore_run_id: i32 },
}

impl JobPurpose {
    fn operation(self) -> &'static str {
        match self {
            JobPurpose::Backup { .. } => "backup",
            JobPurpose::Restore { .. } => "restore",
        }
    }

    fn id(self) -> i32 {
        match self {
            JobPurpose::Backup { backup_id } => backup_id,
            JobPurpose::Restore { restore_run_id } => restore_run_id,
        }
    }
}

/// The workload a backup or restore job is: `script` in the database's
/// image, pinned to the database's node, on the plane's network. `import`
/// is the staged environment's import.
pub fn job_spec(
    target: &PgJobTarget,
    purpose: JobPurpose,
    script: String,
    import: String,
) -> WorkloadSpec {
    let mut labels = std::collections::BTreeMap::from([
        (
            "sh.temps.service".to_string(),
            format!("{}-{}", purpose.operation(), target.service_name),
        ),
        (
            "sh.temps.service_id".to_string(),
            target.service_id.to_string(),
        ),
    ]);
    match purpose {
        JobPurpose::Backup { backup_id } => {
            labels.insert("sh.temps.backup_id".to_string(), backup_id.to_string());
        }
        JobPurpose::Restore { restore_run_id } => {
            labels.insert(
                "sh.temps.restore_run_id".to_string(),
                restore_run_id.to_string(),
            );
        }
    }
    WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Job, i64::from(purpose.id())),
        artifact: Artifact::oci(&target.image),
        isolation: Isolation::Process,
        process: Process {
            entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
            command: Some(vec![script]),
            ..Default::default()
        },
        env: EnvSpec {
            imports: vec![import],
            ..Default::default()
        },
        resources: Default::default(),
        volumes: vec![],
        ports: vec![],
        network: Default::default(),
        probes: Default::default(),
        // Strict (§8): the job runs on the node holding the data, or not
        // at all.
        placement: PlacementSpec {
            nodes: Some(vec![target.node_id]),
            ..Default::default()
        },
        lifetime: Lifetime::RunToCompletion {
            timeout_secs: JOB_TIMEOUT_SECS,
            // The backup executor and the restore run own retries.
            retry_limit: 0,
            ttl_secs_after_finished: Some(FINISHED_TTL_SECS),
        },
        security: None,
        labels,
    }
}

/// Run one database job to completion through `plane`: stage `env`, apply,
/// wait for it to finish, and stop it if `cancel` fires first.
pub async fn run_database_job(
    plane: &WorkloadPlane,
    target: &PgJobTarget,
    purpose: JobPurpose,
    script: String,
    env: Vec<(String, String)>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<i64, DatabaseJobError> {
    let operation = purpose.operation();
    let service_id = target.service_id;
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("{operation}-{}-{}", purpose.id(), &attempt[..12]);
    // Held until the job finishes: its instance reads the values once, when
    // it is created.
    let staged = plane.stage_env(&format!("database-job-{name}"), env);
    let spec = job_spec(target, purpose, script, staged.import());
    let pin = target.pin();
    info!(
        service_id,
        node_id = target.node_id,
        job = %name,
        "database {operation} job {pin}"
    );
    let workload_id = plane
        .apply(ApplyRequest {
            owner: Owner::new(DATABASE_OWNER_KIND, i64::from(service_id)),
            name: name.clone(),
            spec,
            project_slug: None,
            finalizers: Vec::new(),
        })
        .await
        .map_err(|source| DatabaseJobError::NotStarted {
            service_id,
            operation,
            source: Box::new(source),
        })?;
    let wait = plane.wait_for(
        workload_id,
        ConditionKind::Completed,
        Duration::from_secs(u64::from(JOB_TIMEOUT_SECS)),
    );
    let outcome = tokio::select! {
        outcome = wait => outcome,
        _ = cancel.cancelled() => {
            if let Err(error) = plane
                .delete(workload_id, "Cancelled", &format!("the {operation} was cancelled"))
                .await
            {
                warn!(service_id, workload_id, "stopping a cancelled database job failed: {error}");
            }
            return Err(DatabaseJobError::Cancelled { service_id, operation, workload_id });
        }
    };
    drop(staged);
    match outcome {
        Ok(view) if view.status.phase == WorkloadPhase::Succeeded => {
            info!(
                service_id,
                workload_id, "database {operation} job succeeded"
            );
            Ok(workload_id)
        }
        Ok(view) => Err(failed(target, operation, workload_id, &view.status, None)),
        Err(error) => {
            let status = plane.get(workload_id).await.ok().map(|view| view.status);
            Err(match status {
                Some(status) => failed(
                    target,
                    operation,
                    workload_id,
                    &status,
                    Some(error.to_string()),
                ),
                None => DatabaseJobError::Failed {
                    service_id,
                    operation,
                    workload_id,
                    node_id: target.node_id,
                    exit_code: None,
                    reason: error.to_string(),
                },
            })
        }
    }
}

fn failed(
    target: &PgJobTarget,
    operation: &'static str,
    workload_id: i64,
    status: &temps_workload::WorkloadStatus,
    fallback: Option<String>,
) -> DatabaseJobError {
    let step = match status.exit_code {
        Some(exit::DUMP_FAILED) => Some("pg_dumpall failed"),
        Some(exit::UPLOAD_FAILED) => Some("uploading the dump to S3 failed"),
        Some(exit::DOWNLOAD_FAILED) => Some("downloading the dump from S3 failed"),
        Some(exit::APPLY_FAILED) => Some("psql could not apply the dump"),
        Some(exit::NO_WALG) => Some(
            "the database's image has no wal-g; backups of a database on a worker node need \
             one of Temps' PostgreSQL images",
        ),
        _ => None,
    };
    let condition = status
        .condition(ConditionKind::Completed)
        .map(|c| format!("{}: {}", c.reason, c.message));
    let reason = [step.map(str::to_string), condition, fallback]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ");
    DatabaseJobError::Failed {
        service_id: target.service_id,
        operation,
        workload_id,
        node_id: target.node_id,
        exit_code: status.exit_code,
        reason: if reason.is_empty() {
            format!("the job ended in phase {}", status.phase.as_str())
        } else {
            reason
        },
    }
}

/// The connection parameters of a PostgreSQL service's decrypted config.
pub fn pg_target(
    service: &temps_entities::external_services::Model,
    node_id: i32,
    parameters: &Value,
) -> (PgJobTarget, String) {
    let text = |key: &str| parameters.get(key).and_then(Value::as_str);
    let target = PgJobTarget {
        service_id: service.id,
        service_name: service.name.clone(),
        node_id,
        image: text("docker_image")
            .filter(|image| !image.trim().is_empty())
            .unwrap_or("gotempsh/postgres-walg:18-bookworm")
            .to_string(),
        host: super::dispatch::service_container_name(service),
        port: 5432,
        username: text("username").unwrap_or("postgres").to_string(),
        database: text("database")
            .or_else(|| text("db_name"))
            .unwrap_or("postgres")
            .to_string(),
    };
    (target, text("password").unwrap_or_default().to_string())
}

/// The S3 source `s3_source_id`, decrypted, refused when a job on
/// `node_id` could not reach it.
pub async fn s3_target(
    db: &DatabaseConnection,
    encryption: &Arc<temps_core::EncryptionService>,
    s3_source_id: i32,
    service_id: i32,
    node_id: i32,
    operation: &'static str,
) -> Result<S3JobTarget, BackupError> {
    let source = v2_common::load_s3_source(db, s3_source_id).await?;
    let decrypt = |field: &str, value: &str| {
        encryption
            .decrypt_string(value)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypting the {field} of S3 source {s3_source_id}: {e}"),
            })
    };
    let access_key_id = decrypt("access key", &source.access_key_id)?;
    let secret_access_key = decrypt("secret key", &source.secret_key)?;
    let session_token = v2_common::decrypt_session_token(&source, encryption)?;
    if let Some(endpoint) = source.endpoint.as_ref().filter(|e| is_loopback_endpoint(e)) {
        return Err(DatabaseJobError::EndpointUnreachable {
            service_id,
            operation,
            node_id,
            s3_source_id,
            endpoint: endpoint.clone(),
        }
        .into());
    }
    Ok(S3JobTarget {
        s3_source_id,
        bucket: source.bucket_name.clone(),
        region: source.region.clone(),
        endpoint: source.endpoint.clone(),
        force_path_style: source.force_path_style.unwrap_or(true),
        access_key_id,
        secret_access_key,
        session_token,
    })
}

pub struct PostgresWorkloadJobDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// The workload plane, bound once `temps serve` starts it.
    pub plane: Arc<PlaneSlot>,
}

/// `postgres_workload_job`: a backup of a database on a worker node, as a
/// job on that node.
pub struct PostgresWorkloadJobEngine {
    deps: Arc<PostgresWorkloadJobDeps>,
}

impl PostgresWorkloadJobEngine {
    pub fn new(deps: PostgresWorkloadJobDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for PostgresWorkloadJobEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);
        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;

        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!(
                    "db error loading service {service_id} for backup {backup_id}: {e}"
                ),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {service_id} of backup {backup_id} not found"),
            })?;
        // The database may have moved since the backup was enqueued: a
        // database no longer on a worker is backed up by the next run's
        // local engine, never by a job pinned to a node it left.
        let Some(node_id) = worker_node(&service) else {
            return Err(BackupError::PermanentFailure {
                reason: format!(
                    "backup {backup_id}: database {service_id} ('{}') is no longer on a worker \
                     node; the next backup uses the local engine",
                    service.name
                ),
            });
        };
        let plane = deps.plane.get().ok_or(DatabaseJobError::PlaneUnavailable {
            service_id,
            operation: "backup",
        })?;

        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "postgres-workload-job-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;
        let s3 = s3_target(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            service_id,
            node_id,
            "backup",
        )
        .await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "postgres",
            &service.name,
            &backup_uuid,
            DUMP_FILE_SUFFIX,
        );
        let config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypting the config of database {service_id}: {e}"),
            })?;
        let parameters: Value = serde_json::from_str(&config_json).unwrap_or_else(|_| json!({}));
        let (target, password) = pg_target(&service, node_id, &parameters);
        info!(
            backup_id,
            service_id,
            node_id,
            s3_key = %s3_key,
            bucket = %s3_source.bucket_name,
            "PostgresWorkloadJobEngine: starting the backup job",
        );

        let script = backup_script(&target, &s3_key);
        let result = run_database_job(
            &plane,
            &target,
            JobPurpose::Backup { backup_id },
            script,
            job_env(&password, &s3),
            &ctx.cancel,
        )
        .await;
        if let Err(error) = result {
            // A failed dump may have left a partial object behind.
            if let Err(cleanup) = s3_client
                .delete_object()
                .bucket(&s3_source.bucket_name)
                .key(&s3_key)
                .send()
                .await
            {
                warn!(
                    backup_id,
                    s3_key = %s3_key,
                    "removing the partial object of a failed backup job failed: {}",
                    v2_common::describe_sdk_error("delete_object", &cleanup)
                );
            }
            return Err(error.into());
        }

        let head = s3_client
            .head_object()
            .bucket(&s3_source.bucket_name)
            .key(&s3_key)
            .send()
            .await
            .map_err(|e| BackupError::Failed {
                reason: v2_common::describe_sdk_error(
                    &format!(
                        "head_object on '{}/{}' after backup job of backup {backup_id}",
                        s3_source.bucket_name, s3_key
                    ),
                    &e,
                ),
            })?;
        let size = head.content_length().unwrap_or(0);

        let metadata_key = v2_common::derive_metadata_key(&s3_key);
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &s3_key,
            size,
            s3_source_id,
            "gzip",
            Some(json!({
                "service": { "id": service_id, "name": service.name },
                "node_id": node_id,
            })),
        )
        .await?;

        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %s3_key,
            size_bytes = size,
            node_id,
            "PostgresWorkloadJobEngine: backup complete",
        );
        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(size),
            compression: "gzip".to_string(),
        })
    }
}

/// Whether a restore of `backup_location` into `target` runs as a job:
/// the target takes the job path ([`takes_job_path`]) and the backup is a
/// `pg_dumpall` dump (the format both the job and the local pg_dump engine
/// write).
pub fn restore_takes_job_path(
    target: &temps_entities::external_services::Model,
    backup_location: &str,
) -> bool {
    takes_job_path(target) && backup_location.ends_with(".sql.gz")
}

/// The origin database of backup `backup_id`, from its producer link.
pub async fn backup_origin(
    db: &DatabaseConnection,
    backup_id: i32,
) -> Result<Option<i32>, sea_orm::DbErr> {
    use temps_entities::external_service_backups::{Column, Entity};
    Ok(Entity::find()
        .filter(Column::BackupId.eq(backup_id))
        .one(db)
        .await?
        .map(|link| link.service_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(
        node_id: Option<i32>,
        mode: ServiceWorkloadMode,
    ) -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 7,
            name: "orders".into(),
            service_type: "postgres".into(),
            version: None,
            status: "running".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: None,
            config: None,
            node_id,
            topology: "standalone".into(),
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
            workload_mode: mode,
        }
    }

    fn target() -> PgJobTarget {
        PgJobTarget {
            service_id: 7,
            service_name: "orders".into(),
            node_id: 3,
            image: "gotempsh/postgres-walg:18-bookworm".into(),
            host: "postgres-orders".into(),
            port: 5432,
            username: "app".into(),
            database: "orders".into(),
        }
    }

    #[test]
    fn only_enforce_databases_on_a_worker_take_the_job_path() {
        assert!(takes_job_path(&service(
            Some(3),
            ServiceWorkloadMode::Enforce
        )));
        // The default, and shadow, keep today's engine.
        assert!(!takes_job_path(&service(
            Some(3),
            ServiceWorkloadMode::Legacy
        )));
        assert!(!takes_job_path(&service(
            Some(3),
            ServiceWorkloadMode::Shadow
        )));
        // A control-plane database keeps its local chain, in every mode.
        assert!(!takes_job_path(&service(
            None,
            ServiceWorkloadMode::Enforce
        )));
        assert!(!takes_job_path(&service(
            Some(0),
            ServiceWorkloadMode::Enforce
        )));
        let mut cluster = service(Some(3), ServiceWorkloadMode::Enforce);
        cluster.topology = "cluster".into();
        assert!(!takes_job_path(&cluster));
        let mut redis = service(Some(3), ServiceWorkloadMode::Enforce);
        redis.service_type = "redis".into();
        assert!(!takes_job_path(&redis));
        let mut timescale = service(Some(3), ServiceWorkloadMode::Enforce);
        timescale.service_type = "timescaledb".into();
        assert!(takes_job_path(&timescale));
    }

    #[test]
    fn a_restore_takes_the_job_path_only_for_a_dump() {
        let worker = service(Some(3), ServiceWorkloadMode::Enforce);
        assert!(restore_takes_job_path(
            &worker,
            "external_services/postgres/orders/2026/10/23/u/dump.sql.gz"
        ));
        assert!(!restore_takes_job_path(
            &worker,
            "external_services/postgres/orders/basebackups_005/base_000"
        ));
        assert!(!restore_takes_job_path(
            &service(None, ServiceWorkloadMode::Enforce),
            "x/dump.sql.gz"
        ));
    }

    #[test]
    fn the_backup_job_is_pinned_to_the_databases_node_and_holds_no_secret() {
        let spec = job_spec(
            &target(),
            JobPurpose::Backup { backup_id: 41 },
            backup_script(&target(), "a/b/dump.sql.gz"),
            "staged:database-job-x".into(),
        );
        assert_eq!(spec.owner, WorkloadRef::new(WorkloadKind::Job, 41));
        assert_eq!(spec.placement.nodes, Some(vec![3]));
        assert!(matches!(
            spec.lifetime,
            Lifetime::RunToCompletion { retry_limit: 0, .. }
        ));
        assert_eq!(spec.env.imports, vec!["staged:database-job-x".to_string()]);
        assert!(spec.env.vars.is_empty() && spec.env.secrets.is_empty());
        assert!(spec.volumes.is_empty(), "a dump reads over the network");
        assert_eq!(
            spec.artifact,
            Artifact::oci("gotempsh/postgres-walg:18-bookworm")
        );
        assert_eq!(
            spec.labels.get("sh.temps.backup_id").map(String::as_str),
            Some("41")
        );
        let stored = serde_json::to_string(&spec).expect("serializes");
        assert!(!stored.contains("PGPASSWORD="), "{stored}");
        assert_eq!(
            target().pin().to_string(),
            "pinned to node 3 by volume `postgres-orders_data`"
        );
    }

    #[test]
    fn the_backup_script_streams_into_the_object_and_reports_each_step() {
        let script = backup_script(&target(), "p/external_services/postgres/orders/dump.sql.gz");
        assert!(
            script.contains("--host='postgres-orders' --port=5432"),
            "{script}"
        );
        assert!(
            script.contains("--username='app' --database='orders'"),
            "{script}"
        );
        assert!(
            script.contains(
                "| gzip | wal-g st put --read-stdin --no-compress --no-encrypt --force \
                 'p/external_services/postgres/orders/dump.sql.gz'"
            ),
            "{script}"
        );
        assert!(script.contains("exit 2") && script.contains("exit 3"));
        assert!(script.contains("exit 6"), "{script}");
        assert!(!script.contains("pipefail"), "POSIX sh has none: {script}");

        let hostile = PgJobTarget {
            username: "a'; rm -rf / #".into(),
            ..target()
        };
        let script = backup_script(&hostile, "k");
        assert!(
            script.contains("--username='a'\\''; rm -rf / #'"),
            "{script}"
        );
    }

    #[test]
    fn the_restore_script_streams_the_object_into_psql() {
        let script = restore_script(&target(), "p/dump.sql.gz");
        assert!(script.contains("wal-g st cat 'p/dump.sql.gz'"), "{script}");
        assert!(
            script.contains("| gunzip | psql -X -q --no-password"),
            "{script}"
        );
        assert!(script.contains("--dbname=postgres"), "{script}");
        assert!(script.contains("exit 4") && script.contains("exit 5"));
        let spec = job_spec(
            &target(),
            JobPurpose::Restore { restore_run_id: 9 },
            script,
            "staged:r".into(),
        );
        assert_eq!(
            spec.labels
                .get("sh.temps.restore_run_id")
                .map(String::as_str),
            Some("9")
        );
        assert_eq!(spec.placement.nodes, Some(vec![3]));
    }

    #[test]
    fn job_env_carries_the_source_and_omits_what_is_unset() {
        let s3 = S3JobTarget {
            s3_source_id: 2,
            bucket: "backups".into(),
            region: "eu-west-1".into(),
            endpoint: Some("s3.internal.example:9000".into()),
            force_path_style: true,
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
            session_token: None,
        };
        let env: std::collections::BTreeMap<_, _> = job_env("pw", &s3).into_iter().collect();
        assert_eq!(
            env.get("WALG_S3_PREFIX").map(String::as_str),
            Some("s3://backups")
        );
        assert_eq!(
            env.get("AWS_ENDPOINT").map(String::as_str),
            Some("https://s3.internal.example:9000")
        );
        assert_eq!(
            env.get("AWS_S3_FORCE_PATH_STYLE").map(String::as_str),
            Some("true")
        );
        assert_eq!(env.get("PGPASSWORD").map(String::as_str), Some("pw"));
        assert!(!env.contains_key("AWS_SESSION_TOKEN"));
        // Every credential is one the plane redacts from the job's output.
        for name in ["PGPASSWORD", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"] {
            assert!(temps_workload_env::is_secret_variable(name), "{name}");
        }
        assert!(
            !format!("{s3:?}").contains("SK"),
            "Debug never prints credentials"
        );

        let aws = S3JobTarget {
            endpoint: None,
            force_path_style: false,
            session_token: Some("ST".into()),
            ..s3
        };
        let env: std::collections::BTreeMap<_, _> = job_env("pw", &aws).into_iter().collect();
        assert!(!env.contains_key("AWS_ENDPOINT"));
        assert!(!env.contains_key("AWS_S3_FORCE_PATH_STYLE"));
        assert_eq!(env.get("AWS_SESSION_TOKEN").map(String::as_str), Some("ST"));
    }

    #[test]
    fn a_loopback_endpoint_is_unreachable_from_a_worker() {
        for endpoint in [
            "http://localhost:9000",
            "127.0.0.1:9000",
            "http://[::1]:9000/",
            "https://minio.localhost",
            "http://0.0.0.0:9000",
        ] {
            assert!(is_loopback_endpoint(endpoint), "{endpoint}");
        }
        for endpoint in [
            "https://s3.eu-west-1.amazonaws.com",
            "http://10.0.0.5:9000",
            "s3.internal.example:9000",
        ] {
            assert!(!is_loopback_endpoint(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn failures_name_the_step_and_only_a_missing_tool_is_permanent() {
        let mut status = temps_workload::WorkloadStatus {
            phase: WorkloadPhase::Failed,
            exit_code: Some(exit::DUMP_FAILED),
            ..Default::default()
        };
        let error = failed(&target(), "backup", 12, &status, None);
        let text = error.to_string();
        assert!(
            text.contains("backup job 12 of database 7 on node 3"),
            "{text}"
        );
        assert!(text.contains("exit code 2"), "{text}");
        assert!(text.contains("pg_dumpall failed"), "{text}");
        assert!(!error.is_permanent());
        assert!(matches!(
            BackupError::from(error),
            BackupError::Failed { .. }
        ));

        status.exit_code = Some(exit::NO_WALG);
        let error = failed(&target(), "backup", 12, &status, None);
        assert!(error.is_permanent());
        assert!(matches!(
            BackupError::from(error),
            BackupError::PermanentFailure { .. }
        ));

        let cancelled = DatabaseJobError::Cancelled {
            service_id: 7,
            operation: "backup",
            workload_id: 12,
        };
        assert!(matches!(
            BackupError::from(cancelled),
            BackupError::Cancelled
        ));
        let unavailable = DatabaseJobError::PlaneUnavailable {
            service_id: 7,
            operation: "backup",
        };
        assert!(matches!(
            BackupError::from(unavailable),
            BackupError::PermanentFailure { .. }
        ));
    }

    #[test]
    fn pg_target_reads_the_service_config() {
        let svc = service(Some(3), ServiceWorkloadMode::Enforce);
        let (target, password) = pg_target(
            &svc,
            3,
            &json!({"username": "app", "password": "pw", "database": "orders",
                    "docker_image": "gotempsh/pgvector-walg:pg18"}),
        );
        assert_eq!(target.host, "postgres-orders");
        assert_eq!(target.image, "gotempsh/pgvector-walg:pg18");
        assert_eq!(target.username, "app");
        assert_eq!(password, "pw");
        let (defaults, _) = pg_target(&svc, 3, &json!({}));
        assert_eq!(defaults.username, "postgres");
        assert_eq!(defaults.database, "postgres");
        assert_eq!(defaults.image, "gotempsh/postgres-walg:18-bookworm");
    }
}
