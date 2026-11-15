// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`DeployImageJob`] from its `deployment_jobs` row.

use super::super::resolve_placement_constraints;
use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{
    DeployImageJob, DeployImageJobBuilder, DeploymentTarget, PullExternalImageJob, ResourceUsage,
};
use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use temps_entities::deployments;
use tracing::{debug, info, warn};

#[async_trait]
impl FromJobRecord for DeployImageJob {
    const JOB_TYPES: &'static [&'static str] = &["DeployContainerJob", "DeployImageJob"];

    /// Takes the build job from the first dependency (default `build_image`) and the
    /// port and sealed values from the config. Replicas come from the environment,
    /// then the project, then the config, then 1; resources and health settings from
    /// the environment, then the project.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            environment,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        let port = config.get("port").and_then(|v| v.as_i64()).unwrap_or(3000) as u16;

        // Explicit environment/project port override, as resolved by the
        // planner. `Some` here means `resolve_container_port()` must use
        // it directly instead of falling back to image EXPOSE detection.
        let configured_port = config
            .get("configured_port")
            .and_then(|v| v.as_i64())
            .map(|p| p as u16);

        // Get replicas with priority: environment > project > job config > default (1)
        let replicas = environment
            .deployment_config
            .as_ref()
            .map(|c| c.replicas as u32)
            .or_else(|| {
                project
                    .deployment_config
                    .as_ref()
                    .map(|c| c.replicas as u32)
            })
            .or_else(|| {
                config
                    .get("replicas")
                    .and_then(|v| v.as_i64())
                    .map(|r| r as u32)
            })
            .unwrap_or(1);

        debug!(
            "🔢 Deploying with {} replicas (env: {:?}, project: {:?})",
            replicas,
            environment.deployment_config.as_ref().map(|c| c.replicas),
            project.deployment_config.as_ref().map(|c| c.replicas)
        );

        // Unseal sensitive maps from job_config. The planner stores
        // env vars / remote env vars / secrets as AES-256-GCM
        // ciphertext under `*_encrypted` keys (with a non-sensitive
        // `*_keys` list for debugging). `read_sealed` also falls back
        // to plaintext for jobs queued before the encryption rollout.
        let enc_for_unseal = service.encryption_service.get();
        let env_variables = crate::services::sensitive_envelope::read_sealed(
            config,
            enc_for_unseal,
            "environment_variables",
        )
        .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;

        let remote_env_variables = crate::services::sensitive_envelope::read_sealed_optional(
            config,
            enc_for_unseal,
            "remote_environment_variables",
        )
        .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;

        // Secrets are mounted as files under /run/secrets/<KEY>; never
        // injected as env vars. Same envelope as above.
        let secrets =
            crate::services::sensitive_envelope::read_sealed(config, enc_for_unseal, "secrets")
                .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
        if !secrets.is_empty() {
            debug!(
                "🔐 Mounting {} secret file(s) into container: {}",
                secrets.len(),
                secrets.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        debug!(
            "🌍 Using {} environment variables for deployment (from job config): {}",
            env_variables.len(),
            env_variables.keys().cloned().collect::<Vec<_>>().join(", ")
        );

        // Get dependencies to find the build job
        let build_job_id = record.first_dependency_or("build_image");

        // Fetch deployment to get its URL for naming
        let deployment = record.reload_deployment().await?;

        // Check if this is an external image deployment (from remote deployment)
        let use_external_image = config
            .get("use_external_image")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let external_image_tag = if use_external_image {
            config
                .get("image_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        // Same constraints the pre-build placement check used
        let (target_nodes, target_labels) = resolve_placement_constraints(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );

        // Resolve CPU/memory limits + requests from environment first,
        // then project. Each field is resolved independently so an
        // env override can supply only memory while inheriting the
        // project's CPU. When neither side configures a value we leave
        // it unset and the deployer applies no limit at the Docker
        // layer (rather than the previous hardcoded 1000m/512Mi).
        let env_cfg = environment.deployment_config.as_ref();
        let proj_cfg = project.deployment_config.as_ref();
        let resolve_i32 = |getter: fn(
            &temps_entities::deployment_config::DeploymentConfig,
        ) -> Option<i32>|
         -> Option<i32> {
            env_cfg
                .and_then(getter)
                .or_else(|| proj_cfg.and_then(getter))
        };
        // CPU values are stored as microcores in the DB (1_000_000 = 1 core);
        // memory is stored as MB. Format with explicit suffixes the deployer
        // understands: `u` for microcores, `Mi` for mebibytes.
        let cpu_limit_micro = resolve_i32(|c| c.cpu_limit);
        let memory_limit_mb = resolve_i32(|c| c.memory_limit);
        let cpu_request_micro = resolve_i32(|c| c.cpu_request);
        let memory_request_mb = resolve_i32(|c| c.memory_request);
        // Readiness budget: environment override, then project, then
        // the platform default. A slow-booting app gets more time by
        // raising this, instead of failing as a generic timeout.
        let health_check_timeout_secs =
            temps_config::deployment_config::resolve_health_check_timeout_seconds(resolve_i32(
                |c| c.health_check_timeout_seconds,
            ));
        let resources = ResourceUsage {
            cpu_limit: cpu_limit_micro.map(|u| format!("{}u", u)),
            memory_limit: memory_limit_mb.map(|mb| format!("{}Mi", mb)),
            cpu_request: cpu_request_micro.map(|u| format!("{}u", u)),
            memory_request: memory_request_mb.map(|mb| format!("{}Mi", mb)),
        };

        // ADR 045: the executing host compares this against its own
        // grant. A constructor argument, so no deploy path can omit it.
        //
        // The authority comes from the plan, not from an `AuthContext`
        // — there is no request here, this runs from the queue. The
        // planner recorded whether the principal that asked for the
        // deployment was allowed to deploy a host-root project, and
        // `planned_deploy_caller` fails closed when it did not say.
        let mut builder = DeployImageJobBuilder::new(
            project.slug.clone(),
            crate::services::workflow_planner::planned_deploy_caller(config),
        )
        .job_id(db_job.job_id.clone())
        .build_job_id(build_job_id)
        .target(DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: Some(temps_core::NETWORK_NAME.to_string()),
        })
        .service_name(deployment.slug.clone())
        .namespace("default".to_string())
        .audit_logger(service.audit_logger.get().cloned())
        .port(port as u32)
        .configured_port(configured_port)
        .health(
            env_cfg
                .and_then(|c| c.health.clone())
                .or_else(|| proj_cfg.and_then(|c| c.health.clone())),
        )
        .health_check_timeout_secs(health_check_timeout_secs)
        .replicas(replicas)
        .environment_variables(env_variables)
        .remote_environment_variables(remote_env_variables)
        .cross_node_service_blockers(crate::services::workflow_planner::read_cross_node_blockers(
            config,
        ))
        .secrets(secrets)
        .resources(resources)
        .log_id(db_job.log_id.clone())
        .log_service(service.log_service.clone())
        .failed_container_retention(service.db.clone(), deployment.id);

        let runtime = deployment
            .metadata
            .as_ref()
            .map(|metadata| metadata.container_runtime())
            .unwrap_or_default();
        builder = builder
            .command(runtime.command)
            .entrypoint(runtime.entrypoint);

        // Apply explicit deploy-time health-check path override (image/static
        // deploys can't read .temps.yaml, so the deploy request carries it on
        // the deployment metadata). When set, it wins over .temps.yaml.
        if let Some(health_path) = deployment
            .metadata
            .as_ref()
            .and_then(|m| m.health_check_path.clone())
        {
            debug!(
                "🩺 Applying deploy-time health check path override: {}",
                health_path
            );
            builder = builder.health_check_path_override(Some(health_path));
        }

        // Inject node scheduler for multi-node support
        if let Some(scheduler) = service.node_scheduler.get() {
            builder = builder.node_scheduler(scheduler.clone());
        }

        // Inject encryption service for decrypting node tokens
        if let Some(enc_service) = service.encryption_service.get() {
            builder = builder.encryption_service(enc_service.clone());
        }
        // Config service for resolving the cluster CA when deploying to
        // https:// nodes over mTLS (ADR-020 WS-2.1).
        builder = builder.config_service(service.config_service.clone());

        // Inject image builder for transferring images to remote nodes
        builder = builder.image_builder(service.image_builder.clone());

        // Set target nodes if configured
        if let Some(nodes) = target_nodes {
            builder = builder.target_nodes(nodes);
        }

        // Set target labels for label-based node scheduling
        if let Some(labels) = target_labels {
            builder = builder.target_labels(labels);
        }

        // Resolve anti-affinity from environment or project config
        let anti_affinity = environment
            .deployment_config
            .as_ref()
            .map(|c| c.anti_affinity)
            .unwrap_or_else(|| {
                project
                    .deployment_config
                    .as_ref()
                    .map(|c| c.anti_affinity)
                    .unwrap_or(true)
            });
        builder = builder.anti_affinity(anti_affinity);

        builder = apply_workload_mode(record, builder, &deployment).await?;

        builder = apply_environment_volumes(record, builder, &deployment).await?;

        // Rolling update awareness: query existing containers for this
        // environment so the scheduler can avoid placing new replicas on
        // nodes that still host outgoing containers.
        if anti_affinity {
            builder = exclude_outgoing_nodes(record, builder, &deployment).await;
        }

        // If using external image, set the image tag directly (bypasses build job lookup)
        if let Some(ref image_tag) = external_image_tag {
            debug!("🐳 Using external image tag for deployment: {}", image_tag);
            builder = builder.external_image_tag(image_tag.clone());
        }

        // Where the image lives (registry vs. only on this control
        // plane) — decides whether a remote worker pulls it or receives
        // it via import. Absent on job configs planned before the field
        // existed; the job then derives it (see `DeployImageSource::resolve`).
        builder = builder.image_source_from_job_config(config);

        builder = apply_docker_settings(record, builder, external_image_tag.as_deref()).await;

        let job = builder.build(service.container_deployer.clone())?;

        Ok(job)
    }
}

/// The environment's workload mode, and for the modes that read it, its
/// isolation.
async fn apply_workload_mode(
    record: &JobRecord<'_>,
    mut builder: DeployImageJobBuilder,
    deployment: &deployments::Model,
) -> Result<DeployImageJobBuilder, WorkflowExecutionError> {
    let JobRecord {
        service,
        environment,
        ..
    } = *record;

    // ADR-055: the environment's workload mode. `legacy` gets
    // nothing extra, so its deploy path is exactly today's.
    let workload_mode = environment.workload_mode;
    let workload_plane = match (workload_mode, service.workload_placement.get()) {
        (temps_entities::environment_workload_mode::EnvironmentWorkloadMode::Legacy, _)
        | (_, None) => None,
        (_, Some(placement)) => Some(crate::jobs::WorkloadPlaneDeploy {
            placement: placement.clone(),
            plane: service.workload_plane.get().cloned(),
        }),
    };
    builder = builder.workload_mode(workload_mode, workload_plane);
    // ADR-055 Phase 10: only `enforce` runs replicas through the
    // plane, so only it honours the isolation setting. `legacy`
    // does not even read it.
    let reads_isolation =
        workload_mode != temps_entities::environment_workload_mode::EnvironmentWorkloadMode::Legacy;
    if let Some(placement) = service.workload_placement.get().filter(|_| reads_isolation) {
        let isolation = placement
            .environment_workload_isolation(environment.id)
            .await
            .map_err(|e| {
                WorkflowExecutionError::InvalidJobConfig(format!(
                    "Deployment {}: cannot read the workload isolation of \
                     environment {}: {e}",
                    deployment.id, environment.id
                ))
            })?;
        if workload_mode
            == temps_entities::environment_workload_mode::EnvironmentWorkloadMode::Enforce
        {
            builder = builder.workload_isolation(
                crate::services::workload_placement::isolation_for(isolation),
            );
        } else if isolation
            != temps_entities::environment_workload_isolation::EnvironmentWorkloadIsolation::Process
        {
            info!(
                deployment_id = deployment.id,
                environment_id = environment.id,
                workload_isolation = isolation.as_str(),
                workload_mode = workload_mode.as_str(),
                "workload isolation is ignored outside workload_mode enforce; \
                 deploying a container"
            );
        }
    }

    Ok(builder)
}

/// The environment's attached volumes, and the node pin they impose. Unreadable
/// volumes fail the deploy: a replica must never start without its data.
async fn apply_environment_volumes(
    record: &JobRecord<'_>,
    mut builder: DeployImageJobBuilder,
    deployment: &deployments::Model,
) -> Result<DeployImageJobBuilder, WorkflowExecutionError> {
    let JobRecord {
        service,
        environment,
        ..
    } = *record;

    // ADR-055 Phase 9: the environment's attached volumes. One
    // indexed query; empty for every environment without one,
    // which then deploys exactly as before. Unreadable volumes
    // fail the deploy: a replica must never start without its
    // data.
    let volume_owner = crate::services::workload_volumes::environment_volume_owner(environment.id);
    let volume_records = temps_workload_plane::VolumeStore::new(service.db.clone())
        .all_of(&volume_owner)
        .await
        .map_err(|e| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "reading the volumes of environment {} for deployment {}: {e}",
                environment.id, deployment.id
            ))
        })?;
    let volume_mounts: Vec<temps_workload::VolumeMount> = volume_records
        .iter()
        .filter_map(temps_workload_plane::VolumeRecord::mount)
        .collect();
    if !volume_mounts.is_empty() {
        let pin = temps_workload_plane::volumes::pin_for(
            temps_workload::WorkloadRef::new(
                temps_workload::WorkloadKind::Deployment,
                i64::from(deployment.id),
            ),
            &volume_mounts,
            &volume_records,
            None,
        )
        .map_err(|e| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "the volumes of environment {} cannot be mounted by deployment {}: {e}",
                environment.id, deployment.id
            ))
        })?;
        builder = builder.volume_mounts(volume_mounts, pin);
    }

    Ok(builder)
}

/// Keep new replicas off the nodes of the deployment they replace. Best
/// effort: a failed lookup excludes nothing.
async fn exclude_outgoing_nodes(
    record: &JobRecord<'_>,
    mut builder: DeployImageJobBuilder,
    deployment: &deployments::Model,
) -> DeployImageJobBuilder {
    let service = record.service;

    use temps_entities::deployment_containers;

    // Find the most recent previous deployment for this environment
    let prev_deployment = deployments::Entity::find()
        .filter(deployments::Column::EnvironmentId.eq(deployment.environment_id))
        .filter(deployments::Column::ProjectId.eq(deployment.project_id))
        .filter(deployments::Column::Id.ne(deployment.id))
        .filter(deployments::Column::State.eq("deployed"))
        .order_by_desc(deployments::Column::CreatedAt)
        .one(service.db.as_ref())
        .await
        .ok()
        .flatten();

    if let Some(prev) = prev_deployment {
        let existing_containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(prev.id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(service.db.as_ref())
            .await
            .unwrap_or_default();

        let exclude_ids: Vec<i32> = existing_containers
            .iter()
            .filter_map(|c| c.node_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        if !exclude_ids.is_empty() {
            debug!(
                "Anti-affinity: excluding {} node(s) with existing containers: {:?}",
                exclude_ids.len(),
                exclude_ids
            );
            builder = builder.exclude_node_ids(exclude_ids);
        }
    }

    builder
}

/// Settings that are skipped when they cannot be read: container log rotation
/// and registry credentials.
async fn apply_docker_settings(
    record: &JobRecord<'_>,
    mut builder: DeployImageJobBuilder,
    external_image_tag: Option<&str>,
) -> DeployImageJobBuilder {
    let service = record.service;

    // Apply container log rotation settings from config, and — for a
    // registry-sourced image — the same private-registry credentials
    // `PullExternalImageJob` uses, so a worker node can pull the image
    // itself via `POST /agent/images/pull` without the control plane
    // ever needing a Docker daemon. Only forwarded when the image's
    // registry matches the configured registry (same matching rule as
    // `PullExternalImageJob`, never send credentials to a registry that
    // didn't ask for them).
    if let Ok(settings) = service.config_service.get_settings().await {
        builder = builder.container_log_config(temps_deployer::ContainerLogConfig::new(
            settings.container_logs.max_size.clone(),
            settings.container_logs.max_file,
        ));

        if let Some(image_tag) = external_image_tag {
            let reg = &settings.docker_registry;
            if reg.enabled {
                if let (Some(username), Some(password), Some(registry_url)) = (
                    reg.username.clone(),
                    reg.password.clone(),
                    reg.registry_url.clone(),
                ) {
                    let image_registry = PullExternalImageJob::registry_from_image_ref(image_tag);
                    let configured_registry =
                        PullExternalImageJob::registry_host_from_url(&registry_url);
                    if matches!(
                        (&image_registry, &configured_registry),
                        (Some(image_registry), Some(configured_registry))
                            if image_registry == configured_registry
                    ) {
                        builder = builder.registry_credentials(
                            temps_deployer::remote::RemotePullCredentials {
                                username: Some(username),
                                password: Some(password),
                                identity_token: None,
                                server_address: Some(registry_url),
                            },
                        );
                    } else {
                        warn!(
                            image_registry = ?image_registry,
                            configured_registry = ?configured_registry,
                            "Skipping Docker registry credentials for remote pull because the image registry does not match"
                        );
                    }
                }
            }
        }
    }

    builder
}
