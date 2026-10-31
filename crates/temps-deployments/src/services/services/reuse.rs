// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What rollback and promotion share: resolving the build artifact a
//! deployment reuses, its image provenance, health and resources, the static
//! reuse completion, and the failure paths.

use super::*;

fn resolve_reused_health(
    build_outputs: Option<&serde_json::Value>,
    environment: Option<&temps_entities::deployment_config::DeploymentConfig>,
    project: Option<&temps_entities::deployment_config::DeploymentConfig>,
) -> Result<Option<temps_core::repo_config::HealthCheckConfig>, DeploymentError> {
    let repo_health = build_outputs.and_then(|outputs| outputs.get("health"));
    let health = if let Some(value) = repo_health {
        Some(
            serde_json::from_value::<temps_core::repo_config::HealthCheckConfig>(value.clone())
                .map_err(|error| {
                    DeploymentError::InvalidInput(format!(
                        "Cannot reuse image health configuration: {error}"
                    ))
                })?,
        )
    } else {
        environment
            .and_then(|config| config.health.clone())
            .or_else(|| project.and_then(|config| config.health.clone()))
    };
    if let Some(health) = &health {
        health.validate().map_err(|reason| {
            DeploymentError::InvalidInput(format!(
                "Cannot reuse image health configuration: {reason}"
            ))
        })?;
    }
    Ok(health)
}

/// Run a failed mark-complete job's cleanup and describe both outcomes.
///
/// Promotion and rollback drive their jobs by hand instead of through the
/// workflow executor, so they must also do what the executor does when a
/// job fails: call its `cleanup`. For `MarkDeploymentCompleteJob` that tears
/// down the containers the deploy step started, unless the deployment
/// already reached completed/deployed.
pub(super) async fn clean_up_failed_completion(
    job: &dyn WorkflowTask,
    context: &temps_core::WorkflowContext,
    error: &temps_core::WorkflowError,
    operation: &str,
) -> String {
    match job.cleanup(context).await {
        Ok(()) => format!("Mark complete failed: {error}"),
        Err(cleanup_error) => {
            error!(
                "{operation}: cleanup after failed mark-complete for deployment {} also failed: {}",
                context.deployment_id, cleanup_error
            );
            format!(
                "Mark complete failed: {error}; {operation} container cleanup also failed: {cleanup_error}"
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeploymentAssetOrigin {
    pub(super) deployment_id: i32,
    pub(super) environment_id: i32,
    pub(super) slug: String,
}

/// Resolve the immutable build artifact behind a deployment. Promotion and
/// rollback rows may already reference an earlier source, so propagating the
/// immediate row would lose assets after the second hop.
fn complete_deployment_asset_origin(
    deployment_id: i32,
    environment_id: i32,
    slug: &str,
    context: Option<&serde_json::Value>,
) -> Option<DeploymentAssetOrigin> {
    let source_deployment_id = context
        .and_then(|value| value.get("source_deployment_id"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let source_environment_id = context
        .and_then(|value| value.get("source_environment_id"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let source_slug = context
        .and_then(|value| value.get("source_deployment_slug"))
        .and_then(serde_json::Value::as_str);

    match (source_deployment_id, source_environment_id, source_slug) {
        (Some(deployment_id), Some(environment_id), Some(slug)) => Some(DeploymentAssetOrigin {
            deployment_id,
            environment_id,
            slug: slug.to_string(),
        }),
        (None, None, None) => Some(DeploymentAssetOrigin {
            deployment_id,
            environment_id,
            slug: slug.to_string(),
        }),
        _ => None,
    }
}

fn source_deployment_id(context: Option<&serde_json::Value>) -> Option<i32> {
    context
        .and_then(|value| value.get("source_deployment_id"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

/// Resolve both the current complete reuse metadata and the partial metadata
/// written by Temps before source slugs were persisted. Legacy rows are walked
/// back to the immutable build deployment so a second promotion/rollback does
/// not perpetuate an intermediate, asset-less deployment.
pub(super) async fn deployment_asset_origin(
    db: &temps_database::DbConnection,
    deployment: &deployments::Model,
) -> Result<DeploymentAssetOrigin, DeploymentError> {
    let root_deployment_id = deployment.id;
    let project_id = deployment.project_id;
    let mut current = deployment.clone();
    let mut visited = HashSet::from([current.id]);

    loop {
        if let Some(origin) = complete_deployment_asset_origin(
            current.id,
            current.environment_id,
            &current.slug,
            current.context_vars.as_ref(),
        ) {
            return Ok(origin);
        }

        let Some(source_id) = source_deployment_id(current.context_vars.as_ref()) else {
            return Ok(DeploymentAssetOrigin {
                deployment_id: current.id,
                environment_id: current.environment_id,
                slug: current.slug,
            });
        };

        if !visited.insert(source_id) {
            return Err(DeploymentError::AssetOriginCycle {
                project_id,
                deployment_id: root_deployment_id,
                source_deployment_id: source_id,
            });
        }

        current = deployments::Entity::find_by_id(source_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(db)
            .await?
            .ok_or(DeploymentError::AssetOriginNotFound {
                project_id,
                deployment_id: root_deployment_id,
                source_deployment_id: source_id,
            })?;
    }
}

/// What a rollback or promotion knows about the image it reuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReuseImageProvenance {
    pub(super) source: crate::jobs::DeployImageSource,
    pub(super) expected_identity: Option<crate::jobs::ExpectedImageIdentity>,
    /// The workers holding the reused image, when the origin built it on
    /// workers and the control plane cannot serve it itself.
    worker_build: Option<crate::jobs::ReusedWorkerBuild>,
}

impl ReuseImageProvenance {
    pub(super) fn apply(&self, job: crate::jobs::DeployImageJob) -> crate::jobs::DeployImageJob {
        let job = job.with_image_source(self.source);
        let job = match &self.worker_build {
            Some(worker_build) => job.with_reused_worker_build(worker_build.clone()),
            None => job,
        };
        match &self.expected_identity {
            Some(expected) => job.with_expected_image_identity(expected.clone()),
            None => job,
        }
    }

    pub(super) fn expected_identity_label(&self) -> String {
        self.expected_identity
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "none recorded".to_string())
    }
}

impl DeploymentService {
    /// Resolve CPU/memory limits + requests for a deploy from the environment
    /// config first, then the project config, leaving each field unset when
    /// neither configures it (→ no Docker limit = uncapped). Mirrors the
    /// resolution in `WorkflowExecutionService` so every deploy path (initial,
    /// rollback, promote) treats resource limits as opt-in identically.
    ///
    /// CPU is stored as microcores in the DB (1_000_000 = 1 core), memory as MB;
    /// emitted with the `u`/`Mi` suffixes the deployer's parsers understand.
    pub(super) fn resolve_resource_usage(
        env_cfg: Option<&temps_entities::deployment_config::DeploymentConfig>,
        proj_cfg: Option<&temps_entities::deployment_config::DeploymentConfig>,
    ) -> crate::jobs::ResourceUsage {
        let resolve = |getter: fn(
            &temps_entities::deployment_config::DeploymentConfig,
        ) -> Option<i32>|
         -> Option<i32> {
            env_cfg
                .and_then(getter)
                .or_else(|| proj_cfg.and_then(getter))
        };
        crate::jobs::ResourceUsage {
            cpu_limit: resolve(|c| c.cpu_limit).map(|u| format!("{}u", u)),
            memory_limit: resolve(|c| c.memory_limit).map(|mb| format!("{}Mi", mb)),
            cpu_request: resolve(|c| c.cpu_request).map(|u| format!("{}u", u)),
            memory_request: resolve(|c| c.memory_request).map(|mb| format!("{}Mi", mb)),
        }
    }

    /// Where the image reused by a rollback or promotion lives (and so how a
    /// remote worker obtains it), and which image the reuse is bound to.
    ///
    /// Classified from the deployment that originally produced the image
    /// (`origin_deployment_id`, from [`deployment_asset_origin`]): the
    /// rollback/promotion rows in between record no image metadata of their
    /// own. An uploaded or control-plane-built image must be streamed to the
    /// worker; only a registry image can be pulled there.
    ///
    /// For a registry image, the expected identity is the digest recorded on
    /// the origin's registered external image, when one was recorded. Without
    /// it the deploy job never substitutes the control plane's copy of the tag
    /// for a failed worker pull.
    pub(super) async fn reuse_image_provenance(
        &self,
        project: &projects::Model,
        target: &deployments::Model,
        origin_deployment_id: i32,
        image_name: &str,
    ) -> Result<ReuseImageProvenance, DeploymentError> {
        // `deployment_asset_origin` usually resolves the origin id from the
        // row's recorded context vars without loading the origin row, so its
        // metadata has to be fetched here.
        let origin = if origin_deployment_id == target.id {
            None
        } else {
            let origin = deployments::Entity::find_by_id(origin_deployment_id)
                .filter(deployments::Column::ProjectId.eq(project.id))
                .one(self.db.as_ref())
                .await?;
            if origin.is_none() {
                warn!(
                    project_id = project.id,
                    deployment_id = target.id,
                    origin_deployment_id,
                    image = %image_name,
                    "Origin deployment of reused image not found in this project; \
                     classifying the image source from the reused deployment's own metadata"
                );
            }
            origin
        };
        let metadata = origin.as_ref().unwrap_or(target).metadata.as_ref();
        let source = crate::jobs::DeployImageSource::for_existing_deployment(
            project.source_type,
            metadata,
            image_name,
        );

        let expected_identity = match (
            source,
            metadata.and_then(|metadata| metadata.external_image_id),
        ) {
            (crate::jobs::DeployImageSource::Registry, Some(external_image_id)) => {
                temps_entities::external_images::Entity::find_by_id(external_image_id)
                    .filter(temps_entities::external_images::Column::ProjectId.eq(project.id))
                    .one(self.db.as_ref())
                    .await?
                    .and_then(|external_image| external_image.digest)
                    .and_then(|digest| crate::jobs::ExpectedImageIdentity::repo_digest(&digest))
            }
            _ => None,
        };

        let worker_build = self
            .reused_worker_build(project.id, origin_deployment_id, image_name)
            .await?;

        Ok(ReuseImageProvenance {
            source,
            expected_identity,
            worker_build,
        })
    }

    /// What the origin deployment's build recorded about the workers holding
    /// `image_name`, for a rollback/promotion that must reach those workers.
    ///
    /// Used for a mixed-architecture worker build always -- the control
    /// plane never holds its other architectures -- for any worker build
    /// when this control plane runs no workloads, since it then has no copy
    /// of the image to stream from, and for a build whose node reported the
    /// image's configuration, which was therefore not copied back. A
    /// full-profile control plane keeps streaming a single-architecture
    /// image from its own copy when the build reported none, as before.
    pub(super) async fn reused_worker_build(
        &self,
        project_id: i32,
        origin_deployment_id: i32,
        image_name: &str,
    ) -> Result<Option<crate::jobs::ReusedWorkerBuild>, DeploymentError> {
        use temps_entities::deployment_jobs;
        // Only a build of this project's own deployment says where its image lives.
        let origin_in_project = deployments::Entity::find_by_id(origin_deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .is_some();
        if !origin_in_project {
            return Ok(None);
        }
        let build_outputs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(origin_deployment_id))
            .filter(deployment_jobs::Column::JobType.eq("BuildImageJob"))
            .filter(deployment_jobs::Column::Outputs.is_not_null())
            .order_by_desc(deployment_jobs::Column::Id)
            .one(self.db.as_ref())
            .await?
            .and_then(|job| job.outputs);
        let Some(worker_build) = build_outputs.and_then(|outputs| {
            crate::jobs::ReusedWorkerBuild::from_build_outputs(&outputs, image_name)
        }) else {
            return Ok(None);
        };
        let control_plane_has_no_copy = self
            .node_scheduler
            .get()
            .is_some_and(|scheduler| !scheduler.local_workloads_enabled());
        // A build whose node reported the image's configuration was not
        // copied back to the control plane, so its worker is the source.
        let control_plane_was_not_given_a_copy = worker_build.image_config.is_some();
        Ok((worker_build.is_per_architecture()
            || control_plane_has_no_copy
            || control_plane_was_not_given_a_copy)
            .then_some(worker_build))
    }

    /// Repository health travels with the reused image. Target environment settings
    /// retain their usual precedence over project defaults when no repo probe exists.
    pub(super) async fn reused_image_health(
        &self,
        origin_deployment_id: i32,
        environment: &environments::Model,
        project: &projects::Model,
    ) -> Result<Option<temps_core::repo_config::HealthCheckConfig>, DeploymentError> {
        use temps_entities::deployment_jobs;
        let outputs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(origin_deployment_id))
            .filter(deployment_jobs::Column::JobType.eq("BuildImageJob"))
            .filter(deployment_jobs::Column::Outputs.is_not_null())
            .order_by_desc(deployment_jobs::Column::Id)
            .one(self.db.as_ref())
            .await?
            .and_then(|job| job.outputs);
        resolve_reused_health(
            outputs.as_ref(),
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        )
    }

    pub(super) async fn configure_reuse_deploy_builder(
        &self,
        mut builder: crate::jobs::DeployImageJobBuilder,
        project: &projects::Model,
        environment: &environments::Model,
        image_name: &str,
        local_environment_variables: &HashMap<String, String>,
    ) -> crate::jobs::DeployImageJobBuilder {
        if let Some(scheduler) = self.node_scheduler.get() {
            builder = builder.node_scheduler(scheduler.clone());
        }
        builder = builder
            .encryption_service(self.encryption_service.clone())
            .config_service(self.config_service.clone())
            .image_builder(self.image_builder.clone());

        let environment_config = environment.deployment_config.as_ref();
        let project_config = project.deployment_config.as_ref();
        builder = builder.health_check_timeout_secs(
            temps_entities::deployment_config::resolve_health_check_timeout_seconds(
                environment_config
                    .and_then(|config| config.health_check_timeout_seconds)
                    .or_else(|| {
                        project_config.and_then(|config| config.health_check_timeout_seconds)
                    }),
            ),
        );
        if let Some(target_nodes) = environment_config
            .and_then(|config| config.configured_target_nodes().map(<[i32]>::to_vec))
            .or_else(|| {
                project_config
                    .and_then(|config| config.configured_target_nodes().map(<[i32]>::to_vec))
            })
        {
            builder = builder.target_nodes(target_nodes);
        }
        if let Some(target_labels) = environment_config
            .and_then(|config| config.configured_target_labels().cloned())
            .or_else(|| {
                project_config.and_then(|config| config.configured_target_labels().cloned())
            })
        {
            builder = builder.target_labels(target_labels);
        }
        builder = builder.anti_affinity(
            environment_config
                .map(|config| config.anti_affinity)
                .or_else(|| project_config.map(|config| config.anti_affinity))
                .unwrap_or(true),
        );

        if let Some(planner) = self.workflow_planner.get() {
            let remote_plan = planner
                .build_remote_environment_variables(project, local_environment_variables)
                .await;
            builder = builder
                .remote_environment_variables(remote_plan.variables)
                .cross_node_service_blockers(remote_plan.blockers);
        }

        if let Ok(settings) = self.config_service.get_settings().await {
            builder = builder.container_log_config(temps_deployer::ContainerLogConfig::new(
                settings.container_logs.max_size.clone(),
                settings.container_logs.max_file,
            ));
            let registry = &settings.docker_registry;
            if registry.enabled {
                if let (Some(username), Some(password), Some(registry_url)) = (
                    registry.username.clone(),
                    registry.password.clone(),
                    registry.registry_url.clone(),
                ) {
                    let image_registry =
                        crate::jobs::PullExternalImageJob::registry_from_image_ref(image_name);
                    let configured_registry =
                        crate::jobs::PullExternalImageJob::registry_host_from_url(&registry_url);
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
                            "Skipping Docker registry credentials for reused image because the registry does not match"
                        );
                    }
                }
            }
        }

        builder
    }

    pub(super) async fn fail_reuse_deployment(
        &self,
        deployment_id: i32,
        operation: &str,
        error: impl std::fmt::Display,
    ) -> DeploymentError {
        let reason = format!("{operation} for deployment {deployment_id}: {error}");
        let update = deployments::Entity::update_many()
            .col_expr(
                deployments::Column::State,
                sea_orm::sea_query::Expr::value("failed"),
            )
            .col_expr(
                deployments::Column::FinishedAt,
                sea_orm::sea_query::Expr::value(Some(chrono::Utc::now())),
            )
            .col_expr(
                deployments::Column::CancelledReason,
                sea_orm::sea_query::Expr::value(Some(reason.clone())),
            )
            .filter(deployments::Column::Id.eq(deployment_id))
            .filter(deployments::Column::State.is_in(["pending", "running", "creating"]))
            .exec(self.db.as_ref())
            .await;

        match update {
            Ok(result) if result.rows_affected == 1 => DeploymentError::Other(reason),
            Ok(_) => match deployments::Entity::find_by_id(deployment_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(deployment))
                    if matches!(
                        deployment.state.as_str(),
                        "cancelled" | "stopped" | "completed" | "deployed" | "failed"
                    ) =>
                {
                    DeploymentError::Other(format!(
                        "{reason}; terminal state '{}' was preserved{}",
                        deployment.state,
                        deployment
                            .cancelled_reason
                            .as_deref()
                            .map(|existing| format!(": {existing}"))
                            .unwrap_or_default()
                    ))
                }
                Ok(Some(deployment)) => DeploymentError::DatabaseError {
                    reason: format!(
                        "{reason}; additionally failed to persist terminal state from unexpected state '{}'",
                        deployment.state
                    ),
                },
                Ok(None) => DeploymentError::DatabaseError {
                    reason: format!(
                        "{reason}; additionally failed to persist terminal state because the deployment disappeared"
                    ),
                },
                Err(read_error) => DeploymentError::DatabaseError {
                    reason: format!(
                        "{reason}; additionally failed to verify terminal state: {read_error}"
                    ),
                },
            },
            Err(update_error) => DeploymentError::DatabaseError {
                reason: format!(
                    "{reason}; additionally failed to persist terminal state: {update_error}"
                ),
            },
        }
    }

    pub(super) async fn complete_static_reuse(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DeploymentError> {
        let log_id = format!(
            "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-mark_deployment_complete.log",
            project.slug,
            environment.slug,
            now.format("%Y"),
            now.format("%m"),
            now.format("%d"),
            now.format("%H"),
            now.format("%M"),
            deployment.id
        );
        self.log_service
            .create_log_path(&log_id)
            .await
            .map_err(|error| {
                DeploymentError::Other(format!(
                    "Failed to create static completion log path: {error}"
                ))
            })?;

        let job = crate::jobs::MarkDeploymentCompleteJobBuilder::new()
            .environment_addresses(self.environment_addresses.get().cloned())
            .job_id("mark_deployment_complete".to_string())
            .deployment_id(deployment.id)
            .db(self.db.clone())
            .log_id(log_id)
            .log_service(self.log_service.clone())
            .container_deployer(self.deployer.clone())
            .queue(self.queue_service.clone())
            .config_service(self.config_service.clone())
            .encryption_service(self.encryption_service.clone())
            .workload_plane(self.workload_plane.get().cloned())
            .build()
            .map_err(|error| {
                DeploymentError::Other(format!("Failed to create static completion job: {error}"))
            })?;
        let context = temps_core::WorkflowContext::new(
            format!("static-reuse-{}", deployment.id),
            deployment.id,
            project.id,
            environment.id,
            Arc::new(crate::test_utils::MockLogWriter::new(0)),
        );
        match job.execute(context).await {
            Ok(_) => Ok(()),
            Err(error) => {
                self.reconcile_static_completion_error(
                    environment.id,
                    deployment.id,
                    environment.current_deployment_id,
                    error,
                )
                .await
            }
        }
    }

    pub(super) async fn reconcile_static_completion_error(
        &self,
        environment_id: i32,
        deployment_id: i32,
        previous_deployment_id: Option<i32>,
        error: impl std::fmt::Display,
    ) -> Result<(), DeploymentError> {
        let reason = format!("Failed to complete static reuse: {error}");
        let mut route_receiver = self.queue_service.subscribe();
        let transaction = self.db.begin().await?;
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .lock(sea_orm::sea_query::LockType::Update)
            .one(&transaction)
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Environment {environment_id} not found while reconciling static deployment {deployment_id}"
                ))
            })?;
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .lock(sea_orm::sea_query::LockType::Update)
            .one(&transaction)
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Deployment {deployment_id} not found while reconciling static completion for environment {environment_id}"
                ))
            })?;

        if environment.current_deployment_id != Some(deployment_id) {
            transaction.commit().await?;
            return Err(DeploymentError::Other(reason));
        }
        if deployment.state == "completed" {
            let completed_environment = environment.clone();
            let completed_deployment = deployment.clone();
            let mut active_environment: environments::ActiveModel = environment.into();
            active_environment.sleeping = Set(false);
            active_environment.updated_at = Set(chrono::Utc::now());
            active_environment.update(&transaction).await?;
            transaction.commit().await?;

            self.queue_service
                .send(temps_core::Job::ForceRouteReload(
                    temps_core::ForceRouteReloadJob {
                        environment_id: Some(environment_id),
                        deployment_id: Some(deployment_id),
                    },
                ))
                .await
                .map_err(|reload_error| DeploymentError::Other(format!(
                    "{reason}; completed deployment remained selected but its route reload request failed: {reload_error}"
                )))?;
            crate::jobs::MarkDeploymentCompleteJob::wait_for_route_ready(
                &mut route_receiver,
                self.db.as_ref(),
                environment_id,
                deployment_id,
                std::time::Duration::from_secs(60),
            )
            .await
            .map_err(|route_error| DeploymentError::Other(format!(
                "{reason}; completed deployment remained selected but route confirmation failed: {route_error}"
            )))?;
            let url = if !completed_environment.host.is_empty() {
                let scheme = self
                    .config_service
                    .get_url_scheme()
                    .await
                    .unwrap_or_else(|_| "https".to_string());
                Some(format!("{}://{}", scheme, completed_environment.host))
            } else if !completed_environment.subdomain.is_empty() {
                self.config_service
                    .get_deployment_url_by_slug(&completed_environment.subdomain)
                    .await
                    .ok()
            } else {
                None
            };
            if let Err(event_error) = self
                .queue_service
                .send(temps_core::Job::DeploymentSucceeded(
                    temps_core::DeploymentSucceededJob {
                        deployment_id,
                        project_id: completed_deployment.project_id,
                        environment_id,
                        environment_name: completed_environment.name,
                        commit_sha: completed_deployment.commit_sha,
                        url,
                        health_check_path: completed_deployment
                            .metadata
                            .and_then(|metadata| metadata.health_check_path),
                    },
                ))
                .await
            {
                warn!(
                    deployment_id,
                    environment_id,
                    error = %event_error,
                    "Failed to re-emit DeploymentSucceeded during static completion reconciliation"
                );
            }
            warn!(
                deployment_id,
                environment_id,
                error = %error,
                "Static deployment completed and remains selected; treating a post-completion side-effect failure as success"
            );
            return Ok(());
        }

        let previous_is_usable = match previous_deployment_id {
            Some(previous_id) => deployments::Entity::find_by_id(previous_id)
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .filter(deployments::Column::State.is_in(["completed", "deployed", "stopped"]))
                .one(&transaction)
                .await?
                .is_some(),
            None => true,
        };
        if !previous_is_usable {
            transaction.commit().await?;
            return Err(DeploymentError::Other(format!(
                "{reason}; previous deployment is no longer usable, so the selected route was not changed"
            )));
        }

        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(previous_deployment_id);
        active_environment.update(&transaction).await?;
        transaction.commit().await?;

        self.queue_service
            .send(temps_core::Job::ForceRouteReload(
                temps_core::ForceRouteReloadJob {
                    environment_id: Some(environment_id),
                    deployment_id: previous_deployment_id,
                },
            ))
            .await
            .map_err(|reload_error| DeploymentError::Other(format!(
                "{reason}; restored the previous route in the database but failed to request a route reload: {reload_error}"
            )))?;

        if let Some(previous_id) = previous_deployment_id {
            crate::jobs::MarkDeploymentCompleteJob::wait_for_route_ready(
                &mut route_receiver,
                self.db.as_ref(),
                environment_id,
                previous_id,
                std::time::Duration::from_secs(60),
            )
            .await
            .map_err(|route_error| DeploymentError::Other(format!(
                "{reason}; restored deployment {previous_id} but route confirmation failed: {route_error}"
            )))?;
        }

        Err(DeploymentError::Other(reason))
    }
}

#[cfg(test)]
mod failed_completion_cleanup_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_core::{JobResult, WorkflowContext, WorkflowError};

    struct NoopLogWriter;

    #[async_trait::async_trait]
    impl temps_core::LogWriter for NoopLogWriter {
        async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            1
        }
    }

    /// A job whose cleanup counts its calls and optionally fails.
    #[derive(Debug)]
    struct CleanupProbe {
        cleanups: AtomicUsize,
        cleanup_error: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl WorkflowTask for CleanupProbe {
        fn job_id(&self) -> &str {
            "mark_deployment_complete"
        }

        fn name(&self) -> &str {
            "probe"
        }

        fn description(&self) -> &str {
            "records cleanup calls"
        }

        async fn execute(&self, _context: WorkflowContext) -> Result<JobResult, WorkflowError> {
            Err(WorkflowError::JobExecutionFailed("not used".to_string()))
        }

        async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
            self.cleanups.fetch_add(1, Ordering::SeqCst);
            match self.cleanup_error {
                Some(reason) => Err(WorkflowError::JobExecutionFailed(reason.to_string())),
                None => Ok(()),
            }
        }
    }

    fn context() -> WorkflowContext {
        WorkflowContext::new("run-1".to_string(), 31, 1, 2, Arc::new(NoopLogWriter))
    }

    #[tokio::test]
    async fn failed_completion_runs_the_jobs_cleanup() {
        let probe = CleanupProbe {
            cleanups: AtomicUsize::new(0),
            cleanup_error: None,
        };
        let error = WorkflowError::JobExecutionFailed("route never became ready".to_string());

        let message = clean_up_failed_completion(&probe, &context(), &error, "promotion").await;

        assert_eq!(probe.cleanups.load(Ordering::SeqCst), 1);
        assert!(message.contains("route never became ready"), "{message}");
        assert!(!message.contains("cleanup also failed"), "{message}");
    }

    #[tokio::test]
    async fn failed_cleanup_is_reported_alongside_the_original_error() {
        let probe = CleanupProbe {
            cleanups: AtomicUsize::new(0),
            cleanup_error: Some("worker node 4 unreachable"),
        };
        let error = WorkflowError::JobExecutionFailed("HTTP 503".to_string());

        let message = clean_up_failed_completion(&probe, &context(), &error, "rollback").await;

        assert_eq!(probe.cleanups.load(Ordering::SeqCst), 1);
        assert!(message.contains("HTTP 503"), "{message}");
        assert!(
            message.contains("rollback container cleanup also failed"),
            "{message}"
        );
        assert!(message.contains("worker node 4 unreachable"), "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_probe_reuse_preserves_repo_probe_and_target_setting_precedence() {
        use temps_core::repo_config::HealthCheckConfig;
        use temps_entities::deployment_config::DeploymentConfig;
        let exec = HealthCheckConfig {
            probe_type: Some("exec".into()),
            command: vec!["check-worker".into()],
            ..Default::default()
        };
        let project = DeploymentConfig {
            health: Some(exec.clone()),
            ..Default::default()
        };
        let environment = DeploymentConfig {
            health: Some(HealthCheckConfig {
                probe_type: Some("process".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_reused_health(None, None, Some(&project)).unwrap(),
            Some(exec.clone())
        );
        assert_eq!(
            resolve_reused_health(None, Some(&environment), Some(&project)).unwrap(),
            environment.health
        );
        assert_eq!(
            resolve_reused_health(
                Some(&serde_json::json!({"health":exec})),
                Some(&environment),
                Some(&project)
            )
            .unwrap(),
            Some(exec)
        );
        for health in [
            serde_json::json!({"type":"typo"}),
            serde_json::json!({"type":"exec","command":[]}),
            serde_json::json!("malformed"),
        ] {
            assert!(resolve_reused_health(
                Some(&serde_json::json!({"health":health})),
                None,
                Some(&project)
            )
            .is_err());
        }
    }

    #[test]
    fn complete_asset_origin_stays_canonical_across_reuse_hops() {
        let first_reuse_context = serde_json::json!({
            "source_deployment_id": 10,
            "source_environment_id": 20,
            "source_deployment_slug": "original-build",
        });

        let origin =
            complete_deployment_asset_origin(30, 40, "first-promotion", Some(&first_reuse_context))
                .expect("complete reuse metadata should resolve without a database lookup");
        assert_eq!(
            origin,
            DeploymentAssetOrigin {
                deployment_id: 10,
                environment_id: 20,
                slug: "original-build".to_string(),
            }
        );

        let second_reuse_context = serde_json::json!({
            "source_deployment_id": origin.deployment_id,
            "source_environment_id": origin.environment_id,
            "source_deployment_slug": origin.slug.clone(),
        });
        assert_eq!(
            complete_deployment_asset_origin(
                50,
                60,
                "second-promotion",
                Some(&second_reuse_context),
            ),
            Some(origin)
        );
    }
}
