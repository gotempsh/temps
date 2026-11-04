// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`CaptureSourceMapsJob`] from its `deployment_jobs` row.

use super::{required_i32, required_string, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::CaptureSourceMapsJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for CaptureSourceMapsJob {
    const JOB_TYPES: &'static [&'static str] = &["CaptureSourceMapsJob"];

    /// Requires `deployment_id`, `project_id` and `release`; `build_job_id` defaults
    /// to `build_image`. Fails when the source map service is not bound.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        let config = record.config()?;

        let deployment_id = required_i32(config, "deployment_id")?;

        let project_id = required_i32(config, "project_id")?;

        let release = required_string(config, "release")?;

        let build_job_id = config
            .get("build_job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("build_image")
            .to_string();

        let search_paths: Vec<String> = config
            .get("search_paths")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let path_rewrites: Vec<(String, String)> = config
            .get("path_rewrites")
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

        let job = CaptureSourceMapsJob::new(
            db_job.job_id.clone(),
            deployment_id,
            project_id,
            release,
            build_job_id,
            search_paths,
            path_rewrites,
            service.image_builder.clone(),
            source_map_service,
        )
        .with_log_id(db_job.log_id.clone())
        .with_log_service(service.log_service.clone());
        let job = match service.builder_node_resolver() {
            Some(resolver) => job.with_builder_node_resolver(resolver),
            None => job,
        };

        Ok(job)
    }
}
