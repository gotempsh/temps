// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker operations on a database instance that both the control plane and
//! a worker's agent run against their own Docker daemon.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum, HealthStatusEnum};
use bollard::query_parameters::{InspectContainerOptions, StartContainerOptions};
use bollard::Docker;
use serde::{Deserialize, Serialize};
use temps_workload_docker::DockerDriver;
use tracing::info;

use crate::externalsvc::recreate_guard::{
    recreate_preserving_data, PlannedMount, RecreateRequest, ReplacementStorage,
};
use crate::workload::spec::{
    database_instance, DatabaseWorkloadPlan, NodeAttachment, SecretValues,
};
use crate::workload::ServiceWorkloadError;

/// How long a database created from its spec may take to report healthy:
/// the longest engine grace period (MongoDB's 45s) plus a first boot that
/// initialises the data directory.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(180);
const HEALTH_POLL: Duration = Duration::from_secs(1);

/// A database container as Docker reports it, and its image's defaults
/// (which Docker merged into the container's environment and command).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerSnapshot {
    pub found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspect: Option<ContainerInspectResponse>,
    #[serde(default)]
    pub image_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_cmd: Option<Vec<String>>,
}

fn is_not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

/// Inspect `container_name` and its image.
pub async fn snapshot(
    docker: &Docker,
    container_name: &str,
) -> Result<ContainerSnapshot, ServiceWorkloadError> {
    let inspect = match docker
        .inspect_container(container_name, None::<InspectContainerOptions>)
        .await
    {
        Ok(inspect) => inspect,
        Err(e) if is_not_found(&e) => return Ok(ContainerSnapshot::default()),
        Err(e) => {
            return Err(ServiceWorkloadError::Docker {
                operation: "inspect",
                container: container_name.to_string(),
                reason: e.to_string(),
            })
        }
    };
    // The image the container was created from, by id: its tag may have
    // moved since.
    let image_ref = inspect
        .image
        .clone()
        .or_else(|| inspect.config.as_ref().and_then(|c| c.image.clone()));
    let image_config = match image_ref {
        Some(image) => docker
            .inspect_image(&image)
            .await
            .ok()
            .and_then(|i| i.config),
        None => None,
    };
    Ok(ContainerSnapshot {
        found: true,
        image_env: image_config
            .as_ref()
            .and_then(|c| c.env.clone())
            .unwrap_or_default(),
        image_cmd: image_config.and_then(|c| c.cmd),
        inspect: Some(inspect),
    })
}

/// Wait until `container` runs and its healthcheck passes.
pub async fn wait_healthy(
    docker: &Docker,
    container: &str,
    timeout: Duration,
) -> Result<(), ServiceWorkloadError> {
    let started = tokio::time::Instant::now();
    let mut last = String::from("no state reported yet");
    while started.elapsed() < timeout {
        let inspect = docker
            .inspect_container(container, None::<InspectContainerOptions>)
            .await
            .map_err(|e| ServiceWorkloadError::Docker {
                operation: "inspect",
                container: container.to_string(),
                reason: e.to_string(),
            })?;
        if let Some(state) = inspect.state {
            let health = state.health.as_ref().and_then(|h| h.status);
            match state.status {
                Some(ContainerStateStatusEnum::RUNNING)
                    if health.is_none() || health == Some(HealthStatusEnum::HEALTHY) =>
                {
                    return Ok(());
                }
                Some(ContainerStateStatusEnum::EXITED) | Some(ContainerStateStatusEnum::DEAD) => {
                    return Err(ServiceWorkloadError::Unhealthy {
                        container: container.to_string(),
                        timeout_secs: timeout.as_secs(),
                        reason: format!(
                            "the container stopped with exit code {}",
                            state.exit_code.unwrap_or(-1)
                        ),
                    });
                }
                status => last = format!("status {status:?}, health {health:?}"),
            }
        }
        tokio::time::sleep(HEALTH_POLL).await;
    }
    Err(ServiceWorkloadError::Unhealthy {
        container: container.to_string(),
        timeout_secs: timeout.as_secs(),
        reason: last,
    })
}

/// Create the plan's container through the Docker driver and start it.
/// `after_create` runs between the two (the agent joins the overlay there).
/// Returns the container id.
pub async fn create_and_start<F, Fut>(
    docker: &Docker,
    plan: &DatabaseWorkloadPlan,
    secrets: &BTreeMap<String, String>,
    node: &NodeAttachment,
    after_create: F,
) -> Result<String, ServiceWorkloadError>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let instance = database_instance(plan, SecretValues::Resolved(secrets), node)?;
    let handle = DockerDriver::new(docker.clone())
        .create_data_with_binds(
            &instance.spec,
            &instance.probes,
            &instance.extras,
            &instance.attach,
            Vec::new(),
        )
        .await
        .map_err(|e| ServiceWorkloadError::Docker {
            operation: "create",
            container: plan.container_name.clone(),
            reason: e.to_string(),
        })?;
    after_create(handle.id.clone()).await;
    docker
        .start_container(&handle.id, None::<StartContainerOptions>)
        .await
        .map_err(|e| ServiceWorkloadError::Docker {
            operation: "start",
            container: plan.container_name.clone(),
            reason: e.to_string(),
        })?;
    Ok(handle.id)
}

/// The storage a container created from `plan` uses, for the recreate
/// guard. `env` and `cmd` are the running container's, which the plan keeps.
pub fn replacement_storage(
    plan: &DatabaseWorkloadPlan,
    env: &[String],
    cmd: &[String],
) -> ReplacementStorage {
    ReplacementStorage {
        data_dir: plan.engine.managed_engine().data_dir(env, cmd),
        mounts: plan
            .spec
            .volumes
            .iter()
            .map(|v| PlannedMount {
                volume: v.volume.clone(),
                target: v.mount_path.clone(),
            })
            .collect(),
    }
}

/// Replace a running database container with one created from `plan`,
/// through the recreate guard: the replacement must mount the same data,
/// the original is kept aside until the replacement is healthy and shows
/// the same data fingerprint, and is put back otherwise.
pub async fn recreate_from_plan<F, Fut>(
    docker: &Docker,
    plan: &DatabaseWorkloadPlan,
    service_name: &str,
    existing: &ContainerInspectResponse,
    secrets: &BTreeMap<String, String>,
    node: &NodeAttachment,
    after_create: F,
) -> Result<(), ServiceWorkloadError>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let config = existing.config.as_ref();
    let env = config.and_then(|c| c.env.clone()).unwrap_or_default();
    let cmd = config.and_then(|c| c.cmd.clone()).unwrap_or_default();
    let replacement = replacement_storage(plan, &env, &cmd);
    let container = plan.container_name.clone();
    recreate_preserving_data(
        RecreateRequest {
            docker,
            engine: plan.engine.managed_engine(),
            service_name,
            container_name: &container,
            replacement,
            operation: "recreating it from its workload spec",
        },
        || async {
            let id = create_and_start(docker, plan, secrets, node, after_create).await?;
            wait_healthy(docker, &id, HEALTH_TIMEOUT).await
        },
    )
    .await
    .map_err(|e| ServiceWorkloadError::Recreate {
        service_id: plan.service_id,
        container: container.clone(),
        reason: e.to_string(),
    })?;
    info!(
        service_id = plan.service_id,
        container = %container,
        engine = %plan.engine,
        "Recreated database container from its admitted workload spec"
    );
    Ok(())
}
