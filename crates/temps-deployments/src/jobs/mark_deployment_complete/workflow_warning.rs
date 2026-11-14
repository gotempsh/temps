// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Warn before promoting code that active durable workflows will replay on.

use std::time::Duration;

use temps_core::{ActiveWorkflowRuns, WorkflowControl};
use temps_deployment_logs::LogLevel;
use tracing::warn;

use super::MarkDeploymentCompleteJob;

fn describe(active: &[ActiveWorkflowRuns]) -> Option<String> {
    let total: u64 = active.iter().map(|row| row.active).sum();
    if total == 0 {
        return None;
    }
    let names = active
        .iter()
        .filter(|row| row.active > 0)
        .take(10)
        .map(|row| format!("{} ({})", row.workflow, row.active))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Active durable workflows: {total} runs across the reported workflows: {names}. \
         These runs resume on this deployment. Keep existing workflow and step names and \
         replay order compatible; incompatible runs fail with their journal preserved. \
         Inspect affected runs and retry from a compatible step or cancel them if needed."
    ))
}

async fn warning(
    control: &dyn WorkflowControl,
    project_id: i32,
    environment_id: i32,
) -> Option<String> {
    match tokio::time::timeout(Duration::from_secs(5), control.active_runs(project_id, environment_id)).await {
        Ok(Ok(active)) => describe(&active),
        Ok(Err(_)) => Some(format!("Could not check active durable workflows in environment {environment_id}; deployment continues, but replay compatibility has not been checked.")),
        Err(_) => Some(format!("Checking active durable workflows in environment {environment_id} timed out after 5s; deployment continues, but replay compatibility has not been checked.")),
    }
}

impl MarkDeploymentCompleteJob {
    pub(super) async fn warn_about_active_workflows(&self, project_id: i32, environment_id: i32) {
        let Some(control) = &self.workflow_control else {
            return;
        };
        let Some(message) = warning(control.as_ref(), project_id, environment_id).await else {
            return;
        };
        warn!(
            deployment_id = self.deployment_id,
            project_id, environment_id, "{message}"
        );
        if let (Some(log_id), Some(log_service)) = (&self.log_id, &self.log_service) {
            if let Err(error) = log_service
                .append_structured_log(log_id, LogLevel::Warning, message)
                .await
            {
                warn!(deployment_id = self.deployment_id, %error, "could not record the durable workflow deployment warning");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lookup_failure_is_visible_and_a_success_reports_only_active_runs() {
        use crate::services::cron_workflows::fake::FakeWorkflowControl;
        use temps_core::{WorkflowRunPhase, WorkflowStart};
        let control = FakeWorkflowControl::default();
        assert_eq!(warning(&control, 1, 2).await, None);
        let run = control
            .start(WorkflowStart {
                project_id: 1,
                environment_id: 2,
                workflow: "digest".into(),
                input: None,
                idempotency_key: "warning-test".into(),
            })
            .await
            .unwrap();
        assert!(warning(&control, 1, 2)
            .await
            .unwrap()
            .contains("digest (1)"));
        control.finish(run.run_id, WorkflowRunPhase::Completed);
        assert_eq!(warning(&control, 1, 2).await, None);
        *control.refuse.lock().unwrap() = Some("unavailable".into());
        let message = warning(&control, 1, 2).await.unwrap();
        assert!(message.contains("environment 2"));
        assert!(message.contains("Could not check"));
    }

    #[test]
    fn warns_about_active_runs_with_recovery_guidance_and_bounded_names() {
        assert_eq!(describe(&[]), None);
        assert_eq!(
            describe(&[ActiveWorkflowRuns {
                workflow: "finished".into(),
                active: 0
            }]),
            None
        );
        let rows = (0..20)
            .map(|index| ActiveWorkflowRuns {
                workflow: format!("flow-{index}"),
                active: 2,
            })
            .collect::<Vec<_>>();
        let text = describe(&rows).unwrap();
        assert!(text.contains("40 runs"));
        assert!(text.contains("flow-9 (2)"));
        assert!(!text.contains("flow-10"));
        assert!(text.contains("journal preserved"));
        assert!(text.contains("retry from a compatible step"));
    }
}
