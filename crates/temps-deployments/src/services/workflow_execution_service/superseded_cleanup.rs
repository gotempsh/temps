// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Removing containers of superseded deployments that a deploy could not
//! remove, without waiting for the environment's next deploy.
//!
//! A deploy removes the containers of the deployments it supersedes
//! ([`WorkflowExecutionService::teardown_previous_deployment`]). A container
//! on a worker that was unreachable then (it is the reason the environment
//! failed over) stays recorded, and was only retried by the environment's
//! next deploy. When the worker came back, Docker's restart policy started
//! the container again and it ran unrouted, possibly for good.
//!
//! The node health loop calls [`WorkflowExecutionService::remove_superseded_containers`]
//! every tick. It finds the environments that still record such a container
//! on a node that is reachable again (or on the control plane) and runs the
//! same teardown a deploy would, so every guard of that path applies: the
//! environment's lifecycle lock, the re-check that each deployment is still
//! one to sweep (and not one a rollback made serving again), the claim on each container row, the project's stop grace
//! and Docker's ownership labels. The query reads only container rows a
//! teardown has not finished with, so it is O(leaked containers), and each
//! pass handles at most [`MAX_ENVIRONMENTS_PER_PASS`] environments,
//! round-robin, so one worker whose agent keeps failing cannot starve the
//! others.

use super::*;
use sea_orm::{ConnectionTrait, DatabaseBackend, FromQueryResult, Statement};

/// Environments handled per pass.
pub const MAX_ENVIRONMENTS_PER_PASS: u64 = 10;

/// How long an environment's serving deployment must have been settled
/// before this pass touches the environment, so the teardown that deploy
/// runs itself is never raced.
pub const SUPERSEDED_CLEANUP_SETTLE_SECS: i64 = 300;

/// What one pass did, and where the next one starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SupersededCleanupPass {
    /// Environments whose superseded containers were attempted.
    pub environments: usize,
    /// Containers confirmed removed.
    pub removed: usize,
    /// Pass this as `after_environment_id` next time (0 starts over).
    pub next_cursor: i32,
}

#[derive(Debug, FromQueryResult)]
struct EnvironmentToClean {
    project_id: i32,
    environment_id: i32,
    current_deployment_id: i32,
}

/// Environments, by id after `$1`, with a container of a deployment older
/// than the one they serve that a teardown has not finished with, on the
/// control plane or a node that is not offline:
/// - a live row of a deployment a teardown sweeps (it was never removed), or
/// - a row a drain or failover took out of routing without confirming the
///   container was gone (`retired`, `failed_over`, `failed_over_recovering`).
const ENVIRONMENTS_TO_CLEAN_SQL: &str = r#"
SELECT DISTINCT e.project_id, e.id AS environment_id, e.current_deployment_id
FROM deployment_containers dc
JOIN deployments d ON d.id = dc.deployment_id
JOIN environments e ON e.id = d.environment_id
JOIN deployments cur ON cur.id = e.current_deployment_id
LEFT JOIN nodes n ON n.id = dc.node_id
WHERE e.id > $1
  AND e.deleted_at IS NULL
  AND (dc.node_id IS NULL OR n.status <> 'offline')
  AND (
        (dc.deleted_at IS NULL AND d.state = ANY($2))
     OR (dc.deleted_at IS NOT NULL AND dc.node_id IS NOT NULL AND dc.status = ANY($3))
  )
  AND d.id <> cur.id
  AND (d.created_at < cur.created_at OR (d.created_at = cur.created_at AND d.id < cur.id))
  AND cur.updated_at < $4
ORDER BY e.id
LIMIT $5
"#;

/// Deployment states whose live containers a teardown removes: the ones it
/// sweeps, and the finished ones whose containers it retries.
fn swept_states() -> Vec<String> {
    crate::jobs::mark_deployment_complete::TEARDOWN_SWEPT_STATES
        .iter()
        .chain(["failed", "stopped", "cancelled"].iter())
        .map(|state| state.to_string())
        .collect()
}

fn out_of_routing_statuses() -> Vec<String> {
    use crate::services::node_service::{
        CONTAINER_STATUS_FAILED_OVER, CONTAINER_STATUS_FAILED_OVER_RECOVERING,
        CONTAINER_STATUS_RETIRED,
    };
    [
        CONTAINER_STATUS_RETIRED,
        CONTAINER_STATUS_FAILED_OVER,
        CONTAINER_STATUS_FAILED_OVER_RECOVERING,
    ]
    .iter()
    .map(|status| status.to_string())
    .collect()
}

impl WorkflowExecutionService {
    /// One pass (see the [module docs](self)). Failures are logged, never
    /// returned: the next pass retries them.
    pub async fn remove_superseded_containers(
        &self,
        after_environment_id: i32,
    ) -> SupersededCleanupPass {
        let settled_before =
            chrono::Utc::now() - chrono::Duration::seconds(SUPERSEDED_CLEANUP_SETTLE_SECS);
        let environments = match self
            .environments_to_clean(after_environment_id, settled_before)
            .await
        {
            Ok(environments) => environments,
            Err(error) => {
                warn!(
                    after_environment_id,
                    "Could not look up environments with containers of superseded deployments \
                     left on their nodes; retrying next pass: {error}"
                );
                return SupersededCleanupPass {
                    next_cursor: after_environment_id,
                    ..Default::default()
                };
            }
        };

        let mut pass = SupersededCleanupPass {
            next_cursor: if environments.len() as u64 == MAX_ENVIRONMENTS_PER_PASS {
                environments.last().map_or(0, |e| e.environment_id)
            } else {
                0
            },
            ..Default::default()
        };
        for target in environments {
            let current = match deployments::Entity::find_by_id(target.current_deployment_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(current)) => current,
                Ok(None) => continue,
                Err(error) => {
                    warn!(
                        project_id = target.project_id,
                        environment_id = target.environment_id,
                        deployment_id = target.current_deployment_id,
                        "Could not read the serving deployment before removing superseded \
                         containers; retrying next pass: {error}"
                    );
                    continue;
                }
            };
            pass.environments += 1;
            info!(
                project_id = target.project_id,
                environment_id = target.environment_id,
                deployment_id = current.id,
                "Removing containers of superseded deployments that are still recorded on a \
                 reachable node"
            );
            match self
                .teardown_superseded(target.project_id, target.environment_id, &current, true)
                .await
            {
                Ok(Some(_)) => pass.removed += 1,
                Ok(None) => {}
                Err(error) => warn!(
                    project_id = target.project_id,
                    environment_id = target.environment_id,
                    deployment_id = current.id,
                    "Removing containers of superseded deployments failed; retrying next \
                     pass: {error}"
                ),
            }
        }
        pass
    }

    async fn environments_to_clean(
        &self,
        after_environment_id: i32,
        settled_before: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<EnvironmentToClean>, sea_orm::DbErr> {
        let statement = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            ENVIRONMENTS_TO_CLEAN_SQL,
            [
                after_environment_id.into(),
                swept_states().into(),
                out_of_routing_statuses().into(),
                settled_before.into(),
                (MAX_ENVIRONMENTS_PER_PASS as i64).into(),
            ],
        );
        let rows = self.db.query_all(statement).await?;
        rows.iter()
            .map(|row| EnvironmentToClean::from_query_result(row, ""))
            .collect()
    }
}
