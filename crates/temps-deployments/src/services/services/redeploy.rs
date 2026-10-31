// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Redeploys: an environment's current deployment rebuilt from its recorded
//! source, including the node drain and failover planning.

use super::cancel::confined_archive_path;
use super::*;

/// Most redeploys [`DeploymentService::plan_redeploys`] plans in one call.
pub const MAX_REDEPLOY_PLAN_BATCH: usize = 100;

pub(super) fn check_redeploy_plan_batch(targets: usize) -> Result<(), DeploymentError> {
    if targets > MAX_REDEPLOY_PLAN_BATCH {
        return Err(DeploymentError::InvalidInput(format!(
            "Cannot plan {targets} redeploys at once: plan at most {MAX_REDEPLOY_PLAN_BATCH} per call"
        )));
    }
    Ok(())
}

/// What a node drain or failover redeploy rebuilds a deployment from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeploySource {
    /// A pre-built image, pulled again (imports, `deployFromImage`).
    Image {
        image_ref: String,
        health_check_path: Option<String>,
        command: Option<Vec<String>>,
    },
    /// The uploaded source archive the deployment was built from.
    SourceBundle,
    /// The project's repository at the deployment's ref.
    Git {
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
    },
}

impl DeploymentService {
    /// Redeploy the exact workload affected by node drain or failover.
    pub async fn redeploy_environment(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        self.redeploy_environment_inner(project_id, environment_id, deployment_id, None)
            .await
    }

    /// Redeploy a workload after its node went offline. Jobs emitted by this
    /// path are serialized by the deployment processor so a failover fan-out
    /// cannot saturate the build host.
    pub async fn redeploy_environment_for_failover(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        self.redeploy_environment_inner(
            project_id,
            environment_id,
            deployment_id,
            Some(deployment_id),
        )
        .await
    }

    /// Confirm that [`Self::redeploy_environment`] can rebuild this deployment
    /// somewhere else, without queuing anything.
    pub async fn check_redeployable(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        self.plan_redeploys(&[(project_id, environment_id, deployment_id)])
            .await?
            .pop()
            .unwrap_or_else(|| {
                Err(DeploymentError::Other(format!(
                    "No redeploy plan was produced for deployment {deployment_id}"
                )))
            })
            .map(|_| ())
    }

    /// What each `(project_id, environment_id, deployment_id)` would be rebuilt
    /// from, in order, without queuing anything. Node drain plans every
    /// workload it has to move *before* it marks the node draining, so a
    /// workload with nothing to rebuild from is refused up front instead of
    /// leaving the node draining forever, and then redeploys from these plans
    /// with [`Self::redeploy_environment_from`]. Two queries per call; the
    /// outer `Err` is a database failure, or more than
    /// [`MAX_REDEPLOY_PLAN_BATCH`] targets (callers plan in batches, so
    /// memory does not grow with the number of workloads).
    pub async fn plan_redeploys(
        &self,
        targets: &[(i32, i32, i32)],
    ) -> Result<Vec<Result<RedeploySource, DeploymentError>>, DeploymentError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        check_redeploy_plan_batch(targets.len())?;
        let deployment_ids: Vec<i32> = targets.iter().map(|(_, _, id)| *id).collect();
        let loaded: HashMap<i32, deployments::Model> = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids.clone()))
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                DeploymentError::Other(format!(
                    "Failed to load deployments {deployment_ids:?} to plan their redeploys: {e}"
                ))
            })?
            .into_iter()
            .map(|deployment| (deployment.id, deployment))
            .collect();
        // Only Git rebuilds need the project row.
        let project_ids: Vec<i32> = targets
            .iter()
            .filter(|(_, _, id)| {
                loaded
                    .get(id)
                    .is_some_and(|deploy| !Self::rebuilds_from_metadata(deploy))
            })
            .map(|(project_id, _, _)| *project_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let projects: HashMap<i32, projects::Model> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            projects::Entity::find()
                .filter(projects::Column::Id.is_in(project_ids.clone()))
                .filter(projects::Column::IsDeleted.eq(false))
                .all(self.db.as_ref())
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!(
                        "Failed to load projects {project_ids:?} to plan their redeploys: {e}"
                    ))
                })?
                .into_iter()
                .map(|project| (project.id, project))
                .collect()
        };

        let mut plans = Vec::with_capacity(targets.len());
        for &(project_id, environment_id, deployment_id) in targets {
            // Use the deployment that owns the affected containers. Selecting
            // the newest row can race a concurrent failed/cancelled deploy and
            // restore the wrong workload during failover.
            let Some(deploy) = loaded.get(&deployment_id).filter(|deploy| {
                deploy.project_id == project_id && deploy.environment_id == environment_id
            }) else {
                plans.push(Err(DeploymentError::NotFound(format!(
                    "Deployment {deployment_id} was not found in project {project_id}, environment {environment_id}"
                ))));
                continue;
            };
            plans.push(
                self.redeploy_source_for(deploy, projects.get(&project_id))
                    .await,
            );
        }
        Ok(plans)
    }

    /// What each workload in `batch` that a node drain has to move would be
    /// rebuilt from, keyed by deployment id. Only workloads whose every
    /// replica is on the drained node are rebuilt; the rest are retired in
    /// place and get no plan. `batch` holds at most
    /// [`MAX_REDEPLOY_PLAN_BATCH`] workloads.
    pub async fn plan_drain_redeploys(
        &self,
        batch: &[crate::services::node_service::AffectedDeployment],
    ) -> Result<HashMap<i32, Result<RedeploySource, DeploymentError>>, DeploymentError> {
        check_redeploy_plan_batch(batch.len())?;
        let to_move: Vec<&crate::services::node_service::AffectedDeployment> =
            batch.iter().filter(|dep| dep.needs_redeploy()).collect();
        let targets: Vec<(i32, i32, i32)> = to_move
            .iter()
            .map(|dep| (dep.project_id, dep.environment_id, dep.deployment_id))
            .collect();
        let plans = self.plan_redeploys(&targets).await?;
        Ok(to_move
            .iter()
            .map(|dep| dep.deployment_id)
            .zip(plans)
            .collect())
    }

    /// Whether a deployment's own metadata names what to rebuild it from (an
    /// image, or an uploaded source bundle); otherwise it rebuilds from Git.
    fn rebuilds_from_metadata(deploy: &deployments::Model) -> bool {
        deploy.metadata.as_ref().is_some_and(|metadata| {
            metadata.external_image_ref.is_some()
                || super::job_processor::source_bundle_lineage(Some(metadata)).is_some()
        })
    }

    /// What `deploy` would be rebuilt from. `project` is only consulted for a
    /// Git rebuild, and is `None` when the project is gone.
    async fn redeploy_source_for(
        &self,
        deploy: &deployments::Model,
        project: Option<&projects::Model>,
    ) -> Result<RedeploySource, DeploymentError> {
        let (project_id, environment_id, deployment_id) =
            (deploy.project_id, deploy.environment_id, deploy.id);

        // Git-less deployments (docker_image source, e.g. imports or
        // `deployFromImage`) have no branch/tag/commit to rebuild from —
        // `trigger_pipeline` requires `repo_owner`/`repo_name` and fails with
        // "Project repo_owner is missing" for these. Redeploy them from the
        // same image instead, mirroring `trigger_image_deployment`.
        if let Some(image_ref) = deploy
            .metadata
            .as_ref()
            .and_then(|m| m.external_image_ref.clone())
        {
            return Ok(RedeploySource::Image {
                image_ref,
                health_check_path: deploy
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.health_check_path.clone()),
                command: deploy
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.command.clone()),
            });
        }

        // Uploaded-source deployments have no repository either; their only
        // lineage is the archive the user uploaded, which is retained on this
        // server until the project is deleted. Rebuild from it, but only if
        // it is still there: a redeploy that would fail in
        // `PrepareSourceBundleJob` must be refused while the caller can
        // still act on it.
        if let Some((_, bundle_path)) =
            super::job_processor::source_bundle_lineage(deploy.metadata.as_ref())
        {
            let archive = confined_archive_path(&self.config_service.data_dir(), &bundle_path)?;
            return match tokio::fs::try_exists(&archive).await {
                Ok(true) => Ok(RedeploySource::SourceBundle),
                Ok(false) => Err(DeploymentError::InvalidDeploymentState(format!(
                    "Deployment {deployment_id} of project {project_id}, environment {environment_id} \
                     was built from uploaded source '{bundle_path}', which is no longer retained on \
                     this server. Upload the source again so it is built on an eligible node"
                ))),
                Err(error) => Err(DeploymentError::Other(format!(
                    "Could not check the uploaded source '{bundle_path}' of deployment \
                     {deployment_id} (project {project_id}, environment {environment_id}) at \
                     {}: {error}",
                    archive.display()
                ))),
            };
        }

        let project = project
            .ok_or_else(|| DeploymentError::NotFound(format!("project {project_id} not found")))?;
        if project.repo_owner.is_empty() || project.repo_name.is_empty() {
            return Err(DeploymentError::InvalidDeploymentState(format!(
                "Deployment {deployment_id} of project '{}' ({project_id}), environment \
                 {environment_id} cannot be rebuilt elsewhere: it records no Git repository, \
                 image reference or uploaded source bundle. Deploy it again from its source so \
                 it runs on an eligible node",
                project.slug
            )));
        }

        Ok(RedeploySource::Git {
            branch: deploy.branch_ref.clone(),
            tag: deploy.tag_ref.clone(),
            commit: deploy.commit_sha.clone(),
        })
    }

    /// Redeploy a workload from a source [`Self::plan_redeploys`] resolved,
    /// without looking it up again.
    pub async fn redeploy_environment_from(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
        source: RedeploySource,
    ) -> Result<(), DeploymentError> {
        self.dispatch_redeploy(project_id, environment_id, deployment_id, None, source)
            .await
    }

    async fn redeploy_environment_inner(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
        recovery_of_deployment_id: Option<i32>,
    ) -> Result<(), DeploymentError> {
        let source = self
            .plan_redeploys(&[(project_id, environment_id, deployment_id)])
            .await?
            .pop()
            .unwrap_or_else(|| {
                Err(DeploymentError::Other(format!(
                    "No redeploy plan was produced for deployment {deployment_id}"
                )))
            })?;
        self.dispatch_redeploy(
            project_id,
            environment_id,
            deployment_id,
            recovery_of_deployment_id,
            source,
        )
        .await
    }

    async fn dispatch_redeploy(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
        recovery_of_deployment_id: Option<i32>,
        source: RedeploySource,
    ) -> Result<(), DeploymentError> {
        match source {
            RedeploySource::Image {
                image_ref,
                health_check_path,
                command,
            } => {
                self.trigger_image_deployment_inner(
                    project_id,
                    Some(environment_id),
                    image_ref,
                    health_check_path,
                    command,
                    recovery_of_deployment_id,
                    // ADR 045: node drain and failover redeploy the workload
                    // that is already there, from the deployment's own stored
                    // image and command. No caller-chosen input, and refusing
                    // would leave a granted infrastructure service down after
                    // its node died.
                    temps_core::docker_socket_grant::DeployCaller::Platform,
                )
                .await
            }
            RedeploySource::SourceBundle => {
                info!(
                    project_id,
                    environment_id,
                    deployment_id,
                    "Redeploying uploaded-source deployment from its retained source bundle"
                );
                self.queue_service
                    .send(temps_core::Job::RedeploySourceBundleRequested(
                        temps_core::RedeploySourceBundleRequestedJob {
                            project_id,
                            environment_id,
                            source_deployment_id: deployment_id,
                            recovery_of_deployment_id,
                        },
                    ))
                    .await
                    .map_err(|e| {
                        DeploymentError::QueueError(format!(
                            "Failed to queue the uploaded-source redeploy of deployment \
                             {deployment_id} (project {project_id}, environment {environment_id}): {e}"
                        ))
                    })
            }
            RedeploySource::Git {
                branch,
                tag,
                commit,
            } => {
                self.trigger_pipeline_inner(
                    project_id,
                    environment_id,
                    PipelineTriggerOptions {
                        branch,
                        tag,
                        commit,
                        rollback_from_deployment_id: None,
                        recovery_of_deployment_id,
                    },
                )
                .await
            }
        }
    }
}
