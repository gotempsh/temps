// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`DeployStaticBundleJob`] from its `deployment_jobs` row.

use super::{required_string, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::DeployStaticBundleJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for DeployStaticBundleJob {
    const JOB_TYPES: &'static [&'static str] = &["DeployStaticBundleJob"];

    /// Requires `bundle_path`; `content_type` defaults to `application/gzip` and
    /// `source_directory` to `.`.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            environment,
            deployment,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        let bundle_path = required_string(config, "bundle_path")?;

        let content_type = config
            .get("content_type")
            .and_then(|v| v.as_str())
            .unwrap_or("application/gzip")
            .to_string();

        let static_bundle_id = config
            .get("static_bundle_id")
            .and_then(|v| v.as_i64())
            .map(|id| id as i32);
        let source_directory = config
            .get("source_directory")
            .and_then(|value| value.as_str())
            .unwrap_or(".")
            .to_string();

        // Get data directory from config service for local storage
        let data_dir = service.config_service.data_dir();

        let job = DeployStaticBundleJob::new(
            db_job.job_id.clone(),
            project.id,
            bundle_path,
            content_type,
            static_bundle_id,
            project.slug.clone(),
            environment.slug.clone(),
            deployment.slug.clone(),
            source_directory,
            data_dir,
            service.static_deployer.clone(),
        )
        .with_log_service(service.log_service.clone(), db_job.log_id.clone());

        Ok(job)
    }
}
