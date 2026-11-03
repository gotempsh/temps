// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`ConfigureAgentsJob`] from its `deployment_jobs` row.

use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{ConfigureAgentsJob, ConfigureAgentsJobBuilder};
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for ConfigureAgentsJob {
    const JOB_TYPES: &'static [&'static str] = &["ConfigureAgentsJob"];

    /// `download_job_id` defaults to `download_repo`; the deploy job is the first
    /// dependency, or `deploy_container`.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        let download_job_id = config
            .get("download_job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("download_repo")
            .to_string();

        let deploy_container_job_id = record.first_dependency_or("deploy_container");

        let agent_sync_service = service.agent_sync_service.clone();

        let job = ConfigureAgentsJobBuilder::new()
            .job_id(db_job.job_id.clone())
            .download_job_id(download_job_id)
            .deploy_container_job_id(deploy_container_job_id)
            .project_id(project.id)
            .log_id(Some(db_job.log_id.clone()))
            .log_service(service.log_service.clone())
            .build(service.db.clone(), agent_sync_service)?;

        Ok(job)
    }
}
