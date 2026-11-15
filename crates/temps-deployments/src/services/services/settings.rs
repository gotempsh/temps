// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Environment deployment settings and the static asset cache purge.

use super::*;

impl DeploymentService {
    /// Purge all cached static assets for a project or a specific environment.
    /// Deletes static_asset_cache DB rows. Orphaned CAS blobs are cleaned up
    /// by the nightly garbage collector.
    /// Returns the number of cache entries deleted.
    pub async fn purge_asset_cache(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<u64, DeploymentError> {
        use sea_orm::ConnectionTrait;

        let mut sql = format!(
            "DELETE FROM static_asset_cache WHERE project_id = {}",
            project_id
        );
        if let Some(env_id) = environment_id {
            sql.push_str(&format!(" AND environment_id = {}", env_id));
        }

        let result = self
            .db
            .as_ref()
            .execute(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
            .await?;

        let deleted = result.rows_affected();
        info!(
            "Purged {} asset cache entries for project {} (env: {:?})",
            deleted, project_id, environment_id
        );

        Ok(deleted)
    }

    pub async fn update_deployment_settings(
        &self,
        project_id: i32,
        environment_id: i32,
        settings: UpdateDeploymentSettingsRequest,
    ) -> Result<(), DeploymentError> {
        // Find the current deployment for the environment
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Environment not found".to_string()))?;

        // Update the environment with new settings
        let mut active_environment: environments::ActiveModel = environment.clone().into();

        // Update deployment config with new resource settings
        let mut deployment_config = environment.deployment_config.clone().unwrap_or_default();
        deployment_config.cpu_request = settings.cpu_request;
        deployment_config.cpu_limit = settings.cpu_limit;
        deployment_config.memory_request = settings.memory_request;
        deployment_config.memory_limit = settings.memory_limit;

        active_environment.deployment_config = Set(Some(deployment_config));
        active_environment.update(self.db.as_ref()).await?;

        Ok(())
    }
}
