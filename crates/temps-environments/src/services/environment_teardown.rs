// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tearing an environment down: the soft-delete fence, then its deployments
//! and containers.

use std::sync::Arc;

use temps_core::{ContainerCleanupError, DeploymentCanceller, DeploymentContainerCleaner};
use tracing::info;

use super::environment_service::{EnvironmentError, EnvironmentService};

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentTeardownError {
    #[error(transparent)]
    Environment(#[from] EnvironmentError),

    #[error("Failed to cancel active deployments for environment {environment_id}: {reason}")]
    Cancel { environment_id: i32, reason: String },

    #[error(transparent)]
    Cleanup(#[from] ContainerCleanupError),
}

pub struct EnvironmentTeardownService {
    environments: Arc<EnvironmentService>,
    deployments: Arc<dyn DeploymentCanceller>,
    containers: Arc<dyn DeploymentContainerCleaner>,
}

impl EnvironmentTeardownService {
    pub fn new(
        environments: Arc<EnvironmentService>,
        deployments: Arc<dyn DeploymentCanceller>,
        containers: Arc<dyn DeploymentContainerCleaner>,
    ) -> Self {
        Self {
            environments,
            deployments,
            containers,
        }
    }

    /// Soft-delete the environment, then cancel its deployments and remove
    /// its containers. Idempotent: after a failure past the soft delete, a
    /// retry skips the delete and finishes the cleanup.
    pub async fn tear_down(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), EnvironmentTeardownError> {
        // The fence goes first: deployment workers refuse a soft-deleted
        // environment, so nothing new starts while the old work is stopped.
        self.environments
            .delete_environment(project_id, environment_id)
            .await?;

        let cancelled = self
            .deployments
            .cancel_all_environment_deployments(environment_id)
            .await
            .map_err(|error| EnvironmentTeardownError::Cancel {
                environment_id,
                reason: error.to_string(),
            })?;
        if cancelled > 0 {
            info!(
                project_id,
                environment_id, cancelled, "Cancelled active deployments of a deleted environment"
            );
        }

        self.containers
            .cleanup_environment_containers(project_id, environment_id)
            .await?;
        Ok(())
    }
}
