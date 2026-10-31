// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rollback to an earlier deployment of the same environment.

use super::reuse::{clean_up_failed_completion, deployment_asset_origin};
use super::*;

impl DeploymentService {
    fn resource_usage_from_snapshot(
        snapshot: &temps_entities::deployment_config::DeploymentConfigSnapshot,
    ) -> crate::jobs::ResourceUsage {
        crate::jobs::ResourceUsage {
            cpu_limit: snapshot.cpu_limit.map(|value| format!("{value}u")),
            memory_limit: snapshot.memory_limit.map(|value| format!("{value}Mi")),
            cpu_request: snapshot.cpu_request.map(|value| format!("{value}u")),
            memory_request: snapshot.memory_request.map(|value| format!("{value}Mi")),
        }
    }

    fn rollback_snapshot_port_and_replicas(
        snapshot: &temps_entities::deployment_config::DeploymentConfigSnapshot,
        deployment_id: i32,
    ) -> Result<(Option<u16>, u32), DeploymentError> {
        let port = snapshot
            .exposed_port
            .map(u16::try_from)
            .transpose()
            .map_err(|error| {
                DeploymentError::InvalidDeploymentState(format!(
                    "Target deployment {deployment_id} has invalid exposed port {:?}: {error}",
                    snapshot.exposed_port
                ))
            })?;
        let replicas = u32::try_from(snapshot.replicas)
            .ok()
            .filter(|replicas| *replicas > 0)
            .ok_or_else(|| {
                DeploymentError::InvalidDeploymentState(format!(
                    "Target deployment {deployment_id} has invalid replica count {}",
                    snapshot.replicas
                ))
            })?;
        Ok((port, replicas))
    }

    pub async fn rollback_to_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        self.rollback_to_deployment_as(
            project_id,
            deployment_id,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
    }

    /// [`Self::rollback_to_deployment`] for a caller whose authority is known
    /// (ADR 045).
    pub async fn rollback_to_deployment_as(
        &self,
        project_id: i32,
        deployment_id: i32,
        caller: temps_core::docker_socket_grant::DeployCaller,
    ) -> Result<Deployment, DeploymentError> {
        use temps_entities::deployments::DeploymentMetadata;

        self.guard_granted_project_deploy(project_id, caller)
            .await?;

        // Fetch the target deployment (the one we're rolling back TO)
        let target_deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Target deployment not found".to_string()))?;

        // Validate that the deployment is in a valid state for rollback.
        //
        // "stopped" belongs here alongside "completed": once a LATER
        // deployment supersedes this one, `cancel_previous_deployments`
        // stops its containers and flips its state to "stopped" (see that
        // function and `teardown_deployment`) -- that is the terminal state
        // every successful-but-no-longer-current deployment actually ends up
        // in. Rollback's whole purpose is reverting to an older deployment,
        // so its target is virtually always going to be "stopped" in
        // practice; excluding it made rollback reject its own primary use
        // case ("Cannot rollback to deployment in 'stopped' state") for any
        // deployment that had already been superseded -- which is every
        // deployment a real user would ever actually want to roll back to.
        // "deployed" is kept for the one other live path that sets it
        // (`resume_deployment`); "failed"/"cancelled"/"paused" stay excluded
        // -- a failed/cancelled deployment has no reliable image to reuse,
        // and rolling back TO a paused deployment is a distinct, not yet
        // supported, operation.
        let valid_rollback_states = ["deployed", "completed", "stopped"];
        if !valid_rollback_states.contains(&target_deployment.state.as_str()) {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Cannot rollback to deployment in '{}' state. Only deployed, completed, or stopped (superseded) deployments can be rolled back to.",
                target_deployment.state
            )));
        }

        let environment_id = target_deployment.environment_id;

        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        // An uploaded static bundle may use any static-capable preset (for
        // example Vite). Its retained artifact determines how it is reused,
        // rather than the project's framework label.
        let is_static = target_deployment.static_dir_location.is_some()
            || (target_deployment.image_name.is_none()
                && (project.source_type == temps_entities::source_type::SourceType::StaticFiles
                    || project.preset == temps_entities::preset::Preset::Static));
        if !is_static {
            temps_presets::get_preset_for_storage(project.preset, project.preset_config.as_ref())
                .map_err(|error| DeploymentError::InvalidInput(error.to_string()))?
                .ok_or_else(|| DeploymentError::NotFound("Preset not found".to_string()))?;
        }

        // --- Git projects: rebuild from source when the image isn't reusable ---
        //
        // The artifact-reuse path below redeploys the target's immutable image
        // reference or static asset lineage as-is. The selected worker owns
        // image verification/pull, so a daemon-less control plane never probes
        // its own cache.
        //
        // Git projects fall back to rebuilding the recorded commit only when
        // the durable artifact reference itself is absent. Static deployments
        // reuse their stored asset origin without requiring a server image.
        //
        // Non-git projects (docker_image / static_files / manual without a git
        // ref) have no source to rebuild, so they always use image reuse.
        let has_git_ref = target_deployment
            .commit_sha
            .as_ref()
            .is_some_and(|c| !c.is_empty())
            || target_deployment
                .branch_ref
                .as_ref()
                .is_some_and(|b| !b.is_empty());

        // Artifact availability is represented by durable deployment metadata.
        // The selected worker verifies or pulls a container image; the control
        // plane may intentionally have no Docker daemon at all.
        let artifact_available = if is_static {
            target_deployment
                .static_dir_location
                .as_ref()
                .is_some_and(|location| !location.is_empty())
        } else {
            target_deployment.image_name.as_ref().is_some_and(|image| {
                !image.is_empty() && !image.starts_with(&format!("temps-{}:", project.slug))
            })
        };

        if project.source_type == temps_entities::source_type::SourceType::Git
            && has_git_ref
            && !artifact_available
        {
            info!(
                "Rollback: project {} is git-sourced and the target image is unavailable ({}) — rebuilding from source at commit {:?} (rolling back to #{})",
                project_id,
                if is_static { "static artifact missing" } else { "image reference missing" },
                target_deployment.commit_sha,
                deployment_id
            );

            // Snapshot the latest deployment id BEFORE triggering, so we can
            // identify the one the pipeline creates and return it.
            let prev_max_id = deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project_id))
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .order_by_desc(deployments::Column::Id)
                .one(self.db.as_ref())
                .await?
                .map(|d| d.id)
                .unwrap_or(0);

            self.trigger_pipeline_inner(
                project_id,
                environment_id,
                PipelineTriggerOptions {
                    branch: target_deployment.branch_ref.clone(),
                    tag: target_deployment.tag_ref.clone(),
                    commit: target_deployment.commit_sha.clone(),
                    rollback_from_deployment_id: Some(deployment_id),
                    recovery_of_deployment_id: None,
                },
            )
            .await?;

            // Anonymous telemetry: a rollback was initiated. No identifying props.
            self.telemetry()
                .report(temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::RollbackTriggered,
                ));

            // The pipeline created a new deployment row; return it so the API
            // response carries the rollback deployment's id/status. It's the
            // newest row for this environment above the prior max.
            let created = deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project_id))
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .filter(deployments::Column::Id.gt(prev_max_id))
                .order_by_desc(deployments::Column::Id)
                .one(self.db.as_ref())
                .await?;

            let model = match created {
                Some(dep) => dep,
                // The job is queued; the row may not be visible yet. Surface the
                // target as a stand-in rather than failing — the rollback is
                // already in flight.
                None => target_deployment,
            };
            return Ok(self
                .map_db_deployment_to_deployment(model, false, None)
                .await);
        }

        let image_name = target_deployment.image_name.clone();

        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        info!(
            "Initiating rollback for project_id: {}, to deployment_id: {}, image: {}, environment_id: {}",
            project_id,
            deployment_id,
            image_name.as_deref().unwrap_or("static artifact"),
            environment_id
        );

        // --- Create a NEW deployment record for the rollback ---
        // This gives us fresh timestamps, a unique slug, and proper tracking.
        let now = chrono::Utc::now();

        // Get next deployment number
        let deployment_count = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to count deployments: {}", e)))?;
        let deployment_number = deployment_count + 1;

        let rollback_slug = format!("{}-{}", project.slug, deployment_number);

        let rollback_asset_origin =
            deployment_asset_origin(self.db.as_ref(), &target_deployment).await?;
        let rollback_health_check_path = target_deployment
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.health_check_path.clone());
        let rollback_command = target_deployment
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.command.clone());
        let rollback_metadata = DeploymentMetadata {
            is_rollback: true,
            rolled_back_from_id: Some(deployment_id),
            health_check_path: rollback_health_check_path.clone(),
            command: rollback_command.clone(),
            runtime_overrides: target_deployment
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.runtime_overrides.clone()),
            ..Default::default()
        };

        let new_deployment = deployments::ActiveModel {
            id: sea_orm::NotSet,
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set(rollback_slug.clone()),
            state: Set("pending".to_string()),
            metadata: Set(Some(rollback_metadata)),
            branch_ref: Set(target_deployment.branch_ref.clone()),
            tag_ref: Set(target_deployment.tag_ref.clone()),
            commit_sha: Set(target_deployment.commit_sha.clone()),
            commit_message: Set(target_deployment.commit_message.clone()),
            commit_author: Set(target_deployment.commit_author.clone()),
            commit_json: Set(target_deployment.commit_json.clone()),
            image_name: Set(image_name.clone()),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            deploying_at: Set(Some(now)),
            ready_at: Set(None),
            static_dir_location: Set(target_deployment.static_dir_location.clone()),
            screenshot_location: Set(None),
            cancelled_reason: Set(None),
            context_vars: Set(Some(serde_json::json!({
                "trigger": "rollback",
                "source_deployment_id": rollback_asset_origin.deployment_id,
                "source_deployment_slug": rollback_asset_origin.slug.clone(),
                "source_environment_id": rollback_asset_origin.environment_id,
            }))),
            deployment_config: Set(target_deployment.deployment_config.clone()),
            promoted_from_deployment_id: Set(None),
            upload_request_id: Set(None),
            docker_socket_mounted: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
        };

        let rollback_deployment = insert_deployment_with_generation_lock(
            self.db.as_ref(),
            project_id,
            environment_id,
            new_deployment,
        )
        .await
        .map_err(|e| {
            DeploymentError::Other(format!("Failed to create rollback deployment: {}", e))
        })?;

        let rollback_deployment_id = rollback_deployment.id;
        info!(
            "Created rollback deployment #{} (rolling back to #{}, artifact: {})",
            rollback_deployment_id,
            deployment_id,
            image_name.as_deref().unwrap_or("static")
        );

        let admitted = match super::job_processor::JobProcessorService::try_admit_deployment(
            self.db.as_ref(),
            rollback_deployment_id,
        )
        .await
        {
            Ok(admitted) => admitted,
            Err(error) => {
                return Err(self
                    .fail_reuse_deployment(
                        rollback_deployment_id,
                        "Failed to admit rollback",
                        error,
                    )
                    .await);
            }
        };
        if !admitted {
            return Err(self
                .fail_reuse_deployment(
                    rollback_deployment_id,
                    "Rollback was not admitted",
                    "its project or environment is being deleted",
                )
                .await);
        }

        // Anonymous telemetry: a rollback was initiated. No identifying props.
        self.telemetry()
            .report(temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::RollbackTriggered,
            ));

        // From this point onward the deployment row exists and has been
        // admitted. Keep every fallible setup/finalization step inside one
        // boundary so no `?` can leave it pending or running indefinitely.
        let execution_result: Result<(), DeploymentError> = async {
        // Check if preset is static - if so, just update environment without deploying
        if is_static {
            info!("Rollback: Static preset detected - updating environment only");

            if let Err(error) = self
                .complete_static_reuse(
                    &project,
                    &environment,
                    &rollback_deployment,
                    now,
                )
                .await
            {
                return Err(self
                    .fail_reuse_deployment(
                        rollback_deployment_id,
                        "Failed to activate static rollback",
                        error,
                    )
                    .await);
            }

            info!(
                "Rollback completed - environment {} now points to rollback deployment {}",
                environment_id, rollback_deployment_id
            );
        } else {
            let image_name = image_name.ok_or_else(|| {
                DeploymentError::Other(format!(
                    "Target deployment {deployment_id} has no reusable image reference"
                ))
            })?;

            // --- Create per-job log paths (matching normal deployment pattern) ---
            let deploy_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-deploy_container.log",
                project.slug,
                environment.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                rollback_deployment_id
            );
            let complete_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-mark_deployment_complete.log",
                project.slug,
                environment.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                rollback_deployment_id
            );

            self.log_service
                .create_log_path(&deploy_log_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create deploy log path: {}", e))
                })?;
            self.log_service
                .create_log_path(&complete_log_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create complete log path: {}", e))
                })?;

            // --- Create deployment_jobs records so the API can return them ---
            use temps_entities::{deployment_jobs, types::JobStatus};

            let deploy_job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(rollback_deployment_id),
                job_id: Set("deploy_container".to_string()),
                job_type: Set("DeployImageJob".to_string()),
                name: Set("Deploy Container".to_string()),
                description: Set(Some(format!("Rollback: deploy image {}", image_name))),
                status: Set(JobStatus::Running),
                log_id: Set(deploy_log_id.clone()),
                job_config: Set(None),
                dependencies: Set(None),
                execution_order: Set(Some(0)),
                started_at: Set(Some(now)),
                ..Default::default()
            };
            let deploy_job_model =
                deploy_job_record
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!("Failed to create deploy job record: {}", e))
                    })?;

            let complete_job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(rollback_deployment_id),
                job_id: Set("mark_deployment_complete".to_string()),
                job_type: Set("MarkDeploymentCompleteJob".to_string()),
                name: Set("Mark Deployment Complete".to_string()),
                description: Set(Some("Finalize rollback deployment".to_string())),
                status: Set(JobStatus::Pending),
                log_id: Set(complete_log_id.clone()),
                job_config: Set(None),
                dependencies: Set(Some(
                    serde_json::to_value(vec!["deploy_container"]).unwrap_or_default(),
                )),
                execution_order: Set(Some(1)),
                ..Default::default()
            };
            let complete_job_model =
                complete_job_record
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!(
                            "Failed to create complete job record: {}",
                            e
                        ))
                    })?;

            info!("Rollback: Deploying image: {}", image_name);

            // Step 1: Execute DeployImageJob with external image
            // Use the NEW rollback slug as the container name (not the old deployment's slug)
            let (configured_port, rollback_replicas, rollback_resources) =
                if let Some(snapshot) = target_deployment.deployment_config.as_ref() {
                    let (port, replicas) =
                        Self::rollback_snapshot_port_and_replicas(snapshot, deployment_id)?;
                    (port, replicas, Self::resource_usage_from_snapshot(snapshot))
                } else {
                    (
                        super::port_resolver::configured_port_override(&environment, &project),
                        environment
                            .deployment_config
                            .as_ref()
                            .map(|config| config.replicas as u32)
                            .or_else(|| {
                                project
                                    .deployment_config
                                    .as_ref()
                                    .map(|config| config.replicas as u32)
                            })
                            .unwrap_or(1),
                        Self::resolve_resource_usage(
                            environment.deployment_config.as_ref(),
                            project.deployment_config.as_ref(),
                        ),
                    )
                };
            let exposed_port = configured_port.map(u32::from).unwrap_or(3000);
            // ADR 045: the slug travels with every deploy, including this
            // one. A rollback or promotion of a granted project that omitted
            // it would be placed anywhere and started without its socket.
            let mut deploy_builder =
                crate::jobs::DeployImageJobBuilder::new(project.slug.clone(), caller)
                    // ADR 045: the same audit sink an ordinary deploy uses, so a
                    // rollback/promotion that mounts the socket is recorded rather
                    // than only logged.
                    .audit_logger(self.audit_logger.get().cloned())
                    .job_id("deploy_container".to_string())
                    .build_job_id("external-image".to_string())
                    .target(crate::jobs::DeploymentTarget::Docker {
                        registry_url: "local".to_string(),
                        network: Some(temps_core::NETWORK_NAME.to_string()),
                    })
                    .service_name(rollback_slug.clone())
                    .health_check_path(None)
                    .health_check_path_override(rollback_health_check_path)
                    .health(self.reused_image_health(rollback_asset_origin.deployment_id, &environment, &project).await?)
                    .command(target_deployment.metadata.as_ref()
                        .and_then(|metadata| metadata.container_runtime().command))
                    .entrypoint(target_deployment.metadata.as_ref()
                        .and_then(|metadata| metadata.container_runtime().entrypoint))
                    .replicas(rollback_replicas)
                    .port(exposed_port)
                    .configured_port(configured_port)
                    .log_id(deploy_log_id.clone())
                    .log_service(self.log_service.clone())
                    .failed_container_retention(self.db.clone(), rollback_deployment_id);

            // Resolve CPU/memory limits + requests (env → project), matching the
            // normal deploy path (WorkflowExecutionService). Each field resolves
            // independently; when neither side configures a value it stays unset
            // so the deployer applies no Docker limit. Without this, a rollback
            // would inherit `ResourceUsage::default()` (now all-None) and silently
            // drop a configured limit — or, before the default was fixed, cap an
            // unconfigured environment.
            deploy_builder = deploy_builder.resources(rollback_resources);

            // Resolve the environment's env vars exactly as a normal deploy does,
            // so the rolled-back container boots with the full set (user vars,
            // external-service connection strings, SENTRY_DSN, TEMPS_API_TOKEN,
            // CRON_SECRET, OTEL_*) instead of nothing. Without this, a rollback
            // reuses the image but starts it unconfigured.
            let resolved_env = if let Some(resolver) = self.env_resolver.get() {
                let mut resolved = resolver
                    .resolve(&project, &environment, &rollback_deployment)
                    .await?;
                crate::services::env_resolver::apply_deployment_owned_variables(
                    &mut resolved,
                    project.preset,
                    &rollback_asset_origin.slug,
                    (project.preset != temps_entities::preset::Preset::DockerCompose)
                        .then_some(exposed_port),
                );
                resolved
            } else {
                tracing::warn!(
                    "Rollback: env resolver not wired — rolled-back container starts with no resolved env vars"
                );
                std::collections::HashMap::new()
            };
            deploy_builder = deploy_builder.environment_variables(resolved_env.clone());

            deploy_builder = self
                .configure_reuse_deploy_builder(
                    deploy_builder,
                    &project,
                    &environment,
                    &image_name,
                    &resolved_env,
                )
                .await;

            let provenance = self
                .reuse_image_provenance(
                    &project,
                    &target_deployment,
                    rollback_asset_origin.deployment_id,
                    &image_name,
                )
                .await?;
            info!(
                "Rollback: image {} is {}-sourced (origin deployment #{}, expected identity: {})",
                image_name,
                provenance.source,
                rollback_asset_origin.deployment_id,
                provenance.expected_identity_label()
            );

            let deploy_job = provenance.apply(
                deploy_builder
                    .build(self.deployer.clone())
                    .map_err(|e| {
                        DeploymentError::Other(format!("Failed to create deploy job: {}", e))
                    })?
                    .with_external_image_tag(image_name.clone()),
            );

            // Create workflow context for the NEW rollback deployment
            let mock_log_writer = Arc::new(crate::test_utils::MockLogWriter::new(0));
            let mut rollback_context = temps_core::WorkflowContext::new(
                format!("rollback-{}", rollback_deployment_id),
                rollback_deployment_id,
                project_id,
                environment_id,
                mock_log_writer,
            );

            let cancellation_provider =
                super::workflow_execution_service::DatabaseCancellationProvider::new(
                    self.db.clone(),
                    rollback_deployment_id,
                );
            match deploy_job
                .execute_with_cancellation(rollback_context.clone(), &cancellation_provider)
                .await
            {
                Ok(job_result) => {
                    info!("Rollback: Deploy job completed successfully");
                    rollback_context = job_result.context;

                    // Update deploy job record to Success
                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Rollback: Deploy job failed: {}", e);
                    let failure_message = match deploy_job.cleanup(&rollback_context).await {
                        Ok(()) => format!("Deploy failed: {e}"),
                        Err(cleanup_error) => format!(
                            "Deploy failed: {e}; rollback container cleanup also failed: {cleanup_error}"
                        ),
                    };

                    // Update deploy job record to Failure
                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(failure_message.clone()));
                    let _ = active_job.update(self.db.as_ref()).await;

                    // Cancel the pending complete job
                    let mut active_complete: deployment_jobs::ActiveModel =
                        complete_job_model.into();
                    active_complete.status = Set(JobStatus::Cancelled);
                    active_complete.error_message = Set(Some("Deploy job failed".to_string()));
                    let _ = active_complete.update(self.db.as_ref()).await;

                    // Mark the rollback deployment as failed
                    let mut active_dep: deployments::ActiveModel =
                        rollback_deployment.clone().into();
                    active_dep.state = Set("failed".to_string());
                    active_dep.finished_at = Set(Some(chrono::Utc::now()));
                    active_dep.cancelled_reason = Set(Some(failure_message.clone()));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to deploy image during rollback: {failure_message}"
                    )));
                }
            }

            // Step 2: Execute MarkDeploymentCompleteJob on the NEW rollback deployment
            info!(
                "Rollback: Marking deployment {} as complete",
                rollback_deployment_id
            );

            // Update complete job to Running
            let mut active_complete: deployment_jobs::ActiveModel = complete_job_model.into();
            active_complete.status = Set(JobStatus::Running);
            active_complete.started_at = Set(Some(chrono::Utc::now()));
            let complete_job_model =
                active_complete
                    .update(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        DeploymentError::Other(format!(
                            "Failed to update complete job status: {}",
                            e
                        ))
                    })?;

            let mark_complete_job = crate::jobs::MarkDeploymentCompleteJobBuilder::new()
                .environment_addresses(self.environment_addresses.get().cloned())
                .job_id("mark_deployment_complete".to_string())
                .deployment_id(rollback_deployment_id)
                .db(self.db.clone())
                .log_id(complete_log_id)
                .log_service(self.log_service.clone())
                .container_deployer(self.deployer.clone())
                .queue(self.queue_service.clone())
                .config_service(self.config_service.clone())
                .encryption_service(self.encryption_service.clone())
                .workload_plane(self.workload_plane.get().cloned())
                .build()
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to create mark complete job: {}", e))
                })?;

            // `execute` consumes the context; keep a copy for the failure
            // path's cleanup below.
            let cleanup_context = rollback_context.clone();
            match mark_complete_job.execute(rollback_context).await {
                Ok(_) => {
                    info!("Rollback: Mark complete job executed successfully");

                    // Update complete job record to Success
                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Rollback: Mark complete job failed: {}", e);

                    // The deploy step already started this deployment's
                    // containers. The workflow executor runs the job's
                    // cleanup on failure; this hand-rolled path must do the
                    // same, or a readiness or route failure leaves them
                    // running with no owner: failed deployments are excluded
                    // from the previous-deployment sweep. `cleanup` leaves a
                    // deployment that already reached completed/deployed
                    // alone, so a late failure never tears down a serving
                    // deployment.
                    let failure_message = clean_up_failed_completion(
                        &mark_complete_job,
                        &cleanup_context,
                        &e,
                        "rollback",
                    )
                    .await;

                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(failure_message.clone()));
                    let _ = active_job.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to mark deployment complete during rollback: {failure_message}"
                    )));
                }
            }

            info!(
                "Rollback completed - deployment {} is now active",
                rollback_deployment_id
            );
        }
        Ok(())
        }
        .await;
        if let Err(error) = execution_result {
            return Err(self
                .fail_reuse_deployment(
                    rollback_deployment_id,
                    "Rollback artifact reuse failed",
                    error,
                )
                .await);
        }

        // Re-fetch the rollback deployment to get the final state
        let final_deployment = deployments::Entity::find_by_id(rollback_deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::Other("Rollback deployment disappeared".to_string()))?;

        Ok(self
            .map_db_deployment_to_deployment(final_deployment, true, None)
            .await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_runtime_uses_target_deployment_snapshot() {
        let snapshot = temps_entities::deployment_config::DeploymentConfigSnapshot {
            exposed_port: Some(8080),
            replicas: 3,
            cpu_request: Some(500_000),
            cpu_limit: Some(1_000_000),
            memory_request: Some(512),
            memory_limit: Some(1_536),
            ..Default::default()
        };

        let (port, replicas) =
            DeploymentService::rollback_snapshot_port_and_replicas(&snapshot, 42)
                .expect("valid deployment snapshot");
        let resources = DeploymentService::resource_usage_from_snapshot(&snapshot);

        assert_eq!(port, Some(8080));
        assert_eq!(replicas, 3);
        assert_eq!(resources.cpu_request.as_deref(), Some("500000u"));
        assert_eq!(resources.cpu_limit.as_deref(), Some("1000000u"));
        assert_eq!(resources.memory_request.as_deref(), Some("512Mi"));
        assert_eq!(resources.memory_limit.as_deref(), Some("1536Mi"));
    }

    #[test]
    fn rollback_rejects_invalid_target_snapshot_runtime() {
        let invalid_port = temps_entities::deployment_config::DeploymentConfigSnapshot {
            exposed_port: Some(70_000),
            ..Default::default()
        };
        assert!(matches!(
            DeploymentService::rollback_snapshot_port_and_replicas(&invalid_port, 42),
            Err(DeploymentError::InvalidDeploymentState(_))
        ));

        let invalid_replicas = temps_entities::deployment_config::DeploymentConfigSnapshot {
            replicas: 0,
            ..Default::default()
        };
        assert!(matches!(
            DeploymentService::rollback_snapshot_port_and_replicas(&invalid_replicas, 42),
            Err(DeploymentError::InvalidDeploymentState(_))
        ));
    }
}
