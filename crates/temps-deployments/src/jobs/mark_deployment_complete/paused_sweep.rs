// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The part of a deploy's cleanup that concerns older paused deployments: they
//! stay resumable, so their containers are stopped rather than torn down.

use super::*;
use futures::StreamExt;

/// Safety bound on how many containers of paused deployments a single deploy
/// asks to stop. Paused deployments stay paused, and so stay in this query, for
/// as long as they stay resumable; newest first keeps a long history of them
/// from slowing every deploy, and a stop of a stopped container is cheap.
const MAX_PAUSED_CONTAINERS_STOPPED_PER_PASS: u64 = 100;

/// How many replicas a pause or a sweep stops at once. One unreachable worker
/// costs its timeout while the others proceed; the bound keeps a deployment
/// with many replicas from opening a connection per replica at once on a small
/// host.
pub(crate) const MAX_CONCURRENT_REPLICA_STOPS: usize = 8;

/// See [`environment_lifecycle_lock`]. Separate from `ENVIRONMENT_LOCKS`, which
/// a deploy holds for its whole completion: a pause or resume in another
/// deployment of the environment should not wait for that.
static ENVIRONMENT_LIFECYCLE_LOCKS: std::sync::LazyLock<EnvironmentLocks> =
    std::sync::LazyLock::new(EnvironmentLocks::default);

/// Held by whatever stops, starts or removes the replicas of a live or paused
/// deployment in an environment: a pause, a resume, a deploy's sweeps. Each reads
/// the deployment's state after taking it, so the state it acts on is still true
/// when its operations land. Without it, a sweep that read "paused" could stop a
/// container a resume had just started, leaving a "deployed" deployment with
/// nothing running, and a teardown that read "deployed" could remove the
/// containers of a deployment just paused, leaving a resume nothing to start.
///
/// Nothing holding it waits for `ENVIRONMENT_LOCKS`, so a deploy holding that
/// one and waiting for this one cannot deadlock. Holders only wait on replica
/// operations, each bounded by a timeout, and hold no database connection
/// meanwhile. The teardown sweeps take it per deployment, so a pause waits for
/// at most one deployment's containers, not a whole sweep, and a worker that
/// stops answering costs each batch of them one timeout.
/// Process-level, like `ENVIRONMENT_LOCKS`.
pub(crate) fn environment_lifecycle_lock(environment_id: i32) -> Arc<tokio::sync::Mutex<()>> {
    ENVIRONMENT_LIFECYCLE_LOCKS.get(environment_id)
}

/// The deployment as it is now, if a teardown sweep may still remove its
/// containers. A sweep selects deployments before taking
/// [`environment_lifecycle_lock`], so it reads this under the lock: a deployment
/// paused in between must keep its containers for a resume.
pub(crate) async fn deployment_still_swept(
    db: &sea_orm::DatabaseConnection,
    deployment_id: i32,
) -> Result<Option<deployments::Model>, sea_orm::DbErr> {
    deployments::Entity::find_by_id(deployment_id)
        .filter(deployments::Column::State.is_in(TEARDOWN_SWEPT_STATES))
        .one(db)
        .await
}

/// Why a container of a paused deployment was not stopped. Each of these leaves
/// the row as it was, and the next deployment asks again.
#[derive(Debug, thiserror::Error)]
enum PausedSweepError {
    #[error(
        "Failed to connect to worker node {node_id} to stop container {container_id} of paused \
         deployment {deployment_id}: {reason}"
    )]
    WorkerUnreachable {
        container_id: String,
        deployment_id: i32,
        node_id: i32,
        reason: String,
    },

    #[error(
        "Failed to stop container {container_id} of paused deployment {deployment_id}: {source}"
    )]
    Stop {
        container_id: String,
        deployment_id: i32,
        #[source]
        source: temps_deployer::DeployerError,
    },

    #[error(
        "Timed out stopping container {container_id} of paused deployment {deployment_id} after \
         {timeout_seconds}s"
    )]
    TimedOut {
        container_id: String,
        deployment_id: i32,
        timeout_seconds: u64,
    },

    #[error(
        "Failed to mark container {container_id} of paused deployment {deployment_id} stopped: \
         {source}"
    )]
    MarkStopped {
        container_id: String,
        deployment_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(
        "Failed to retire microVM replica {container_id} of paused deployment {deployment_id}: \
         {reason}"
    )]
    RetireMicroVm {
        container_id: String,
        deployment_id: i32,
        reason: String,
    },
}

impl MarkDeploymentCompleteJob {
    /// Make sure nothing of an older paused deployment is still running next
    /// to this one. Paused deployments are outside the teardown sweep because
    /// they must stay resumable, so their containers are stopped, not removed,
    /// and the deployment stays paused.
    ///
    /// A row's status is deliberately not trusted here: a pause used to record
    /// "stopped" for a worker's container it had never reached, so those rows
    /// are exactly the ones that need this. A failed stop leaves the row as it
    /// was and the next deployment asks again.
    pub(super) async fn stop_paused_deployments(
        &self,
        environment_id: i32,
        current_created_at: chrono::DateTime<chrono::Utc>,
    ) {
        let lifecycle_lock = environment_lifecycle_lock(environment_id);
        let _lifecycle_guard = lifecycle_lock.lock().await;

        let containers = match deployment_containers::Entity::find()
            .find_also_related(deployments::Entity)
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::State.eq("paused"))
            .filter(self.created_before_this_deployment(current_created_at))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .order_by_desc(deployment_containers::Column::Id)
            .limit(MAX_PAUSED_CONTAINERS_STOPPED_PER_PASS)
            .all(self.db.as_ref())
            .await
        {
            Ok(containers) => containers,
            Err(e) => {
                self.log(format!(
                    "Failed to fetch containers of paused deployments in environment {}: {}",
                    environment_id, e
                ))
                .await
                .ok();
                return;
            }
        };

        // A microVM has no stopped state and Docker cannot see one, so without
        // knowing which these are, nothing here can be stopped correctly. This
        // does not need a plane running here: without one, retiring a microVM
        // fails and is retried, rather than Docker's 404 reading as "stopped".
        let container_ids: Vec<String> = containers
            .iter()
            .map(|(container, _)| container.container_id.clone())
            .collect();
        let micro_vms = match crate::services::plane_deployer::micro_vm_replica_workloads(
            self.db.as_ref(),
            &container_ids,
        )
        .await
        {
            Ok(micro_vms) => micro_vms,
            Err(e) => {
                self.log(format!(
                    "Failed to classify containers of paused deployments in environment \
                     {environment_id}: {e} — will retry on next deployment"
                ))
                .await
                .ok();
                return;
            }
        };

        let stops = containers
            .into_iter()
            .filter_map(|(container, deployment)| Some((container, deployment?)))
            .map(|(container, deployment)| {
                let is_micro_vm = micro_vms.contains_key(&container.container_id);
                async move {
                    let container_id = container.container_id.clone();
                    let deployment_id = deployment.id;
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(CONTAINER_TEARDOWN_TIMEOUT_SECS),
                        self.stop_paused_container(
                            container,
                            is_micro_vm,
                            deployment.project_id,
                            environment_id,
                        ),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err(PausedSweepError::TimedOut {
                            container_id: container_id.clone(),
                            deployment_id,
                            timeout_seconds: CONTAINER_TEARDOWN_TIMEOUT_SECS,
                        })
                    });
                    let message = match result {
                        Ok(()) => format!(
                            "Container {container_id} of paused deployment {deployment_id} is not \
                             running"
                        ),
                        Err(error) => format!("{error} — will retry on next deployment"),
                    };
                    self.log(message).await.ok();
                }
            });
        futures::stream::iter(stops)
            .buffer_unordered(MAX_CONCURRENT_REPLICA_STOPS)
            .collect::<Vec<()>>()
            .await;
    }

    /// Stop one container of a paused deployment on the node that runs it,
    /// never falling back to this host, whose Docker does not own a worker's
    /// container. A microVM has no stopped state, so the only way to stop it
    /// is to retire it. A pause refuses them now, so this is only one paused
    /// before it did.
    async fn stop_paused_container(
        &self,
        container: deployment_containers::Model,
        is_micro_vm: bool,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), PausedSweepError> {
        let container_id = container.container_id.clone();
        let deployment_id = container.deployment_id;
        if is_micro_vm {
            return self
                // A paused deployment serves nothing, so there is nothing to
                // drain: the default grace period.
                .teardown_container(
                    container,
                    project_id,
                    environment_id,
                    std::time::Duration::from_secs(
                        temps_config::deployment_config::resolve_stop_timeout_seconds(None),
                    ),
                )
                .await
                .map_err(|reason| PausedSweepError::RetireMicroVm {
                    container_id,
                    deployment_id,
                    reason,
                });
        }

        let deployer = match container.node_id {
            Some(node_id) => self.get_remote_deployer(node_id).await.map_err(|reason| {
                PausedSweepError::WorkerUnreachable {
                    container_id: container_id.clone(),
                    deployment_id,
                    node_id,
                    reason,
                }
            })?,
            None => self.container_deployer.clone(),
        };
        match deployer.stop_container(&container_id).await {
            Ok(()) | Err(temps_deployer::DeployerError::ContainerNotFound(_)) => {}
            Err(source) => {
                return Err(PausedSweepError::Stop {
                    container_id,
                    deployment_id,
                    source,
                });
            }
        }

        if container.status.as_deref() != Some("stopped") {
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("stopped".to_string()));
            active_container.finished_at = Set(Some(chrono::Utc::now()));
            active_container
                .update(self.db.as_ref())
                .await
                .map_err(|source| PausedSweepError::MarkStopped {
                    container_id,
                    deployment_id,
                    source,
                })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_errors_name_the_container_deployment_and_node() {
        let message = PausedSweepError::WorkerUnreachable {
            container_id: "c-1".to_string(),
            deployment_id: 7,
            node_id: 3,
            reason: "connection refused".to_string(),
        }
        .to_string();
        for part in ["c-1", "deployment 7", "node 3", "connection refused"] {
            assert!(message.contains(part), "{part} missing from: {message}");
        }
    }
}
