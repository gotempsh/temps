// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment drain pauses claims, not event delivery or journal completion.
//! The short database lease recovers automatically if the deployment is killed.
use super::MarkDeploymentCompleteJob;
use crate::workflow::WorkflowError;
use std::{sync::Arc, time::Duration};
use temps_core::{WorkflowControl, WorkflowDrain};
use uuid::Uuid;

pub(super) struct DrainGuard {
    control: Arc<dyn WorkflowControl>,
    project_id: i32,
    environment_id: i32,
    token: Uuid,
    pulse: tokio::task::JoinHandle<()>,
}
impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.pulse.abort();
    }
}
impl DrainGuard {
    pub(super) async fn check(&self) -> Result<(), WorkflowError> {
        let active = renew(
            self.control.as_ref(),
            self.project_id,
            self.environment_id,
            self.token,
        )
        .await?;
        if active > 0 {
            return Err(WorkflowError::JobExecutionFailed(format!("Workflow drain of environment {} has {active} running invocations; deployment was not promoted",self.environment_id)));
        }
        Ok(())
    }
    pub(super) async fn finish(self) {
        self.pulse.abort();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            self.control
                .end_deploy_drain(self.project_id, self.environment_id, self.token),
        )
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(
                project_id = self.project_id,
                environment_id = self.environment_id,
                "Could not release workflow drain; its lease expires within 90 seconds"
            );
        }
    }
}
async fn renew(
    control: &dyn WorkflowControl,
    project_id: i32,
    environment_id: i32,
    token: Uuid,
) -> Result<u64, WorkflowError> {
    tokio::time::timeout(
        Duration::from_secs(5),
        control.renew_deploy_drain(project_id, environment_id, token),
    )
    .await
    .map_err(|_| {
        WorkflowError::JobExecutionFailed(format!(
            "Renewing workflow drain of environment {environment_id} timed out after 5 seconds"
        ))
    })?
    .map_err(|e| WorkflowError::JobExecutionFailed(e.to_string()))
}
async fn wait(
    control: &dyn WorkflowControl,
    project_id: i32,
    environment_id: i32,
    drain: WorkflowDrain,
) -> Result<(), WorkflowError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(u64::from(drain.timeout_secs));
    loop {
        let active = renew(control, project_id, environment_id, drain.token).await?;
        if active == 0 {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(WorkflowError::JobExecutionFailed(format!("Workflow drain of environment {environment_id} timed out with {active} running invocations after {} seconds; old deployment remains active",drain.timeout_secs)));
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_secs(1)).min(deadline),
        )
        .await;
    }
}
impl MarkDeploymentCompleteJob {
    pub(super) async fn drain_workflow_invocations(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Option<DrainGuard>, WorkflowError> {
        let Some(control) = &self.workflow_control else {
            return Ok(None);
        };
        let drain = tokio::time::timeout(
            Duration::from_secs(5),
            control.begin_deploy_drain(project_id, environment_id, self.deployment_id),
        )
        .await
        .map_err(|_| {
            WorkflowError::JobExecutionFailed(format!(
                "Starting workflow drain of environment {environment_id} timed out"
            ))
        })?
        .map_err(|e| WorkflowError::JobExecutionFailed(e.to_string()))?;
        let Some(drain) = drain else {
            return Ok(None);
        };
        if let Err(error) = self.log(format!("Draining running workflow invocations for up to {} seconds; queued and suspended runs resume after promotion",drain.timeout_secs)).await {
            let _=tokio::time::timeout(Duration::from_secs(5),control.end_deploy_drain(project_id,environment_id,drain.token)).await;
            return Err(error);
        }
        if let Err(error) = wait(control.as_ref(), project_id, environment_id, drain).await {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                control.end_deploy_drain(project_id, environment_id, drain.token),
            )
            .await;
            return Err(error);
        }
        let service = control.clone();
        let token = drain.token;
        let pulse = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                if renew(service.as_ref(), project_id, environment_id, token)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        Ok(Some(DrainGuard {
            control: control.clone(),
            project_id,
            environment_id,
            token,
            pulse,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cron_workflows::fake::FakeWorkflowControl;
    use std::sync::atomic::Ordering;
    #[tokio::test]
    async fn drain_waits_for_active_attempts_and_reports_timeout_or_lost_lease() {
        let control = Arc::new(FakeWorkflowControl::default());
        let drain = WorkflowDrain {
            token: Uuid::new_v4(),
            timeout_secs: 0,
        };
        wait(control.as_ref(), 1, 2, drain).await.unwrap();
        control.drain_active.store(2, Ordering::SeqCst);
        let error = wait(control.as_ref(), 1, 2, drain).await.unwrap_err();
        assert!(error.to_string().contains("2 running invocations"));
        assert!(error.to_string().contains("old deployment remains active"));
        let service = control.clone();
        let finish = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            service.drain_active.store(0, Ordering::SeqCst);
        });
        wait(
            control.as_ref(),
            1,
            2,
            WorkflowDrain {
                timeout_secs: 2,
                ..drain
            },
        )
        .await
        .unwrap();
        finish.await.unwrap();
        control.drain_fail.store(true, Ordering::SeqCst);
        assert!(wait(control.as_ref(), 1, 2, drain)
            .await
            .unwrap_err()
            .to_string()
            .contains("lease expired"));
    }
}
