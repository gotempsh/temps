// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::WorkflowService;
use crate::{error::WorkflowResult, types::RunStatus};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, ToSchema)]
pub struct WorkflowDiagnosis {
    pub run_id: Uuid,
    pub status: RunStatus,
    pub blocked_reason: Option<String>,
    pub error_kind: Option<String>,
    pub next_action: String,
}
fn next_action(status: RunStatus, blocked: bool) -> &'static str {
    if blocked {
        return "Resolve the environment's blocked reason before retrying.";
    }
    match status {
        RunStatus::Waiting => "Inspect the awaited event name and key; send the matching event when its condition is satisfied.",
        RunStatus::Stuck => "Inspect executor health and deployment logs, then explicitly resume the run.",
        RunStatus::Failed => "Inspect step metadata and application logs; retry from a chosen step after correcting the failure.",
        RunStatus::Sleeping | RunStatus::Retrying => "The scheduler will resume this run at due_at.",
        RunStatus::Queued | RunStatus::Running => "The run is active; inspect its journal and executor health if progress stops.",
        RunStatus::Completed | RunStatus::Cancelled => "The run is terminal; no automatic execution remains.",
    }
}
impl WorkflowService {
    pub async fn diagnose(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    ) -> WorkflowResult<WorkflowDiagnosis> {
        let run = self.get_run(project_id, environment_id, run_id).await?;
        let status = RunStatus::parse(&run.status).unwrap_or(RunStatus::Failed);
        Ok(WorkflowDiagnosis {
            run_id,
            status,
            next_action: next_action(status, run.blocked_reason.is_some()).into(),
            blocked_reason: run.blocked_reason,
            error_kind: run.error_kind,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnosis_prioritizes_blockers_and_distinguishes_operator_actions() {
        assert!(next_action(RunStatus::Waiting, true).contains("blocked reason"));
        assert!(next_action(RunStatus::Waiting, false).contains("matching event"));
        assert!(next_action(RunStatus::Stuck, false).contains("resume"));
        assert!(next_action(RunStatus::Completed, false).contains("terminal"));
    }
}
