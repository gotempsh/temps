// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`MarkDeploymentCompleteJob`] from its `deployment_jobs` row.

use super::{required_i32, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{MarkDeploymentCompleteJob, MarkDeploymentCompleteJobBuilder};
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for MarkDeploymentCompleteJob {
    const JOB_TYPES: &'static [&'static str] = &["MarkDeploymentCompleteJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        let config = record.config()?;

        let deployment_id = required_i32(config, "deployment_id")?;

        let mut builder = MarkDeploymentCompleteJobBuilder::new()
            .environment_addresses(service.environment_addresses.get().cloned())
            .job_id(db_job.job_id.clone())
            .deployment_id(deployment_id)
            .db(service.db.clone())
            .log_id(db_job.log_id.clone())
            .log_service(service.log_service.clone())
            .container_deployer(service.container_deployer.clone())
            .queue(service.queue.clone())
            .config_service(service.config_service.clone())
            // Superseded `enforce` replicas are retired through it.
            .workload_plane(service.workload_plane.get().cloned());

        if let Some(enc_service) = service.encryption_service.get() {
            builder = builder.encryption_service(enc_service.clone());
        }
        // Config service for resolving the cluster CA when deploying to
        // https:// nodes over mTLS (ADR-020 WS-2.1).
        builder = builder.config_service(service.config_service.clone());

        let job = builder.build()?;

        Ok(job)
    }
}
