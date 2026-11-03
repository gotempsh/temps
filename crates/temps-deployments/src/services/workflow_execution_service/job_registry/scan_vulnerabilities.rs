// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`ScanVulnerabilitiesJob`] from its `deployment_jobs` row.

use super::{required_i32, required_string, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::ScanVulnerabilitiesJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for ScanVulnerabilitiesJob {
    const JOB_TYPES: &'static [&'static str] = &["ScanVulnerabilitiesJob"];

    /// Requires `deployment_id`, `project_id`, `environment_id`, `branch`,
    /// `download_job_id` and `build_job_id`. `commit_hash` is optional: manual
    /// triggers have none.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        let config = record.config()?;

        let deployment_id = required_i32(config, "deployment_id")?;

        let project_id = required_i32(config, "project_id")?;

        let environment_id = required_i32(config, "environment_id")?;

        let branch = required_string(config, "branch")?;

        // Manual triggers (redeploy, import) have no commit — the
        // scan still runs against the built image; the commit is
        // only recorded as scan metadata when known.
        let commit_hash = config
            .get("commit_hash")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let download_job_id = required_string(config, "download_job_id")?;

        let build_job_id = required_string(config, "build_job_id")?;

        let job = ScanVulnerabilitiesJob::new(
            db_job.job_id.clone(),
            deployment_id,
            project_id,
            environment_id,
            branch,
            commit_hash,
            download_job_id,
            build_job_id,
            service.db.clone(),
            service.docker_handle.clone(),
        )
        .with_log_id(db_job.log_id.clone())
        .with_log_service(service.log_service.clone())
        .with_image_builder(service.image_builder.clone());
        let job = match service.builder_node_resolver() {
            Some(resolver) => job.with_builder_node_resolver(resolver),
            None => job,
        };

        Ok(job)
    }
}
