// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`DeployStaticJob`] from its `deployment_jobs` row.

use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::DeployStaticJob;
use async_trait::async_trait;
use tracing::debug;

#[async_trait]
impl FromJobRecord for DeployStaticJob {
    const JOB_TYPES: &'static [&'static str] = &["DeployStaticJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            environment,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        // Get dependencies to find the build job (DeployStaticJob depends on BuildImageJob)
        let build_job_id = record.first_dependency_or("build_image");

        // Get static output directory from config (path inside container after build)
        // This is typically "/app/dist" or "/app/build" depending on the framework
        let static_output_dir = config
            .get("static_output_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("/app/dist")
            .to_string();

        debug!(
            "📦 DeployStaticJob will extract files from container path: {}",
            static_output_dir
        );

        // Fetch deployment to get slugs
        let deployment = record.reload_deployment().await?;

        let job = DeployStaticJob::new(
            db_job.job_id.clone(),
            build_job_id,
            static_output_dir,
            project.slug.clone(),
            environment.slug.clone(),
            deployment.slug.clone(),
            service.static_deployer.clone(),
            service.image_builder.clone(),
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
