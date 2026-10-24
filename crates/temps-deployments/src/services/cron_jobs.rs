// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cron `job` mode (ADR-055 §14).
//!
//! A `job` cron runs its `command` in a new container from the
//! environment's current deployment image, as a run-to-completion workload
//! on the workload plane. This module holds the pieces the scheduler uses:
//!
//! - [`job_dispatch_decision`]: Kubernetes CronJob semantics for one due
//!   run (starting deadline, then `allow | forbid | replace`);
//! - [`job_spec`]: the workload a run is;
//! - [`CronRunFinalizer`]: writes a finished run into `cron_executions`;
//! - [`DeploymentEnvSource`]: gives a run the variables of the deployment
//!   it runs beside.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveValue::Set, ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value,
};
use temps_core::CronConcurrencyPolicy;
use temps_entities::{cron_executions, crons};
use temps_workload::{
    Artifact, ConditionKind, EnvSpec, Isolation, Lifetime, PlacementSpec, Process, SecretRef,
    WorkloadKind, WorkloadPhase, WorkloadRef, WorkloadSpec,
};
use temps_workload_env::SecretResolver;
use temps_workload_plane::{EnvSource, Finalizer, WorkloadView};

use crate::jobs::configure_crons::{CronConfig, JobCronConfig};
use crate::services::DeploymentService;

/// `owner_kind` of the workloads a `job` cron runs.
pub const CRON_OWNER_KIND: &str = "cron";

/// The finalizer that records a finished run in `cron_executions`.
pub const RECORD_RUN_FINALIZER: &str = "cron.temps.sh/record-run";

/// Import source a run's environment comes from:
/// `deployment_container:<project_id>:<environment_id>:<container_id>`.
pub const DEPLOYMENT_CONTAINER_IMPORT: &str = "deployment_container";

/// Variables a deployment container has that are its own, not the
/// environment's, and must not be copied onto a run.
const NOT_IMPORTED: &[&str] = &["HOSTNAME"];

/// The `crons` columns a `.temps.yaml` entry decides beyond path and
/// schedule. An `http` entry's values equal the column defaults, so
/// existing crons never compare as changed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CronJobColumns {
    mode: &'static str,
    command: Option<serde_json::Value>,
    concurrency_policy: &'static str,
    starting_deadline_seconds: Option<i32>,
    retry_limit: i32,
    timeout_seconds: Option<i32>,
    ttl_seconds_after_finished: Option<i32>,
}

fn to_column(value: Option<u32>) -> Option<i32> {
    value.map(|v| i32::try_from(v).unwrap_or(i32::MAX))
}

impl CronJobColumns {
    pub(crate) fn from_config(config: &CronConfig) -> Self {
        match &config.job {
            None => Self {
                mode: crons::MODE_HTTP,
                command: None,
                concurrency_policy: CronConcurrencyPolicy::default().as_str(),
                starting_deadline_seconds: None,
                retry_limit: 0,
                timeout_seconds: None,
                ttl_seconds_after_finished: None,
            },
            Some(JobCronConfig {
                command,
                concurrency_policy,
                starting_deadline_seconds,
                retry_limit,
                timeout_seconds,
                ttl_seconds_after_finished,
            }) => Self {
                mode: crons::MODE_JOB,
                command: Some(serde_json::Value::from(command.clone())),
                concurrency_policy: concurrency_policy.as_str(),
                starting_deadline_seconds: to_column(*starting_deadline_seconds),
                retry_limit: to_column(Some(*retry_limit)).unwrap_or(0),
                timeout_seconds: to_column(*timeout_seconds),
                ttl_seconds_after_finished: to_column(*ttl_seconds_after_finished),
            },
        }
    }

    pub(crate) fn matches(&self, row: &crons::Model) -> bool {
        row.mode == self.mode
            && row.command == self.command
            && row.concurrency_policy == self.concurrency_policy
            && row.starting_deadline_seconds == self.starting_deadline_seconds
            && row.retry_limit == self.retry_limit
            && row.timeout_seconds == self.timeout_seconds
            && row.ttl_seconds_after_finished == self.ttl_seconds_after_finished
    }

    pub(crate) fn apply(&self, row: &mut crons::ActiveModel) {
        row.mode = Set(self.mode.to_string());
        row.command = Set(self.command.clone());
        row.concurrency_policy = Set(self.concurrency_policy.to_string());
        row.starting_deadline_seconds = Set(self.starting_deadline_seconds);
        row.retry_limit = Set(self.retry_limit);
        row.timeout_seconds = Set(self.timeout_seconds);
        row.ttl_seconds_after_finished = Set(self.ttl_seconds_after_finished);
    }
}

/// What one due run of a `job` cron does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobDispatch {
    /// Start it.
    Run,
    /// Delete these active runs, then start it (`replace`).
    Replace { active: Vec<i64> },
    /// Do not start it; record why.
    Skip {
        reason: &'static str,
        message: String,
    },
}

/// Kubernetes CronJob semantics for a run due at `due_at`, decided at
/// `now` with `active` (workload ids) still running. The starting deadline
/// is checked first: a run that can no longer start in time is skipped
/// whatever the policy.
pub fn job_dispatch_decision(
    policy: CronConcurrencyPolicy,
    starting_deadline_seconds: Option<i32>,
    due_at: DateTime<Utc>,
    now: DateTime<Utc>,
    active: &[i64],
) -> JobDispatch {
    if let Some(deadline) = starting_deadline_seconds {
        let late = now.signed_duration_since(due_at);
        if late > chrono::Duration::seconds(i64::from(deadline.max(0))) {
            return JobDispatch::Skip {
                reason: "MissedStartingDeadline",
                message: format!(
                    "the run due at {} could not start within its starting deadline of {}s \
                     (it is {}s late)",
                    due_at.to_rfc3339(),
                    deadline,
                    late.num_seconds()
                ),
            };
        }
    }
    if active.is_empty() {
        return JobDispatch::Run;
    }
    match policy {
        CronConcurrencyPolicy::Allow => JobDispatch::Run,
        CronConcurrencyPolicy::Forbid => JobDispatch::Skip {
            reason: "ConcurrencyForbidden",
            message: format!(
                "the run due at {} was skipped: concurrency_policy is forbid and {} earlier \
                 run(s) are still active (workload {})",
                due_at.to_rfc3339(),
                active.len(),
                active
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        CronConcurrencyPolicy::Replace => JobDispatch::Replace {
            active: active.to_vec(),
        },
    }
}

/// A run's workload name: one per cron and due time, so dispatching the
/// same occurrence twice is one run.
pub fn run_name(cron_id: i32, due_at: DateTime<Utc>) -> String {
    format!("cron-{cron_id}-{}", due_at.timestamp())
}

/// The import a run takes its variables from.
pub fn deployment_container_import(
    project_id: i32,
    environment_id: i32,
    container_id: &str,
) -> String {
    format!("{DEPLOYMENT_CONTAINER_IMPORT}:{project_id}:{environment_id}:{container_id}")
}

fn parse_import(source: &str) -> Option<(i32, i32, &str)> {
    let rest = source
        .strip_prefix(DEPLOYMENT_CONTAINER_IMPORT)?
        .strip_prefix(':')?;
    let mut parts = rest.splitn(3, ':');
    let project_id = parts.next()?.parse().ok()?;
    let environment_id = parts.next()?.parse().ok()?;
    let container_id = parts.next().filter(|id| !id.is_empty())?;
    Some((project_id, environment_id, container_id))
}

/// Why a `job` cron could not become a run.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CronJobError {
    #[error("job cron {cron_id} ('{path}') has no stored command")]
    MissingCommand { cron_id: i32, path: String },
    #[error("job cron {cron_id} ('{path}') has an unreadable stored command: {reason}")]
    InvalidCommand {
        cron_id: i32,
        path: String,
        reason: String,
    },
}

fn to_u32(value: Option<i32>) -> Option<u32> {
    value.and_then(|v| u32::try_from(v).ok())
}

/// The workload one run of `cron` is: `command` in `image`, with the
/// variables of `import`, labelled for the log aggregator.
///
/// The run is pinned to `source_node_id`, the node of the container
/// `import` reads (`None`: the control plane). The variables are resolved
/// for that node -- read from the container on the control plane, with
/// Docker hostnames only it resolves, or recomputed with the worker's
/// `*.temps.local` rewrites on a worker -- and the import has no way to
/// learn which node the run lands on. Placed anywhere else, the run would
/// start with connection strings its node cannot reach.
pub fn job_spec(
    cron: &crons::Model,
    image: &str,
    import: String,
    source_node_id: Option<i32>,
) -> Result<WorkloadSpec, CronJobError> {
    let command: Vec<String> = match &cron.command {
        None => {
            return Err(CronJobError::MissingCommand {
                cron_id: cron.id,
                path: cron.path.clone(),
            })
        }
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|e| CronJobError::InvalidCommand {
                cron_id: cron.id,
                path: cron.path.clone(),
                reason: e.to_string(),
            })?
        }
    };
    if command.is_empty() {
        return Err(CronJobError::MissingCommand {
            cron_id: cron.id,
            path: cron.path.clone(),
        });
    }
    Ok(WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Job, i64::from(cron.id)),
        artifact: Artifact::oci(image),
        isolation: Isolation::Process,
        process: Process {
            command: Some(command),
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
        placement: PlacementSpec {
            nodes: Some(vec![
                source_node_id.unwrap_or(temps_workload_plane::scheduler::CONTROL_PLANE_NODE_ID)
            ]),
            ..Default::default()
        },
        lifetime: Lifetime::RunToCompletion {
            // 0 and NULL take the plane's defaults.
            timeout_secs: to_u32(cron.timeout_seconds).unwrap_or(0),
            retry_limit: to_u32(Some(cron.retry_limit)).unwrap_or(0),
            ttl_secs_after_finished: to_u32(cron.ttl_seconds_after_finished),
        },
        security: None,
        labels: BTreeMap::from([
            (
                "sh.temps.project_id".to_string(),
                cron.project_id.to_string(),
            ),
            (
                "sh.temps.environment".to_string(),
                cron.environment_id.to_string(),
            ),
            ("sh.temps.service".to_string(), format!("cron-{}", cron.id)),
        ]),
    })
}

/// A finished run's `cron_executions` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunRecord {
    pub cron_id: i32,
    pub workload_id: i64,
    pub executed_at: DateTime<Utc>,
    pub state: &'static str,
    pub status_code: i32,
    pub response_time_ms: i32,
    pub error_message: Option<String>,
    pub exit_code: Option<i32>,
    pub attempt: Option<i32>,
    pub finished_at: DateTime<Utc>,
    pub output_tail: Option<String>,
}

impl RunRecord {
    /// The record of `view`, or `None` when it is not a cron's run.
    pub(crate) fn of(view: &WorkloadView, now: DateTime<Utc>) -> Option<Self> {
        if view.owner.kind != CRON_OWNER_KIND {
            return None;
        }
        let cron_id = i32::try_from(view.owner.id).ok()?;
        let succeeded = view.status.phase == WorkloadPhase::Succeeded;
        let finished_at = view.finished_at.unwrap_or(now);
        let elapsed = finished_at
            .signed_duration_since(view.created_at)
            .num_milliseconds()
            .max(0);
        let error_message = if succeeded {
            None
        } else {
            Some(match view.status.condition(ConditionKind::Completed) {
                Some(completed) => format!("{}: {}", completed.reason, completed.message),
                None => format!(
                    "the run ended in phase {} without a completion reason",
                    view.status.phase.as_str()
                ),
            })
        };
        Some(Self {
            cron_id,
            workload_id: view.id,
            executed_at: view.created_at,
            state: if succeeded {
                cron_executions::STATE_SUCCEEDED
            } else {
                cron_executions::STATE_FAILED
            },
            status_code: if succeeded { 200 } else { 500 },
            response_time_ms: i32::try_from(elapsed).unwrap_or(i32::MAX),
            error_message,
            exit_code: view.status.exit_code.and_then(|c| i32::try_from(c).ok()),
            attempt: i32::try_from(view.attempt).ok().filter(|a| *a > 0),
            finished_at,
            output_tail: view.output_tail.clone(),
        })
    }
}

/// Insert or complete the `cron_executions` row of a run. Keyed by
/// `workload_id`, so the dispatcher's `running` row and this one are the
/// same row whichever lands first.
pub(crate) async fn upsert_run<C: ConnectionTrait>(
    db: &C,
    record: &RunRecord,
) -> Result<(), sea_orm::DbErr> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO cron_executions \
             (cron_id, executed_at, url, status_code, headers, response_time_ms, error_message, \
              workload_id, state, exit_code, attempt, finished_at, output_tail) \
         SELECT c.id, $2, c.path, $3, '{}', $4, $5, $6, $7, $8, $9, $10, $11 \
         FROM crons c WHERE c.id = $1 \
         ON CONFLICT (workload_id) WHERE workload_id IS NOT NULL DO UPDATE SET \
             status_code = EXCLUDED.status_code, \
             response_time_ms = EXCLUDED.response_time_ms, \
             error_message = EXCLUDED.error_message, \
             state = EXCLUDED.state, \
             exit_code = EXCLUDED.exit_code, \
             attempt = EXCLUDED.attempt, \
             finished_at = EXCLUDED.finished_at, \
             output_tail = EXCLUDED.output_tail",
        vec![
            record.cron_id.into(),
            record.executed_at.into(),
            record.status_code.into(),
            record.response_time_ms.into(),
            record.error_message.clone().into(),
            record.workload_id.into(),
            record.state.into(),
            record.exit_code.into(),
            record.attempt.into(),
            record.finished_at.into(),
            record.output_tail.clone().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Record a just-applied run as `running`. Does nothing if the run already
/// has a row (its finalizer was faster).
pub(crate) async fn insert_running<C: ConnectionTrait>(
    db: &C,
    cron: &crons::Model,
    workload_id: i64,
    executed_at: DateTime<Utc>,
) -> Result<(), sea_orm::DbErr> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO cron_executions \
             (cron_id, executed_at, url, status_code, headers, response_time_ms, workload_id, \
              state) \
         VALUES ($1, $2, $3, 0, '{}', 0, $4, $5) \
         ON CONFLICT (workload_id) WHERE workload_id IS NOT NULL DO NOTHING",
        vec![
            cron.id.into(),
            executed_at.into(),
            cron.path.clone().into(),
            workload_id.into(),
            Value::from(cron_executions::STATE_RUNNING),
        ],
    ))
    .await?;
    Ok(())
}

/// Writes a finished (or deleted) run into `cron_executions`.
pub struct CronRunFinalizer {
    db: Arc<DatabaseConnection>,
}

impl CronRunFinalizer {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Finalizer for CronRunFinalizer {
    fn name(&self) -> &str {
        RECORD_RUN_FINALIZER
    }

    async fn finalize(&self, workload: &WorkloadView) -> Result<(), String> {
        let Some(record) = RunRecord::of(workload, Utc::now()) else {
            return Ok(());
        };
        upsert_run(self.db.as_ref(), &record).await.map_err(|e| {
            format!(
                "recording run {} (workload {}) of cron {} in cron_executions failed: {e}",
                workload.name, workload.id, record.cron_id
            )
        })?;
        tracing::info!(
            cron_id = record.cron_id,
            workload_id = workload.id,
            state = record.state,
            exit_code = ?record.exit_code,
            "job cron run finished"
        );
        Ok(())
    }
}

/// A run's variables: those of a running container of the deployment it
/// runs beside, read from that container on this host and recomputed from
/// its deployment on a worker (see
/// [`DeploymentService::job_import_env_variables`]).
pub struct DeploymentEnvSource {
    deployment_service: Arc<DeploymentService>,
}

impl DeploymentEnvSource {
    pub fn new(deployment_service: Arc<DeploymentService>) -> Self {
        Self { deployment_service }
    }
}

#[async_trait]
impl SecretResolver for DeploymentEnvSource {
    async fn resolve(&self, secret: &SecretRef) -> Result<String, String> {
        Err(format!(
            "job cron runs take their variables from the deployment they run beside; \
             secret reference '{}' ({}) is not supported",
            secret.name, secret.source
        ))
    }
}

#[async_trait]
impl EnvSource for DeploymentEnvSource {
    async fn import(&self, source: &str) -> Result<Vec<(String, String)>, String> {
        let (project_id, environment_id, container_id) = parse_import(source).ok_or_else(|| {
            format!(
                "unsupported import '{source}'; expected \
                 {DEPLOYMENT_CONTAINER_IMPORT}:<project_id>:<environment_id>:<container_id>"
            )
        })?;
        let vars = self
            .deployment_service
            .job_import_env_variables(project_id, environment_id, container_id.to_string())
            .await
            .map_err(|e| {
                format!(
                    "reading the variables of container {container_id} (project {project_id}, \
                     environment {environment_id}) failed: {e}"
                )
            })?;
        Ok(vars
            .into_iter()
            .filter(|(name, _)| !NOT_IMPORTED.contains(&name.as_str()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::{ConditionStatus, WorkloadStatus};
    use temps_workload_plane::Owner;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid time")
    }

    #[test]
    fn with_nothing_active_every_policy_runs() {
        for policy in [
            CronConcurrencyPolicy::Allow,
            CronConcurrencyPolicy::Forbid,
            CronConcurrencyPolicy::Replace,
        ] {
            assert_eq!(
                job_dispatch_decision(policy, None, at(0), at(1), &[]),
                JobDispatch::Run,
                "{policy:?}"
            );
        }
    }

    #[test]
    fn allow_runs_beside_an_active_run() {
        assert_eq!(
            job_dispatch_decision(CronConcurrencyPolicy::Allow, None, at(0), at(1), &[4]),
            JobDispatch::Run
        );
    }

    #[test]
    fn forbid_skips_while_a_run_is_active_and_names_it() {
        match job_dispatch_decision(CronConcurrencyPolicy::Forbid, None, at(0), at(1), &[4, 9]) {
            JobDispatch::Skip { reason, message } => {
                assert_eq!(reason, "ConcurrencyForbidden");
                assert!(message.contains("workload 4, 9"), "{message}");
            }
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    #[test]
    fn replace_deletes_every_active_run_first() {
        assert_eq!(
            job_dispatch_decision(CronConcurrencyPolicy::Replace, None, at(0), at(1), &[4, 9]),
            JobDispatch::Replace { active: vec![4, 9] }
        );
    }

    #[test]
    fn a_run_past_its_starting_deadline_is_skipped_whatever_the_policy() {
        for policy in [
            CronConcurrencyPolicy::Allow,
            CronConcurrencyPolicy::Forbid,
            CronConcurrencyPolicy::Replace,
        ] {
            match job_dispatch_decision(policy, Some(30), at(0), at(31), &[]) {
                JobDispatch::Skip { reason, message } => {
                    assert_eq!(reason, "MissedStartingDeadline");
                    assert!(message.contains("31s late"), "{message}");
                }
                other => panic!("expected a skip for {policy:?}, got {other:?}"),
            }
        }
        // Exactly on the deadline still starts.
        assert_eq!(
            job_dispatch_decision(CronConcurrencyPolicy::Forbid, Some(30), at(0), at(30), &[]),
            JobDispatch::Run
        );
        // No deadline: however late, it starts.
        assert_eq!(
            job_dispatch_decision(CronConcurrencyPolicy::Forbid, None, at(0), at(86_400), &[]),
            JobDispatch::Run
        );
    }

    fn job_cron() -> crons::Model {
        crons::Model {
            id: 12,
            project_id: 3,
            environment_id: 5,
            path: "job:nightly-report".into(),
            schedule: "0 3 * * *".into(),
            created_at: at(0),
            updated_at: at(0),
            next_run: None,
            deleted_at: None,
            mode: crons::MODE_JOB.into(),
            command: Some(serde_json::json!(["bin/report", "--yesterday"])),
            concurrency_policy: "forbid".into(),
            starting_deadline_seconds: None,
            retry_limit: 2,
            timeout_seconds: Some(1800),
            ttl_seconds_after_finished: None,
        }
    }

    #[test]
    fn a_run_is_the_command_in_the_deployment_image_with_its_variables() {
        let import = deployment_container_import(3, 5, "abc123");
        let spec = job_spec(
            &job_cron(),
            "registry.example.test/shop:41",
            import.clone(),
            None,
        )
        .expect("a spec");
        assert_eq!(spec.owner.kind, WorkloadKind::Job);
        assert_eq!(
            spec.process.command,
            Some(vec!["bin/report".to_string(), "--yesterday".to_string()])
        );
        assert_eq!(spec.process.entrypoint, None);
        assert_eq!(spec.env.imports, vec![import]);
        assert!(spec.env.vars.is_empty() && spec.env.secrets.is_empty());
        assert_eq!(
            spec.lifetime,
            Lifetime::RunToCompletion {
                timeout_secs: 1800,
                retry_limit: 2,
                ttl_secs_after_finished: None,
            }
        );
        assert_eq!(spec.labels["sh.temps.service"], "cron-12");
        assert_eq!(spec.labels["sh.temps.environment"], "5");
        assert_eq!(run_name(12, at(0)), "cron-12-1700000000");
    }

    /// The import resolves variables for the node of the container it reads
    /// (control-plane Docker hostnames for a local container, `*.temps.local`
    /// rewrites for a worker's), so the run must land on that same node:
    /// unpinned, the plane could start a run beside a control-plane
    /// container on a worker that cannot resolve those hostnames.
    #[test]
    fn a_run_is_pinned_to_the_node_its_variables_are_resolved_for() {
        let import = deployment_container_import(3, 5, "abc123");
        let beside_local = job_spec(&job_cron(), "img", import.clone(), None).expect("a spec");
        assert_eq!(
            beside_local.placement.pinned_nodes(),
            Some(&[temps_workload_plane::scheduler::CONTROL_PLANE_NODE_ID][..]),
            "a control-plane source keeps the run on the control plane, never on a worker"
        );
        let beside_worker = job_spec(&job_cron(), "img", import, Some(7)).expect("a spec");
        assert_eq!(
            beside_worker.placement.pinned_nodes(),
            Some(&[7][..]),
            "a worker source keeps the run on that worker"
        );
        assert_eq!(beside_worker.placement.replicas, 1);
    }

    #[test]
    fn a_cron_without_a_command_is_not_a_run() {
        let mut cron = job_cron();
        cron.command = None;
        assert_eq!(
            job_spec(&cron, "img", "x".into(), None),
            Err(CronJobError::MissingCommand {
                cron_id: 12,
                path: "job:nightly-report".into()
            })
        );
        cron.command = Some(serde_json::json!({"not": "a list"}));
        assert!(matches!(
            job_spec(&cron, "img", "x".into(), None),
            Err(CronJobError::InvalidCommand { cron_id: 12, .. })
        ));
    }

    #[test]
    fn imports_parse_back_and_reject_other_sources() {
        let import = deployment_container_import(3, 5, "abc:def");
        assert_eq!(parse_import(&import), Some((3, 5, "abc:def")));
        assert_eq!(parse_import("deployment_container:3:5:"), None);
        assert_eq!(parse_import("env_var:3:5:abc"), None);
        assert_eq!(parse_import("deployment_container:x:5:abc"), None);
    }

    #[test]
    fn http_columns_equal_the_column_defaults() {
        let http = CronConfig {
            path: "/api/cleanup".into(),
            schedule: "0 0 * * *".into(),
            job: None,
        };
        let mut row = job_cron();
        row.mode = crons::MODE_HTTP.into();
        row.command = None;
        row.retry_limit = 0;
        row.timeout_seconds = None;
        assert!(CronJobColumns::from_config(&http).matches(&row));

        let job = CronConfig {
            path: "job:nightly-report".into(),
            schedule: "0 3 * * *".into(),
            job: Some(JobCronConfig {
                command: vec!["bin/report".into(), "--yesterday".into()],
                concurrency_policy: CronConcurrencyPolicy::Forbid,
                starting_deadline_seconds: None,
                retry_limit: 2,
                timeout_seconds: Some(1800),
                ttl_seconds_after_finished: None,
            }),
        };
        assert!(CronJobColumns::from_config(&job).matches(&job_cron()));
        let mut changed = job_cron();
        changed.retry_limit = 1;
        assert!(!CronJobColumns::from_config(&job).matches(&changed));
    }

    fn finished_view(phase: WorkloadPhase, reason: &str, exit_code: i64) -> WorkloadView {
        let mut status = WorkloadStatus {
            phase,
            exit_code: Some(exit_code),
            ..Default::default()
        };
        status.set_condition(
            ConditionKind::Completed,
            if phase == WorkloadPhase::Succeeded {
                ConditionStatus::True
            } else {
                ConditionStatus::False
            },
            reason,
            "attempt 3 exited 1; retry limit 2 reached",
            at(90),
        );
        WorkloadView {
            id: 77,
            owner: Owner::new(CRON_OWNER_KIND, 12),
            name: run_name(12, at(0)),
            spec: job_spec(&job_cron(), "img", "x".into(), None).expect("a spec"),
            status,
            generation: 1,
            observed_generation: 1,
            change_seq: 4,
            finalizers: vec![RECORD_RUN_FINALIZER.into()],
            created_at: at(0),
            deleted_at: None,
            expires_at: None,
            attempt: 3,
            finished_at: Some(at(90)),
            output_tail: Some("report written\n".into()),
        }
    }

    #[test]
    fn a_failed_run_records_its_reason_exit_code_and_attempts() {
        let record = RunRecord::of(
            &finished_view(WorkloadPhase::Failed, "BackoffLimitExceeded", 1),
            at(100),
        )
        .expect("a cron run");
        assert_eq!(record.cron_id, 12);
        assert_eq!(record.workload_id, 77);
        assert_eq!(record.state, cron_executions::STATE_FAILED);
        assert_eq!(record.status_code, 500);
        assert_eq!(record.exit_code, Some(1));
        assert_eq!(record.attempt, Some(3));
        assert_eq!(record.response_time_ms, 90_000);
        assert_eq!(
            record.error_message.as_deref(),
            Some("BackoffLimitExceeded: attempt 3 exited 1; retry limit 2 reached")
        );
        assert_eq!(record.output_tail.as_deref(), Some("report written\n"));
    }

    #[test]
    fn a_succeeded_run_records_200_and_no_error() {
        let record = RunRecord::of(
            &finished_view(WorkloadPhase::Succeeded, "Succeeded", 0),
            at(100),
        )
        .expect("a cron run");
        assert_eq!(record.state, cron_executions::STATE_SUCCEEDED);
        assert_eq!(record.status_code, 200);
        assert_eq!(record.error_message, None);
    }

    #[test]
    fn workloads_of_other_owners_are_not_cron_runs() {
        let mut view = finished_view(WorkloadPhase::Succeeded, "Succeeded", 0);
        view.owner = Owner::new("deployment", 12);
        assert_eq!(RunRecord::of(&view, at(100)), None);
    }
}
