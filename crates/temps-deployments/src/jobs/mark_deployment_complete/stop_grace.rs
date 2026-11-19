// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The stop grace period a superseded deployment's containers get, the
//! teardown budget that has to cover it, and the graceful stop and removal
//! of a legacy container.

use sea_orm::EntityTrait;

/// The grace period a superseded deployment's containers get between SIGTERM
/// and SIGKILL: the environment's stop timeout, else the project's, else the
/// platform default (10 s). A failed lookup falls back to the default, with a
/// warning: the teardown must still happen.
pub(crate) async fn superseded_stop_grace(
    db: &sea_orm::DatabaseConnection,
    environment_id: i32,
) -> std::time::Duration {
    let configured = async {
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .one(db)
            .await?;
        let Some(environment) = environment else {
            return Ok::<_, sea_orm::DbErr>(None);
        };
        if let Some(seconds) = environment
            .deployment_config
            .as_ref()
            .and_then(|config| config.stop_timeout_seconds)
        {
            return Ok(Some(seconds));
        }
        let project = temps_entities::projects::Entity::find_by_id(environment.project_id)
            .one(db)
            .await?;
        Ok(project
            .and_then(|project| project.deployment_config)
            .and_then(|config| config.stop_timeout_seconds))
    }
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(
            environment_id,
            %error,
            "Could not read the stop timeout; stopping superseded containers with the default grace period"
        );
        None
    });
    std::time::Duration::from_secs(
        temps_config::deployment_config::resolve_stop_timeout_seconds(configured),
    )
}

/// How long one container's stop-and-remove may take before the sweep gives
/// up on it: `base`, which already covers the default 10 s grace, plus any
/// grace beyond that. A long stop timeout must not be cut short by the
/// teardown's own ceiling.
pub(crate) fn teardown_budget(
    base: std::time::Duration,
    grace: std::time::Duration,
) -> std::time::Duration {
    let default_grace = std::time::Duration::from_secs(
        temps_config::deployment_config::resolve_stop_timeout_seconds(None),
    );
    base.saturating_add(grace.saturating_sub(default_grace))
}

impl super::MarkDeploymentCompleteJob {
    /// Stop (best effort) and remove a legacy container through its node's
    /// deployer. An already-removed container counts as removed.
    pub(super) async fn stop_and_remove_container(
        &self,
        deployer: &dyn temps_deployer::ContainerDeployer,
        container_id: &str,
        stop_grace: std::time::Duration,
    ) -> Result<(), String> {
        // Stop container first, giving in-flight requests the stop timeout.
        match deployer
            .stop_container_with_grace(container_id, stop_grace)
            .await
        {
            Ok(_) => {
                self.log(format!("Stopped container {}", container_id))
                    .await
                    .ok();
            }
            Err(e) => {
                self.log(format!("Failed to stop container {}: {}", container_id, e))
                    .await
                    .ok();
            }
        }

        // Remove container from Docker. A container that is already gone
        // (pruned, removed by hand, lost with its node's disk) is torn down:
        // retrying could never succeed and would keep the deployment in the
        // sweep forever.
        match deployer.remove_container(container_id).await {
            Ok(_) => {
                self.log(format!("Removed container {}", container_id))
                    .await
                    .ok();
            }
            Err(temps_deployer::DeployerError::ContainerNotFound(_)) => {
                self.log(format!(
                    "Container {} was already removed from Docker",
                    container_id
                ))
                .await
                .ok();
            }
            Err(e) => {
                self.log(format!(
                    "Failed to remove container {}: {}",
                    container_id, e
                ))
                .await
                .ok();
                return Err(format!("Failed to remove container {container_id}: {e}"));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teardown_budget_grows_with_a_grace_period_longer_than_the_default() {
        let base = std::time::Duration::from_secs(super::super::CONTAINER_TEARDOWN_TIMEOUT_SECS);
        assert_eq!(
            teardown_budget(base, std::time::Duration::from_secs(10)),
            base,
            "the default grace keeps today's ceiling"
        );
        assert_eq!(
            teardown_budget(base, std::time::Duration::from_secs(1)),
            base
        );
        assert_eq!(
            teardown_budget(base, std::time::Duration::from_secs(600)),
            base + std::time::Duration::from_secs(590)
        );
    }
}
