// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cancellation of running deployments, including the bulk cancel and
//! source-archive cleanup a project or environment deletion runs.

use super::*;

pub(super) fn confined_archive_path(
    data_dir: &std::path::Path,
    stored_path: &str,
) -> Result<std::path::PathBuf, DeploymentError> {
    let relative = std::path::Path::new(stored_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(DeploymentError::InvalidBundlePath {
            path: stored_path.to_string(),
            reason: "stored archive path escapes the Temps data directory".to_string(),
        });
    }
    Ok(data_dir.join(relative))
}

impl DeploymentService {
    /// Cancel all running deployments with a given reason
    /// This is typically called during server shutdown or startup
    pub async fn cancel_running_deployments(
        &self,
        cancelled_reason: &str,
    ) -> Result<u64, DeploymentError> {
        use sea_orm::sea_query::Expr;
        use temps_entities::deployments;

        debug!(
            "Cancelling all running deployments with reason: {}",
            cancelled_reason
        );

        // Update all running deployments to cancelled status in a single query
        let result = deployments::Entity::update_many()
            .filter(deployments::Column::State.eq("running"))
            .col_expr(deployments::Column::State, Expr::value("cancelled"))
            .col_expr(
                deployments::Column::CancelledReason,
                Expr::value(cancelled_reason),
            )
            .col_expr(
                deployments::Column::FinishedAt,
                Expr::current_timestamp().into(),
            )
            .col_expr(
                deployments::Column::UpdatedAt,
                Expr::current_timestamp().into(),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?;

        let count = result.rows_affected;

        if count > 0 {
            info!("Successfully cancelled {} running deployment(s)", count);
        } else {
            debug!("No running deployments found");
        }

        Ok(count)
    }

    /// Cancel all active deployments for an environment before its containers
    /// are removed by `DeploymentContainerCleaner`.
    pub async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, DeploymentError> {
        self.cancel_deployments_for_deletion(None, Some(environment_id), "Environment deleted")
            .await
    }

    pub async fn cancel_all_project_deployments(
        &self,
        project_id: i32,
    ) -> Result<u64, DeploymentError> {
        self.cancel_deployments_for_deletion(Some(project_id), None, "Project deleted")
            .await
    }

    async fn cancel_deployments_for_deletion(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        reason: &str,
    ) -> Result<u64, DeploymentError> {
        use temps_entities::{deployment_jobs, types::JobStatus};

        let mut query = deployments::Entity::find().filter(
            Condition::all()
                .add(deployments::Column::State.ne("cancelled"))
                .add(deployments::Column::State.ne("completed"))
                .add(deployments::Column::State.ne("deployed"))
                .add(deployments::Column::State.ne("failed"))
                .add(deployments::Column::State.ne("paused"))
                .add(deployments::Column::State.ne("stopped")),
        );
        if let Some(project_id) = project_id {
            query = query.filter(deployments::Column::ProjectId.eq(project_id));
        }
        if let Some(environment_id) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(environment_id));
        }

        let active_deployments = query.all(self.db.as_ref()).await?;
        let count = active_deployments.len() as u64;

        for deployment in active_deployments {
            let running_jobs = deployment_jobs::Entity::find()
                .filter(deployment_jobs::Column::DeploymentId.eq(deployment.id))
                .filter(deployment_jobs::Column::Status.eq(JobStatus::Running))
                .all(self.db.as_ref())
                .await?;

            for job in running_jobs {
                let message = format!(
                    "DEPLOYMENT CANCELLED: {reason} - Job '{}' is being terminated",
                    job.name
                );
                if let Err(error) = self
                    .log_service
                    .append_structured_log(
                        &job.log_id,
                        temps_deployment_logs::LogLevel::Error,
                        &message,
                    )
                    .await
                {
                    warn!(
                        deployment_id = deployment.id,
                        job_log_id = %job.log_id,
                        %error,
                        "Failed to append deletion cancellation to deployment job log"
                    );
                }
            }

            let deployment_id = deployment.id;
            let mut active_deployment: deployments::ActiveModel = deployment.into();
            active_deployment.state = Set("cancelled".to_string());
            active_deployment.cancelled_reason = Set(Some(reason.to_string()));
            active_deployment.finished_at = Set(Some(chrono::Utc::now()));
            active_deployment.updated_at = Set(chrono::Utc::now());
            active_deployment
                .update(self.db.as_ref())
                .await
                .map_err(|error| DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to cancel deployment {deployment_id} before owner deletion: {error}"
                    ),
                })?;
        }

        info!(
            ?project_id,
            ?environment_id,
            count,
            "Cancelled deployments before owner deletion"
        );
        Ok(count)
    }

    /// Remove every uploaded archive recorded for a project before deletion.
    /// Runtime containers are handled by `DeploymentContainerCleaner`.
    pub async fn cleanup_project_archives(&self, project_id: i32) -> Result<u64, DeploymentError> {
        let data_dir = self.config_service.data_dir();
        let source_archives = temps_entities::source_bundles::Entity::find()
            .filter(temps_entities::source_bundles::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        let static_archives = temps_entities::static_bundles::Entity::find()
            .filter(temps_entities::static_bundles::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        let archive_paths: Vec<String> = source_archives
            .into_iter()
            .map(|bundle| bundle.archive_path)
            .chain(static_archives.into_iter().map(|bundle| bundle.blob_path))
            .collect();
        let removed = archive_paths.len() as u64;
        for relative_path in archive_paths {
            let archive_path =
                confined_archive_path(&data_dir, &relative_path).map_err(|error| {
                    DeploymentError::InvalidBundlePath {
                        path: relative_path.clone(),
                        reason: format!("stored path for project {project_id} is invalid: {error}"),
                    }
                })?;
            match tokio::fs::remove_file(&archive_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(DeploymentError::Other(format!(
                        "Failed to remove archive '{}' for project {}: {}",
                        archive_path.display(),
                        project_id,
                        error
                    )));
                }
            }
        }

        info!(
            "Removed {} uploaded archive(s) before deleting project {}",
            removed, project_id
        );
        Ok(removed)
    }

    /// Cancel a specific deployment
    pub async fn cancel_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::{deployment_jobs, types::JobStatus};

        info!(
            "Cancelling deployment {} for project {}",
            deployment_id, project_id
        );

        // Verify the deployment exists and belongs to the project
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        info!(
            "Deployment {} current state: '{}' - checking if cancellable",
            deployment_id, deployment.state
        );

        // Only allow cancelling deployments in pending or running state
        if deployment.state != "pending" && deployment.state != "running" {
            info!(
                "Cannot cancel deployment {} - already in '{}' state",
                deployment_id, deployment.state
            );
            return Err(DeploymentError::InvalidInput(format!(
                "Cannot cancel deployment in '{}' state. Only 'pending' or 'running' deployments can be cancelled.",
                deployment.state
            )));
        }

        // Find currently running job and write cancellation message to its logs
        let running_jobs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::Status.eq(JobStatus::Running))
            .all(self.db.as_ref())
            .await?;

        for job in running_jobs {
            info!(
                "📝 Writing cancellation message to running job: {} ({})",
                job.name, job.log_id
            );

            // Write cancellation message to the job's log
            let cancel_msg = format!(
                "DEPLOYMENT CANCELLED BY USER - Job '{}' is being terminated",
                job.name
            );
            if let Err(e) = self
                .log_service
                .append_structured_log(
                    &job.log_id,
                    temps_deployment_logs::LogLevel::Error,
                    &cancel_msg,
                )
                .await
            {
                warn!(
                    "Failed to write cancellation message to job log {}: {}",
                    job.log_id, e
                );
            }
        }

        // Snapshot fields we'll need *after* the move into ActiveModel for
        // the queue event below — the active model takes ownership of the row.
        let environment_id = deployment.environment_id;

        // Update deployment to cancelled state
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("cancelled".to_string());
        active_deployment.cancelled_reason = Set(Some("Cancelled by user".to_string()));
        active_deployment.finished_at = Set(Some(chrono::Utc::now()));
        active_deployment.updated_at = Set(chrono::Utc::now());
        active_deployment.update(self.db.as_ref()).await?;

        // Publish a DeploymentCancelled event so downstream listeners (PR
        // commenter, notifications, audit consumers) can react. The workflow
        // executor publishes the same event when it transitions to Cancelled
        // mid-pipeline; this site covers user-initiated cancels from the UI /
        // API, which previously left the PR comment stuck on "Deploying preview".
        //
        // Best-effort: a queue failure here must NOT undo the cancellation —
        // log and move on, mirroring how DeploymentFailed/Succeeded handle it
        // elsewhere in this file.
        let environment_name =
            match temps_entities::environments::Entity::find_by_id(environment_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(env)) => env.name,
                _ => String::new(),
            };
        let event = temps_core::Job::DeploymentCancelled(temps_core::DeploymentCancelledJob {
            deployment_id,
            project_id,
            environment_id,
            environment_name,
        });
        if let Err(e) = self.queue_service.send(event).await {
            warn!(
                "Failed to send DeploymentCancelled event for deployment {}: {}",
                deployment_id, e
            );
        }

        // Anonymous telemetry: this is the explicit "user clicked Cancel"
        // path (the other emission site, in WorkflowExecutionService, only
        // fires as a fallback when the executor detects a "cancelled" error
        // without this method having already set the state — see that
        // file's comment). Deliberately NOT emitted from the supersede
        // (cancel_in_flight_deployments) or bulk-shutdown
        // (cancel_running_deployments) paths, since those fire automatically
        // on every push / restart and would swamp the funnel signal with
        // non-user-initiated noise.
        let template_provenance = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|project| project.template_slug);
        self.telemetry().report(
            temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::DeployCancelled,
            )
            .with("trigger", "user")
            .with_template_provenance(template_provenance.as_deref()),
        );

        info!(
            "Successfully cancelled deployment {} for project {} - workflow will stop at next checkpoint",
            deployment_id, project_id
        );

        Ok(())
    }
}

// Implement DeploymentCanceller trait from temps-core
#[async_trait::async_trait]
impl temps_core::DeploymentCanceller for DeploymentService {
    async fn cancel_all_project_deployments(
        &self,
        project_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.cancel_all_project_deployments(project_id)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }

    async fn cancel_all_environment_deployments(
        &self,
        environment_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.cancel_all_environment_deployments(environment_id)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[async_trait::async_trait]
impl temps_core::ProjectArchiveCleaner for DeploymentService {
    async fn cleanup_project_archives(
        &self,
        project_id: i32,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.cleanup_project_archives(project_id)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_cleanup_paths_are_lexically_confined() {
        let root = std::path::Path::new("/var/lib/temps");
        assert_eq!(
            confined_archive_path(root, "source-bundles/archive.zip").unwrap(),
            root.join("source-bundles/archive.zip")
        );
        for path in [
            "../archive.zip",
            "source-bundles/../../archive.zip",
            "/tmp/archive.zip",
        ] {
            assert!(matches!(
                confined_archive_path(root, path),
                Err(DeploymentError::InvalidBundlePath { .. })
            ));
        }
    }
}
