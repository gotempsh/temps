// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`DeployComposeJob`] from its `deployment_jobs` row.

use super::super::compose_service_security_settings;
use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{DeployComposeJob, DeployComposeJobBuilder};
use crate::workflow::WorkflowError;
use async_trait::async_trait;
use sea_orm::EntityTrait;
use std::sync::Arc;

#[async_trait]
impl FromJobRecord for DeployComposeJob {
    const JOB_TYPES: &'static [&'static str] = &["DeployComposeJob"];

    /// Nothing here is required beyond a config object. Sealed values come from the
    /// config, security and placement settings from the project and environment, and
    /// the download job from the first dependency (default `download_repo`). Fails
    /// when the stack's placement cannot be decided, but not on a host with no Docker
    /// daemon: the job refuses that itself, where its row records the reason.
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

        let compose_path = config
            .get("compose_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let env_vars = crate::services::sensitive_envelope::read_sealed(
            config,
            service.encryption_service.get(),
            "environment_vars",
        )
        .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
        let build_args = crate::services::sensitive_envelope::read_sealed(
            config,
            service.encryption_service.get(),
            "build_args",
        )
        .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
        // Absent on jobs queued before compose secret support; the
        // reader returns an empty map, so those deploy unchanged.
        let secrets = crate::services::sensitive_envelope::read_sealed(
            config,
            service.encryption_service.get(),
            "secrets",
        )
        .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
        // Plain JSON, not sealed: service names are not sensitive.
        // Absent on jobs queued before scoping shipped, which reads as
        // "every secret goes to every service" — the prior behaviour.
        let secret_compose_services: std::collections::HashMap<String, Vec<String>> = config
            .get("secret_compose_services")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?
            .unwrap_or_default();

        // Projects created before directory normalization was applied on
        // every write path can hold "" or "/" here. Both are rejected by
        // the job's path confinement, so normalize to the repo root
        // marker instead of failing the deployment.
        let directory = config
            .get("directory")
            .and_then(|v| v.as_str())
            .map(|d| d.trim().trim_start_matches('/'))
            .filter(|d| !d.is_empty())
            .unwrap_or(".")
            .to_string();

        // Get dependencies to find the download job ID
        let download_job_id = record.first_dependency_or("download_repo");

        // Inline compose content (for manual projects without git repo)
        let compose_content = config
            .get("compose_content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // User-provided compose override from project preset_config
        let compose_override = project.preset_config.as_ref().and_then(|pc| {
            if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc {
                cfg.compose_override.clone()
            } else {
                None
            }
        });

        // Services the user opted to exclude from this project's compose
        // stack (e.g. an unmanaged database in favor of a Temps-managed one).
        let (excluded_services, relaxed_capability_services, unsandboxed_services) =
            compose_service_security_settings(project.preset_config.as_ref());
        let public_ports = project
            .preset_config
            .as_ref()
            .and_then(|config| match config {
                temps_entities::preset::PresetConfig::DockerCompose(config) => {
                    Some(config.public_ports.clone())
                }
                _ => None,
            })
            .unwrap_or_default();

        let compose_policy =
            temps_entities::compose_security_policies::Entity::find_by_id(project.id)
                .one(service.db.as_ref())
                .await
                .map_err(|error| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to load Compose security policy for project {}: {error}",
                        project.id
                    ))
                })?
                .map(|row| row.policy)
                .unwrap_or_default();
        // Build the executor from the handle rather than from a
        // resolved client: `ComposeExecutor` already carries the
        // "no daemon here" case (`docker_available()`), and
        // `DeployComposeJob::execute_locked` uses it to refuse with a
        // `LocalWorkloadsDisabled` failure naming the remedy.
        //
        // Resolving the daemon *here* instead would abort job
        // construction, which happens before `WorkflowExecutor`
        // exists — so the deployment would be failed by the outer
        // processor while its already-inserted `deployment_jobs` rows
        // stayed `pending` forever, with no per-job reason anywhere in
        // the UI. Constructing unconditionally keeps the refusal on
        // the job's own execution path, where the tracker records it.
        // Where the stack lives: its pinned home, or (first
        // deployment) the node its Target Nodes / Label Selectors
        // choose. See `services::compose_placement`.
        let (placement_plan, compose_host, compose_vacate_host) = service
            .plan_compose_stack(project, environment, deployment, &compose_policy)
            .await?;
        let compose_executor = Arc::new(
            temps_deployer::compose::ComposeExecutor::new_with_handle(
                service.docker_handle.clone(),
                service.config_service.data_dir(),
            )
            .with_security_policy(compose_policy),
        );

        // Same startup-timeout setting as single-container deploys:
        // environment override, then project, then the default.
        let compose_ready_timeout_secs =
            temps_config::deployment_config::resolve_health_check_timeout_seconds(
                environment
                    .deployment_config
                    .as_ref()
                    .and_then(|c| c.health_check_timeout_seconds)
                    .or_else(|| {
                        project
                            .deployment_config
                            .as_ref()
                            .and_then(|c| c.health_check_timeout_seconds)
                    }),
            );

        let job = DeployComposeJobBuilder::new()
            .job_id(db_job.job_id.clone())
            .deployment_id(deployment.id)
            .project_id(project.id)
            .environment_id(environment.id)
            .db(service.db.clone())
            .compose_executor(compose_executor)
            .compose_path(compose_path)
            .directory(directory)
            .compose_content(compose_content)
            .compose_override(compose_override)
            .excluded_services(excluded_services)
            .relaxed_capability_services(relaxed_capability_services)
            .unsandboxed_services(unsandboxed_services)
            .ready_timeout_secs(compose_ready_timeout_secs)
            .public_ports(public_ports)
            .download_job_id(download_job_id)
            .environment_vars(env_vars)
            .secrets(secrets)
            .secret_compose_services(secret_compose_services)
            .build_args(build_args)
            .log_id(Some(db_job.log_id.clone()))
            .log_service(service.log_service.clone())
            .workload_mode(environment.workload_mode)
            .host(compose_host)
            .vacate_host(compose_vacate_host)
            .pin_home(!placement_plan.pinned)
            .placement_drift(placement_plan.drift)
            .build()?;

        Ok(job)
    }
}
