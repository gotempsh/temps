// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`PersistStaticAssetsJob`] from its `deployment_jobs` row.

use super::{required_i32, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::PersistStaticAssetsJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for PersistStaticAssetsJob {
    const JOB_TYPES: &'static [&'static str] = &["PersistStaticAssetsJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        let config = record.config()?;

        let deployment_id = required_i32(config, "deployment_id")?;

        let project_id = required_i32(config, "project_id")?;

        let environment_id = required_i32(config, "environment_id")?;

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

        let deployment_slug = config
            .get("deployment_slug")
            .and_then(|v| v.as_str())
            .unwrap_or(&deployment_id.to_string())
            .to_string();

        let chunks_dir = service
            .config_service
            .static_dir()
            .join("chunks")
            .join(project_id.to_string())
            .join(environment_id.to_string())
            .join(deployment_slug);

        let mut job = PersistStaticAssetsJob::new(
            db_job.job_id.clone(),
            deployment_id,
            project_id,
            environment_id,
            build_job_id,
            search_paths,
            path_rewrites,
            service.image_builder.clone(),
            chunks_dir,
        )
        .with_log_id(db_job.log_id.clone())
        .with_log_service(service.log_service.clone());

        // Wire file store for CAS blob storage
        if let Some(store) = service.file_store.get() {
            job = job.with_file_store(store.clone());
        }

        // Wire database for URL→hash mapping
        job = job.with_db(service.db.clone());

        if let Some(resolver) = service.builder_node_resolver() {
            job = job.with_builder_node_resolver(resolver);
        }

        Ok(job)
    }
}
