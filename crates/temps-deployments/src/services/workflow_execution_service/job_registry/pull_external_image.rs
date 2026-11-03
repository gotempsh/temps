// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`PullExternalImageJob`] from its `deployment_jobs` row.

use super::{required_string, FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::PullExternalImageJob;
use async_trait::async_trait;
use tracing::warn;

#[async_trait]
impl FromJobRecord for PullExternalImageJob {
    const JOB_TYPES: &'static [&'static str] = &["PullExternalImageJob"];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service, db_job, ..
        } = *record;

        let config = record.config()?;

        let image_ref = required_string(config, "image_ref")?;

        let external_image_id = config
            .get("external_image_id")
            .and_then(|v| v.as_i64())
            .map(|id| id as i32);

        let image_registry = PullExternalImageJob::registry_from_image_ref(&image_ref);

        let mut job = PullExternalImageJob::new(
            db_job.job_id.clone(),
            image_ref,
            external_image_id,
            service.docker_handle.clone(),
        )
        .with_log_service(service.log_service.clone(), db_job.log_id.clone());

        // Pass private registry credentials only to the configured registry.
        if let Ok(settings) = service.config_service.get_settings().await {
            let reg = &settings.docker_registry;
            if reg.enabled {
                if let (Some(username), Some(password), Some(registry_url)) = (
                    reg.username.clone(),
                    reg.password.clone(),
                    reg.registry_url.clone(),
                ) {
                    let configured_registry =
                        PullExternalImageJob::registry_host_from_url(&registry_url);
                    if matches!(
                        (&image_registry, &configured_registry),
                        (Some(image_registry), Some(configured_registry))
                            if image_registry == configured_registry
                    ) {
                        job = job.with_registry_credentials(bollard::auth::DockerCredentials {
                            username: Some(username),
                            password: Some(password),
                            serveraddress: Some(registry_url),
                            ..Default::default()
                        });
                    } else {
                        warn!(
                            image_registry = ?image_registry,
                            configured_registry = ?configured_registry,
                            "Skipping Docker registry credentials because the image registry does not match"
                        );
                    }
                }
            }
        }

        Ok(job)
    }
}
