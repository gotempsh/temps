// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::deployment_workflows::DeploymentWorkflowsJob;
use async_trait::async_trait;
#[async_trait]
impl FromJobRecord for DeploymentWorkflowsJob {
    const JOB_TYPES: &'static [&'static str] = &["DeploymentWorkflowsJob"];
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let config = record.config()?;
        let control = record
            .service
            .workflow_control
            .get()
            .cloned()
            .ok_or_else(|| {
                WorkflowExecutionError::JobCreationFailed(
                    "Durable workflows service is required for deployment workflow jobs".into(),
                )
            })?;
        Ok(Self {
            job_id: record.db_job.job_id.clone(),
            download_job_id: config
                .get("download_job_id")
                .and_then(|v| v.as_str())
                .unwrap_or("download_repo")
                .into(),
            dependency: record.first_dependency_or("mark_deployment_complete"),
            project_id: record.project.id,
            environment_id: record.environment.id,
            db: record.service.db.clone(),
            control,
            log_id: record.db_job.log_id.clone(),
            log_service: record.service.log_service.clone(),
        })
    }
}
