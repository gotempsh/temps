// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tearing an environment down: the soft-delete fence, then its deployments
//! and containers. Shared by the delete endpoint and by preview cleanup when
//! a branch is deleted or its pull request closes.

use std::sync::Arc;
use std::time::Duration;

use temps_core::retry::RetryConfig;
use temps_core::{ContainerCleanupError, DeploymentCanceller, DeploymentContainerCleaner};
use temps_entities::environments;
use tracing::{info, warn};

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

impl EnvironmentTeardownError {
    /// Whether waiting can fix it: a database or worker that is briefly
    /// unreachable, never a refused or missing environment.
    fn transient(&self) -> bool {
        match self {
            Self::Environment(error) => matches!(
                error,
                EnvironmentError::DatabaseConnectionError(_)
                    | EnvironmentError::DatabaseError { .. }
            ),
            Self::Cancel { .. } => true,
            Self::Cleanup(error) => matches!(error, ContainerCleanupError::NodeUnreachable { .. }),
        }
    }
}

pub struct EnvironmentTeardownService {
    environments: Arc<EnvironmentService>,
    deployments: Arc<dyn DeploymentCanceller>,
    containers: Arc<dyn DeploymentContainerCleaner>,
    retry: RetryConfig,
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
            retry: RetryConfig::new(5)
                .with_base_delay(Duration::from_secs(5))
                .with_max_delay(Duration::from_secs(60)),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
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
        self.release(project_id, environment_id).await
    }

    /// Cancel a soft-deleted environment's deployments and remove its
    /// containers.
    async fn release(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), EnvironmentTeardownError> {
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

    /// Tear down every live preview environment of `branch` in the project,
    /// retrying while a database or worker is unreachable. Each preview is
    /// attempted even when another fails. Only a failed lookup is an error.
    pub async fn tear_down_branch_previews(
        &self,
        project_id: i32,
        branch: &str,
    ) -> Result<BranchTeardown, EnvironmentTeardownError> {
        let previews = self
            .retry
            .retry_if(
                || async {
                    self.environments
                        .branch_previews(project_id, branch)
                        .await
                        .map_err(EnvironmentTeardownError::from)
                },
                EnvironmentTeardownError::transient,
            )
            .await?;

        let mut outcome = BranchTeardown::default();
        for preview in previews {
            match self.retire_preview(project_id, preview.id).await {
                Ok(true) => outcome.retired.push(preview),
                Ok(false) => info!(
                    project_id,
                    environment_id = preview.id,
                    branch,
                    "Preview environment was restored during its teardown; leaving it running"
                ),
                Err(error) => {
                    warn!(
                        project_id,
                        environment_id = preview.id,
                        branch,
                        %error,
                        "Failed to tear down preview environment"
                    );
                    outcome.failed.push((preview, error));
                }
            }
        }

        Ok(outcome)
    }

    /// Fence one preview, then release it. Returns false when the preview
    /// was restored (an operator re-created it) while a retry waited: its
    /// new deployments are not this teardown's to stop.
    async fn retire_preview(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<bool, EnvironmentTeardownError> {
        self.retry
            .retry_if(
                || async {
                    self.environments
                        .delete_environment(project_id, environment_id)
                        .await
                        .map_err(EnvironmentTeardownError::from)
                },
                EnvironmentTeardownError::transient,
            )
            .await?;

        self.retry
            .retry_if(
                || async {
                    let environment = self
                        .environments
                        .get_environment_for_deletion(project_id, environment_id)
                        .await?;
                    if environment.deleted_at.is_none() {
                        return Ok(false);
                    }
                    self.release(project_id, environment_id).await?;
                    Ok(true)
                },
                EnvironmentTeardownError::transient,
            )
            .await
    }
}

/// What [`EnvironmentTeardownService::tear_down_branch_previews`] did to each
/// preview of a branch.
#[derive(Debug, Default)]
pub struct BranchTeardown {
    /// Soft-deleted, with deployments cancelled and containers removed.
    pub retired: Vec<environments::Model>,
    /// Not fully torn down. One that failed after its soft delete keeps its
    /// containers until its deletion is retried through the environment
    /// delete endpoint.
    pub failed: Vec<(environments::Model, EnvironmentTeardownError)>,
}
