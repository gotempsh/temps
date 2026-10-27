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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};
use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
use temps_workload::{
    Artifact, ConditionKind, EnvSpec, Isolation, Lifetime, PlacementSpec, Process, WorkloadKind,
    WorkloadPhase, WorkloadRef, WorkloadSpec,
};
use temps_workload_plane::plane::StagedEnvGuard;
use temps_workload_plane::{
    ApplyRequest, Owner, PlaneError, PlaneSlot, RetireOnDrop, WorkloadPlane,
};
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
    /// The dump read from S3 is not a valid gzip stream (corrupt or
    /// truncated). Found by a verification pass before anything is applied;
    /// see [`super::restore_script`].
    pub const CORRUPT_DUMP: i64 = 7;
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

    #[error(
        "{operation} job {workload_id} of database {service_id} could not be stopped: {reason}"
    )]
    NotStopped {
        service_id: i32,
        operation: &'static str,
        workload_id: i64,
        reason: String,
    },

    #[error("listing the running {operation} jobs of database {service_id} failed: {source}")]
    ListJobs {
        service_id: i32,
        operation: &'static str,
        #[source]
        source: Box<PlaneError>,
    },
}

impl DatabaseJobError {
    /// Whether retrying the same job cannot help.
    pub fn is_permanent(&self) -> bool {
        match self {
            DatabaseJobError::PlaneUnavailable { .. }
            | DatabaseJobError::EndpointUnreachable { .. } => true,
            // A missing tool, or an object that is corrupt in the bucket,
            // fails the same way every time.
            DatabaseJobError::Failed { exit_code, .. } => {
                matches!(*exit_code, Some(exit::NO_WALG | exit::CORRUPT_DUMP))
            }
            // Admission refusals are permanent; a store or scheduling error
            // may pass.
            DatabaseJobError::NotStarted { source, .. } => {
                matches!(**source, PlaneError::Admission(_))
            }
            DatabaseJobError::Cancelled { .. }
            | DatabaseJobError::NotStopped { .. }
            | DatabaseJobError::ListJobs { .. } => false,
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
///
/// POSIX `sh` has no `pipefail`, and a pipeline's status is only its last
/// command's: `psql` happily applies empty or truncated input and exits 0.
/// So the exit status of `wal-g st cat` and of `gunzip` are each carried out
/// of the pipeline in a file (removed first, so a stage killed before it
/// wrote one reads as a failure, never as a stale success), and every stage
/// is checked.
///
/// The object is streamed twice. The first pass only verifies it
/// (`gunzip -t` checks the gzip CRC and length), so a corrupt or truncated
/// object that S3 serves successfully is refused before `psql` touches the
/// database: a `--clean` dump drops every object first, and half of one
/// would leave the database emptier than before. Applying it cannot be made
/// atomic instead: `pg_dumpall` output runs `CREATE DATABASE` and
/// `\connect`, which cannot run inside `--single-transaction`, and the local
/// restore deliberately runs without `ON_ERROR_STOP` (TimescaleDB rejects
/// the dump's `DROP ... ONLY` on hypertables, harmlessly), so this one
/// matches it. The second pass checks every stage again, in case the object
/// changed in between. Memory stays constant: both passes stream.
///
/// A `gunzip` failure whose fetch either succeeded or was killed by SIGPIPE
/// (status 141: gunzip stopped reading first) is a corrupt object
/// ([`exit::CORRUPT_DUMP`]); any other fetch failure is a download failure.
pub fn restore_script(target: &PgJobTarget, object_key: &str) -> String {
    format!(
        "{REQUIRE_WALG}\n\
         fetch_rc=\"${{TMPDIR:-/tmp}}/temps-restore-fetch.rc\"\n\
         unzip_rc=\"${{TMPDIR:-/tmp}}/temps-restore-gunzip.rc\"\n\
         stream() {{ rm -f \"$fetch_rc\" \"$unzip_rc\"; \
         {{ wal-g st cat {key}; echo $? > \"$fetch_rc\"; }} \
         | {{ gunzip \"$@\"; echo $? > \"$unzip_rc\"; }}; }}\n\
         check() {{ fetch=$(cat \"$fetch_rc\" 2>/dev/null); unzip=$(cat \"$unzip_rc\" 2>/dev/null); \
         if [ \"$unzip\" != 0 ] && {{ [ \"$fetch\" = 0 ] || [ \"$fetch\" = 141 ]; }}; then \
         echo \"temps: the dump is corrupt or truncated: gunzip exited ${{unzip:-without a status}}$1\" >&2; \
         exit 7; fi; \
         [ \"$fetch\" = 0 ] || {{ echo \"temps: downloading the dump failed: wal-g exited \
         ${{fetch:-without a status}}$1\" >&2; exit 4; }}; \
         [ \"$unzip\" = 0 ] || {{ echo \"temps: decompressing the dump failed: gunzip exited \
         ${{unzip:-without a status}}$1\" >&2; exit 7; }}; }}\n\
         stream -t\n\
         check '; nothing was applied'\n\
         stream | psql -X -q --no-password --host={host} --port={port} --username={user} \
         --dbname=postgres >/dev/null\n\
         psql_rc=$?\n\
         check '; psql may have applied part of it'\n\
         [ \"$psql_rc\" = 0 ] || {{ echo \"temps: psql could not apply the dump: psql exited \
         $psql_rc\" >&2; exit 5; }}\n\
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

/// A database job the plane is running: what [`start_database_job`]
/// returns and [`DatabaseJob::wait`] waits on.
///
/// A backup job is retired when this is dropped before it finished. The
/// backup executor drops the engine's future when its own cancel branch or
/// wall-clock timeout wins; without the guard the job would keep dumping for
/// up to [`JOB_TIMEOUT_SECS`] while the backup is already failed and a retry
/// starts another one. A restore job is not: past its safe point nothing may
/// interrupt the apply (see `run_job_restore`), and stopping `psql` halfway
/// would leave the database worse off than letting it finish.
#[must_use = "dropping a backup job retires it"]
pub struct DatabaseJob {
    plane: Arc<WorkloadPlane>,
    target: PgJobTarget,
    operation: &'static str,
    workload_id: i64,
    /// Held until the job finishes: its instance reads the values once,
    /// when it is created.
    staged: StagedEnvGuard,
    retire: Option<RetireOnDrop>,
}

impl std::fmt::Debug for DatabaseJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseJob")
            .field("service_id", &self.target.service_id)
            .field("operation", &self.operation)
            .field("workload_id", &self.workload_id)
            .field("retire_on_drop", &self.retire.is_some())
            .finish_non_exhaustive()
    }
}

/// Run one database job to completion through `plane`: stage `env`, apply,
/// wait for it to finish, and stop it if `cancel` fires first. A backup job
/// is also stopped when this future is dropped (see [`DatabaseJob`]).
pub async fn run_database_job(
    plane: &Arc<WorkloadPlane>,
    target: &PgJobTarget,
    purpose: JobPurpose,
    script: String,
    env: Vec<(String, String)>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<i64, DatabaseJobError> {
    start_database_job(plane, target, purpose, script, env)
        .await?
        .wait(cancel)
        .await
}

/// Stage `env` and apply one database job through `plane`.
pub async fn start_database_job(
    plane: &Arc<WorkloadPlane>,
    target: &PgJobTarget,
    purpose: JobPurpose,
    script: String,
    env: Vec<(String, String)>,
) -> Result<DatabaseJob, DatabaseJobError> {
    let operation = purpose.operation();
    let service_id = target.service_id;
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("{operation}-{}-{}", purpose.id(), &attempt[..12]);
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
    let retire = match purpose {
        JobPurpose::Backup { backup_id } => Some(plane.retire_on_drop(
            workload_id,
            "Abandoned",
            format!(
                "backup {backup_id} of database {service_id} stopped waiting for its job \
                 (cancelled, or past the backup's time limit)"
            ),
        )),
        JobPurpose::Restore { .. } => None,
    };
    Ok(DatabaseJob {
        plane: Arc::clone(plane),
        target: target.clone(),
        operation,
        workload_id,
        staged,
        retire,
    })
}

impl DatabaseJob {
    pub fn workload_id(&self) -> i64 {
        self.workload_id
    }

    /// Wait for the job to finish, and stop it if `cancel` fires first.
    pub async fn wait(
        self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<i64, DatabaseJobError> {
        let DatabaseJob {
            plane,
            target,
            operation,
            workload_id,
            staged,
            mut retire,
        } = self;
        let service_id = target.service_id;
        let wait = plane.wait_for(
            workload_id,
            ConditionKind::Completed,
            Duration::from_secs(u64::from(JOB_TIMEOUT_SECS)),
        );
        let outcome = tokio::select! {
            outcome = wait => outcome,
            _ = cancel.cancelled() => {
                let message = format!("the {operation} was cancelled");
                let stopped = match retire.take() {
                    Some(guard) => guard.retire(&message).await,
                    None => plane.delete(workload_id, "Cancelled", &message).await,
                };
                if let Err(error) = stopped {
                    warn!(service_id, workload_id, "stopping a cancelled database job failed: {error}");
                }
                return Err(DatabaseJobError::Cancelled { service_id, operation, workload_id });
            }
        };
        drop(staged);
        match outcome {
            Ok(view) if view.status.phase == WorkloadPhase::Succeeded => {
                if let Some(guard) = retire.take() {
                    guard.disarm();
                }
                info!(
                    service_id,
                    workload_id, "database {operation} job succeeded"
                );
                Ok(workload_id)
            }
            Ok(view) => {
                // Finished: kept until its TTL, so its output can be read.
                if let Some(guard) = retire.take() {
                    guard.disarm();
                }
                Err(failed(&target, operation, workload_id, &view.status, None))
            }
            Err(error) => {
                let status = plane.get(workload_id).await.ok().map(|view| view.status);
                let finished = status
                    .as_ref()
                    .is_some_and(|status| status.phase.is_terminal());
                if let Some(guard) = retire.take() {
                    if finished {
                        guard.disarm();
                    } else if let Err(stop) = guard
                        .retire(&format!("waiting for the {operation} job failed: {error}"))
                        .await
                    {
                        warn!(
                            service_id,
                            workload_id, "stopping a database job nobody waits on failed: {stop}"
                        );
                    }
                }
                Err(match status {
                    Some(status) => failed(
                        &target,
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
}

/// Where a database job stands, as far as stopping it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// The plane has removed it, instance and row.
    Gone,
    /// It finished on its own: its process has exited, and its row is kept
    /// for inspection until its TTL.
    Finished,
    /// Deleted; the reconciler has not removed its instance yet.
    Removing,
    /// Still running, or not started yet.
    Running,
}

/// The slice of the workload plane that stopping a database job needs, so
/// the cleanup below can be tested without a plane.
#[async_trait]
pub trait DatabaseJobControl: Send + Sync {
    async fn job_state(&self, workload_id: i64) -> Result<JobState, PlaneError>;
    /// Delete the job: the plane stops and removes its instance.
    async fn stop_job(
        &self,
        workload_id: i64,
        reason: &str,
        message: &str,
    ) -> Result<(), PlaneError>;
    /// Live, unfinished jobs of `owner` labelled `label = value`.
    async fn live_jobs(
        &self,
        owner: &Owner,
        label: &str,
        value: &str,
    ) -> Result<Vec<i64>, PlaneError>;
}

#[async_trait]
impl DatabaseJobControl for WorkloadPlane {
    async fn job_state(&self, workload_id: i64) -> Result<JobState, PlaneError> {
        match self.get(workload_id).await {
            Err(PlaneError::NotFound { .. }) => Ok(JobState::Gone),
            Err(error) => Err(error),
            Ok(view) if view.deleted_at.is_some() => Ok(JobState::Removing),
            Ok(view) if view.status.phase.is_terminal() => Ok(JobState::Finished),
            Ok(_) => Ok(JobState::Running),
        }
    }

    async fn stop_job(
        &self,
        workload_id: i64,
        reason: &str,
        message: &str,
    ) -> Result<(), PlaneError> {
        self.delete(workload_id, reason, message).await
    }

    async fn live_jobs(
        &self,
        owner: &Owner,
        label: &str,
        value: &str,
    ) -> Result<Vec<i64>, PlaneError> {
        Ok(self
            .list_active(owner)
            .await?
            .into_iter()
            .filter(|view| view.spec.labels.get(label).map(String::as_str) == Some(value))
            .map(|view| view.id)
            .collect())
    }
}

/// How long to wait for a stopped job to be removed, and how often to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovalTiming {
    pub timeout: Duration,
    pub poll: Duration,
}

impl Default for RemovalTiming {
    fn default() -> Self {
        // A container stop is a 10s grace period; a worker adds a round
        // trip, and the reconciler its pass.
        Self {
            timeout: Duration::from_secs(120),
            poll: Duration::from_secs(1),
        }
    }
}

/// Stop job `workload_id` if it is still running and wait until the plane
/// has removed it. A job that finished on its own is left for inspection
/// (its process has exited); one that is gone needs nothing.
pub async fn stop_and_await_removal(
    jobs: &dyn DatabaseJobControl,
    service_id: i32,
    operation: &'static str,
    workload_id: i64,
    message: &str,
    timing: RemovalTiming,
) -> Result<(), DatabaseJobError> {
    let not_stopped = |reason: String| DatabaseJobError::NotStopped {
        service_id,
        operation,
        workload_id,
        reason,
    };
    let started = tokio::time::Instant::now();
    loop {
        match jobs
            .job_state(workload_id)
            .await
            .map_err(|e| not_stopped(format!("reading its state failed: {e}")))?
        {
            JobState::Gone | JobState::Finished => return Ok(()),
            JobState::Running => match jobs.stop_job(workload_id, "Abandoned", message).await {
                Ok(()) | Err(PlaneError::NotFound { .. }) => {}
                Err(error) => return Err(not_stopped(format!("deleting it failed: {error}"))),
            },
            JobState::Removing => {}
        }
        if started.elapsed() >= timing.timeout {
            return Err(not_stopped(format!(
                "the plane had not removed it after {}s",
                timing.timeout.as_secs()
            )));
        }
        tokio::time::sleep(timing.poll).await;
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
        Some(exit::CORRUPT_DUMP) => Some(
            "the dump in S3 is corrupt or truncated (gunzip could not decompress it); \
             restore from another backup",
        ),
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

/// An object a failed backup attempt may have left half-written.
#[async_trait]
pub trait PartialObject: Send + Sync {
    /// Remove it; failures are logged, never returned (a later attempt
    /// overwrites the key anyway).
    async fn remove(&self);
}

/// The backup's dump object in S3.
struct S3PartialObject {
    backup_id: i32,
    client: aws_sdk_s3::Client,
    bucket: String,
    key: String,
}

#[async_trait]
impl PartialObject for S3PartialObject {
    async fn remove(&self) {
        if let Err(error) = self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(&self.key)
            .send()
            .await
        {
            warn!(
                backup_id = self.backup_id,
                s3_key = %self.key,
                "removing the partial object of a failed backup job failed: {}",
                v2_common::describe_sdk_error("delete_object", &error)
            );
        }
    }
}

/// No object to remove: [`BackupAttempt::stop_jobs`] only.
struct NoPartialObject;

#[async_trait]
impl PartialObject for NoPartialObject {
    async fn remove(&self) {}
}

/// What one backup attempt started, so it can be stopped and cleaned up.
pub struct BackupAttempt {
    pub backup_id: i32,
    pub service_id: i32,
    jobs: Arc<dyn DatabaseJobControl>,
    object: Arc<dyn PartialObject>,
    /// The attempt's job, once applied.
    workload_id: Option<i64>,
    timing: RemovalTiming,
}

impl BackupAttempt {
    pub fn new(
        backup_id: i32,
        service_id: i32,
        jobs: Arc<dyn DatabaseJobControl>,
        object: Arc<dyn PartialObject>,
        timing: RemovalTiming,
    ) -> Self {
        Self {
            backup_id,
            service_id,
            jobs,
            object,
            workload_id: None,
            timing,
        }
    }

    /// Stop every job of this backup that may still be running, the
    /// attempt's own and any live one labelled with this backup (an earlier
    /// attempt, or one whose future was dropped while it was being applied),
    /// and wait until the plane has removed them.
    pub async fn stop_jobs(&self) -> Result<(), DatabaseJobError> {
        let owner = Owner::new(DATABASE_OWNER_KIND, i64::from(self.service_id));
        let mut ids = self
            .jobs
            .live_jobs(&owner, "sh.temps.backup_id", &self.backup_id.to_string())
            .await
            .map_err(|source| DatabaseJobError::ListJobs {
                service_id: self.service_id,
                operation: "backup",
                source: Box::new(source),
            })?;
        ids.extend(self.workload_id);
        ids.sort_unstable();
        ids.dedup();
        let message = format!(
            "backup {} of database {} is being retried or was abandoned; this job no longer \
             belongs to a running attempt",
            self.backup_id, self.service_id
        );
        for workload_id in ids {
            stop_and_await_removal(
                self.jobs.as_ref(),
                self.service_id,
                "backup",
                workload_id,
                &message,
                self.timing,
            )
            .await?;
        }
        Ok(())
    }

    /// Stop the attempt's jobs, then remove what they may have uploaded.
    /// The object goes only once no job can still be writing it.
    pub async fn reap(&self) -> Result<(), DatabaseJobError> {
        self.stop_jobs().await?;
        self.object.remove().await;
        Ok(())
    }
}

/// Cleanups of backup attempts whose future was dropped (the executor's
/// cancel or timeout won), by backup id. The next attempt and the executor's
/// `cleanup` wait for them, so a retry never overlaps a job that is still
/// dumping into the same object. Finished entries are pruned on every
/// insert, so the map holds only cleanups in flight plus a few just done.
#[derive(Default)]
pub struct AbandonedAttempts(std::sync::Mutex<HashMap<i32, Vec<ReapHandle>>>);

/// The spawned cleanup of one abandoned attempt.
type ReapHandle = JoinHandle<Result<(), DatabaseJobError>>;

impl AbandonedAttempts {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i32, Vec<ReapHandle>>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn insert(&self, backup_id: i32, handle: ReapHandle) {
        let mut map = self.lock();
        map.retain(|_, handles| {
            handles.retain(|handle| !handle.is_finished());
            !handles.is_empty()
        });
        map.entry(backup_id).or_default().push(handle);
    }

    /// Whether a cleanup of `backup_id` is recorded (finished or not).
    pub fn is_pending(&self, backup_id: i32) -> bool {
        self.lock().contains_key(&backup_id)
    }

    /// Wait (up to `timeout`) for every recorded cleanup of `backup_id`.
    /// A cleanup still running at the deadline stays recorded, and is an
    /// error: starting another job now could overlap it.
    pub async fn settle(&self, backup_id: i32, timeout: Duration) -> Result<(), BackupError> {
        let handles = self.lock().remove(&backup_id).unwrap_or_default();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut handles = handles.into_iter();
        let mut first_error = None;
        while let Some(mut handle) = handles.next() {
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    first_error.get_or_insert(BackupError::Failed {
                        reason: format!(
                            "backup {backup_id}: cleaning up an abandoned attempt failed: {error}"
                        ),
                    });
                }
                Ok(Err(join)) => {
                    first_error.get_or_insert(BackupError::Failed {
                        reason: format!(
                            "backup {backup_id}: the cleanup of an abandoned attempt did not \
                             finish: {join}"
                        ),
                    });
                }
                Err(_) => {
                    let mut map = self.lock();
                    let pending = map.entry(backup_id).or_default();
                    pending.push(handle);
                    pending.extend(handles);
                    return Err(BackupError::Failed {
                        reason: format!(
                            "backup {backup_id}: the job of an abandoned attempt was still being \
                             stopped after {}s; not starting another one next to it",
                            timeout.as_secs()
                        ),
                    });
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Reaps a backup attempt if the engine's future is dropped before the
/// attempt concluded: the cleanup is spawned on the runtime the guard was
/// created on and recorded in [`AbandonedAttempts`].
#[must_use = "dropping the guard abandons the attempt"]
pub struct AttemptGuard {
    attempt: Option<BackupAttempt>,
    abandoned: Arc<AbandonedAttempts>,
    runtime: Option<tokio::runtime::Handle>,
}

impl AttemptGuard {
    pub fn new(attempt: BackupAttempt, abandoned: Arc<AbandonedAttempts>) -> Self {
        Self {
            attempt: Some(attempt),
            abandoned,
            runtime: tokio::runtime::Handle::try_current().ok(),
        }
    }

    /// The attempt's job was applied as workload `workload_id`.
    pub fn job_started(&mut self, workload_id: i64) {
        if let Some(attempt) = self.attempt.as_mut() {
            attempt.workload_id = Some(workload_id);
        }
    }

    /// The attempt concluded: its job finished and its object is the
    /// backup. Nothing to clean up.
    pub fn disarm(mut self) {
        self.attempt = None;
    }

    /// Clean the attempt up now. On failure the guard stays armed, so
    /// dropping it records another try for the next attempt to wait on.
    pub async fn reap_now(&mut self) -> Result<(), DatabaseJobError> {
        if let Some(attempt) = self.attempt.as_ref() {
            attempt.reap().await?;
        }
        self.attempt = None;
        Ok(())
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        let Some(attempt) = self.attempt.take() else {
            return;
        };
        let backup_id = attempt.backup_id;
        let workload_id = attempt.workload_id;
        let Some(runtime) = self.runtime.as_ref() else {
            error!(
                backup_id,
                ?workload_id,
                "backup {backup_id} was abandoned outside a runtime; its job and partial object \
                 are left for the next attempt"
            );
            return;
        };
        warn!(
            backup_id,
            ?workload_id,
            "backup {backup_id} was abandoned before its job concluded; stopping the job and \
             removing its partial object"
        );
        let handle = runtime.spawn(async move {
            let result = attempt.reap().await;
            match &result {
                Ok(()) => info!(
                    backup_id,
                    ?workload_id,
                    "the abandoned attempt of backup {backup_id} was cleaned up"
                ),
                Err(error) => error!(
                    backup_id,
                    ?workload_id,
                    "cleaning up the abandoned attempt of backup {backup_id} failed: {error}"
                ),
            }
            result
        });
        self.abandoned.insert(backup_id, handle);
    }
}

pub struct PostgresWorkloadJobDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// The workload plane, bound once `temps serve` starts it.
    pub plane: Arc<PlaneSlot>,
}

/// `postgres_workload_job`: a backup of a database on a worker node, as a
/// job on that node.
///
/// ## Attempts never overlap
///
/// The executor retries a failed backup 30s later, into the same object key
/// (`--force`). A retry must not run next to a job of an earlier attempt
/// that is still dumping. So each attempt holds an [`AttemptGuard`]: when the
/// executor drops the attempt's future (its cancel or wall-clock timeout
/// won), the guard stops the job, waits until the plane removed it, and only
/// then removes the partial object. [`BackupEngine::cleanup`] (which the
/// executor runs before a retry) and the start of every attempt wait for
/// that, and also stop any live job still labelled with the backup. An
/// attempt that cannot confirm the earlier job is gone fails (transiently)
/// rather than start a second one.
pub struct PostgresWorkloadJobEngine {
    deps: Arc<PostgresWorkloadJobDeps>,
    abandoned: Arc<AbandonedAttempts>,
    timing: RemovalTiming,
}

impl PostgresWorkloadJobEngine {
    pub fn new(deps: PostgresWorkloadJobDeps) -> Self {
        Self {
            deps: Arc::new(deps),
            abandoned: Arc::new(AbandonedAttempts::default()),
            timing: RemovalTiming::default(),
        }
    }

    /// How long to wait for the cleanup of an abandoned attempt: its own
    /// removal wait, plus slack for the object delete.
    fn settle_timeout(&self) -> Duration {
        self.timing.timeout + Duration::from_secs(30)
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
        // An earlier attempt whose future was dropped may still be stopping
        // its job: never start another one next to it.
        self.abandoned
            .settle(backup_id, self.settle_timeout())
            .await?;

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

        let attempt = BackupAttempt::new(
            backup_id,
            service_id,
            plane.clone(),
            Arc::new(S3PartialObject {
                backup_id,
                client: s3_client.clone(),
                bucket: s3_source.bucket_name.clone(),
                key: s3_key.clone(),
            }),
            self.timing,
        );
        // A job of this backup still live (an attempt dropped while its job
        // was being applied, or one from before a restart) writes the same
        // key: stop it first.
        attempt.stop_jobs().await?;
        let mut guard = AttemptGuard::new(attempt, Arc::clone(&self.abandoned));

        let script = backup_script(&target, &s3_key);
        let result = match start_database_job(
            &plane,
            &target,
            JobPurpose::Backup { backup_id },
            script,
            job_env(&password, &s3),
        )
        .await
        {
            Ok(job) => {
                guard.job_started(job.workload_id());
                job.wait(&ctx.cancel).await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(_) => guard.disarm(),
            Err(error) => {
                // A failed dump may have left a partial object behind, and a
                // cancelled job may still be stopping.
                if let Err(cleanup) = guard.reap_now().await {
                    warn!(
                        backup_id,
                        s3_key = %s3_key,
                        "cleaning up the failed backup job failed; the next attempt waits for \
                         it: {cleanup}"
                    );
                }
                return Err(error.into());
            }
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

    /// Run by the executor after a failed or timed-out attempt (and before
    /// its retry): wait until a dropped attempt's job is stopped and its
    /// partial object removed, then stop any job still live for the backup.
    async fn cleanup(&self, ctx: &BackupContext) -> Result<(), BackupError> {
        let backup_id = ctx.backup_id;
        let settled = self
            .abandoned
            .settle(backup_id, self.settle_timeout())
            .await;
        // Without a plane no job of this process can be running.
        let swept = match (
            v2_common::require_i32_param(&ctx.params, "service_id"),
            self.deps.plane.get(),
        ) {
            (Ok(service_id), Some(plane)) => {
                // No object to remove here: a job stopped by this sweep was
                // never this process's attempt (its guard would have
                // recorded it), and the retry overwrites the key.
                BackupAttempt::new(
                    backup_id,
                    service_id,
                    plane,
                    Arc::new(NoPartialObject),
                    self.timing,
                )
                .stop_jobs()
                .await
                .map_err(BackupError::from)
            }
            _ => Ok(()),
        };
        settled.and(swept)
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
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
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
        assert!(script.contains("{ gunzip \"$@\"; echo $?"), "{script}");
        // Verified before anything is applied, then applied.
        assert!(script.contains("stream -t\n"), "{script}");
        assert!(
            script.contains("stream | psql -X -q --no-password"),
            "{script}"
        );
        assert!(script.contains("--dbname=postgres"), "{script}");
        assert!(script.contains("exit 4") && script.contains("exit 5"));
        assert!(script.contains("exit 7"), "{script}");
        assert!(!script.contains("pipefail"), "POSIX sh has none: {script}");
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

    // ── The restore script, run under `sh` with stub tools ──────────────

    /// A directory of executables put first on `PATH`.
    #[cfg(unix)]
    struct Stubs {
        dir: tempfile::TempDir,
    }

    #[cfg(unix)]
    impl Stubs {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().expect("stub dir"),
            }
        }

        fn add(&self, name: &str, body: &str) {
            use std::os::unix::fs::PermissionsExt;
            let path = self.dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("writing a stub");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("making a stub executable");
        }

        /// `wal-g st cat` prints `$STUB_OBJECT` and exits `$STUB_FETCH_RC`;
        /// `psql` records its input in `$STUB_PSQL_INPUT` and exits
        /// `$STUB_PSQL_RC`.
        fn with_walg_and_psql() -> Self {
            let stubs = Self::new();
            stubs.add(
                "wal-g",
                "[ \"$1 $2\" = 'st cat' ] || exit 99\ncat \"$STUB_OBJECT\"\nexit \"${STUB_FETCH_RC:-0}\"",
            );
            stubs.add(
                "psql",
                "cat > \"$STUB_PSQL_INPUT\"\nexit \"${STUB_PSQL_RC:-0}\"",
            );
            stubs
        }

        /// Run `script` under `sh`; its exit code, stderr, and what `psql`
        /// was given (`None`: it never ran).
        fn run(&self, script: &str, env: &[(&str, &str)]) -> (i32, String, Option<Vec<u8>>) {
            let work = tempfile::tempdir().expect("work dir");
            let psql_input = work.path().join("psql-input");
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .env_clear()
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", self.dir.path().display()),
                )
                // The script's status files go here, never in a shared /tmp.
                .env("TMPDIR", work.path())
                .env("STUB_PSQL_INPUT", &psql_input)
                .envs(env.iter().copied())
                .output()
                .expect("running sh");
            (
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).into_owned(),
                std::fs::read(&psql_input).ok(),
            )
        }
    }

    #[cfg(unix)]
    fn gzip(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data).expect("gzip");
        encoder.finish().expect("gzip")
    }

    /// With a stub `gunzip` whose verify (`-t`) and apply passes exit with
    /// the given codes: every failing stage fails the job with its own code,
    /// and only a clean stream reaches `psql` with success.
    #[cfg(unix)]
    #[test]
    fn the_restore_script_fails_when_any_stage_of_the_stream_fails() {
        let stubs = Stubs::with_walg_and_psql();
        stubs.add(
            "gunzip",
            "if [ \"$1\" = -t ]; then cat >/dev/null; exit \"${STUB_GUNZIP_TEST_RC:-0}\"; fi\n\
             cat\nexit \"${STUB_GUNZIP_RC:-0}\"",
        );
        let object = tempfile::NamedTempFile::new().expect("object");
        std::fs::write(object.path(), b"SELECT 1;\n").expect("writing the object");
        let object_path = object.path().to_str().expect("utf-8 path");
        let script = restore_script(&target(), "p/dump.sql.gz");
        let run = |env: &[(&str, &str)]| {
            let mut all = vec![("STUB_OBJECT", object_path)];
            all.extend_from_slice(env);
            stubs.run(&script, &all)
        };

        // Happy path.
        let (code, stderr, applied) = run(&[]);
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(applied.as_deref(), Some(&b"SELECT 1;\n"[..]));

        // A corrupt object: refused by the verify pass, psql never runs.
        let (code, stderr, applied) = run(&[("STUB_GUNZIP_TEST_RC", "1")]);
        assert_eq!(code, exit::CORRUPT_DUMP as i32, "{stderr}");
        assert!(stderr.contains("corrupt or truncated"), "{stderr}");
        assert!(stderr.contains("nothing was applied"), "{stderr}");
        assert_eq!(applied, None, "psql ran on a corrupt dump");

        // gunzip stopped reading first, so the fetch died of SIGPIPE: still
        // the object's fault, not the download's.
        let (code, stderr, _) = run(&[("STUB_GUNZIP_TEST_RC", "1"), ("STUB_FETCH_RC", "141")]);
        assert_eq!(code, exit::CORRUPT_DUMP as i32, "{stderr}");

        // Decompression fails while applying (the object changed between
        // the passes): psql exits 0 on its partial input, the job does not.
        let (code, stderr, applied) = run(&[("STUB_GUNZIP_RC", "1")]);
        assert_eq!(code, exit::CORRUPT_DUMP as i32, "{stderr}");
        assert!(
            stderr.contains("psql may have applied part of it"),
            "{stderr}"
        );
        assert!(applied.is_some());

        // The download fails.
        let (code, stderr, applied) = run(&[("STUB_FETCH_RC", "1")]);
        assert_eq!(code, exit::DOWNLOAD_FAILED as i32, "{stderr}");
        assert!(stderr.contains("wal-g exited 1"), "{stderr}");
        assert_eq!(applied, None);

        // psql fails.
        let (code, stderr, _) = run(&[("STUB_PSQL_RC", "2")]);
        assert_eq!(code, exit::APPLY_FAILED as i32, "{stderr}");
        assert!(stderr.contains("psql exited 2"), "{stderr}");

        // An image without wal-g.
        let bare = Stubs::new();
        let (code, _, _) = bare.run(&script, &[]);
        // `wal-g` may exist on the host running the test; only check the
        // message when it does not.
        if !std::path::Path::new("/usr/bin/wal-g").exists() {
            assert_eq!(code, exit::NO_WALG as i32);
        }
    }

    /// The reported failure itself, with the host's real `gunzip`: S3 serves
    /// a truncated `.sql.gz` successfully, and the restore must fail before
    /// `psql` sees any of it.
    #[cfg(unix)]
    #[test]
    fn a_truncated_dump_fails_the_restore_with_the_real_gunzip() {
        let gunzip = ["/usr/bin/gunzip", "/bin/gunzip"]
            .into_iter()
            .find(|path| std::path::Path::new(path).exists());
        if gunzip.is_none() {
            println!("no gunzip on this host, skipping");
            return;
        }
        let stubs = Stubs::with_walg_and_psql();
        let script = restore_script(&target(), "p/dump.sql.gz");
        let sql = "CREATE TABLE proof(x int);\n".repeat(2_000);
        let whole = gzip(sql.as_bytes());

        let object = tempfile::NamedTempFile::new().expect("object");
        std::fs::write(object.path(), &whole).expect("writing the object");
        let object_path = object.path().to_str().expect("utf-8 path");
        let (code, stderr, applied) = stubs.run(&script, &[("STUB_OBJECT", object_path)]);
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(applied.as_deref(), Some(sql.as_bytes()));

        std::fs::write(object.path(), &whole[..whole.len() / 2]).expect("truncating");
        let (code, stderr, applied) = stubs.run(&script, &[("STUB_OBJECT", object_path)]);
        assert_eq!(code, exit::CORRUPT_DUMP as i32, "{stderr}");
        assert_eq!(applied, None, "psql ran on a truncated dump");

        let mut corrupt = whole.clone();
        let middle = corrupt.len() / 2;
        corrupt[middle] ^= 0xff;
        std::fs::write(object.path(), &corrupt).expect("corrupting");
        let (code, stderr, applied) = stubs.run(&script, &[("STUB_OBJECT", object_path)]);
        assert_eq!(code, exit::CORRUPT_DUMP as i32, "{stderr}");
        assert_eq!(applied, None, "psql ran on a corrupt dump");
    }

    #[test]
    fn a_corrupt_dump_names_its_step_and_is_permanent() {
        let status = temps_workload::WorkloadStatus {
            phase: WorkloadPhase::Failed,
            exit_code: Some(exit::CORRUPT_DUMP),
            ..Default::default()
        };
        let error = failed(&target(), "restore", 12, &status, None);
        assert!(
            error.to_string().contains("corrupt or truncated"),
            "{error}"
        );
        assert!(error.is_permanent());
    }

    // ── Abandoned backup attempts ─────────────────────────────────────────

    /// The plane, as far as stopping jobs goes. A deleted job takes
    /// `removal_polls` looks before it is gone.
    #[derive(Default)]
    struct FakeJobs {
        states: std::sync::Mutex<HashMap<i64, (JobState, u32)>>,
        live: Vec<i64>,
        removal_polls: u32,
        fail_stop: bool,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeJobs {
        fn with(jobs: &[(i64, JobState)], log: Arc<std::sync::Mutex<Vec<String>>>) -> Self {
            Self {
                states: std::sync::Mutex::new(
                    jobs.iter().map(|(id, state)| (*id, (*state, 0))).collect(),
                ),
                removal_polls: 2,
                log,
                ..Default::default()
            }
        }

        fn record(&self, entry: String) {
            self.log.lock().expect("log").push(entry);
        }
    }

    #[async_trait]
    impl DatabaseJobControl for FakeJobs {
        async fn job_state(&self, workload_id: i64) -> Result<JobState, PlaneError> {
            let mut states = self.states.lock().expect("states");
            let Some((state, polls)) = states.get_mut(&workload_id) else {
                return Ok(JobState::Gone);
            };
            if *state == JobState::Removing {
                *polls += 1;
                if *polls > self.removal_polls {
                    *state = JobState::Gone;
                    drop(states);
                    self.record(format!("removed {workload_id}"));
                    return Ok(JobState::Gone);
                }
            }
            Ok(*state)
        }

        async fn stop_job(&self, workload_id: i64, _: &str, _: &str) -> Result<(), PlaneError> {
            self.record(format!("stop {workload_id}"));
            if self.fail_stop {
                return Err(PlaneError::Corrupt {
                    workload_id,
                    field: "status",
                    reason: "injected".into(),
                });
            }
            let mut states = self.states.lock().expect("states");
            match states.get_mut(&workload_id) {
                Some((state, _)) => {
                    *state = JobState::Removing;
                    Ok(())
                }
                None => Err(PlaneError::NotFound { workload_id }),
            }
        }

        async fn live_jobs(
            &self,
            owner: &Owner,
            label: &str,
            _: &str,
        ) -> Result<Vec<i64>, PlaneError> {
            assert_eq!(owner.kind, DATABASE_OWNER_KIND);
            assert_eq!(label, "sh.temps.backup_id");
            Ok(self.live.clone())
        }
    }

    struct FakeObject(Arc<std::sync::Mutex<Vec<String>>>);

    #[async_trait]
    impl PartialObject for FakeObject {
        async fn remove(&self) {
            self.0.lock().expect("log").push("object removed".into());
        }
    }

    const FAST: RemovalTiming = RemovalTiming {
        timeout: Duration::from_secs(5),
        poll: Duration::from_millis(1),
    };

    fn attempt(jobs: Arc<FakeJobs>, log: &Arc<std::sync::Mutex<Vec<String>>>) -> BackupAttempt {
        BackupAttempt::new(41, 7, jobs, Arc::new(FakeObject(log.clone())), FAST)
    }

    fn entries(log: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().expect("log").clone()
    }

    /// The executor's timeout (or cancel branch) drops the engine's future
    /// while the job runs: the job is stopped, and only once the plane has
    /// removed it does the partial object go. The retry waits for that.
    #[tokio::test]
    async fn dropping_an_attempt_after_its_job_started_stops_the_job_then_removes_the_object() {
        let log = Arc::default();
        let jobs = Arc::new(FakeJobs::with(
            &[(500, JobState::Running)],
            Arc::clone(&log),
        ));
        let abandoned = Arc::new(AbandonedAttempts::default());

        let engine_future = {
            let abandoned = Arc::clone(&abandoned);
            let attempt = attempt(Arc::clone(&jobs), &log);
            async move {
                let mut guard = AttemptGuard::new(attempt, abandoned);
                guard.job_started(500);
                // The job's wait: never returns before the executor gives up.
                std::future::pending::<()>().await;
                guard.disarm();
            }
        };
        let timed_out = tokio::time::timeout(Duration::from_millis(20), engine_future).await;
        assert!(timed_out.is_err(), "the executor's timeout won");
        assert!(
            abandoned.is_pending(41),
            "the cleanup is recorded for the retry"
        );

        abandoned
            .settle(41, Duration::from_secs(5))
            .await
            .expect("the abandoned attempt is cleaned up");
        assert_eq!(
            entries(&log),
            vec!["stop 500", "removed 500", "object removed"],
            "the object goes only after the job"
        );
        assert!(!abandoned.is_pending(41));
    }

    #[tokio::test]
    async fn a_concluded_attempt_leaves_its_job_and_object() {
        let log = Arc::default();
        let jobs = Arc::new(FakeJobs::with(
            &[(500, JobState::Finished)],
            Arc::clone(&log),
        ));
        let abandoned = Arc::new(AbandonedAttempts::default());
        let mut guard = AttemptGuard::new(attempt(jobs, &log), Arc::clone(&abandoned));
        guard.job_started(500);
        guard.disarm();
        assert!(!abandoned.is_pending(41));
        assert!(entries(&log).is_empty());
    }

    /// A job that finished on its own is kept for its TTL, so its output
    /// can be read; a live job of the same backup is stopped first.
    #[tokio::test]
    async fn reaping_keeps_a_finished_job_and_stops_a_live_one_of_the_backup() {
        let log = Arc::default();
        let jobs = Arc::new(FakeJobs {
            live: vec![77],
            ..FakeJobs::with(
                &[(500, JobState::Finished), (77, JobState::Running)],
                Arc::clone(&log),
            )
        });
        let abandoned = Arc::new(AbandonedAttempts::default());
        let mut guard = AttemptGuard::new(attempt(jobs, &log), Arc::clone(&abandoned));
        guard.job_started(500);
        guard.reap_now().await.expect("reaped");
        drop(guard);
        assert_eq!(
            entries(&log),
            vec!["stop 77", "removed 77", "object removed"]
        );
        assert!(!abandoned.is_pending(41), "nothing left for the retry");
    }

    /// A job that cannot be stopped keeps the attempt armed, the object in
    /// place, and fails the next attempt instead of starting a second job.
    #[tokio::test]
    async fn a_job_that_cannot_be_stopped_blocks_the_retry() {
        let log = Arc::default();
        let jobs = Arc::new(FakeJobs {
            fail_stop: true,
            ..FakeJobs::with(&[(500, JobState::Running)], Arc::clone(&log))
        });
        let abandoned = Arc::new(AbandonedAttempts::default());
        let mut guard = AttemptGuard::new(attempt(jobs, &log), Arc::clone(&abandoned));
        guard.job_started(500);
        let error = guard.reap_now().await.expect_err("the stop fails");
        assert!(
            matches!(
                error,
                DatabaseJobError::NotStopped {
                    workload_id: 500,
                    ..
                }
            ),
            "{error}"
        );
        assert!(!error.is_permanent());
        drop(guard);
        assert!(abandoned.is_pending(41));
        let blocked = abandoned
            .settle(41, Duration::from_secs(5))
            .await
            .expect_err("the retry must not start");
        assert!(matches!(blocked, BackupError::Failed { .. }), "{blocked}");
        assert!(
            !entries(&log).contains(&"object removed".to_string()),
            "the object stays while the job may be writing it"
        );
    }

    /// A removal slower than the caller's patience is an error, and stays
    /// recorded so the next caller waits for it too.
    #[tokio::test]
    async fn settling_times_out_while_the_job_is_still_being_removed() {
        let log = Arc::default();
        let jobs = Arc::new(FakeJobs {
            removal_polls: u32::MAX,
            ..FakeJobs::with(&[(500, JobState::Running)], Arc::clone(&log))
        });
        let abandoned = Arc::new(AbandonedAttempts::default());
        {
            let mut guard = AttemptGuard::new(attempt(jobs, &log), Arc::clone(&abandoned));
            guard.job_started(500);
        }
        let error = abandoned
            .settle(41, Duration::from_millis(30))
            .await
            .expect_err("still removing");
        assert!(error.to_string().contains("still being stopped"), "{error}");
        assert!(abandoned.is_pending(41));
    }

    /// `stop_and_await_removal` gives up after its timeout with the job's id.
    #[tokio::test]
    async fn stop_and_await_removal_names_the_job_it_could_not_remove() {
        let log = Arc::default();
        let jobs = FakeJobs {
            removal_polls: u32::MAX,
            ..FakeJobs::with(&[(9, JobState::Running)], log)
        };
        let error = stop_and_await_removal(
            &jobs,
            7,
            "backup",
            9,
            "test",
            RemovalTiming {
                timeout: Duration::from_millis(10),
                poll: Duration::from_millis(1),
            },
        )
        .await
        .expect_err("never removed");
        let text = error.to_string();
        assert!(text.contains("backup job 9 of database 7"), "{text}");
        assert!(text.contains("had not removed it"), "{text}");
        // Gone already: nothing to do.
        stop_and_await_removal(&jobs, 7, "backup", 1234, "test", FAST)
            .await
            .expect("a job that is gone is done");
    }

    /// The executor's `cleanup` (run before a retry) waits for the dropped
    /// attempt's cleanup.
    #[tokio::test]
    async fn the_engine_cleanup_waits_for_an_abandoned_attempt() {
        let log = Arc::default();
        let jobs = Arc::new(FakeJobs::with(
            &[(500, JobState::Running)],
            Arc::clone(&log),
        ));
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let engine = PostgresWorkloadJobEngine::new(PostgresWorkloadJobDeps {
            db: db.clone(),
            encryption_service: Arc::new(
                temps_core::EncryptionService::new("test_encryption_key_1234567890ab")
                    .expect("encryption"),
            ),
            plane: Arc::new(PlaneSlot::new()),
        });
        {
            let mut guard = AttemptGuard::new(attempt(jobs, &log), Arc::clone(&engine.abandoned));
            guard.job_started(500);
        }
        let ctx = BackupContext {
            backup_id: 41,
            engine_key: ENGINE_KEY.to_string(),
            params: json!({"service_id": 7, "s3_source_id": 2}),
            cancel: tokio_util::sync::CancellationToken::new(),
            db,
        };
        engine.cleanup(&ctx).await.expect("cleanup");
        assert_eq!(
            entries(&log),
            vec!["stop 500", "removed 500", "object removed"]
        );
        assert!(!engine.abandoned.is_pending(41));
    }
}
