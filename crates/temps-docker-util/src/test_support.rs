// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared by the Docker-backed tests: they skip when no daemon answers, and
//! name and remove only their own resources.

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptions, StartContainerOptions,
};
use bollard::Docker;

use crate::pull::{pull_image, PullSpec};

pub(crate) const TEST_IMAGE: &str = "busybox:1.36";

/// The local Docker client, or `None` (with a message) if no daemon answers.
pub(crate) async fn docker_or_skip(test: &str) -> Option<Docker> {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(error) => {
            println!("Docker client unavailable, skipping {test}: {error}");
            return None;
        }
    };
    if let Err(error) = docker.ping().await {
        println!("Docker daemon not responding, skipping {test}: {error}");
        return None;
    }
    Some(docker)
}

/// A name no other test or running instance uses.
pub(crate) fn unique_name(prefix: &str) -> String {
    format!(
        "temps-docker-util-test-{prefix}-{}",
        uuid::Uuid::new_v4().simple()
    )
}

/// Create and start a container that sleeps, for exec tests. Returns its name.
pub(crate) async fn start_sleeping_container(docker: &Docker, prefix: &str) -> String {
    pull_image(docker, PullSpec::new(TEST_IMAGE))
        .await
        .expect("pull the test image");
    let name = unique_name(prefix);
    docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(&name).build()),
            ContainerCreateBody {
                image: Some(TEST_IMAGE.to_string()),
                cmd: Some(vec!["sleep".to_string(), "300".to_string()]),
                ..Default::default()
            },
        )
        .await
        .expect("create the test container");
    docker
        .start_container(&name, None::<StartContainerOptions>)
        .await
        .expect("start the test container");
    name
}

pub(crate) async fn remove_container(docker: &Docker, name: &str) {
    let _ = docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                v: true,
                ..Default::default()
            }),
        )
        .await;
}
