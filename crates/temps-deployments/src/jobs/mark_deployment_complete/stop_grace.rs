// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The stop grace period a superseded deployment's containers get, and the
//! teardown budget that has to cover it.

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
