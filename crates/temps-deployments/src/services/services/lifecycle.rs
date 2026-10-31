// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment lifecycle: teardown, pause and resume.

use super::*;

/// Deployment states a pause applies to: the deployment is live and serving.
const PAUSABLE_DEPLOYMENT_STATES: &[&str] = &["deployed", "completed"];

/// Deployment states a resume applies to: only one paused by `pause_deployment`.
const RESUMABLE_DEPLOYMENT_STATES: &[&str] = &["paused"];

/// Refuse a pause/resume whose current state is not one it applies to.
/// Without this, resume restarted the containers of a stopped, failed or
/// cancelled deployment and marked it "deployed" again.
fn check_lifecycle_transition(
    deployment: &deployments::Model,
    operation: &'static str,
    allowed: &[&str],
    expected: &'static str,
) -> Result<(), DeploymentError> {
    if allowed.contains(&deployment.state.as_str()) {
        return Ok(());
    }
    Err(DeploymentError::DeploymentStateConflict {
        deployment_id: deployment.id,
        operation,
        state: deployment.state.clone(),
        expected,
    })
}

impl DeploymentService {
    /// Tears down a specific deployment, removing containers and cleaning up resources
    pub async fn teardown_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::deployment_containers;

        // Find the deployment
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;

        // Stop all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for container in containers {
            self.stop_replica_for_teardown(&container).await?;

            // Mark container as deleted
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.deleted_at = Set(Some(chrono::Utc::now()));
            active_container.status = Set(Some("stopped".to_string()));
            active_container.update(self.db.as_ref()).await?;
        }

        // Update deployment state to "stopped"
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("stopped".to_string());
        active_deployment.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Stop one replica of a deployment being torn down, wherever it runs.
    /// A replica the workload plane started (a microVM, or a container on a
    /// worker) is retired by deleting its workload: this host's Docker
    /// cannot see it and answers 404. Any other replica is stopped by the
    /// deployer of the node that runs it.
    async fn stop_replica_for_teardown(
        &self,
        container: &temps_entities::deployment_containers::Model,
    ) -> Result<(), DeploymentError> {
        let workload = crate::services::plane_deployer::deployment_replica_workload(
            self.db.as_ref(),
            container.deployment_id,
            &container.container_id,
        )
        .await?;
        if let Some(workload) = workload {
            crate::services::plane_deployer::retire_replica_workload(
                self.workload_plane.get().map(Arc::as_ref),
                container.deployment_id,
                &container.container_id,
                workload,
            )
            .await?;
            info!(
                deployment_id = container.deployment_id,
                container_id = container.container_id,
                workload_id = workload.workload_id,
                "Retired workload-plane replica on teardown"
            );
            return Ok(());
        }
        self.deployer_for_node(container.node_id)
            .await?
            .stop_container(&container.container_id)
            .await
            .map_err(|e| DeploymentError::ContainerOperation {
                container_id: container.container_id.clone(),
                operation: "stop",
                location: container
                    .node_id
                    .map_or_else(|| "this host".to_string(), |id| format!("node {id}")),
                reason: e.to_string(),
            })
    }

    /// Tears down an environment and all its active deployments
    pub async fn teardown_environment(
        &self,
        project_id: i32,
        env_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::deployment_containers;

        // Find all deployments in this environment
        let deployments = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(env_id))
            .all(self.db.as_ref())
            .await?;

        // Stop all containers for all deployments
        for deployment in &deployments {
            let containers = deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(self.db.as_ref())
                .await?;

            for container in containers {
                // Stop container with timeout - don't fail the whole teardown if one container fails
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    self.stop_replica_for_teardown(&container),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    // The plane still runs it: keep the row, so a later
                    // cleanup finds the workload and retires it, rather than
                    // leaving a microVM no row accounts for.
                    Ok(Err(e @ DeploymentError::ReplicaRetire(_))) => {
                        warn!(
                            "Failed to retire workload-plane replica {} during teardown: {} (continuing)",
                            container.container_id, e
                        );
                        continue;
                    }
                    Ok(Err(e)) => {
                        warn!(
                            "Failed to stop container {} during teardown: {} (continuing)",
                            container.container_id, e
                        );
                    }
                    Err(_) => {
                        warn!(
                            "Timed out stopping container {} after 30s during teardown (continuing)",
                            container.container_id
                        );
                    }
                }

                // Mark container as deleted
                let mut active_container: deployment_containers::ActiveModel = container.into();
                active_container.deleted_at = Set(Some(chrono::Utc::now()));
                active_container.status = Set(Some("stopped".to_string()));
                active_container.update(self.db.as_ref()).await?;
            }
        }

        // Update all deployment states to "stopped"
        for deployment in deployments {
            let mut active_deployment: deployments::ActiveModel = deployment.into();
            active_deployment.state = Set("stopped".to_string());
            active_deployment.update(self.db.as_ref()).await?;
        }

        Ok(())
    }

    pub async fn pause_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployment_containers, deployments, status_incidents};

        // First verify the deployment exists and belongs to the project
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;
        check_lifecycle_transition(&deployment, "pause", PAUSABLE_DEPLOYMENT_STATES, "deployed")?;

        let environment_id = deployment.environment_id;

        // Persist "paused" BEFORE touching any container. Monitoring (the
        // container-health poller and the uptime health checker) treats
        // `state == "paused"` as "stopped on purpose, don't alert" — if we
        // stopped containers first and updated this row last, a poll that
        // lands in between would see an exited container against a
        // not-yet-paused deployment and fire a false crash/downtime alert.
        // Flipping the state first closes that window: any concurrent read
        // of this deployment either sees the old state with all containers
        // still running (nothing exited yet to alert on) or sees "paused"
        // once anything might be mid-stop.
        //
        // That alone still leaves a plain check-then-write race against a
        // concurrent outage check in flight: it can read "not paused" a
        // moment before this write commits, then create an incident for a
        // deployment that's paused by the time the incident actually lands.
        // Closing that requires more than moving this write earlier — the
        // check and the write need to serialize. Take the same Postgres
        // advisory lock, keyed on the environment, that
        // `OutageDetectionService::handle_outage_event` takes around its
        // final live pause re-check + incident insert: whichever side gets
        // the lock first commits (or observes "paused" and bails) before the
        // other proceeds, so no unpaused-read can ever precede this write
        // without the write also being visible to it.
        let txn = self.db.begin().await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [sea_orm::Value::BigInt(Some(environment_id as i64))],
        ))
        .await?;
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("paused".to_string());
        active_deployment.update(&txn).await?;

        // The lock above only serializes the incident *insert* against this
        // write — `OutageDetectionService` still sends the notification,
        // fires the alarm, and dispatches the workflow AFTER releasing the
        // lock, since those are external I/O (webhook/email delivery, job
        // queue send) that can't reasonably sit inside a DB transaction. A
        // pause landing in that specific gap would otherwise still produce
        // an alert for an incident that's already stale by the time it goes
        // out. Rather than trying to also lock out external I/O (which a DB
        // lock structurally can't do), make this side of the race
        // proactive: while still holding the lock, resolve any incident for
        // this environment that's still open. `handle_outage_event`
        // re-reads the incident's own status immediately before each side
        // effect (see its comment), so a resolve that lands here — even a
        // moment after that incident was created — is what that re-read is
        // watching for.
        status_incidents::Entity::update_many()
            .col_expr(
                status_incidents::Column::Status,
                sea_orm::sea_query::Expr::value("resolved"),
            )
            .col_expr(
                status_incidents::Column::ResolvedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(status_incidents::Column::EnvironmentId.eq(environment_id))
            .filter(status_incidents::Column::Status.ne("resolved"))
            .exec(&txn)
            .await?;

        txn.commit().await?;

        // Stop and remove all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        // Stop (but do not remove) each container. Keeping the Docker
        // container object around — rather than force-removing it, as this
        // used to do — is what makes `resume_deployment` able to bring the
        // exact same containers back with a plain `docker start` instead of
        // trying to "unpause" containers that no longer exist (see the fix
        // note on `resume_deployment` below). The route table additionally
        // stops sending live traffic to any container whose `status` isn't
        // "running" (see `route_table::load_routes`), so a stopped-but-not-
        // removed container is just as inert from the outside as a removed
        // one, without sacrificing resumability.
        // Best-effort like the `stop_container` call above: the deployment
        // row is already committed as "paused", so aborting this loop on a
        // single container's DB write failure would strand the *remaining*
        // containers untouched (still "running" in the DB, never even
        // asked to stop) and skip the route-table reload below, while the
        // deployment stays paused indefinitely. Warn and keep going so one
        // failure can't silently drop the rest of the pause.
        for container in containers {
            let container_id = container.container_id.clone();
            if let Err(e) = self.deployer.stop_container(&container_id).await {
                warn!(
                    "Failed to stop container {} during deployment pause: {}",
                    container_id, e
                );
            }

            // Retry the DB write: a container whose status never makes it to
            // "stopped" keeps being treated as routable by
            // `route_table::load_routes` even though we just told Docker to
            // stop it — retrying absorbs the transient connection blips that
            // are the realistic cause of a single UPDATE failing right after
            // the read and the deployment-state write above it both
            // succeeded, so routes don't go stale on something recoverable.
            let retry = temps_core::retry::RetryConfig::new(3)
                .with_base_delay(std::time::Duration::from_millis(100))
                .with_max_delay(std::time::Duration::from_secs(2));
            let stopped_at = chrono::Utc::now();
            let update_result = retry
                .retry(|| async {
                    let active_container = deployment_containers::ActiveModel {
                        status: Set(Some("stopped".to_string())),
                        finished_at: Set(Some(stopped_at)),
                        ..deployment_containers::ActiveModel::from(container.clone())
                    };
                    active_container.update(self.db.as_ref()).await
                })
                .await;
            if let Err(e) = update_result {
                warn!(
                    "Failed to persist stopped status for container {} during deployment pause \
                     after retrying: {} — the route table may still treat it as routable until \
                     the next successful status update",
                    container_id, e
                );
            }
        }

        // Force an in-process route-table reload (same mechanism
        // `mark_deployment_complete.rs` uses after a normal deploy — see its
        // comment for why this is needed in addition to PG NOTIFY). Nothing
        // else about a pause touches `environments` or `projects`, which are
        // the only tables with a NOTIFY trigger wired up (see
        // `m20251209_000001_add_environments_route_trigger.rs` /
        // `m20250205_000003_add_projects_route_trigger.rs`) — a bare
        // `deployment_containers` status UPDATE fires no trigger at all. So
        // without this, the proxy's cached peer table keeps the container's
        // OLD (still "valid-looking") address indefinitely and only
        // discovers the pause when the next unrelated route change happens
        // to reload it, in the meantime returning "upstream connection
        // refused" instead of the intended "not currently serving" state.
        if let Err(e) = self
            .queue_service
            .send(temps_core::Job::ForceRouteReload(
                temps_core::ForceRouteReloadJob {
                    environment_id: Some(environment_id),
                    deployment_id: Some(deployment_id),
                },
            ))
            .await
        {
            warn!(
                "Failed to publish in-process ForceRouteReload after pausing deployment {}: {} \
                 — falling back to the next PG NOTIFY-triggered reload",
                deployment_id, e
            );
        }

        info!(
            "Successfully paused deployment {}: stopped all containers",
            deployment_id
        );
        Ok(())
    }

    pub async fn resume_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), DeploymentError> {
        use temps_entities::deployment_containers;

        // First verify the deployment exists and belongs to the project
        let deployment = deployments::Entity::find()
            .filter(deployments::Column::Id.eq(deployment_id))
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Deployment not found".to_string()))?;
        check_lifecycle_transition(&deployment, "resume", RESUMABLE_DEPLOYMENT_STATES, "paused")?;

        // Resume all containers for this deployment
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        // `pause_deployment` stops (not removes) containers, so bring them
        // back with a plain `docker start` on the same container id/name —
        // not `resume_container` (Docker's `unpause`/cgroup-freeze reverse).
        // `unpause` only undoes a genuine `docker pause`, which nothing in
        // this codebase's real pause path ever calls; using it here against
        // a merely-stopped container always failed ("container is not
        // paused"), so resume could never actually succeed after a real
        // pause.
        for container in containers {
            self.deployer
                .start_container(&container.container_id)
                .await
                .map_err(|e| {
                    DeploymentError::Other(format!("Failed to resume container: {}", e))
                })?;

            // Update container status
            let mut active_container: deployment_containers::ActiveModel = container.into();
            active_container.status = Set(Some("running".to_string()));
            active_container.update(self.db.as_ref()).await?;
        }

        let environment_id = deployment.environment_id;

        // Update deployment state to "deployed"
        let mut active_deployment: deployments::ActiveModel = deployment.into();
        active_deployment.state = Set("deployed".to_string());
        active_deployment.update(self.db.as_ref()).await?;

        // See the matching comment in `pause_deployment`: a container-status
        // UPDATE fires no DB trigger, so force an in-process reload rather
        // than leaving the proxy's cached peer table to notice the resume
        // only whenever some unrelated route change happens to trigger one.
        if let Err(e) = self
            .queue_service
            .send(temps_core::Job::ForceRouteReload(
                temps_core::ForceRouteReloadJob {
                    environment_id: Some(environment_id),
                    deployment_id: Some(deployment_id),
                },
            ))
            .await
        {
            warn!(
                "Failed to publish in-process ForceRouteReload after resuming deployment {}: {} \
                 — falling back to the next PG NOTIFY-triggered reload",
                deployment_id, e
            );
        }

        info!("Successfully resumed deployment: {}", deployment_id);
        Ok(())
    }
}

#[cfg(test)]
mod lifecycle_transition_tests {
    use super::*;

    fn deployment_in(state: &str) -> deployments::Model {
        deployments::Model {
            id: 17,
            project_id: 1,
            environment_id: 1,
            slug: "deploy-17".to_string(),
            state: state.to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: None,
            context_vars: None,
            branch_ref: Some("main".to_string()),
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
            upload_request_id: None,
            docker_socket_mounted: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn only_a_paused_deployment_can_be_resumed() {
        let resume = |state| {
            check_lifecycle_transition(
                &deployment_in(state),
                "resume",
                RESUMABLE_DEPLOYMENT_STATES,
                "paused",
            )
        };
        assert!(resume("paused").is_ok());
        for state in [
            "stopped",
            "failed",
            "cancelled",
            "pending",
            "running",
            "deployed",
        ] {
            match resume(state) {
                Err(DeploymentError::DeploymentStateConflict {
                    deployment_id: 17,
                    operation: "resume",
                    state: rejected,
                    ..
                }) => assert_eq!(rejected, state),
                other => panic!("resuming a '{state}' deployment must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn only_a_live_deployment_can_be_paused() {
        let pause = |state| {
            check_lifecycle_transition(
                &deployment_in(state),
                "pause",
                PAUSABLE_DEPLOYMENT_STATES,
                "deployed",
            )
        };
        assert!(pause("deployed").is_ok());
        assert!(pause("completed").is_ok());
        for state in [
            "paused",
            "stopped",
            "failed",
            "cancelled",
            "pending",
            "running",
        ] {
            assert!(
                matches!(
                    pause(state),
                    Err(DeploymentError::DeploymentStateConflict {
                        operation: "pause",
                        ..
                    })
                ),
                "pausing a '{state}' deployment must be refused"
            );
        }
    }

    #[test]
    fn conflict_message_names_the_deployment_and_its_state() {
        let message = check_lifecycle_transition(
            &deployment_in("stopped"),
            "resume",
            RESUMABLE_DEPLOYMENT_STATES,
            "paused",
        )
        .unwrap_err()
        .to_string();
        assert!(message.contains("deployment 17"), "{message}");
        assert!(message.contains("'stopped'"), "{message}");
        assert!(message.contains("paused"), "{message}");
    }
}
