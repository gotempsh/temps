// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`ConfigureMetricAlertsJob`] from its `deployment_jobs` row.

use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{ConfigureMetricAlertsJob, ConfigureMetricAlertsJobBuilder};
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for ConfigureMetricAlertsJob {
    const JOB_TYPES: &'static [&'static str] = &["ConfigureMetricAlertsJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            environment,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        let download_job_id = config
            .get("download_job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("download_repo")
            .to_string();

        let deploy_container_job_id = record.first_dependency_or("mark_deployment_complete");

        let alert_service = service.alert_service.clone();

        let job = ConfigureMetricAlertsJobBuilder::new()
            .job_id(db_job.job_id.clone())
            .download_job_id(download_job_id)
            .deploy_container_job_id(deploy_container_job_id)
            .project_id(project.id)
            .environment_id(environment.id)
            .log_id(db_job.log_id.clone())
            .log_service(service.log_service.clone())
            .build(service.db.clone(), alert_service)?;

        Ok(job)
    }
}
