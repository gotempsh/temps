// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`TakeScreenshotJob`] from its `deployment_jobs` row.

use super::{required_i32, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{TakeScreenshotJob, TakeScreenshotJobBuilder};
use async_trait::async_trait;

#[async_trait]
impl FromJobRecord for TakeScreenshotJob {
    const JOB_TYPES: &'static [&'static str] = &["TakeScreenshotJob"];

    /// Requires `deployment_id`.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        // Screenshot service is always available now
        let screenshot_service = &service.screenshot_service;

        let config = record.config()?;

        let deployment_id = required_i32(config, "deployment_id")?;

        let job = TakeScreenshotJobBuilder::new()
            .job_id(db_job.job_id.clone())
            .deployment_id(deployment_id)
            .screenshot_service(screenshot_service.clone())
            .capture_guard(service.capture_guard.clone())
            .config_service(service.config_service.clone())
            .db(service.db.clone())
            .log_id(db_job.log_id.clone())
            .log_service(service.log_service.clone())
            .build()?;

        Ok(job)
    }
}
