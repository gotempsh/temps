// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Promotion of a deployment's build into another environment.

use super::reuse::{clean_up_failed_completion, deployment_asset_origin};
use super::*;

impl DeploymentService {
    /// Promote a deployment to a different environment.
    /// Creates a new deployment in the target environment using the source
    /// deployment's image. The target environment must belong to the same project.
    pub async fn promote_deployment(
        &self,
        project_id: i32,
        source_deployment_id: i32,
        target_environment_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        self.promote_deployment_as(
            project_id,
            source_deployment_id,
            target_environment_id,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
    }

    /// [`Self::promote_deployment`] for a caller whose authority is known
    /// (ADR 045).
    pub async fn promote_deployment_as(
        &self,
        project_id: i32,
        source_deployment_id: i32,
        target_environment_id: i32,
        caller: temps_core::docker_socket_grant::DeployCaller,
    ) -> Result<Deployment, DeploymentError> {
        use temps_entities::deployments::DeploymentMetadata;

        self.guard_granted_project_deploy(project_id, caller)
            .await?;

        // Fetch the source deployment
        let source = deployments::Entity::find_by_id(source_deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Source deployment {} not found in project {}",
                    source_deployment_id, project_id
                ))
            })?;

        // Validate state — only successful deployments can be promoted.
        // "stopped" is included for the same reason `rollback_to_deployment`
        // includes it (see the comment there): a source deployment that has
        // since been superseded by a newer one in ITS OWN environment is
        // "stopped", not "completed" -- and promoting an older, already-
        // superseded deployment's image into a different environment is
        // exactly the kind of thing a real user does (e.g. re-promote a
        // known-good build after a bad one shipped on top of it).
        let valid_states = ["deployed", "completed", "ready", "stopped"];
        if !valid_states.contains(&source.state.as_str()) {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Cannot promote deployment in '{}' state. Only deployed/completed/ready/stopped deployments can be promoted.",
                source.state
            )));
        }

        // Static deployments reuse their durable asset origin and do not have
        // a container image. Validate the appropriate artifact after the
        // project preset has been resolved below.
        let image_name = source.image_name.clone();

        // Fetch target environment and verify it belongs to the same project
        let target_env = environments::Entity::find_by_id(target_environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Target environment {} not found in project {}",
                    target_environment_id, project_id
                ))
            })?;

        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Project not found".to_string()))?;

        info!(
            "Promoting deployment {} to environment '{}' (project {}, image: {})",
            source_deployment_id,
            target_env.name,
            project_id,
            image_name.as_deref().unwrap_or("static artifact")
        );

        let is_static = source.static_dir_location.is_some()
            || (source.image_name.is_none()
                && (project.source_type == temps_entities::source_type::SourceType::StaticFiles
                    || project.preset == temps_entities::preset::Preset::Static));
        if !is_static {
            temps_presets::get_preset_for_storage(project.preset, project.preset_config.as_ref())
                .map_err(|error| DeploymentError::InvalidInput(error.to_string()))?
                .ok_or_else(|| DeploymentError::NotFound("Preset not found".to_string()))?;
        }

        if is_static
            && source
                .static_dir_location
                .as_ref()
                .is_none_or(|location| location.is_empty())
        {
            return Err(DeploymentError::Other(format!(
                "Source deployment {source_deployment_id} has no reusable static artifact"
            )));
        }

        let now = chrono::Utc::now();

        // Get next deployment number
        let deployment_count = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to count deployments: {}", e)))?;
        let deployment_number = deployment_count + 1;

        let promote_slug = format!("{}-{}", project.slug, deployment_number);

        let promote_metadata = DeploymentMetadata {
            runtime_overrides: source
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.runtime_overrides.clone()),
            // Reuse build info from source
            builder: source.metadata.as_ref().and_then(|m| m.builder.clone()),
            image_size_bytes: source.metadata.as_ref().and_then(|m| m.image_size_bytes),
            health_check_path: source
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.health_check_path.clone()),
            command: source
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.command.clone()),
            ..Default::default()
        };

        // Merge deployment config for the target environment
        let merged_config = if let Some(project_config) = &project.deployment_config {
            if let Some(env_config) = &target_env.deployment_config {
                Some(project_config.merge(env_config))
            } else {
                Some(project_config.clone())
            }
        } else {
            target_env.deployment_config.clone()
        };

        let deployment_config_snapshot = merged_config.map(|config| {
            temps_entities::deployment_config::DeploymentConfigSnapshot::from_config(
                &config,
                std::collections::HashMap::new(),
            )
        });

        let promotion_asset_origin = deployment_asset_origin(self.db.as_ref(), &source).await?;
        let new_deployment = deployments::ActiveModel {
            id: sea_orm::NotSet,
            project_id: Set(project_id),
            environment_id: Set(target_environment_id),
            slug: Set(promote_slug.clone()),
            state: Set("pending".to_string()),
            metadata: Set(Some(promote_metadata)),
            branch_ref: Set(source.branch_ref.clone()),
            tag_ref: Set(source.tag_ref.clone()),
            commit_sha: Set(source.commit_sha.clone()),
            commit_message: Set(source.commit_message.clone()),
            commit_author: Set(source.commit_author.clone()),
            commit_json: Set(source.commit_json.clone()),
            image_name: Set(image_name.clone()),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            deploying_at: Set(Some(now)),
            ready_at: Set(None),
            static_dir_location: Set(source.static_dir_location.clone()),
            screenshot_location: Set(None),
            cancelled_reason: Set(None),
            context_vars: Set(Some(serde_json::json!({
                "trigger": "promotion",
                "source_deployment_id": promotion_asset_origin.deployment_id,
                "source_deployment_slug": promotion_asset_origin.slug.clone(),
                "source_environment_id": promotion_asset_origin.environment_id,
            }))),
            deployment_config: Set(deployment_config_snapshot),
            promoted_from_deployment_id: Set(Some(source_deployment_id)),
            upload_request_id: Set(None),
            docker_socket_mounted: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
        };

        let promoted_deployment = insert_deployment_with_generation_lock(
            self.db.as_ref(),
            project_id,
            target_environment_id,
            new_deployment,
        )
        .await
        .map_err(|e| {
            DeploymentError::Other(format!("Failed to create promoted deployment: {}", e))
        })?;

        let promoted_id = promoted_deployment.id;
        info!(
            "Created promoted deployment #{} (from #{} to environment '{}')",
            promoted_id, source_deployment_id, target_env.name
        );

        let admitted = match super::job_processor::JobProcessorService::try_admit_deployment(
            self.db.as_ref(),
            promoted_id,
        )
        .await
        {
            Ok(admitted) => admitted,
            Err(error) => {
                return Err(self
                    .fail_reuse_deployment(promoted_id, "Failed to admit promotion", error)
                    .await);
            }
        };
        if !admitted {
            return Err(self
                .fail_reuse_deployment(
                    promoted_id,
                    "Promotion was not admitted",
                    "its project or environment is being deleted",
                )
                .await);
        }

        // As with rollback, every error after admission must converge the new
        // row to a durable terminal state.
        let execution_result: Result<(), DeploymentError> = async {
        // Same logic as rollback — for static presets, just update env pointer
        if is_static {
            info!("Promotion: Static preset detected — updating environment only");

            if let Err(error) = self
                .complete_static_reuse(&project, &target_env, &promoted_deployment, now)
                .await
            {
                return Err(self
                    .fail_reuse_deployment(
                        promoted_id,
                        "Failed to activate static promotion",
                        error,
                    )
                    .await);
            }
        } else {
            let image_name = image_name.ok_or_else(|| {
                DeploymentError::Other(format!(
                    "Source deployment {source_deployment_id} has no reusable image reference"
                ))
            })?;

            // --- Create per-job log paths (matching rollback/normal deployment pattern) ---
            let deploy_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-deploy_container.log",
                project.slug,
                target_env.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                promoted_id
            );
            let complete_log_id = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-mark_deployment_complete.log",
                project.slug,
                target_env.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                promoted_id
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

            // --- Create deployment_jobs records ---
            use temps_entities::{deployment_jobs, types::JobStatus};

            let deploy_job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(promoted_id),
                job_id: Set("deploy_container".to_string()),
                job_type: Set("DeployImageJob".to_string()),
                name: Set("Deploy Container".to_string()),
                description: Set(Some(format!("Promote: deploy image {}", image_name))),
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
                deployment_id: Set(promoted_id),
                job_id: Set("mark_deployment_complete".to_string()),
                job_type: Set("MarkDeploymentCompleteJob".to_string()),
                name: Set("Mark Deployment Complete".to_string()),
                description: Set(Some("Finalize promoted deployment".to_string())),
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

            info!("Promotion: Deploying image: {}", image_name);

            // Execute DeployImageJob with external image
            let configured_port =
                super::port_resolver::configured_port_override(&target_env, &project);
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
                    .health(self.reused_image_health(promotion_asset_origin.deployment_id, &target_env, &project).await?)
                    .target(crate::jobs::DeploymentTarget::Docker {
                        registry_url: "local".to_string(),
                        network: Some(temps_core::NETWORK_NAME.to_string()),
                    })
                    .service_name(promote_slug.clone())
                    .health_check_path(None)
                    .health_check_path_override(
                        promoted_deployment
                            .metadata
                            .as_ref()
                            .and_then(|metadata| metadata.health_check_path.clone()),
                    )
                    .command(
                        promoted_deployment
                            .metadata
                            .as_ref()
                            .and_then(|metadata| metadata.container_runtime().command),
                    )
                    .entrypoint(promoted_deployment.metadata.as_ref()
                        .and_then(|metadata| metadata.container_runtime().entrypoint))
                    .replicas(
                        target_env
                            .deployment_config
                            .as_ref()
                            .map(|c| c.replicas as u32)
                            .or_else(|| {
                                project
                                    .deployment_config
                                    .as_ref()
                                    .map(|c| c.replicas as u32)
                            })
                            .unwrap_or(1),
                    )
                    .port(exposed_port)
                    .configured_port(configured_port)
                    .log_id(deploy_log_id.clone())
                    .log_service(self.log_service.clone())
                    .failed_container_retention(self.db.clone(), promoted_id);

            // Resolve CPU/memory limits + requests (target env → project),
            // matching the normal deploy path so a promotion preserves a
            // configured limit and leaves an unconfigured environment uncapped.
            deploy_builder = deploy_builder.resources(Self::resolve_resource_usage(
                target_env.deployment_config.as_ref(),
                project.deployment_config.as_ref(),
            ));

            // Resolve the TARGET environment's env vars exactly as a normal
            // deploy does, so the promoted container boots with the full set
            // (user vars, external-service connection strings, SENTRY_DSN,
            // TEMPS_API_TOKEN/URL, CRON_SECRET, OTEL_*) instead of nothing.
            // Without this, promotion reuses the image but starts it unconfigured.
            let resolved_env = if let Some(resolver) = self.env_resolver.get() {
                let mut resolved = resolver
                    .resolve(&project, &target_env, &promoted_deployment)
                    .await?;
                crate::services::env_resolver::apply_deployment_owned_variables(
                    &mut resolved,
                    project.preset,
                    &promotion_asset_origin.slug,
                    (project.preset != temps_entities::preset::Preset::DockerCompose)
                        .then_some(exposed_port),
                );
                resolved
            } else {
                tracing::warn!(
                    "Promotion: env resolver not wired — promoted container starts with no resolved env vars"
                );
                std::collections::HashMap::new()
            };
            deploy_builder = deploy_builder.environment_variables(resolved_env.clone());

            deploy_builder = self
                .configure_reuse_deploy_builder(
                    deploy_builder,
                    &project,
                    &target_env,
                    &image_name,
                    &resolved_env,
                )
                .await;

            let provenance = self
                .reuse_image_provenance(
                    &project,
                    &source,
                    promotion_asset_origin.deployment_id,
                    &image_name,
                )
                .await?;
            info!(
                "Promotion: image {} is {}-sourced (origin deployment #{}, expected identity: {})",
                image_name,
                provenance.source,
                promotion_asset_origin.deployment_id,
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

            // Create workflow context for the promoted deployment
            let mock_log_writer = Arc::new(crate::test_utils::MockLogWriter::new(0));
            let mut promote_context = temps_core::WorkflowContext::new(
                format!("promote-{}", promoted_id),
                promoted_id,
                project_id,
                target_environment_id,
                mock_log_writer,
            );

            let cancellation_provider =
                super::workflow_execution_service::DatabaseCancellationProvider::new(
                    self.db.clone(),
                    promoted_id,
                );
            match deploy_job
                .execute_with_cancellation(promote_context.clone(), &cancellation_provider)
                .await
            {
                Ok(job_result) => {
                    info!("Promotion: Deploy job completed successfully");
                    promote_context = job_result.context;

                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Promotion: Deploy job failed: {}", e);
                    let failure_message = match deploy_job.cleanup(&promote_context).await {
                        Ok(()) => format!("Deploy failed: {e}"),
                        Err(cleanup_error) => format!(
                            "Deploy failed: {e}; promoted container cleanup also failed: {cleanup_error}"
                        ),
                    };

                    let mut active_job: deployment_jobs::ActiveModel = deploy_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(failure_message.clone()));
                    let _ = active_job.update(self.db.as_ref()).await;

                    let mut active_complete: deployment_jobs::ActiveModel =
                        complete_job_model.into();
                    active_complete.status = Set(JobStatus::Cancelled);
                    active_complete.error_message = Set(Some("Deploy job failed".to_string()));
                    let _ = active_complete.update(self.db.as_ref()).await;

                    let mut active_dep: deployments::ActiveModel =
                        promoted_deployment.clone().into();
                    active_dep.state = Set("failed".to_string());
                    active_dep.finished_at = Set(Some(chrono::Utc::now()));
                    active_dep.cancelled_reason = Set(Some(failure_message.clone()));
                    let _ = active_dep.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to deploy image during promotion: {failure_message}"
                    )));
                }
            }

            // Execute MarkDeploymentCompleteJob
            info!("Promotion: Marking deployment {} as complete", promoted_id);

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
                .deployment_id(promoted_id)
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
            let cleanup_context = promote_context.clone();
            match mark_complete_job.execute(promote_context).await {
                Ok(_) => {
                    info!("Promotion: Mark complete job executed successfully");

                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Success);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    let _ = active_job.update(self.db.as_ref()).await;
                }
                Err(e) => {
                    error!("Promotion: Mark complete job failed: {}", e);

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
                        "promotion",
                    )
                    .await;

                    let mut active_job: deployment_jobs::ActiveModel = complete_job_model.into();
                    active_job.status = Set(temps_entities::types::JobStatus::Failure);
                    active_job.finished_at = Set(Some(chrono::Utc::now()));
                    active_job.error_message = Set(Some(failure_message.clone()));
                    let _ = active_job.update(self.db.as_ref()).await;

                    return Err(DeploymentError::Other(format!(
                        "Failed to mark deployment complete during promotion: {failure_message}"
                    )));
                }
            }

            info!(
                "Promotion completed - deployment {} is now active",
                promoted_id
            );
        }
        Ok(())
        }
        .await;
        if let Err(error) = execution_result {
            return Err(self
                .fail_reuse_deployment(promoted_id, "Promotion artifact reuse failed", error)
                .await);
        }

        // Re-fetch the promoted deployment to get the final state
        let final_deployment = deployments::Entity::find_by_id(promoted_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::Other("Promoted deployment disappeared".to_string()))?;

        Ok(self
            .map_db_deployment_to_deployment(final_deployment, true, None)
            .await)
    }
}
