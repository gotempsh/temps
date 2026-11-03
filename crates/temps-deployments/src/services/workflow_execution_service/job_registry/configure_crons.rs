// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`ConfigureCronsJob`] from its `deployment_jobs` row.

use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{ConfigureCronsJob, ConfigureCronsJobBuilder};
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for ConfigureCronsJob {
    const JOB_TYPES: &'static [&'static str] = &["ConfigureCronsJob"];

    /// `download_job_id` defaults to `download_repo`; the deploy job is the first
    /// dependency, or `deploy_container`.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            environment,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        // Get download_job_id from config, fallback to default
        let download_job_id = config
            .get("download_job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("download_repo")
            .to_string();

        // Get deploy_container_job_id from dependencies
        let deploy_container_job_id = record.first_dependency_or("deploy_container");

        // Get cron service
        let cron_service = service.cron_service.clone();

        let job = ConfigureCronsJobBuilder::new()
            .job_id(db_job.job_id.clone())
            .download_job_id(download_job_id)
            .deploy_container_job_id(deploy_container_job_id)
            .project_id(project.id)
            .environment_id(environment.id)
            .log_id(db_job.log_id.clone())
            .log_service(service.log_service.clone())
            .build(service.db.clone(), cron_service)?;

        Ok(job)
    }
}
