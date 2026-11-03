// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`DeployStaticFromSourceJob`] from its `deployment_jobs` row.

use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::DeployStaticFromSourceJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for DeployStaticFromSourceJob {
    const JOB_TYPES: &'static [&'static str] = &["DeployStaticFromSourceJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            environment,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        let download_job_id = record.first_dependency_or("download_repo");

        let directory = config
            .get("directory")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();

        let deployment = record.reload_deployment().await?;

        let job = DeployStaticFromSourceJob::new(
            db_job.job_id.clone(),
            download_job_id,
            directory,
            project.slug.clone(),
            environment.slug.clone(),
            deployment.slug.clone(),
            service.static_deployer.clone(),
        )
        .with_log_id(db_job.log_id.clone())
        .with_log_service(service.log_service.clone());

        Ok(job)
    }
}
