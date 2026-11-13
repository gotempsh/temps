// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cron `workflow` mode (ADR-103 §9, M2).
//!
//! A `workflow` cron starts a run of a durable workflow at each occurrence,
//! through [`WorkflowControl`]; nothing runs in the cron scheduler itself.
//! Each occurrence is one `cron_executions` row:
//!
//! - `running` with `workflow_run_id` once the run is started, then
//!   `succeeded` or `failed` when it finishes, settled the next time the
//!   cron fires or its history is read;
//! - `skipped` when the starting deadline passed or `forbid` found an active
//!   run;
//! - `failed` when the start was refused (an unknown name, a quota) or
//!   workflows are not available on this server.
//!
//! A start is idempotent per cron and occurrence
//! ([`workflow_idempotency_key`]), so a scheduler that crashed after starting
//! a run and fires the occurrence again starts nothing new.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect,
};
use temps_core::{
    CronConcurrencyPolicy, WorkflowControl, WorkflowControlError, WorkflowRunPhase,
    WorkflowRunState, WorkflowStart,
};
use temps_entities::{cron_executions, crons};
use tracing::{info, warn};
use uuid::Uuid;

/// Most unsettled runs fetched per page while checking concurrency.
const MAX_FOLLOWED_RUNS: u64 = 100;

/// The idempotency key of the run a cron's occurrence starts.
pub fn workflow_idempotency_key(cron_id: i32, due_at: DateTime<Utc>) -> String {
    format!("cron:{cron_id}:{}", due_at.timestamp())
}

/// What one due occurrence of a `workflow` cron does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowDispatch {
    Run,
    /// Cancel these active runs, then start (`replace`).
    Replace {
        active: Vec<Uuid>,
    },
    Skip {
        reason: &'static str,
        message: String,
    },
}

/// Kubernetes CronJob semantics, as for `job` crons: the starting deadline
/// first, then `allow | forbid | replace` against the runs still active.
pub fn workflow_dispatch_decision(
    policy: CronConcurrencyPolicy,
    starting_deadline_seconds: Option<i32>,
    due_at: DateTime<Utc>,
    now: DateTime<Utc>,
    active: &[Uuid],
) -> WorkflowDispatch {
    if let Some(deadline) = starting_deadline_seconds {
        let late = now.signed_duration_since(due_at);
        if late > chrono::Duration::seconds(i64::from(deadline.max(0))) {
            return WorkflowDispatch::Skip {
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
        return WorkflowDispatch::Run;
    }
    match policy {
        CronConcurrencyPolicy::Allow => WorkflowDispatch::Run,
        CronConcurrencyPolicy::Forbid => WorkflowDispatch::Skip {
            reason: "ConcurrencyForbidden",
            message: format!(
                "the run due at {} was skipped: concurrency_policy is forbid and {} earlier \
                 run(s) are still active (workflow run {})",
                due_at.to_rfc3339(),
                active.len(),
                active
                    .iter()
                    .map(Uuid::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        CronConcurrencyPolicy::Replace => WorkflowDispatch::Replace {
            active: active.to_vec(),
        },
    }
}

/// How a finished run settles its execution row: state, status code and
/// message. `None`: still active.
fn settlement(
    run_id: Uuid,
    state: Option<&WorkflowRunState>,
) -> Option<(&'static str, i32, Option<String>)> {
    let Some(state) = state else {
        return Some((
            cron_executions::STATE_FAILED,
            500,
            Some(format!(
                "workflow run {run_id} no longer exists (deleted, or expired by retention) \
                 before its outcome was recorded"
            )),
        ));
    };
    match state.phase {
        WorkflowRunPhase::Active => None,
        WorkflowRunPhase::Completed => Some((cron_executions::STATE_SUCCEEDED, 200, None)),
        WorkflowRunPhase::Failed => Some((
            cron_executions::STATE_FAILED,
            500,
            Some(format!(
                "workflow run {run_id} of '{}' failed ({})",
                state.workflow,
                state.error_kind.as_deref().unwrap_or("unknown")
            )),
        )),
        WorkflowRunPhase::Cancelled => Some((
            cron_executions::STATE_FAILED,
            500,
            Some(format!(
                "workflow run {run_id} of '{}' was cancelled",
                state.workflow
            )),
        )),
    }
}

/// Settle the `running` rows among `executions` whose run finished. Returns
/// the run ids still active. A lookup failure leaves every row as it is.
pub async fn settle_workflow_executions(
    db: &DatabaseConnection,
    control: &dyn WorkflowControl,
    cron: &crons::Model,
    executions: &mut [cron_executions::Model],
) -> Result<Vec<Uuid>, SettleError> {
    let followed: Vec<Uuid> = executions
        .iter()
        .filter(|row| row.state.as_deref() == Some(cron_executions::STATE_RUNNING))
        .filter_map(|row| row.workflow_run_id)
        .collect();
    if followed.is_empty() {
        return Ok(Vec::new());
    }
    let states = control
        .run_states(cron.project_id, cron.environment_id, &followed)
        .await?;
    let now = Utc::now();
    let mut active = Vec::new();
    for row in executions.iter_mut() {
        let Some(run_id) = row.workflow_run_id else {
            continue;
        };
        if row.state.as_deref() != Some(cron_executions::STATE_RUNNING) {
            continue;
        }
        let state = states.iter().find(|state| state.run_id == run_id);
        let Some((settled, status_code, message)) = settlement(run_id, state) else {
            active.push(run_id);
            continue;
        };
        let mut update: cron_executions::ActiveModel = row.clone().into();
        update.state = Set(Some(settled.to_string()));
        update.status_code = Set(status_code);
        update.error_message = Set(message);
        update.finished_at = Set(Some(now));
        *row = update
            .update(db)
            .await
            .map_err(|source| SettleError::Database {
                cron_id: cron.id,
                source,
            })?;
    }
    Ok(active)
}

#[derive(Debug, thiserror::Error)]
pub enum SettleError {
    #[error("Database error while settling runs for workflow cron {cron_id}: {source}")]
    Database {
        cron_id: i32,
        #[source]
        source: DbErr,
    },
    #[error(transparent)]
    Workflows(#[from] WorkflowControlError),
}

/// Record an occurrence that did not become a run.
async fn record_unstarted(
    db: &DatabaseConnection,
    cron: &crons::Model,
    now: DateTime<Utc>,
    state: &'static str,
    message: String,
) -> Result<(), DbErr> {
    let status_code = if state == cron_executions::STATE_SKIPPED {
        0
    } else {
        500
    };
    cron_executions::ActiveModel {
        cron_id: Set(cron.id),
        executed_at: Set(now),
        url: Set(cron.path.clone()),
        status_code: Set(status_code),
        headers: Set("{}".to_string()),
        response_time_ms: Set(0),
        error_message: Set(Some(message)),
        state: Set(Some(state.to_string())),
        finished_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// Recovering an occurrence records the same execution only once.
async fn record_started(
    db: &DatabaseConnection,
    cron: &crons::Model,
    now: DateTime<Utc>,
    run_id: Uuid,
) -> Result<(), DbErr> {
    cron_executions::Entity::insert(cron_executions::ActiveModel {
        cron_id: Set(cron.id),
        executed_at: Set(now),
        url: Set(cron.path.clone()),
        status_code: Set(0),
        headers: Set("{}".to_string()),
        response_time_ms: Set(0),
        state: Set(Some(cron_executions::STATE_RUNNING.to_string())),
        workflow_run_id: Set(Some(run_id)),
        ..Default::default()
    })
    .on_conflict(
        sea_orm::sea_query::OnConflict::column(cron_executions::Column::WorkflowRunId)
            .do_nothing()
            .to_owned(),
    )
    .do_nothing()
    .exec(db)
    .await?;
    Ok(())
}

/// Start the run of `cron`'s occurrence due at `due_at`. Every outcome is
/// recorded in `cron_executions`; only a failure to write it is returned.
pub async fn dispatch_workflow(
    db: &DatabaseConnection,
    control: Option<&Arc<dyn WorkflowControl>>,
    cron: &crons::Model,
    due_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let Some(control) = control else {
        warn!(
            cron_id = cron.id,
            project_id = cron.project_id,
            "workflow cron is due but durable workflows are not available on this server"
        );
        return record_unstarted(
            db,
            cron,
            now,
            cron_executions::STATE_FAILED,
            "WorkflowsUnavailable: durable workflows are not available on this server, so \
             workflow crons cannot start runs (HTTP and job crons are unaffected)"
                .to_string(),
        )
        .await;
    };
    let Some(workflow) = cron.workflow_name.clone() else {
        return record_unstarted(
            db,
            cron,
            now,
            cron_executions::STATE_FAILED,
            format!(
                "InvalidCron: cron {} is in workflow mode but names no workflow; redeploy \
                 with `workflow:` set in .temps.yaml",
                cron.id
            ),
        )
        .await;
    };

    // Replay must find the occurrence before `replace` can cancel anything,
    // including after a crash between starting the run and recording history.
    let key = workflow_idempotency_key(cron.id, due_at);
    match control
        .find_started(cron.project_id, cron.environment_id, &workflow, &key)
        .await
    {
        Ok(Some(run_id)) => return record_started(db, cron, now, run_id).await,
        Ok(None) => {}
        Err(error) => {
            return record_unstarted(
                db,
                cron,
                now,
                cron_executions::STATE_FAILED,
                format!("WorkflowsUnavailable: could not recover the cron occurrence: {error}"),
            )
            .await
        }
    }
    let policy = CronConcurrencyPolicy::from_stored(&cron.concurrency_policy);
    let mut after = 0;
    loop {
        let mut running = cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron.id))
            .filter(cron_executions::Column::State.eq(cron_executions::STATE_RUNNING))
            .filter(cron_executions::Column::WorkflowRunId.is_not_null())
            .filter(cron_executions::Column::Id.gt(after))
            .order_by_asc(cron_executions::Column::Id)
            .limit(MAX_FOLLOWED_RUNS)
            .all(db)
            .await?;
        let last = running.last().map(|row| row.id);
        let active =
            match settle_workflow_executions(db, control.as_ref(), cron, &mut running).await {
                Ok(active) => active,
                Err(SettleError::Database { source, .. }) => return Err(source),
                Err(SettleError::Workflows(error)) => {
                    return record_unstarted(
                        db,
                        cron,
                        now,
                        cron_executions::STATE_FAILED,
                        format!(
                            "WorkflowsUnavailable: could not read the cron's earlier runs: {error}"
                        ),
                    )
                    .await;
                }
            };
        match workflow_dispatch_decision(
            policy,
            cron.starting_deadline_seconds,
            due_at,
            now,
            &active,
        ) {
            WorkflowDispatch::Run => {}
            WorkflowDispatch::Skip { reason, message } => {
                return record_unstarted(
                    db,
                    cron,
                    now,
                    cron_executions::STATE_SKIPPED,
                    format!("{reason}: {message}"),
                )
                .await;
            }
            WorkflowDispatch::Replace { active } => {
                for run_id in active {
                    if let Err(error) = control
                        .cancel(cron.project_id, cron.environment_id, run_id)
                        .await
                    {
                        return record_unstarted(db, cron, now, cron_executions::STATE_FAILED,
                            format!("ReplaceFailed: could not cancel active workflow run {run_id}: {error}")).await;
                    }
                }
            }
        }
        // Allow needs no full concurrency scan. Other policies must account
        // for every page, without retaining all executions or run IDs.
        if policy == CronConcurrencyPolicy::Allow || running.len() < MAX_FOLLOWED_RUNS as usize {
            break;
        }
        let Some(last) = last else {
            break;
        };
        after = last;
    }

    let start = WorkflowStart {
        project_id: cron.project_id,
        environment_id: cron.environment_id,
        workflow: workflow.clone(),
        input: cron.workflow_input.clone(),
        idempotency_key: workflow_idempotency_key(cron.id, due_at),
    };
    match control.start(start).await {
        Ok(started) => {
            record_started(db, cron, now, started.run_id).await?;
            info!(
                cron_id = cron.id,
                project_id = cron.project_id,
                environment_id = cron.environment_id,
                workflow = %workflow,
                run_id = %started.run_id,
                created = started.created,
                "workflow cron started a run"
            );
            Ok(())
        }
        Err(error) => {
            let reason = match error {
                WorkflowControlError::Refused { .. } => "StartRefused",
                WorkflowControlError::Failed { .. } => "WorkflowsUnavailable",
            };
            warn!(cron_id = cron.id, workflow = %workflow, %error, "workflow cron could not start a run");
            record_unstarted(
                db,
                cron,
                now,
                cron_executions::STATE_FAILED,
                format!("{reason}: {error}"),
            )
            .await
        }
    }
}

/// A `WorkflowControl` that keeps runs in memory, for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use temps_core::{ActiveWorkflowRuns, StartedWorkflowRun};

    use super::*;

    type FakeRun = (String, WorkflowRunPhase, Option<serde_json::Value>);

    #[derive(Default)]
    pub(crate) struct FakeWorkflowControl {
        /// run id → (workflow, phase, input)
        pub runs: Mutex<HashMap<Uuid, FakeRun>>,
        keys: Mutex<HashMap<String, Uuid>>,
        /// Refuse every start with this reason.
        pub refuse: Mutex<Option<String>>,
        pub cancelled: Mutex<Vec<Uuid>>,
        pub drain_active: std::sync::atomic::AtomicU64,
        pub drain_fail: std::sync::atomic::AtomicBool,
    }

    impl FakeWorkflowControl {
        pub fn finish(&self, run_id: Uuid, phase: WorkflowRunPhase) {
            if let Some(run) = self.runs.lock().unwrap().get_mut(&run_id) {
                run.1 = phase;
            }
        }
    }

    #[async_trait]
    impl WorkflowControl for FakeWorkflowControl {
        async fn begin_deploy_drain(
            &self,
            _project_id: i32,
            _environment_id: i32,
            _deployment_id: i32,
        ) -> Result<Option<temps_core::WorkflowDrain>, WorkflowControlError> {
            Ok(None)
        }
        async fn renew_deploy_drain(
            &self,
            project_id: i32,
            environment_id: i32,
            _token: Uuid,
        ) -> Result<u64, WorkflowControlError> {
            if self.drain_fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(WorkflowControlError::Failed {
                    project_id,
                    environment_id,
                    operation: "renew drain",
                    reason: "lease expired".into(),
                });
            }
            Ok(self.drain_active.load(std::sync::atomic::Ordering::SeqCst))
        }
        async fn end_deploy_drain(
            &self,
            _project_id: i32,
            _environment_id: i32,
            _token: Uuid,
        ) -> Result<(), WorkflowControlError> {
            Ok(())
        }

        async fn find_started(
            &self,
            _project_id: i32,
            _environment_id: i32,
            _workflow: &str,
            idempotency_key: &str,
        ) -> Result<Option<Uuid>, WorkflowControlError> {
            Ok(self.keys.lock().unwrap().get(idempotency_key).copied())
        }

        async fn start(
            &self,
            start: WorkflowStart,
        ) -> Result<StartedWorkflowRun, WorkflowControlError> {
            if let Some(reason) = self.refuse.lock().unwrap().clone() {
                return Err(WorkflowControlError::Refused {
                    project_id: start.project_id,
                    environment_id: start.environment_id,
                    workflow: start.workflow,
                    reason,
                });
            }
            let mut keys = self.keys.lock().unwrap();
            if let Some(run_id) = keys.get(&start.idempotency_key) {
                return Ok(StartedWorkflowRun {
                    run_id: *run_id,
                    created: false,
                });
            }
            let run_id = Uuid::new_v4();
            keys.insert(start.idempotency_key, run_id);
            self.runs.lock().unwrap().insert(
                run_id,
                (start.workflow, WorkflowRunPhase::Active, start.input),
            );
            Ok(StartedWorkflowRun {
                run_id,
                created: true,
            })
        }

        async fn run_states(
            &self,
            _project_id: i32,
            _environment_id: i32,
            run_ids: &[Uuid],
        ) -> Result<Vec<WorkflowRunState>, WorkflowControlError> {
            let runs = self.runs.lock().unwrap();
            Ok(run_ids
                .iter()
                .filter_map(|id| {
                    runs.get(id).map(|(workflow, phase, _)| WorkflowRunState {
                        run_id: *id,
                        workflow: workflow.clone(),
                        phase: *phase,
                        status: format!("{phase:?}").to_lowercase(),
                        error_kind: (*phase == WorkflowRunPhase::Failed)
                            .then(|| "step_failed".into()),
                    })
                })
                .collect())
        }

        async fn cancel(
            &self,
            _project_id: i32,
            _environment_id: i32,
            run_id: Uuid,
        ) -> Result<bool, WorkflowControlError> {
            self.cancelled.lock().unwrap().push(run_id);
            self.finish(run_id, WorkflowRunPhase::Cancelled);
            Ok(true)
        }

        async fn active_runs(
            &self,
            project_id: i32,
            environment_id: i32,
        ) -> Result<Vec<ActiveWorkflowRuns>, WorkflowControlError> {
            if self.refuse.lock().unwrap().is_some() {
                return Err(WorkflowControlError::Failed {
                    project_id,
                    environment_id,
                    operation: "count active runs",
                    reason: "unavailable".into(),
                });
            }
            let mut counts = std::collections::BTreeMap::new();
            for (workflow, phase, _) in self.runs.lock().unwrap().values() {
                if *phase == WorkflowRunPhase::Active {
                    *counts.entry(workflow.clone()).or_insert(0) += 1;
                }
            }
            Ok(counts
                .into_iter()
                .map(|(workflow, active)| ActiveWorkflowRuns { workflow, active })
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeWorkflowControl;
    use super::*;
    use temps_test_utils::TestDatabase;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap()
    }

    #[test]
    fn decisions_follow_the_deadline_then_the_policy() {
        let run = Uuid::new_v4();
        assert_eq!(
            workflow_dispatch_decision(CronConcurrencyPolicy::Forbid, None, at(0), at(5), &[]),
            WorkflowDispatch::Run
        );
        assert!(matches!(
            workflow_dispatch_decision(CronConcurrencyPolicy::Allow, Some(30), at(0), at(31), &[]),
            WorkflowDispatch::Skip {
                reason: "MissedStartingDeadline",
                ..
            }
        ));
        assert!(matches!(
            workflow_dispatch_decision(CronConcurrencyPolicy::Forbid, None, at(0), at(1), &[run]),
            WorkflowDispatch::Skip { reason: "ConcurrencyForbidden", message } if message.contains(&run.to_string())
        ));
        assert_eq!(
            workflow_dispatch_decision(CronConcurrencyPolicy::Allow, None, at(0), at(1), &[run]),
            WorkflowDispatch::Run
        );
        assert_eq!(
            workflow_dispatch_decision(CronConcurrencyPolicy::Replace, None, at(0), at(1), &[run]),
            WorkflowDispatch::Replace { active: vec![run] }
        );
    }

    #[test]
    fn one_key_per_cron_and_occurrence() {
        assert_eq!(workflow_idempotency_key(4, at(0)), "cron:4:1700000000");
        assert_ne!(
            workflow_idempotency_key(4, at(0)),
            workflow_idempotency_key(4, at(60))
        );
        assert_ne!(
            workflow_idempotency_key(4, at(0)),
            workflow_idempotency_key(5, at(0))
        );
    }

    #[test]
    fn finished_runs_settle_and_active_ones_stay_running() {
        let id = Uuid::new_v4();
        let state = |phase| WorkflowRunState {
            run_id: id,
            workflow: "digest".into(),
            phase,
            status: "x".into(),
            error_kind: Some("step_failed".into()),
        };
        assert_eq!(settlement(id, Some(&state(WorkflowRunPhase::Active))), None);
        assert_eq!(
            settlement(id, Some(&state(WorkflowRunPhase::Completed))),
            Some((cron_executions::STATE_SUCCEEDED, 200, None))
        );
        let (failed, code, message) =
            settlement(id, Some(&state(WorkflowRunPhase::Failed))).unwrap();
        assert_eq!((failed, code), (cron_executions::STATE_FAILED, 500));
        assert!(message.unwrap().contains("step_failed"));
        assert!(settlement(id, Some(&state(WorkflowRunPhase::Cancelled)))
            .unwrap()
            .2
            .unwrap()
            .contains("cancelled"));
        assert!(settlement(id, None)
            .unwrap()
            .2
            .unwrap()
            .contains("no longer exists"));
    }

    async fn workflow_cron(db: &DatabaseConnection, policy: &str) -> Option<crons::Model> {
        use temps_entities::{environments, projects};
        let project = projects::ActiveModel {
            name: Set("wf-cron".to_string()),
            slug: Set(format!("wf-cron-{}", Uuid::new_v4().simple())),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(db)
        .await
        .ok()?;
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(format!("wf-cron-{}", project.id)),
            host: Set(format!("wf-cron-{}.local", project.id)),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            ..Default::default()
        }
        .insert(db)
        .await
        .ok()?;
        crons::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            path: Set("workflow:send-digest".to_string()),
            schedule: Set("0 8 * * 1".to_string()),
            mode: Set(crons::MODE_WORKFLOW.to_string()),
            concurrency_policy: Set(policy.to_string()),
            workflow_name: Set(Some("send-digest".to_string())),
            workflow_input: Set(Some(serde_json::json!({ "period": "weekly" }))),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .ok()
    }

    async fn history(db: &DatabaseConnection, cron: &crons::Model) -> Vec<cron_executions::Model> {
        cron_executions::Entity::find()
            .filter(cron_executions::Column::CronId.eq(cron.id))
            .order_by_asc(cron_executions::Column::Id)
            .all(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_occurrence_starts_one_run_and_forbid_skips_while_it_is_active() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            eprintln!("skipping: no test database (Docker unavailable)");
            return;
        };
        let db = test_db.db.as_ref();
        let cron = workflow_cron(db, "forbid").await.unwrap();
        let fake = Arc::new(FakeWorkflowControl::default());
        let control: Arc<dyn WorkflowControl> = fake.clone();

        dispatch_workflow(db, Some(&control), &cron, at(0), at(1))
            .await
            .unwrap();
        let rows = history(db, &cron).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].state.as_deref(),
            Some(cron_executions::STATE_RUNNING)
        );
        let run_id = rows[0].workflow_run_id.unwrap();
        let runs = fake.runs.lock().unwrap().clone();
        assert_eq!(runs[&run_id].0, "send-digest");
        assert_eq!(
            runs[&run_id].2,
            Some(serde_json::json!({ "period": "weekly" }))
        );

        // The next occurrence: the first run is still active, so forbid skips.
        dispatch_workflow(db, Some(&control), &cron, at(60), at(61))
            .await
            .unwrap();
        let rows = history(db, &cron).await;
        assert_eq!(
            rows[1].state.as_deref(),
            Some(cron_executions::STATE_SKIPPED)
        );
        assert!(rows[1]
            .error_message
            .as_deref()
            .unwrap()
            .contains("ConcurrencyForbidden"));

        // Once it completes, the history settles and the next one starts.
        fake.finish(run_id, WorkflowRunPhase::Completed);
        dispatch_workflow(db, Some(&control), &cron, at(120), at(121))
            .await
            .unwrap();
        let rows = history(db, &cron).await;
        assert_eq!(
            rows[0].state.as_deref(),
            Some(cron_executions::STATE_SUCCEEDED)
        );
        assert_eq!(rows[0].status_code, 200);
        assert!(rows[0].finished_at.is_some());
        assert_eq!(
            rows[2].state.as_deref(),
            Some(cron_executions::STATE_RUNNING)
        );
        assert_ne!(rows[2].workflow_run_id, Some(run_id));

        // A failed run settles as failed, with its category.
        fake.finish(rows[2].workflow_run_id.unwrap(), WorkflowRunPhase::Failed);
        let mut page = history(db, &cron).await;
        let active = settle_workflow_executions(db, fake.as_ref(), &cron, &mut page)
            .await
            .unwrap();
        assert!(active.is_empty());
        assert_eq!(
            page[2].state.as_deref(),
            Some(cron_executions::STATE_FAILED)
        );
        assert!(page[2]
            .error_message
            .as_deref()
            .unwrap()
            .contains("step_failed"));
    }

    #[tokio::test]
    async fn the_same_occurrence_twice_is_one_run_and_replace_cancels_the_active_run() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            eprintln!("skipping: no test database (Docker unavailable)");
            return;
        };
        let db = test_db.db.as_ref();
        let cron = workflow_cron(db, "replace").await.unwrap();
        let fake = Arc::new(FakeWorkflowControl::default());
        let control: Arc<dyn WorkflowControl> = fake.clone();

        dispatch_workflow(db, Some(&control), &cron, at(0), at(1))
            .await
            .unwrap();
        let first = history(db, &cron).await[0].workflow_run_id.unwrap();
        // A scheduler that crashed after starting fires the same occurrence
        // again: replace cancels the active run, and the start finds it.
        dispatch_workflow(db, Some(&control), &cron, at(60), at(61))
            .await
            .unwrap();
        assert_eq!(fake.cancelled.lock().unwrap().clone(), vec![first]);
        let rows = history(db, &cron).await;
        assert_eq!(
            rows[0].state.as_deref(),
            Some(cron_executions::STATE_RUNNING)
        );
        assert_ne!(rows[1].workflow_run_id, Some(first));
        assert_eq!(fake.runs.lock().unwrap().len(), 2);
        // The cancelled run settles as failed on the next read.
        let mut page = history(db, &cron).await;
        settle_workflow_executions(db, fake.as_ref(), &cron, &mut page)
            .await
            .unwrap();
        assert!(page[0]
            .error_message
            .as_deref()
            .unwrap()
            .contains("cancelled"));

        // Same occurrence again: the same run, no third one.
        let again = cron_executions::Entity::find().all(db).await.unwrap().len();
        let key = workflow_idempotency_key(cron.id, at(60));
        let started = control
            .start(WorkflowStart {
                project_id: cron.project_id,
                environment_id: cron.environment_id,
                workflow: "send-digest".into(),
                input: None,
                idempotency_key: key,
            })
            .await
            .unwrap();
        assert!(!started.created);
        assert_eq!(Some(started.run_id), rows[1].workflow_run_id);
        assert_eq!(
            cron_executions::Entity::find().all(db).await.unwrap().len(),
            again
        );
    }

    #[tokio::test]
    async fn replaying_replace_recovers_history_without_cancelling_its_run() {
        if !temps_test_utils::docker_available().await
            && !temps_test_utils::external_database_configured()
        {
            eprintln!("skipping: Docker and external test database unavailable");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.as_ref();
        let cron = workflow_cron(db, "replace").await.unwrap();
        let fake = Arc::new(FakeWorkflowControl::default());
        let control: Arc<dyn WorkflowControl> = fake.clone();
        // The run committed but the scheduler crashed before writing history.
        let started = control
            .start(WorkflowStart {
                project_id: cron.project_id,
                environment_id: cron.environment_id,
                workflow: "send-digest".into(),
                input: None,
                idempotency_key: workflow_idempotency_key(cron.id, at(0)),
            })
            .await
            .unwrap();
        for _ in 0..2 {
            dispatch_workflow(db, Some(&control), &cron, at(0), at(1))
                .await
                .unwrap();
        }
        let rows = history(db, &cron).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].workflow_run_id, Some(started.run_id));
        assert!(fake.cancelled.lock().unwrap().is_empty());
        assert_eq!(
            fake.runs.lock().unwrap()[&started.run_id].1,
            WorkflowRunPhase::Active
        );
    }

    #[tokio::test]
    async fn concurrency_accounts_for_active_runs_across_history_pages() {
        if !temps_test_utils::docker_available().await
            && !temps_test_utils::external_database_configured()
        {
            eprintln!("skipping: Docker and external test database unavailable");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.as_ref();
        for policy in ["forbid", "replace"] {
            let cron = workflow_cron(db, policy).await.unwrap();
            let fake = Arc::new(FakeWorkflowControl::default());
            let control: Arc<dyn WorkflowControl> = fake.clone();
            let mut expected_active = Vec::new();
            for i in 0..102 {
                let started = control
                    .start(WorkflowStart {
                        project_id: cron.project_id,
                        environment_id: cron.environment_id,
                        workflow: "send-digest".into(),
                        input: None,
                        idempotency_key: workflow_idempotency_key(cron.id, at(i)),
                    })
                    .await
                    .unwrap();
                record_started(db, &cron, at(i), started.run_id)
                    .await
                    .unwrap();
                // Both ends of the history matter, even if a whole page
                // between them has finished but is not yet settled.
                if i == 0 || i == 101 {
                    expected_active.push(started.run_id);
                } else {
                    fake.finish(started.run_id, WorkflowRunPhase::Completed);
                }
            }
            dispatch_workflow(db, Some(&control), &cron, at(200), at(201))
                .await
                .unwrap();
            let rows = history(db, &cron).await;
            if policy == "forbid" {
                assert_eq!(
                    rows.last().unwrap().state.as_deref(),
                    Some(cron_executions::STATE_SKIPPED)
                );
                assert_eq!(fake.runs.lock().unwrap().len(), 102);
            } else {
                assert_eq!(*fake.cancelled.lock().unwrap(), expected_active);
                assert_eq!(fake.runs.lock().unwrap().len(), 103);
            }
        }
    }

    #[tokio::test]
    async fn refused_and_unavailable_starts_are_recorded_with_why() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            eprintln!("skipping: no test database (Docker unavailable)");
            return;
        };
        let db = test_db.db.as_ref();
        let cron = workflow_cron(db, "allow").await.unwrap();

        dispatch_workflow(db, None, &cron, at(0), at(1))
            .await
            .unwrap();
        let fake = Arc::new(FakeWorkflowControl::default());
        *fake.refuse.lock().unwrap() = Some("quota 'max_active_runs' is reached".into());
        let control: Arc<dyn WorkflowControl> = fake.clone();
        dispatch_workflow(db, Some(&control), &cron, at(60), at(61))
            .await
            .unwrap();

        let rows = history(db, &cron).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(
            |row| row.state.as_deref() == Some(cron_executions::STATE_FAILED)
                && row.status_code == 500
                && row.workflow_run_id.is_none()
        ));
        assert!(rows[0]
            .error_message
            .as_deref()
            .unwrap()
            .starts_with("WorkflowsUnavailable"));
        let refused = rows[1].error_message.as_deref().unwrap();
        assert!(
            refused.starts_with("StartRefused") && refused.contains("max_active_runs"),
            "{refused}"
        );
    }
}
