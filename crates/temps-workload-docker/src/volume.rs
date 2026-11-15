// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Docker [`VolumeProvider`] (ADR-055 §12): a volume's backing is a
//! Docker named volume, labelled with the volume it belongs to so `docker
//! volume ls --filter label=sh.temps.volume_id` finds it.
//!
//! Both operations are idempotent: creating an existing volume returns it,
//! removing an absent one is success.

use std::collections::HashMap;

use async_trait::async_trait;
use bollard::models::VolumeCreateRequest;
use bollard::Docker;
use temps_workload::VolumeKinds;
use temps_workload_volumes::{Backing, Volume, VolumeError, VolumeProvider};

/// Label carrying the volume's row id.
pub const VOLUME_ID_LABEL: &str = "sh.temps.volume_id";
/// Label carrying the volume's resource name.
pub const VOLUME_NAME_LABEL: &str = "sh.temps.volume_name";

pub struct DockerVolumeProvider {
    docker: Docker,
}

impl DockerVolumeProvider {
    pub fn new(docker: Docker) -> Self {
        Self { docker }
    }
}

/// The Docker volume name of `volume`, or why it has none.
fn docker_name<'a>(volume: &'a Volume, operation: &'static str) -> Result<&'a str, VolumeError> {
    match &volume.backing {
        Backing::Named { name } => Ok(name),
        other => Err(VolumeError::Provider {
            volume_id: volume.id,
            name: volume.name.clone(),
            operation,
            reason: format!(
                "the docker provider stores named volumes only, not {}",
                other.kind()
            ),
        }),
    }
}

/// The create request for `volume`: its Docker name and ownership labels.
pub fn volume_create_request(volume: &Volume, docker_name: &str) -> VolumeCreateRequest {
    VolumeCreateRequest {
        name: Some(docker_name.to_string()),
        labels: Some(HashMap::from([
            (VOLUME_ID_LABEL.to_string(), volume.id.to_string()),
            (VOLUME_NAME_LABEL.to_string(), volume.name.clone()),
            (
                temps_workload::labels::KIND.to_string(),
                volume.owner.kind.as_str().to_string(),
            ),
            (
                temps_workload::labels::OWNER_ID.to_string(),
                volume.owner.id.to_string(),
            ),
        ])),
        ..Default::default()
    }
}

#[async_trait]
impl VolumeProvider for DockerVolumeProvider {
    fn name(&self) -> &str {
        "docker"
    }

    fn kinds(&self) -> VolumeKinds {
        VolumeKinds {
            named: true,
            block_image: false,
            preopened_dir: false,
        }
    }

    async fn ensure(&self, volume: &Volume) -> Result<(), VolumeError> {
        let name = docker_name(volume, "create")?;
        // Docker returns the existing volume for an existing name.
        self.docker
            .create_volume(volume_create_request(volume, name))
            .await
            .map_err(|e| VolumeError::Provider {
                volume_id: volume.id,
                name: volume.name.clone(),
                operation: "create",
                reason: format!("docker volume '{name}': {e}"),
            })?;
        tracing::debug!(volume_id = volume.id, docker_volume = %name, "volume backing ensured");
        Ok(())
    }

    async fn delete(&self, volume: &Volume) -> Result<(), VolumeError> {
        let name = docker_name(volume, "delete")?;
        match self
            .docker
            .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
            .await
        {
            Ok(()) => {
                tracing::info!(volume_id = volume.id, docker_volume = %name, "volume backing removed");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(VolumeError::Provider {
                volume_id: volume.id,
                name: volume.name.clone(),
                operation: "delete",
                reason: format!("docker volume '{name}': {e}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::{WorkloadKind, WorkloadRef};

    fn volume(backing: Backing) -> Volume {
        Volume {
            id: 9,
            name: "data".into(),
            owner: WorkloadRef::new(WorkloadKind::Deployment, 42),
            backing,
            node_id: Some(0),
            size_mb: None,
            retain_on_delete: false,
        }
    }

    #[test]
    fn the_create_request_names_and_labels_the_volume() {
        let v = volume(Backing::Named {
            name: "temps-vol-environment-3-data".into(),
        });
        let request = volume_create_request(&v, "temps-vol-environment-3-data");
        assert_eq!(
            request.name.as_deref(),
            Some("temps-vol-environment-3-data")
        );
        let labels = request.labels.expect("labels");
        assert_eq!(labels.get(VOLUME_ID_LABEL).map(String::as_str), Some("9"));
        assert_eq!(
            labels.get(VOLUME_NAME_LABEL).map(String::as_str),
            Some("data")
        );
    }

    #[test]
    fn a_non_named_backing_is_refused_with_the_volume() {
        let v = volume(Backing::BlockImage {
            path: "/var/lib/temps/vol.img".into(),
        });
        let error = docker_name(&v, "create").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("volume 9 ('data')"), "{text}");
        assert!(text.contains("block_image"), "{text}");
    }

    #[tokio::test]
    async fn ensure_and_delete_are_idempotent_against_docker() {
        let Ok(docker) = Docker::connect_with_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let name = format!("temps-vol-test-p9-{}", std::process::id());
        let provider = DockerVolumeProvider::new(docker.clone());
        let v = volume(Backing::Named { name: name.clone() });
        provider.ensure(&v).await.expect("created");
        provider.ensure(&v).await.expect("created again");
        let inspected = docker.inspect_volume(&name).await.expect("exists");
        assert_eq!(
            inspected.labels.get(VOLUME_ID_LABEL).map(String::as_str),
            Some("9")
        );
        provider.delete(&v).await.expect("removed");
        provider
            .delete(&v)
            .await
            .expect("removing an absent volume is fine");
        assert!(docker.inspect_volume(&name).await.is_err());
    }
}
