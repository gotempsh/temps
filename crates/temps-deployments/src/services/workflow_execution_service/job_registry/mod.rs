// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turns a persisted `deployment_jobs` row into the workflow job it describes.
//!
//! `deployment_jobs.job_type` is free-form text, so nothing but this table
//! decides what a string means. Each job type constructs itself by implementing
//! [`FromJobRecord`] in its own file here. To add a job, implement the trait in
//! a new file, add one line to [`JOB_REGISTRY`], and add its rows to the table
//! test in the parent module.
//!
//! These files live under `workflow_execution_service` rather than beside the
//! jobs because constructing a job needs the service's dependencies: as child
//! modules they read its private fields, so nothing is widened to reach them
//! and `jobs` does not depend on the service.

use super::{WorkflowExecutionError, WorkflowExecutionService};
use crate::workflow::WorkflowTask;
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::sync::Arc;
use temps_entities::{deployment_jobs, deployments, environments, projects};

mod build_image;
mod capture_source_files;
mod capture_source_maps;
mod configure_agents;
mod configure_crons;
mod configure_metric_alerts;
mod deploy_compose;
mod deploy_static;
mod deploy_static_bundle;
mod deploy_static_from_source;
mod download_repo;
mod mark_deployment_complete;
mod persist_static_assets;
mod prepare_source_bundle;
mod pull_external_image;
mod scan_vulnerabilities;
mod take_screenshot;
mod verify_local_image;

/// One `deployment_jobs` row and everything needed to build its job.
pub(super) struct JobRecord<'a> {
    pub(super) service: &'a WorkflowExecutionService,
    pub(super) project: &'a projects::Model,
    pub(super) environment: &'a environments::Model,
    pub(super) deployment: &'a deployments::Model,
    pub(super) db_job: &'a deployment_jobs::Model,
}

impl JobRecord<'_> {
    /// The row's `job_config`, which every job type requires.
    fn config(&self) -> Result<&serde_json::Value, WorkflowExecutionError> {
        self.db_job
            .job_config
            .as_ref()
            .ok_or_else(|| WorkflowExecutionError::MissingJobConfig(self.db_job.job_id.clone()))
    }

    /// The job this one reads its input from: its first dependency, or
    /// `default` when the row names none.
    fn first_dependency_or(&self, default: &str) -> String {
        let dependencies: Vec<String> = self
            .db_job
            .dependencies
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        dependencies
            .first()
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    /// The row's deployment as it is now, rather than as the caller loaded it.
    async fn reload_deployment(&self) -> Result<deployments::Model, WorkflowExecutionError> {
        use sea_orm::EntityTrait;

        deployments::Entity::find_by_id(self.db_job.deployment_id)
            .one(self.service.db.as_ref())
            .await?
            .ok_or(WorkflowExecutionError::DeploymentNotFound(
                self.db_job.deployment_id,
            ))
    }
}

/// A job config field that must hold an integer.
fn required_i32(config: &serde_json::Value, field: &str) -> Result<i32, WorkflowExecutionError> {
    config
        .get(field)
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .ok_or_else(|| WorkflowExecutionError::InvalidJobConfig(format!("{field} is required")))
}

/// A job config field that must hold a string.
fn required_string(
    config: &serde_json::Value,
    field: &str,
) -> Result<String, WorkflowExecutionError> {
    config
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| WorkflowExecutionError::InvalidJobConfig(format!("{field} is required")))
}

/// A workflow job that can build itself from its `deployment_jobs` row.
#[async_trait]
trait FromJobRecord: WorkflowTask + Sized + 'static {
    /// The `deployment_jobs.job_type` strings that mean this job.
    const JOB_TYPES: &'static [&'static str];

    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError>;
}

type BuildJob = for<'a> fn(
    &'a JobRecord<'a>,
) -> BoxFuture<'a, Result<Arc<dyn WorkflowTask>, WorkflowExecutionError>>;

struct JobRegistration {
    job_types: &'static [&'static str],
    build: BuildJob,
}

const fn register<J: FromJobRecord>() -> JobRegistration {
    JobRegistration {
        job_types: J::JOB_TYPES,
        build: build_job::<J>,
    }
}

fn build_job<'a, J: FromJobRecord>(
    record: &'a JobRecord<'a>,
) -> BoxFuture<'a, Result<Arc<dyn WorkflowTask>, WorkflowExecutionError>> {
    Box::pin(async move { Ok(Arc::new(J::from_record(record).await?) as Arc<dyn WorkflowTask>) })
}

/// Every job a `deployment_jobs` row can build.
const JOB_REGISTRY: &[JobRegistration] = &[
    register::<crate::jobs::ConfigureCronsJob>(),
    register::<crate::jobs::ConfigureMetricAlertsJob>(),
    register::<crate::jobs::ConfigureAgentsJob>(),
    register::<crate::jobs::MarkDeploymentCompleteJob>(),
    register::<crate::jobs::TakeScreenshotJob>(),
    register::<crate::jobs::ScanVulnerabilitiesJob>(),
    register::<crate::jobs::CaptureSourceMapsJob>(),
    register::<crate::jobs::CaptureSourceFilesJob>(),
    register::<crate::jobs::PersistStaticAssetsJob>(),
    register::<crate::jobs::PullExternalImageJob>(),
    register::<crate::jobs::VerifyLocalImageJob>(),
    register::<crate::jobs::PrepareSourceBundleJob>(),
    register::<crate::jobs::DeployStaticJob>(),
    register::<crate::jobs::DeployStaticFromSourceJob>(),
    register::<crate::jobs::DeployStaticBundleJob>(),
    register::<crate::jobs::DownloadRepoJob>(),
    register::<crate::jobs::DeployComposeJob>(),
    register::<crate::jobs::BuildImageJob>(),
];

fn registration_for(job_type: &str) -> Option<&'static JobRegistration> {
    JOB_REGISTRY
        .iter()
        .find(|registration| registration.job_types.contains(&job_type))
}

/// Build the job a row describes, or `None` when no job claims its type.
pub(super) async fn build_registered_job(
    record: &JobRecord<'_>,
) -> Option<Result<Arc<dyn WorkflowTask>, WorkflowExecutionError>> {
    let registration = registration_for(record.db_job.job_type.as_str())?;
    Some((registration.build)(record).await)
}
