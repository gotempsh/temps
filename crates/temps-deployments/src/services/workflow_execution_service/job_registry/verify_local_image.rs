// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`VerifyLocalImageJob`] from its `deployment_jobs` row.

use super::{required_string, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::VerifyLocalImageJob;
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for VerifyLocalImageJob {
    const JOB_TYPES: &'static [&'static str] = &["VerifyLocalImageJob"];

    /// Requires `image_ref`; `expected_image_id` is optional.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        let config = record.config()?;

        let image_ref = required_string(config, "image_ref")?;

        let expected_image_id = config
            .get("expected_image_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let job = VerifyLocalImageJob::new(
            db_job.job_id.clone(),
            image_ref,
            expected_image_id,
            service.docker_handle.clone(),
        )
        .with_log_service(service.log_service.clone(), db_job.log_id.clone());

        Ok(job)
    }
}
