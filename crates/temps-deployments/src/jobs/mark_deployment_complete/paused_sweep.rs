// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The part of a deploy's cleanup that concerns older paused deployments: they
//! stay resumable, so their containers are stopped rather than torn down.

use super::*;

/// Safety bound on how many containers of paused deployments a single deploy
/// asks to stop. Paused deployments stay paused, and so stay in this query, for
/// as long as they stay resumable; newest first keeps a long history of them
/// from slowing every deploy, and a stop of a stopped container is cheap.
const MAX_PAUSED_CONTAINERS_STOPPED_PER_PASS: u64 = 100;

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

        let stops = containers
            .into_iter()
            .filter_map(|(container, deployment)| Some((container, deployment?)))
            .map(|(container, deployment)| async move {
                let container_id = container.container_id.clone();
                let deployment_id = deployment.id;
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(CONTAINER_TEARDOWN_TIMEOUT_SECS),
                    self.stop_paused_container(container, deployment.project_id, environment_id),
                )
                .await;
                let message = match result {
                    Ok(Ok(())) => format!(
                        "Container {container_id} of paused deployment {deployment_id} is not running"
                    ),
                    Ok(Err(error)) => format!("{error} — will retry on next deployment"),
                    Err(_) => format!(
                        "Timed out stopping container {container_id} of paused deployment \
                         {deployment_id} after {CONTAINER_TEARDOWN_TIMEOUT_SECS}s — will retry on \
                         next deployment"
                    ),
                };
                self.log(message).await.ok();
            });
        futures::future::join_all(stops).await;
    }

    /// Stop one container of a paused deployment on the node that runs it,
    /// never falling back to this host, whose Docker does not own a worker's
    /// container. A microVM has no stopped state, so the only way to stop it
    /// is to retire it. A pause refuses them now, so this is only one paused
    /// before it did.
    async fn stop_paused_container(
        &self,
        container: deployment_containers::Model,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), String> {
        let container_id = container.container_id.clone();
        if let Some(plane) = self.workload_plane.as_deref() {
            let micro_vm =
                crate::services::plane_deployer::micro_vm_replica_workload(plane, &container_id)
                    .await
                    .map_err(|e| e.to_string())?;
            if micro_vm.is_some() {
                return self
                    .teardown_container(container, project_id, environment_id)
                    .await;
            }
        }

        let deployer = match container.node_id {
            Some(node_id) => self.get_remote_deployer(node_id).await.map_err(|e| {
                format!(
                    "Failed to connect to worker node {node_id} to stop container {container_id} \
                     of paused deployment {}: {e}",
                    container.deployment_id
                )
            })?,
            None => self.container_deployer.clone(),
        };
        match deployer.stop_container(&container_id).await {
            Ok(()) | Err(temps_deployer::DeployerError::ContainerNotFound(_)) => {}
            Err(e) => {
                return Err(format!(
                    "Failed to stop container {container_id} of paused deployment {}: {e}",
                    container.deployment_id
                ));
            }
        }

        if container.status.as_deref() != Some("stopped") {
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("stopped".to_string()));
            active_container.finished_at = Set(Some(chrono::Utc::now()));
            active_container
                .update(self.db.as_ref())
                .await
                .map_err(|e| format!("Failed to mark container {container_id} stopped: {e}"))?;
        }
        Ok(())
    }
}
