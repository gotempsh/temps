// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`WorkflowControl`] for the rest of Temps: cron schedules start runs
//! through it, and deploys read the active runs it reports.

use std::sync::Arc;

use async_trait::async_trait;
use temps_core::{
    ActiveWorkflowRuns, StartedWorkflowRun, WorkflowControl, WorkflowControlError,
    WorkflowRunPhase, WorkflowRunState, WorkflowStart,
};
use uuid::Uuid;

use crate::error::WorkflowError;
use crate::service::{StartRun, WorkflowService};
use crate::store::{Starter, WorkflowStore};
use crate::types::RunStatus;

/// Most run states read in one call.
pub const MAX_RUN_STATES: usize = 100;

pub struct WorkflowControlService {
    service: Arc<WorkflowService>,
    store: Arc<dyn WorkflowStore>,
}

impl WorkflowControlService {
    pub fn new(service: Arc<WorkflowService>, store: Arc<dyn WorkflowStore>) -> Self {
        Self { service, store }
    }

    async fn require_environment(
        &self,
        project_id: i32,
        environment_id: i32,
        operation: &'static str,
    ) -> Result<(), WorkflowControlError> {
        match self.store.environment_project(environment_id).await {
            Ok(Some(owner)) if owner == project_id => Ok(()),
            Ok(_) => Err(WorkflowControlError::Failed {
                project_id,
                environment_id,
                operation,
                reason: "the environment does not belong to the project".into(),
            }),
            Err(error) => Err(failed(project_id, environment_id, operation, error)),
        }
    }
}

fn failed(
    project_id: i32,
    environment_id: i32,
    operation: &'static str,
    error: WorkflowError,
) -> WorkflowControlError {
    WorkflowControlError::Failed {
        project_id,
        environment_id,
        operation,
        reason: error.to_string(),
    }
}

pub(crate) fn phase(status: &str) -> WorkflowRunPhase {
    match RunStatus::parse(status) {
        Some(RunStatus::Completed) => WorkflowRunPhase::Completed,
        Some(RunStatus::Failed) => WorkflowRunPhase::Failed,
        Some(RunStatus::Cancelled) => WorkflowRunPhase::Cancelled,
        _ => WorkflowRunPhase::Active,
    }
}

#[async_trait]
impl WorkflowControl for WorkflowControlService {
    async fn begin_deploy_drain(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
    ) -> Result<Option<temps_core::WorkflowDrain>, WorkflowControlError> {
        self.require_environment(project_id, environment_id, "begin deployment drain")
            .await?;
        let settings = self
            .store
            .settings(environment_id)
            .await
            .map_err(|e| failed(project_id, environment_id, "read drain policy", e))?;
        if settings.drain_timeout_secs == 0 {
            return Ok(None);
        }
        let token = self
            .store
            .begin_drain(environment_id, deployment_id)
            .await
            .map_err(|e| failed(project_id, environment_id, "begin deployment drain", e))?;
        Ok(Some(temps_core::WorkflowDrain {
            token,
            timeout_secs: settings.drain_timeout_secs as u32,
        }))
    }
    async fn renew_deploy_drain(
        &self,
        project_id: i32,
        environment_id: i32,
        token: Uuid,
    ) -> Result<u64, WorkflowControlError> {
        self.require_environment(project_id, environment_id, "renew deployment drain")
            .await?;
        self.store
            .renew_drain(environment_id, token)
            .await
            .map_err(|e| failed(project_id, environment_id, "renew deployment drain", e))
    }
    async fn end_deploy_drain(
        &self,
        project_id: i32,
        environment_id: i32,
        token: Uuid,
    ) -> Result<(), WorkflowControlError> {
        self.require_environment(project_id, environment_id, "release deployment drain")
            .await?;
        self.store
            .end_drain(environment_id, token)
            .await
            .map_err(|e| failed(project_id, environment_id, "release deployment drain", e))
    }

    async fn find_started(
        &self,
        project_id: i32,
        environment_id: i32,
        workflow: &str,
        idempotency_key: &str,
    ) -> Result<Option<Uuid>, WorkflowControlError> {
        self.require_environment(project_id, environment_id, "find started occurrence")
            .await?;
        self.store
            .find_started(environment_id, workflow, idempotency_key)
            .await
            .map_err(|error| failed(project_id, environment_id, "find started occurrence", error))
    }

    async fn start(
        &self,
        start: WorkflowStart,
    ) -> Result<StartedWorkflowRun, WorkflowControlError> {
        let (project_id, environment_id) = (start.project_id, start.environment_id);
        let workflow = start.workflow.clone();
        let outcome = self
            .service
            .start(
                project_id,
                environment_id,
                StartRun {
                    workflow: start.workflow,
                    input: start.input,
                    idempotency_key: Some(start.idempotency_key),
                },
                Starter::Machine,
            )
            .await;
        match outcome {
            Ok((run, created)) => Ok(StartedWorkflowRun {
                run_id: run.id,
                created,
            }),
            // What the caller can fix or wait out: say it as is.
            Err(
                error @ (WorkflowError::Validation { .. }
                | WorkflowError::QuotaExceeded { .. }
                | WorkflowError::PayloadTooLarge { .. }
                | WorkflowError::EnvironmentNotFound { .. }),
            ) => Err(WorkflowControlError::Refused {
                project_id,
                environment_id,
                workflow,
                reason: error.to_string(),
            }),
            Err(error) => Err(failed(project_id, environment_id, "start", error)),
        }
    }

    async fn run_states(
        &self,
        project_id: i32,
        environment_id: i32,
        run_ids: &[Uuid],
    ) -> Result<Vec<WorkflowRunState>, WorkflowControlError> {
        if run_ids.len() > MAX_RUN_STATES {
            return Err(WorkflowControlError::Failed {
                project_id,
                environment_id,
                operation: "read run states",
                reason: format!(
                    "{} runs asked for at once; at most {MAX_RUN_STATES}",
                    run_ids.len()
                ),
            });
        }
        self.require_environment(project_id, environment_id, "read run states")
            .await?;
        let briefs = self
            .store
            .run_briefs(environment_id, run_ids)
            .await
            .map_err(|error| failed(project_id, environment_id, "read run states", error))?;
        Ok(briefs
            .into_iter()
            .map(|brief| WorkflowRunState {
                run_id: brief.id,
                phase: phase(&brief.status),
                workflow: brief.workflow_name,
                status: brief.status,
                error_kind: brief.error_kind,
            })
            .collect())
    }

    async fn cancel(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    ) -> Result<bool, WorkflowControlError> {
        match self
            .service
            .cancel(project_id, environment_id, run_id)
            .await
        {
            Ok(_) => Ok(true),
            Err(WorkflowError::InvalidState { .. } | WorkflowError::RunNotFound { .. }) => {
                Ok(false)
            }
            Err(error) => Err(failed(project_id, environment_id, "cancel", error)),
        }
    }

    async fn active_runs(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<ActiveWorkflowRuns>, WorkflowControlError> {
        self.require_environment(project_id, environment_id, "count active runs")
            .await?;
        let counts = self
            .store
            .active_by_workflow(environment_id)
            .await
            .map_err(|error| failed(project_id, environment_id, "count active runs", error))?;
        Ok(counts
            .into_iter()
            .map(|(workflow, active)| ActiveWorkflowRuns {
                workflow,
                active: u64::try_from(active).unwrap_or(0),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::engine::tests::{harness, Reply, Script};
    use crate::metrics::WorkflowMetrics;
    use crate::protocol::Outcome;

    fn sleeping() -> Script {
        Arc::new(|_, _, _| {
            Box::pin(async {
                Reply::Outcome(Outcome::Completed {
                    output: Some(json!("done")),
                })
            })
        })
    }

    #[tokio::test]
    async fn control_starts_idempotently_and_reports_states_and_active_counts() {
        let Some(h) = harness("workflow control test", &["nightly"], sleeping(), false).await
        else {
            return;
        };
        let service = Arc::new(WorkflowService::new(
            h.store.clone(),
            h.engine.clone(),
            h.codec.clone(),
            Arc::new(WorkflowMetrics::new(Arc::new(
                temps_core::NoopProjectMetricsSink,
            ))),
        ));
        let control = WorkflowControlService::new(service, h.store.clone());
        let (p, e) = (h.env.project_id, h.env.environment_id);
        let start = WorkflowStart {
            project_id: p,
            environment_id: e,
            workflow: "nightly".into(),
            input: Some(json!({ "at": "02:00" })),
            idempotency_key: "cron:1:2026-11-13T02:00:00Z".into(),
        };
        let first = control.start(start.clone()).await.unwrap();
        assert!(first.created);
        let again = control.start(start.clone()).await.unwrap();
        assert_eq!(
            again,
            StartedWorkflowRun {
                run_id: first.run_id,
                created: false
            }
        );

        assert_eq!(
            control
                .find_started(p, e, "nightly", &start.idempotency_key)
                .await
                .unwrap(),
            Some(first.run_id)
        );
        assert_eq!(
            control
                .find_started(p, e, "another", &start.idempotency_key)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            control
                .find_started(p, e, "nightly", "missing")
                .await
                .unwrap(),
            None
        );
        assert!(control
            .find_started(p + 1000, e, "nightly", &start.idempotency_key)
            .await
            .is_err());

        // Another project's environment is not reachable through this one.
        let error = control
            .start(WorkflowStart {
                project_id: p + 1000,
                ..start.clone()
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, WorkflowControlError::Refused { .. }),
            "{error}"
        );
        // An invalid name is refused with the reason.
        let error = control
            .start(WorkflowStart {
                workflow: "bad name".into(),
                idempotency_key: "k2".into(),
                ..start.clone()
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("bad name"), "{error}");

        let missing = Uuid::new_v4();
        let states = control
            .run_states(p, e, &[first.run_id, missing])
            .await
            .unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].phase, WorkflowRunPhase::Active);
        assert_eq!(states[0].status, "queued");
        assert_eq!(
            control.active_runs(p, e).await.unwrap(),
            vec![ActiveWorkflowRuns {
                workflow: "nightly".into(),
                active: 1
            }]
        );
        assert!(control
            .run_states(p, e, &vec![missing; MAX_RUN_STATES + 1])
            .await
            .is_err());

        assert!(control.cancel(p, e, first.run_id).await.unwrap());
        assert!(!control.cancel(p, e, first.run_id).await.unwrap());
        assert!(!control.cancel(p, e, missing).await.unwrap());
        let states = control.run_states(p, e, &[first.run_id]).await.unwrap();
        assert_eq!(states[0].phase, WorkflowRunPhase::Cancelled);
        assert!(control.active_runs(p, e).await.unwrap().is_empty());
    }
}
