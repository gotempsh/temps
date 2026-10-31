// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Manual pipeline and image deployment triggers.

use super::*;

impl DeploymentService {
    pub async fn trigger_pipeline(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
    ) -> Result<(), DeploymentError> {
        self.trigger_pipeline_as(
            project_id,
            environment_id,
            branch,
            tag,
            commit,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
    }

    /// [`Self::trigger_pipeline`] for a caller whose authority is known
    /// (ADR 045).
    pub async fn trigger_pipeline_as(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
        caller: temps_core::docker_socket_grant::DeployCaller,
    ) -> Result<(), DeploymentError> {
        self.guard_granted_project_deploy(project_id, caller)
            .await?;
        self.trigger_pipeline_inner(
            project_id,
            environment_id,
            PipelineTriggerOptions {
                branch,
                tag,
                commit,
                rollback_from_deployment_id: None,
                recovery_of_deployment_id: None,
            },
        )
        .await
    }

    /// Internal pipeline trigger that also carries an optional rollback marker.
    /// `rollback_from_deployment_id` is `Some(id)` only for rebuild-from-source
    /// rollbacks, which tags the resulting deployment as a rollback of `id`.
    pub(super) async fn trigger_pipeline_inner(
        &self,
        project_id: i32,
        environment_id: i32,
        options: PipelineTriggerOptions,
    ) -> Result<(), DeploymentError> {
        info!("Triggering pipeline for project_id: {}", project_id);
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        let project = project.ok_or_else(|| {
            DeploymentError::NotFound(format!("project {} not found", project_id))
        })?;
        if project.source_type == temps_entities::source_type::SourceType::External {
            return Err(DeploymentError::InvalidInput(format!(
                "Project {project_id} is for monitoring only. Configure hosting before deploying."
            )));
        }
        debug!(
            "Project found id={} slug={} preset={}",
            project.id, project.slug, project.preset
        );

        debug!(
            "Before invoking pipeline service project_id: {}, environment_id: {}",
            project_id, environment_id
        );
        // Check if repo_owner and repo_name are present
        let repo_owner = project.repo_owner.clone();
        let repo_name = project.repo_name.clone();

        // Validate that they're not empty
        if repo_owner.is_empty() {
            return Err(DeploymentError::InvalidInput(
                "Project repo_owner is missing".to_string(),
            ));
        }
        if repo_name.is_empty() {
            return Err(DeploymentError::InvalidInput(
                "Project repo_name is missing".to_string(),
            ));
        }
        let git_push_job = temps_core::GitPushEventJob {
            owner: repo_owner,
            repo: repo_name,
            branch: options.branch,
            tag: options.tag,
            commit: options.commit.unwrap_or_default(),
            project_id,
            // User-initiated trigger — bypasses environments.automatic_deploy.
            manual_trigger: true,
            rollback_from_deployment_id: options.rollback_from_deployment_id,
            // This trigger names a concrete environment (redeploy, rollback,
            // node-drain reschedule) — deploy to it directly instead of
            // re-inferring the target from the branch.
            target_environment_id: Some(environment_id),
            recovery_of_deployment_id: options.recovery_of_deployment_id,
        };

        tracing::debug!(
            "🔥 Sending GitPushEvent to queue - owner: {}, repo: {}, branch: {:?}, tag: {:?}, commit: {}",
            git_push_job.owner, git_push_job.repo, git_push_job.branch, git_push_job.tag, git_push_job.commit
        );

        self.queue_service
            .send(temps_core::Job::GitPushEvent(git_push_job))
            .await
            .map_err(|e| {
                tracing::error!("Failed to send GitPushEvent to queue: {}", e);
                DeploymentError::QueueError(e.to_string())
            })?;

        tracing::debug!("GitPushEvent successfully sent to queue");
        Ok(())
    }

    /// Trigger a real deployment of a pre-built Docker image, with no build
    /// step. This is the DockerImage-source counterpart to `trigger_pipeline`
    /// (which is Git-only and requires `repo_owner`/`repo_name`) — used to
    /// deploy projects that have no git repository at all, e.g. imports from
    /// Portainer, Kubernetes, or Kamal.
    ///
    /// Reuses the `DeployImageRequested` job already driven by the template
    /// one-click-deploy flow (see `job_processor::process_deploy_image_requested_job`),
    /// which creates the deployment row itself (with `external_image_ref` in
    /// its metadata) and plans a pull+run pipeline for the project's
    /// non-preview environment(s).
    pub async fn trigger_image_deployment(
        &self,
        project_id: i32,
        target_environment_id: Option<i32>,
        image_ref: String,
        health_check_path: Option<String>,
        command: Option<Vec<String>>,
    ) -> Result<(), DeploymentError> {
        self.trigger_image_deployment_as(
            project_id,
            target_environment_id,
            image_ref,
            health_check_path,
            command,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
    }

    /// [`Self::trigger_image_deployment`] for a caller whose authority is
    /// known (ADR 045).
    ///
    /// This is the path that matters most for a granted project: the caller
    /// supplies the image reference and the command that will run as host
    /// root.
    pub async fn trigger_image_deployment_as(
        &self,
        project_id: i32,
        target_environment_id: Option<i32>,
        image_ref: String,
        health_check_path: Option<String>,
        command: Option<Vec<String>>,
        caller: temps_core::docker_socket_grant::DeployCaller,
    ) -> Result<(), DeploymentError> {
        self.guard_granted_project_deploy(project_id, caller)
            .await?;
        self.trigger_image_deployment_inner(
            project_id,
            target_environment_id,
            image_ref,
            health_check_path,
            command,
            None,
            caller,
        )
        .await
    }

    // One argument over the lint's threshold, and the one that pushed it over
    // is `caller` (ADR 045). Bundling the rest into a struct to satisfy the
    // count would move five positional `Option`s into a struct literal that
    // the two call sites would then have to keep in sync by name — no clearer,
    // and a larger diff on a security fix.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn trigger_image_deployment_inner(
        &self,
        project_id: i32,
        target_environment_id: Option<i32>,
        image_ref: String,
        health_check_path: Option<String>,
        command: Option<Vec<String>>,
        recovery_of_deployment_id: Option<i32>,
        caller: temps_core::docker_socket_grant::DeployCaller,
    ) -> Result<(), DeploymentError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound(format!("project {project_id} not found")))?;
        if project.source_type == temps_entities::source_type::SourceType::External {
            return Err(DeploymentError::InvalidInput(format!(
                "Project {project_id} is for monitoring only. Configure hosting before deploying."
            )));
        }
        if image_ref.is_empty() {
            return Err(DeploymentError::InvalidInput(
                "Image reference is missing".to_string(),
            ));
        }

        info!(
            "Triggering image deployment for project_id: {} (image: {})",
            project_id, image_ref
        );

        self.queue_service
            .send(temps_core::Job::DeployImageRequested(
                temps_core::DeployImageRequestedJob {
                    project_id,
                    target_environment_id,
                    image_ref,
                    health_check_path,
                    command,
                    recovery_of_deployment_id,
                    // ADR 045: carried on the job because the consumer plans
                    // the deployment with no request to re-derive it from.
                    docker_socket_authorized: caller.may_deploy_granted_project(),
                },
            ))
            .await
            .map_err(|e| {
                tracing::error!("Failed to send DeployImageRequested to queue: {}", e);
                DeploymentError::QueueError(e.to_string())
            })?;

        tracing::debug!("DeployImageRequested successfully sent to queue");
        Ok(())
    }
}
