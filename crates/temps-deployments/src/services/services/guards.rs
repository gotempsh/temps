// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deploy guards: the Docker-socket grant check and the external-hosting
//! refusal every deploy path runs before it creates a deployment.

use super::*;

/// Whether a project's hosting allows Temps to deploy it.
fn hosting_deploy_check(project: &projects::Model) -> Result<(), DeploymentError> {
    match project.hosting.deploy_refusal(&project.name) {
        Some(reason) => Err(DeploymentError::ProjectHostedExternally {
            project_id: project.id,
            project_name: project.name.clone(),
            reason,
        }),
        None => Ok(()),
    }
}

impl DeploymentService {
    /// Refuse a deployment of a project this control plane declares as
    /// requiring the host Docker socket (ADR 045), unless the caller is an
    /// instance admin (or Temps itself).
    ///
    /// Every user-initiated path that can start a container for a project goes
    /// through one of the methods that calls this. The check is here rather
    /// than in the handlers because a granted project's container runs the
    /// caller's image and command as host root, and a new deploy route added
    /// later must not be able to miss it by forgetting a macro.
    pub(super) async fn guard_granted_project_deploy(
        &self,
        project_id: i32,
        caller: temps_core::docker_socket_grant::DeployCaller,
    ) -> Result<(), DeploymentError> {
        // Cheap and skipped entirely on every install that never set the
        // variable: with nothing declared, no slug can require the check, so
        // the project row is never even read.
        if temps_core::docker_socket_grant::process_grant().is_empty()
            || caller.may_deploy_granted_project()
        {
            return Ok(());
        }
        let slug = projects::Entity::find_by_id(project_id)
            .select_only()
            .column(projects::Column::Slug)
            .into_tuple::<String>()
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound(format!("project {project_id} not found")))?;
        if temps_core::docker_socket_grant::deploy_requires_instance_admin(
            temps_core::docker_socket_grant::process_grant(),
            &slug,
            caller,
        ) {
            tracing::warn!(
                project_id,
                slug = %slug,
                env = temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV,
                "Refused a non-admin deployment of a project that holds host Docker access \
                 (ADR 045)"
            );
            return Err(DeploymentError::DockerSocketDeployRequiresAdmin { slug });
        }
        Ok(())
    }

    /// Refuse to create a deployment for a project whose app runs outside
    /// Temps (`hosting: external`).
    ///
    /// A project that no longer exists is let through: the operation that
    /// follows reports the missing project with its own context.
    pub async fn ensure_project_hosted_locally(
        &self,
        project_id: i32,
    ) -> Result<(), DeploymentError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|error| {
                tracing::error!(%error, project_id, "Could not load project before deploying");
                DeploymentError::DatabaseError {
                    reason: format!(
                        "Could not load project {project_id} to check where it is hosted: {error}"
                    ),
                }
            })?;
        match project {
            Some(project) => hosting_deploy_check(&project),
            None => Ok(()),
        }
    }
}
