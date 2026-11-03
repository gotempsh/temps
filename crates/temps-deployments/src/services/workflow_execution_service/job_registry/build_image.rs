// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build a [`BuildImageJob`] from its `deployment_jobs` row, which decides who
//! builds the image.

use super::super::{
    inlined_variables_allow_offload, resolve_placement_constraints, withheld_inlined_variables,
};
use super::{FromJobRecord, JobRecord, WorkflowExecutionError};
use crate::jobs::{BuildImageJob, BuildImageJobBuilder, OffloadFallback};
use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use temps_config::deployment_config::DeploymentConfigExt;
use temps_deployer::ImageBuilder;
use temps_entities::{deployment_jobs, deployments};
use tracing::{debug, info, warn};

#[async_trait]
impl FromJobRecord for BuildImageJob {
    const JOB_TYPES: &'static [&'static str] = &["BuildImageJob"];

    /// Reads `dockerfile_path` (default `Dockerfile`), `build_context` and the
    /// sealed `build_args` from the config, takes the download job from the first
    /// dependency (default `download_repo`), names the image after the deployment
    /// as it is now, and chooses who builds it from the build location.
    async fn from_record(record: &JobRecord<'_>) -> Result<Self, WorkflowExecutionError> {
        let JobRecord {
            service,
            project,
            db_job,
            ..
        } = *record;

        let config = record.config()?;

        let dockerfile_path = config
            .get("dockerfile_path")
            .and_then(|v| v.as_str())
            .unwrap_or("Dockerfile");

        // Get dependencies to find the download job
        let download_job_id = record.first_dependency_or("download_repo");

        // Fetch deployment to get its URL for naming
        let deployment = record.reload_deployment().await?;

        let image_tag = format!("{}:latest", deployment.slug);

        // Route implicit Docker Hub base images through the
        // operator's configured registry mirror/prefix, if any.
        // Falls back to unconfigured (no rewriting) rather than
        // failing the build if settings can't be read -- the
        // existing anonymous-pull behavior is always a safe default.
        let registry_mirror_prefix = service
            .config_service
            .get_settings()
            .await
            .ok()
            .and_then(|settings| settings.registry_mirror_prefix);

        // Same source of truth as placement: a control-plane profile
        // must select a remote builder before constructing the job.
        let local_workloads_enabled = service
            .node_scheduler
            .get()
            .map(|scheduler| scheduler.local_workloads_enabled())
            .unwrap_or(true);

        let mut builder = BuildImageJobBuilder::new()
            .job_id(db_job.job_id.clone())
            .download_job_id(download_job_id)
            .image_tag(image_tag)
            .dockerfile_path(dockerfile_path.to_string())
            .log_id(db_job.log_id.clone())
            .log_service(service.log_service.clone())
            .registry_mirror_prefix(registry_mirror_prefix)
            .local_workloads_enabled(local_workloads_enabled);

        builder = builder
            .preset(project.preset)
            .preset_config(project.preset_config.clone());

        // Unseal build args from job_config. The planner derives build
        // args from env vars and stores them encrypted (build_args
        // contain everything env vars contain, including any value
        // marked secret — they leak into `docker history` if dumped
        // in plaintext).
        let enc_for_build_args = service.encryption_service.get();
        let build_args_map = crate::services::sensitive_envelope::read_sealed(
            config,
            enc_for_build_args,
            "build_args",
        )
        .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
        // Worker builds cannot receive registry credentials, so a
        // build that needs them stays on the control plane.
        let needs_npm_credentials = crate::jobs::npmrc::plan_npmrc(&build_args_map).is_some();
        // Names whose values may be sent to a worker build (names
        // only, plaintext). A job planned before the field existed
        // has none: a worker then receives no value at all.
        let worker_build_arg_keys: std::collections::BTreeSet<String> = config
            .get(crate::services::workflow_planner::WORKER_BUILD_ARG_KEYS_FIELD)
            .and_then(|value| value.as_array())
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| key.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        // Only a secret value the framework inlines changes what a
        // worker builds; such a build stays here when it can.
        let inlined_build_variables =
            withheld_inlined_variables(build_args_map.keys(), &worker_build_arg_keys);
        builder = builder.worker_build_arg_keys(worker_build_arg_keys);
        if !build_args_map.is_empty() {
            let build_args: Vec<(String, String)> = build_args_map.into_iter().collect();
            builder = builder.build_args(build_args);
        }

        // Add build context if present (for monorepo subdirectories)
        if let Some(build_context_value) = config.get("build_context") {
            if let Some(build_context_str) = build_context_value.as_str() {
                if !build_context_str.is_empty() && build_context_str != "." {
                    builder = builder.build_context(build_context_str.to_string());
                }
            }
        }

        let (builder, cross_build_platforms) =
            cross_build_platforms(record, builder, local_workloads_enabled).await?;
        let choice = BuilderChoice {
            record,
            deployment: &deployment,
            local_workloads_enabled,
            cross_build_platforms: &cross_build_platforms,
            needs_npm_credentials,
            inlined_build_variables: &inlined_build_variables,
        };
        let (builder, image_builder) = choose_image_builder(&choice, builder).await?;
        let job = builder.build(image_builder)?;

        Ok(job)
    }
}

/// What deciding who builds the image depends on.
struct BuilderChoice<'a> {
    record: &'a JobRecord<'a>,
    deployment: &'a deployments::Model,
    local_workloads_enabled: bool,
    cross_build_platforms: &'a [String],
    needs_npm_credentials: bool,
    inlined_build_variables: &'a [String],
}

/// The platforms a multi-arch build must cover; empty for the usual single
/// native build.
async fn cross_build_platforms(
    record: &JobRecord<'_>,
    mut builder: BuildImageJobBuilder,
    local_workloads_enabled: bool,
) -> Result<(BuildImageJobBuilder, Vec<String>), WorkflowExecutionError> {
    let JobRecord {
        service,
        project,
        environment,
        db_job,
        ..
    } = *record;

    // Cross-build for any worker architecture this deployment could
    // be scheduled onto. Empty on a homogeneous cluster, which is
    // the single native build every deployment does today.
    //
    // This runs at build time even though placement happens later,
    // because the build job precedes scheduling in the workflow;
    // the node selectors below are the same ones the scheduler will
    // apply, so we never build for an architecture this deployment
    // cannot land on.
    //
    // Off unless the operator asked for it. Cross-builds are
    // emulated and slow, and deriving them from cluster topology
    // would let one node joining change build behaviour for every
    // deployment — including breaking builds outright on a control
    // plane without the matching QEMU binfmt handlers. Environment
    // setting wins, inheriting the project's when unset.
    let cross_builds_enabled = environment
        .deployment_config
        .as_ref()
        .and_then(|c| c.cross_architecture_builds)
        .or_else(|| {
            project
                .deployment_config
                .as_ref()
                .and_then(|c| c.cross_architecture_builds)
        })
        .unwrap_or(false);

    if !cross_builds_enabled && local_workloads_enabled {
        debug!(
            deployment_id = db_job.deployment_id,
            "Cross-architecture builds disabled; building for the control plane's \
             platform only"
        );
    }

    // The platforms a multi-arch build must cover; empty for the
    // usual single native build.
    let mut cross_build_platforms: Vec<String> = Vec::new();
    if let (true, Some(scheduler)) = (
        cross_builds_enabled && local_workloads_enabled,
        service.node_scheduler.get(),
    ) {
        let (target_nodes, target_labels) = resolve_placement_constraints(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );

        match scheduler
            .required_build_platforms_for_project(
                target_labels.as_ref(),
                target_nodes.as_deref(),
                Some(&project.slug),
            )
            .await
        {
            Ok(platforms) if !platforms.is_empty() => {
                info!(
                    deployment_id = db_job.deployment_id,
                    platforms = ?platforms,
                    "Cluster spans multiple architectures — building one image per platform"
                );
                cross_build_platforms = platforms.clone();
                builder = builder.target_platforms(platforms);
            }
            Ok(_) => {}
            Err(crate::services::node_service::NodeError::Validation { message }) => {
                return Err(WorkflowExecutionError::InvalidJobConfig(message));
            }
            // Not being able to enumerate nodes must not block a
            // build: fall back to the native-only build and let the
            // deploy path's architecture check catch a mismatch.
            Err(e) => {
                warn!(
                    deployment_id = db_job.deployment_id,
                    "Could not determine cluster architectures ({}); \
                     building for the control plane's platform only",
                    e
                );
            }
        }
    }

    Ok((builder, cross_build_platforms))
}

/// The image builder the environment's build location calls for.
async fn choose_image_builder(
    choice: &BuilderChoice<'_>,
    builder: BuildImageJobBuilder,
) -> Result<(BuildImageJobBuilder, Arc<dyn ImageBuilder>), WorkflowExecutionError> {
    let JobRecord {
        service,
        project,
        environment,
        ..
    } = *choice.record;

    let build_location =
        temps_entities::deployment_config::DeploymentConfig::resolve_build_location(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );
    if build_location == temps_entities::deployment_config::BuildLocation::Ci {
        ci_image_builder(choice, builder).await
    } else if choice.local_workloads_enabled
        && build_location == temps_entities::deployment_config::BuildLocation::ControlPlane
    {
        Ok((builder, service.image_builder.clone()))
    } else if choice.local_workloads_enabled {
        Ok(offload_image_builder(choice, builder).await)
    } else {
        worker_image_builder(choice, builder).await
    }
}

/// Whether this deployment's image is only read for its static output files.
async fn builds_static_output_only(
    choice: &BuilderChoice<'_>,
) -> Result<bool, WorkflowExecutionError> {
    Ok(deployment_jobs::Entity::find()
        .filter(deployment_jobs::Column::DeploymentId.eq(choice.deployment.id))
        .filter(deployment_jobs::Column::JobType.eq("DeployStaticJob"))
        .one(choice.record.service.db.as_ref())
        .await?
        .is_some())
}

/// CI builds the image remotely; its artifacts land on a worker that can
/// receive them, or on this host's daemon when no worker can.
async fn ci_image_builder(
    choice: &BuilderChoice<'_>,
    mut builder: BuildImageJobBuilder,
) -> Result<(BuildImageJobBuilder, Arc<dyn ImageBuilder>), WorkflowExecutionError> {
    let BuilderChoice {
        record, deployment, ..
    } = *choice;
    let JobRecord {
        service,
        project,
        environment,
        ..
    } = *record;

    let image_builder: Arc<dyn ImageBuilder> = {
        inlined_variables_allow_offload(choice.inlined_build_variables).map_err(|reason| {
            WorkflowExecutionError::InvalidJobConfig(format!(
                "Deployment {} CI build cannot transfer inlined secrets: {reason}",
                deployment.id
            ))
        })?;
        if choice.needs_npm_credentials {
            return Err(WorkflowExecutionError::InvalidJobConfig(format!(
                "Deployment {} CI build cannot transfer package-manager credentials",
                deployment.id
            )));
        }
        let encryption = service.encryption_service.get().ok_or_else(|| {
            WorkflowExecutionError::InvalidJobConfig(
                "CI build encryption service unavailable".into(),
            )
        })?;
        let ci_dispatch = crate::services::ci_dispatch::CiDispatchService::new(
            service.db.clone(),
            encryption.clone(),
            service.config_service.data_dir(),
        )
        .map_err(|error| WorkflowExecutionError::InvalidJobConfig(error.to_string()))?;
        builder = builder.external_build(true);
        let (target_nodes, target_labels) = resolve_placement_constraints(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );
        let local_platform = if choice.local_workloads_enabled {
            service.image_builder.ensure_platform_discovered().await
        } else {
            None
        };
        let selected = if let Some(platform) = &local_platform {
            let platforms = if choice.cross_build_platforms.is_empty() {
                vec![platform.clone()]
            } else {
                choice.cross_build_platforms.to_vec()
            };
            service
                .select_platform_builders(
                    deployment.id,
                    &project.slug,
                    target_nodes.as_deref(),
                    target_labels.as_ref(),
                    &platforms,
                )
                .await
        } else {
            let static_output_only = builds_static_output_only(choice).await?;
            service
                .select_node_builder(
                    deployment.id,
                    &project.slug,
                    target_nodes.as_deref(),
                    target_labels.as_ref(),
                    static_output_only,
                )
                .await
        };
        match selected {
            Ok(selected) => {
                let targets: Vec<_> = selected
                    .into_iter()
                    .map(|selected| crate::jobs::build_image::WorkerPlatformBuilder {
                        platform: selected.platform,
                        node_id: selected.node_id,
                        node_name: selected.node_name,
                        builder: Arc::new(crate::services::ci_dispatch::CiImageBuilder {
                            service: ci_dispatch.clone(),
                            deployment_id: deployment.id,
                            target: selected.remote,
                        }),
                    })
                    .collect();
                let primary = targets
                    .first()
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "CI build has no artifact target worker".into(),
                        )
                    })?
                    .builder
                    .clone();
                builder = builder.platform_builders(targets);
                primary
            }
            Err(error) if local_platform.is_some() => {
                if choice.cross_build_platforms.is_empty() {
                    builder = builder.target_platforms(local_platform.into_iter().collect());
                }
                builder = builder.build_location_notice(format!("CI builds run remotely. Artifacts use the control plane's daemon because no eligible worker can receive them: {error}"));
                Arc::new(crate::services::ci_dispatch::CiImageBuilder {
                    service: ci_dispatch,
                    deployment_id: deployment.id,
                    target: service.image_builder.clone(),
                })
            }
            Err(error) => return Err(error),
        }
    };
    Ok((builder, image_builder))
}

/// Prefer a worker for a build the operator asked to keep off the control
/// plane.
async fn offload_image_builder(
    choice: &BuilderChoice<'_>,
    mut builder: BuildImageJobBuilder,
) -> (BuildImageJobBuilder, Arc<dyn ImageBuilder>) {
    let BuilderChoice {
        record, deployment, ..
    } = *choice;
    let JobRecord {
        service,
        project,
        environment,
        ..
    } = *record;

    let image_builder: Arc<dyn ImageBuilder> = {
        // The operator asked to keep this build off the control
        // plane. A worker build is a preference: when no worker
        // can produce the same image the control plane would, the
        // build stays here and the build log says why.
        let (target_nodes, target_labels) = resolve_placement_constraints(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );
        match service
            .select_offload_builder(
                deployment.id,
                &project.slug,
                target_nodes.as_deref(),
                target_labels.as_ref(),
                choice.cross_build_platforms,
                choice.needs_npm_credentials,
                choice.inlined_build_variables,
            )
            .await
        {
            Ok(selected) => {
                // Whether the node has a free build slot is only
                // known when the build is about to be sent, after
                // the repository download. The build job asks
                // then, and builds on the control plane instead
                // when the node stays busy. This must come before
                // `target_platforms`, which it records to restore.
                builder = builder.offload_fallback(OffloadFallback::new(
                    selected.agent.clone(),
                    selected.node_id,
                    selected.node_name.clone(),
                    selected.agent.build_queue_wait(),
                    service.image_builder.clone(),
                ));
                // The control plane's own post-build jobs read
                // the image from its daemon, so the finished
                // image is streamed back once the build is done.
                builder = builder
                    .remote_builder_node_id(selected.node_id)
                    .target_platforms(vec![selected.platform.clone()])
                    .copy_image_to_control_plane(
                        service.image_builder.clone(),
                        selected.node_name.clone(),
                    );
                selected.remote
            }
            Err(reason) => {
                warn!(
                    deployment_id = deployment.id,
                    project_slug = %project.slug,
                    "Build location is 'node' but building on the control plane: {}",
                    reason
                );
                builder = builder.build_location_notice(format!(
                    "⚠️ Build location is set to 'node', but this build runs on the \
                     control plane: {reason}"
                ));
                service.image_builder.clone()
            }
        }
    };
    (builder, image_builder)
}

/// With no local daemon, a node builds the image.
async fn worker_image_builder(
    choice: &BuilderChoice<'_>,
    mut builder: BuildImageJobBuilder,
) -> Result<(BuildImageJobBuilder, Arc<dyn ImageBuilder>), WorkflowExecutionError> {
    let BuilderChoice {
        record, deployment, ..
    } = *choice;
    let JobRecord {
        service,
        project,
        environment,
        ..
    } = *record;

    let image_builder: Arc<dyn ImageBuilder> = {
        // A static site's image is only read for its output
        // files, which DeployStaticJob fetches from the build node
        // and the control plane serves; it never runs anywhere.
        let static_output_only = builds_static_output_only(choice).await?;
        // No local daemon: build on a node. The image is then
        // handed to each replica's node by DeployImageJob.
        let (target_nodes, target_labels) = resolve_placement_constraints(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );
        let mut selected = service
            .select_node_builder(
                deployment.id,
                &project.slug,
                target_nodes.as_deref(),
                target_labels.as_ref(),
                static_output_only,
            )
            .await?;
        if selected.len() > 1 {
            // A mixed-architecture deployment: one native build
            // per architecture, each on its own worker.
            let primary = selected[0].remote.clone();
            builder = builder.platform_builders(
                selected
                    .into_iter()
                    .map(|selected| crate::jobs::build_image::WorkerPlatformBuilder {
                        platform: selected.platform,
                        node_id: selected.node_id,
                        node_name: selected.node_name,
                        builder: selected.remote,
                    })
                    .collect(),
            );
            primary
        } else {
            let selected = selected.pop().ok_or_else(|| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Deployment {}: no build node was selected",
                    deployment.id
                ))
            })?;
            builder = builder
                .remote_builder_node_id(selected.node_id)
                .target_platforms(vec![selected.platform.clone()]);
            selected.remote
        }
    };
    Ok((builder, image_builder))
}
