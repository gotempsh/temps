// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Owner cleanup: removing a project's or environment's recorded containers
//! when it is deleted, and the ownership checks that guard it.

use super::*;

/// Why owner cleanup may skip a container whose runtime cannot be resolved,
/// or `None` when the failure must stop the deletion.
///
/// Only a row already recorded as removed qualifies: an unreachable runtime
/// cannot contradict that. Anything else may still exist, so the deletion
/// stops and says where -- including a row that records no node on a
/// process with no Docker daemon, which may be a container this server ran
/// itself before it was restarted without workloads.
fn unreachable_container_skip(
    container: &deployment_containers::Model,
    error: &DeploymentError,
) -> Option<String> {
    (container.status.as_deref() == Some(CONTAINER_STATUS_REMOVED)).then(|| {
        format!(
            "Skipping container already recorded as removed; its runtime cannot be checked: {error}"
        )
    })
}

/// The reason a deletion stops on a container whose runtime cannot be
/// resolved, with the remedy for the one case operators cannot infer: a row
/// with no node on a process that has no Docker daemon.
fn unreachable_container_reason(
    container: &deployment_containers::Model,
    error: &DeploymentError,
) -> String {
    if container.node_id.is_none() && matches!(error, DeploymentError::DockerUnavailable(_)) {
        format!(
            "{error}. Container '{}' (status {}) records no node, so it is either a local \
             container from when this server ran workloads -- start it with a profile that runs \
             workloads and delete again -- or it was left on a worker removed before Temps \
             recorded such containers; remove it on that host with `docker rm -f`",
            container.container_name,
            container.status.as_deref().unwrap_or("unknown")
        )
    } else {
        error.to_string()
    }
}

/// Label keys that have named a container's project over time.
const PROJECT_LABEL_KEYS: &[&str] = &["sh.temps.project_id"];

/// Label keys that have named a container's environment over time:
/// Compose stacks used `sh.temps.environment_id` until they adopted the image
/// deployer's `sh.temps.environment`.
const ENVIRONMENT_LABEL_KEYS: &[&str] = &["sh.temps.environment", "sh.temps.environment_id"];

/// Why owner cleanup may remove a recorded container it found running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecordedContainerOwnership {
    /// The current label scheme names exactly this project and environment.
    Labelled,
    /// The container predates the current label scheme (no labels, no
    /// `sh.temps.managed`, or a project ID written as a UUID), no label
    /// contradicts ownership, and Docker resolved the recorded reference to
    /// that very container -- so the database row is the ownership record.
    Recorded { missing_labels: Vec<&'static str> },
}

/// How the values under `keys` compare with `expected`.
enum LabelVerdict {
    /// At least one key carries `expected`, and none carries another ID.
    Matches,
    /// No key carries a comparable numeric ID (absent, or a legacy format).
    Unknown,
}

/// Compare every label in `keys` against `expected`, failing on the first one
/// that names a *different* numeric ID. A non-numeric value is a legacy
/// format (the first label scheme wrote project IDs as UUIDs) that can
/// neither prove nor disprove ownership, so it is reported as `Unknown`.
fn compare_id_labels(
    labels: &HashMap<String, String>,
    keys: &[&'static str],
    expected: i32,
) -> Result<LabelVerdict, String> {
    let mut verdict = LabelVerdict::Unknown;
    for key in keys {
        let Some(value) = labels.get(*key) else {
            continue;
        };
        match value.trim().parse::<i32>() {
            Ok(id) if id == expected => verdict = LabelVerdict::Matches,
            Ok(_) => return Err(format!("label {key}={value} (expected {expected})")),
            Err(_) => {}
        }
    }
    Ok(verdict)
}

/// Decide whether owner cleanup may remove `info`, the runtime container that
/// Docker returned for the recorded reference `recorded_container_id`, when
/// deleting `project_id`/`environment_id`.
///
/// The database row is the authority -- labels carry numeric IDs that are
/// local to one database, so they can never prove ownership on their own.
/// What labels *can* do is contradict it: a container labelled with another
/// project or environment, or explicitly marked unmanaged, is refused. A
/// container whose labels are absent or in a legacy format is accepted only
/// when Docker resolved the recorded reference to that exact container ID, so
/// a reference that now names someone else's container is still refused.
///
/// The error is the reason the container was refused, for the cleanup error.
pub(super) fn recorded_container_ownership(
    recorded_container_id: &str,
    info: &temps_deployer::ContainerInfo,
    project_id: i32,
    environment_id: i32,
) -> Result<RecordedContainerOwnership, String> {
    let labels = &info.labels;
    let mut conflicts = Vec::new();
    let managed = labels.get("sh.temps.managed").map(String::as_str);
    if let Some(value) = managed.filter(|value| *value != "true") {
        conflicts.push(format!("label sh.temps.managed={value}"));
    }
    let mut compare = |keys, expected| match compare_id_labels(labels, keys, expected) {
        Ok(verdict) => Some(verdict),
        Err(conflict) => {
            conflicts.push(conflict);
            None
        }
    };
    let project = compare(PROJECT_LABEL_KEYS, project_id);
    let environment = compare(ENVIRONMENT_LABEL_KEYS, environment_id);
    if !conflicts.is_empty() {
        return Err(format!(
            "runtime container labels name a different owner than project {project_id}, \
             environment {environment_id}: {}",
            conflicts.join(", ")
        ));
    }

    let mut missing_labels = Vec::new();
    if managed.is_none() {
        missing_labels.push("sh.temps.managed");
    }
    if !matches!(project, Some(LabelVerdict::Matches)) {
        missing_labels.push("sh.temps.project_id");
    }
    if !matches!(environment, Some(LabelVerdict::Matches)) {
        missing_labels.push("sh.temps.environment");
    }
    if missing_labels.is_empty() {
        return Ok(RecordedContainerOwnership::Labelled);
    }

    // Docker resolves names and ID prefixes too, and some rows (importers)
    // record a source app name rather than an ID, so a short reference can
    // land on an unrelated container. Without labels to vouch for it, only a
    // row that recorded the container's full, exact ID proves ownership.
    if is_full_container_id(recorded_container_id) && info.container_id == recorded_container_id {
        return Ok(RecordedContainerOwnership::Recorded { missing_labels });
    }
    Err(format!(
        "runtime container '{}' is missing ownership labels ({}) and the recorded reference \
         '{recorded_container_id}' is not its full container ID, so it cannot be confirmed as \
         belonging to project {project_id}, environment {environment_id}",
        info.container_id,
        missing_labels.join(", "),
    ))
}

/// Whether `value` is a full Docker container ID: 64 lowercase hex digits,
/// the form Docker returns from create and inspect. Prefixes and names are
/// ambiguous references, never identities.
fn is_full_container_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl DeploymentService {
    async fn cleanup_containers(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<u64, temps_core::ContainerCleanupError> {
        let mut query = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::ProjectId.eq(project_id));
        if let Some(environment_id) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(environment_id));
        }

        let containers = query.all(self.db.as_ref()).await.map_err(|error| {
            temps_core::ContainerCleanupError::Discovery {
                project_id,
                environment_id,
                reason: error.to_string(),
            }
        })?;

        let recorded_ids: HashSet<&str> =
            containers.iter().map(|c| c.container_id.as_str()).collect();
        let expected_project = project_id.to_string();
        let expected_environment = environment_id.map(|id| id.to_string());
        // Discovery identifies ambiguity; it never authorizes destruction.
        for runtime in self.deployer.list_containers().await.unwrap_or_else(|error| {
            warn!(project_id, environment_id, %error, "Could not list unrecorded containers; preserving them and continuing recorded cleanup");
            Vec::new()
        }) {
            if runtime.labels.get("sh.temps.managed").map(String::as_str) == Some("true")
                && runtime.labels.get("sh.temps.project_id") == Some(&expected_project)
                && expected_environment
                    .as_ref()
                    .is_none_or(|id| runtime.labels.get("sh.temps.environment") == Some(id))
                && !recorded_ids.contains(runtime.container_id.as_str())
            {
                warn!(project_id, environment_id, container_id = %runtime.container_id,
                    "Preserving unrecorded container with ambiguous database-local labels; only recorded container IDs will be removed");
            }
        }

        if self.compose_executor.get().is_some() {
            if let Some(docker) = self.docker_handle.get() {
                let mut query = environments::Entity::find()
                    .filter(environments::Column::ProjectId.eq(project_id));
                if let Some(id) = environment_id {
                    query = query.filter(environments::Column::Id.eq(id));
                }
                let envs = query.all(self.db.as_ref()).await.unwrap_or_else(|error| {
                    warn!(project_id, environment_id, %error, "Could not list Compose environments; preserving their resources and continuing recorded cleanup");
                    Vec::new()
                });
                for env in envs {
                    let compose_name = format!("temps-{project_id}-{}", env.id);
                    let filters = HashMap::from([(
                        "label".to_string(),
                        vec![format!("com.docker.compose.project={compose_name}")],
                    )]);
                    let networks = docker
                        .list_networks(Some(
                            bollard::query_parameters::ListNetworksOptionsBuilder::new()
                                .filters(&filters)
                                .build(),
                        ))
                        .await
                        .unwrap_or_else(|error| {
                            warn!(project_id, environment_id, %error, "Could not list Compose networks; preserving them and continuing recorded cleanup");
                            Vec::new()
                        });
                    let volumes = docker
                        .list_volumes(Some(
                            bollard::query_parameters::ListVolumesOptionsBuilder::new()
                                .filters(&filters)
                                .build(),
                        ))
                        .await
                        .map(|response| response.volumes.unwrap_or_default())
                        .unwrap_or_else(|error| {
                            warn!(project_id, environment_id, %error, "Could not list Compose volumes; preserving them and continuing recorded cleanup");
                            Vec::new()
                        });
                    let resources: Vec<String> = networks
                        .into_iter()
                        .filter_map(|n| n.id.map(|id| format!("network {id}")))
                        .chain(volumes.into_iter().map(|v| format!("volume {}", v.name)))
                        .collect();
                    if !resources.is_empty() {
                        warn!(project_id, environment_id, compose_project = %compose_name,
                            resources = ?resources,
                            "Preserving Compose networks and volumes without durable ownership records; reclaim only after verifying ownership with docker inspect");
                    }
                }
            }
        }

        let deployment_ids: Vec<i32> = containers
            .iter()
            .map(|container| container.deployment_id)
            .collect();
        let deployment_environment_ids: HashMap<i32, i32> = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|error| temps_core::ContainerCleanupError::Discovery {
                project_id,
                environment_id,
                reason: format!("failed to resolve container environments: {error}"),
            })?
            .into_iter()
            .map(|deployment| (deployment.id, deployment.environment_id))
            .collect();

        let mut removed = 0_u64;
        for original in containers {
            let container_id = original.container_id.clone();
            let node_id = original.node_id;
            let container_environment_id = deployment_environment_ids
                .get(&original.deployment_id)
                .copied()
                .ok_or_else(|| temps_core::ContainerCleanupError::Discovery {
                    project_id,
                    environment_id,
                    reason: format!(
                        "deployment {} disappeared while preparing container '{}' for cleanup",
                        original.deployment_id, container_id
                    ),
                })?;

            // Left on a node that was removed from the cluster: Temps no
            // longer manages that host, so there is nothing it may remove.
            if original.status.as_deref() == Some(CONTAINER_STATUS_ORPHANED) {
                warn!(
                    project_id,
                    environment_id = container_environment_id,
                    container_id,
                    note = original
                        .error_message
                        .as_deref()
                        .unwrap_or("no placement recorded"),
                    "Skipping container orphaned by a removed node; it is not removed by Temps"
                );
                continue;
            }

            // A replica an `enforce` deployment started through the workload
            // plane is retired by deleting its workload: the plane removes
            // the instance on whichever node runs it. Docker on this host
            // cannot see a microVM (or a worker's container), so its 404 must
            // never count as "already removed".
            let plane_workload = crate::services::plane_deployer::deployment_replica_workload(
                self.db.as_ref(),
                original.deployment_id,
                &container_id,
            )
            .await
            .map_err(|error| temps_core::ContainerCleanupError::Removal {
                project_id,
                environment_id: container_environment_id,
                container_id: container_id.clone(),
                node_id,
                reason: error.to_string(),
            })?;
            if let Some(workload) = plane_workload {
                let prepared = self
                    .mark_container_removing(&original, project_id, container_environment_id)
                    .await?;
                if let Err(error) = crate::services::plane_deployer::retire_replica_workload(
                    self.workload_plane.get().map(Arc::as_ref),
                    original.deployment_id,
                    &container_id,
                    workload,
                )
                .await
                {
                    // Keep the `removing` marker: routing stays fenced and a
                    // retry converges once the plane can retire it.
                    return Err(temps_core::ContainerCleanupError::Removal {
                        project_id,
                        environment_id: container_environment_id,
                        container_id,
                        node_id,
                        reason: error.to_string(),
                    });
                }
                let mut active: deployment_containers::ActiveModel = prepared.into();
                active.status = Set(Some("removed".to_string()));
                active.update(self.db.as_ref()).await.map_err(|error| {
                    temps_core::ContainerCleanupError::Finalize {
                        project_id,
                        environment_id: container_environment_id,
                        container_id: container_id.clone(),
                        node_id,
                        reason: error.to_string(),
                    }
                })?;
                removed += 1;
                info!(
                    project_id,
                    environment_id = container_environment_id,
                    container_id,
                    ?node_id,
                    workload_id = workload.workload_id,
                    "Retired workload-plane replica before owner deletion"
                );
                continue;
            }

            // Resolve where the container lives before touching its row.
            let deployer = match self.deployer_for_node(node_id).await {
                Ok(deployer) => deployer,
                Err(error) => {
                    if let Some(skip) = unreachable_container_skip(&original, &error) {
                        warn!(
                            project_id,
                            environment_id = container_environment_id,
                            container_id,
                            "{skip}"
                        );
                        continue;
                    }
                    return Err(temps_core::ContainerCleanupError::Removal {
                        project_id,
                        environment_id: container_environment_id,
                        container_id,
                        node_id,
                        reason: unreachable_container_reason(&original, &error),
                    });
                }
            };

            // Hide intentional teardown from routing and health monitoring before
            // Docker observes the removal. The distinct `removing` state makes a
            // process crash retryable instead of turning the row into a false
            // completed cleanup.
            let already_prepared = original.status.as_deref() == Some("removing");
            let prepared = self
                .mark_container_removing(&original, project_id, container_environment_id)
                .await?;

            // The exact container that was inspected and judged ours. Removal
            // targets this ID, never the recorded reference: Docker resolves
            // names and ID prefixes again on every call, so the reference
            // could name a different container by the time it is removed.
            let verified_runtime_id = match deployer.get_container_info(&container_id).await {
                Ok(info) => {
                    match recorded_container_ownership(
                        &container_id,
                        &info,
                        project_id,
                        container_environment_id,
                    ) {
                        Ok(RecordedContainerOwnership::Labelled) => {}
                        Ok(RecordedContainerOwnership::Recorded { missing_labels }) => {
                            warn!(
                                project_id,
                                environment_id = container_environment_id,
                                container_id,
                                ?node_id,
                                ?missing_labels,
                                "Removing recorded container that predates Temps ownership labels; the database record is its ownership proof"
                            );
                        }
                        Err(reason) => {
                            let _ = self
                                .restore_cleanup_marker(&original, already_prepared)
                                .await;
                            return Err(temps_core::ContainerCleanupError::Removal {
                                project_id,
                                environment_id: container_environment_id,
                                container_id,
                                node_id,
                                reason,
                            });
                        }
                    }
                    Some(if info.container_id.is_empty() {
                        container_id.clone()
                    } else {
                        info.container_id
                    })
                }
                Err(temps_deployer::DeployerError::ContainerNotFound(_)) => None,
                Err(error) => {
                    let _ = self
                        .restore_cleanup_marker(&original, already_prepared)
                        .await;
                    // Already recorded as removed: an unreachable node cannot
                    // contradict that, so it must not block the deletion.
                    if original.status.as_deref() == Some(CONTAINER_STATUS_REMOVED) {
                        warn!(
                            project_id,
                            environment_id = container_environment_id,
                            container_id,
                            ?node_id,
                            %error,
                            "Skipping container already recorded as removed; its node could not confirm it"
                        );
                        continue;
                    }
                    return Err(temps_core::ContainerCleanupError::Removal {
                        project_id,
                        environment_id: container_environment_id,
                        container_id,
                        node_id,
                        reason: format!("failed to verify runtime container ownership: {error}"),
                    });
                }
            };

            let removal_error = if let Some(runtime_id) = verified_runtime_id {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    deployer.remove_container(&runtime_id),
                )
                .await
                {
                    Ok(Ok(())) | Ok(Err(temps_deployer::DeployerError::ContainerNotFound(_))) => {
                        None
                    }
                    Ok(Err(error)) => Some(error.to_string()),
                    Err(_) => Some("container removal timed out after 30 seconds".to_string()),
                }
            } else {
                None
            };
            if let Some(reason) = removal_error {
                // Once a request may have reached Docker/the agent, its result is
                // ambiguous. Keep the durable `removing` marker so routing stays
                // fenced and a retry converges idempotently.
                return Err(temps_core::ContainerCleanupError::Removal {
                    project_id,
                    environment_id: container_environment_id,
                    container_id,
                    node_id,
                    reason,
                });
            }

            let mut active: deployment_containers::ActiveModel = prepared.into();
            active.status = Set(Some("removed".to_string()));
            active.update(self.db.as_ref()).await.map_err(|error| {
                temps_core::ContainerCleanupError::Finalize {
                    project_id,
                    environment_id: container_environment_id,
                    container_id: container_id.clone(),
                    node_id,
                    reason: error.to_string(),
                }
            })?;

            removed += 1;
            info!(
                project_id,
                environment_id = container_environment_id,
                container_id,
                ?node_id,
                "Removed application container before owner deletion"
            );
        }

        // Numeric project/environment IDs and Compose project names are local
        // to one database, not ownership proof on a shared Docker host. Never
        // discover or destroy additional resources by those labels: another
        // Temps instance can have the same IDs. Only the exact container IDs
        // recorded above are safe to remove. Unrecorded legacy Compose resources
        // require manual cleanup until durable ownership is recorded.

        Ok(removed)
    }

    /// Hide a container being torn down from routing and health monitoring:
    /// tombstoned with the distinct `removing` state, so a crash mid-removal
    /// is retried rather than read as a completed cleanup. A row already in
    /// that state is returned as is.
    async fn mark_container_removing(
        &self,
        original: &deployment_containers::Model,
        project_id: i32,
        environment_id: i32,
    ) -> Result<deployment_containers::Model, temps_core::ContainerCleanupError> {
        if original.status.as_deref() == Some("removing") {
            return Ok(original.clone());
        }
        let mut active: deployment_containers::ActiveModel = original.clone().into();
        active.deleted_at = Set(Some(chrono::Utc::now()));
        active.status = Set(Some("removing".to_string()));
        active.update(self.db.as_ref()).await.map_err(|error| {
            temps_core::ContainerCleanupError::Prepare {
                project_id,
                environment_id,
                container_id: original.container_id.clone(),
                node_id: original.node_id,
                reason: error.to_string(),
            }
        })
    }

    async fn restore_cleanup_marker(
        &self,
        original: &deployment_containers::Model,
        already_prepared: bool,
    ) -> Result<(), sea_orm::DbErr> {
        if already_prepared {
            return Ok(());
        }
        let mut active: deployment_containers::ActiveModel = original.clone().into();
        active.status = Set(original.status.clone());
        active.deleted_at = Set(original.deleted_at);
        active.update(self.db.as_ref()).await.map(|_| ())
    }
}

#[async_trait::async_trait]
impl temps_core::DeploymentContainerCleaner for DeploymentService {
    async fn cleanup_project_containers(
        &self,
        project_id: i32,
    ) -> Result<u64, temps_core::ContainerCleanupError> {
        self.cleanup_containers(project_id, None).await
    }

    async fn cleanup_environment_containers(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<u64, temps_core::ContainerCleanupError> {
        self.cleanup_containers(project_id, Some(environment_id))
            .await
    }
}
