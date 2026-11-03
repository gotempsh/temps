// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`CaptureSourceFilesJob`] from its `deployment_jobs` row.

use super::{required_i32, required_string, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::CaptureSourceFilesJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for CaptureSourceFilesJob {
    const JOB_TYPES: &'static [&'static str] = &["CaptureSourceFilesJob"];

    /// Requires `project_id` and `release`; `download_job_id` defaults to
    /// `download_repo` and `build_job_id` to `build_image`. Fails when the source map
    /// service is not bound.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        let config = record.config()?;

        let project_id = required_i32(config, "project_id")?;

        let release = required_string(config, "release")?;

        let download_job_id = config
            .get("download_job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("download_repo")
            .to_string();

        let build_job_id = config
            .get("build_job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("build_image")
            .to_string();

        // None (or null) = default to the Docker build context.
        let error_source_root = config
            .get("error_source_root")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let extensions: Vec<String> = config
            .get("extensions")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let source_map_service = service
            .source_map_service
            .get()
            .ok_or_else(|| {
                WorkflowExecutionError::InvalidJobConfig(
                    "SourceMapService not configured".to_string(),
                )
            })?
            .clone();

        let job = CaptureSourceFilesJob::new(
            db_job.job_id.clone(),
            project_id,
            release,
            download_job_id,
            build_job_id,
            error_source_root,
            extensions,
            source_map_service,
        )
        .with_log_id(db_job.log_id.clone())
        .with_log_service(service.log_service.clone());

        Ok(job)
    }
}
