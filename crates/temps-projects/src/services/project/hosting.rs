// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Source type, alternate sources and hosting toggles.

use super::*;

impl ProjectService {
    /// Claim the reserved environment names when a telemetry project gains hosting.
    /// Telemetry and environment IDs remain unchanged.
    pub(super) async fn activate_hosting_domains(
        &self,
        txn: &DatabaseTransaction,
        project_id: i32,
    ) -> Result<(), ProjectError> {
        let environments = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::Id)
            .all(txn)
            .await?;
        for environment in environments {
            self.environment_service
                .reconcile_managed_domain(
                    txn,
                    &environment,
                    temps_entities::source_type::SourceType::DockerImage,
                    None,
                )
                .await.map_err(|error| ProjectError::DatabaseError {
                reason: format!("Could not enable hosting for project {project_id}, environment {}: domain '{}' could not be registered: {error}", environment.id, environment.subdomain),
            })?;
        }
        Ok(())
    }

    pub(super) async fn notify_hosting_enabled(&self, project_id: i32) {
        match self.environment_service.get_environments(project_id).await {
            Ok(environments) => {
                for environment in environments {
                    let job = Job::EnvironmentCreated(temps_core::EnvironmentCreatedJob {
                        project_id,
                        environment_id: environment.id,
                        environment_name: environment.name,
                        subdomain: environment.subdomain,
                    });
                    if let Err(error) = self.queue_service.send(job).await {
                        tracing::warn!(project_id, %error, "Could not initialize hosting monitor");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(project_id, %error, "Could not load environments to initialize hosting monitors")
            }
        }
    }

    /// Authorize source changes using the slug protected by this transaction.
    /// Each independently committed settings write must take this lock itself.
    pub(super) async fn lock_project_for_source_write(
        &self,
        txn: &DatabaseTransaction,
        project_id: i32,
        field: &str,
        caller: DeployCaller,
    ) -> Result<projects::Model, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("Project {project_id} not found")))?;
        self.guard_granted_project_write(&project.slug, field, caller)?;
        Ok(project)
    }

    /// Change a project's source type to a Git-less type (docker_image /
    /// static_files / manual). Switching TO Git is rejected here because that
    /// needs repo + provider-connection config — it goes through
    /// [`Self::update_git_settings`] instead (which sets `source_type = Git`).
    pub async fn set_source_type(
        &self,
        project_id: i32,
        source_type: temps_entities::source_type::SourceType,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        use temps_entities::source_type::SourceType;
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                project_id
            )))?;
        if project.project_type == ProjectType::Service {
            return Err(ProjectError::InvalidInput(
                "A service project's source type is fixed by its applied template".to_string(),
            ));
        }
        // ADR 045, defense in depth: `source_type` decides which deploy
        // pipeline runs, and every image/static/drop deploy path already
        // gates on `guard_deploy` and `DeployImageJobBuilder::build`
        // independently of this flag -- so this is not currently a way
        // around those gates. Gated anyway, unconditionally, since this
        // method has exactly one caller-facing effect and the alternative is
        // trusting that stays true as the deploy pipelines evolve.
        self.guard_granted_project_write(&project.slug, "the source type", caller)?;

        // Switching to Git is a direct flip only when a repository is already
        // configured (repo owner + name). A project can carry git info without
        // being Git-typed — that's a one-click switch. Otherwise the user must
        // set a repository up first (via Git settings).
        if matches!(source_type, SourceType::Git) {
            let has_repo = !project.repo_owner.trim().is_empty()
                && !project.repo_name.trim().is_empty()
                && project.repo_owner != "unknown"
                && project.repo_name != "unknown";
            if !has_repo {
                return Err(ProjectError::InvalidInput(
                    "To switch to a Git source, configure a repository in Git settings \
                     (a provider connection, repository, and branch are required)."
                        .to_string(),
                ));
            }
        }

        if source_type == SourceType::External {
            return Err(ProjectError::InvalidInput(format!(
                "Project {project_id} cannot switch to monitoring-only through a source change. Create a monitoring project instead; disabling hosting here could suppress deployment monitoring."
            )));
        }
        let mut active_project: projects::ActiveModel = project.into();
        active_project.source_type = Set(source_type);
        active_project.updated_at = Set(chrono::Utc::now());
        let txn = self.db.begin().await?;
        let locked_project = projects::Entity::find_by_id(project_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("Project {project_id} not found")))?;
        self.guard_granted_project_write(&locked_project.slug, "the source type", caller)?;
        let enable_hosting = locked_project.source_type == SourceType::External;
        let updated = active_project.update(&txn).await?;
        if enable_hosting {
            self.activate_hosting_domains(&txn, project_id).await?;
        }
        txn.commit().await?;
        if enable_hosting {
            self.notify_hosting_enabled(project_id).await;
        }

        // Deploy routing / behavior keys off source_type — notify consumers
        // before anything else awaits, so a cancelled request can only cost
        // the response, never the notification.
        if let Err(e) = self
            .queue_service
            .send(Job::ProjectUpdated(ProjectUpdatedJob {
                project_id: updated.id,
                project_name: updated.name.clone(),
            }))
            .await
        {
            warn!(
                "Failed to emit ProjectUpdated after source-type change for {}: {}",
                updated.id, e
            );
        }
        Ok(self.map_written_project(updated).await)
    }

    /// Toggle whether the project accepts deployments from a source other than
    /// its configured `source_type`.
    ///
    /// This deliberately does NOT touch `source_type`: the project keeps its
    /// primary source (and, for a Git project, its repository, webhook-driven
    /// auto-deploy and rollback rebuild-from-source), and simply gains the
    /// ability to also take an uploaded source archive via `drop`.
    pub async fn set_allow_alternate_sources(
        &self,
        project_id: i32,
        allow: bool,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("project {} not found", project_id)))?;

        // ADR 045, defense in depth: accepting an alternate source (a
        // dropped archive) is not by itself a deploy -- `SourceDropService`
        // plans with the fail-closed `DeployCaller::default()` regardless of
        // this flag -- but it does widen what can trigger one, so it is
        // gated the same way as `set_source_type`.
        self.guard_granted_project_write(
            &project.slug,
            "whether alternate sources are accepted",
            caller,
        )?;

        let mut active_project: projects::ActiveModel = project.into();
        active_project.allow_alternate_sources = Set(Some(allow));
        active_project.updated_at = Set(chrono::Utc::now());
        let updated = active_project.update(self.db.as_ref()).await?;
        Ok(self.map_written_project(updated).await)
    }

    /// Mark whether Temps runs the project's app (`hosted`) or only observes
    /// an app running elsewhere (`external`). An external project is never
    /// deployed; deployments already running keep running until stopped.
    pub async fn set_hosting(
        &self,
        project_id: i32,
        hosting: temps_entities::types::Hosting,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("project {} not found", project_id)))?;
        if project.hosting == hosting {
            return Ok(self.map_written_project(project).await);
        }
        // Switching back to hosted re-enables deploys, so it is gated like
        // the other settings that widen what can deploy the project.
        if hosting == temps_entities::types::Hosting::Hosted {
            self.guard_granted_project_write(&project.slug, "whether Temps hosts it", caller)?;
        }
        let mut active_project: projects::ActiveModel = project.into();
        active_project.hosting = Set(hosting);
        active_project.updated_at = Set(chrono::Utc::now());
        let updated = active_project.update(self.db.as_ref()).await?;
        Ok(self.map_written_project(updated).await)
    }
}
