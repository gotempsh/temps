// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`PrepareSourceBundleJob`] from its `deployment_jobs` row.

use super::{required_string, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::PrepareSourceBundleJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for PrepareSourceBundleJob {
    const JOB_TYPES: &'static [&'static str] = &["PrepareSourceBundleJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            db_job,
            ..
        } = *record;

        let config = record.config()?;
        let archive_path = required_string(config, "archive_path")?;
        let archive_path = std::path::Path::new(&archive_path);
        if archive_path.is_absolute()
            || archive_path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(WorkflowExecutionError::InvalidJobConfig(
                "source archive path must remain inside the Temps data directory".to_string(),
            ));
        }
        let absolute_path = service.config_service.data_dir().join(archive_path);
        Ok(
            PrepareSourceBundleJob::new(db_job.job_id.clone(), absolute_path, project.slug.clone())
                .with_log_service(service.log_service.clone(), db_job.log_id.clone()),
        )
    }
}
