// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test helpers shared by the `docker` submodules' tests.

use crate::docker::build_limits::BuildResourceLimits;
use crate::docker::DockerRuntime;
use bollard::Docker;
use std::sync::Arc;

pub(super) async fn create_test_docker_runtime() -> Result<DockerRuntime, Box<dyn std::error::Error>>
{
    let docker = Docker::connect_with_local_defaults()?;

    // Every test using this runtime deploys `alpine:latest`, but
    // `create_container` doesn't auto-pull a missing image -- it 404s.
    // These tests only ever worked by luck, relying on some other test
    // happening to pull it first; under parallel test execution (this
    // job doesn't set --test-threads=1) that ordering isn't guaranteed
    // and a test run first hits "No such image". Pull once, up front.
    use bollard::query_parameters::CreateImageOptions;
    use futures::StreamExt;
    let mut pull_stream = docker.create_image(
        Some(CreateImageOptions {
            from_image: Some("alpine".to_string()),
            tag: Some("latest".to_string()),
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(result) = pull_stream.next().await {
        result?;
    }

    Ok(DockerRuntime::new(
        Arc::new(docker),
        false,
        "test-network".to_string(),
    ))
}

/// `Docker::connect_with_local_defaults` only builds the client — it
/// doesn't dial the daemon — so this is safe to call without Docker
/// running, unlike `create_test_docker_runtime`'s async siblings.
pub(super) fn test_runtime() -> DockerRuntime {
    let docker = Docker::connect_with_local_defaults()
        .expect("bollard client construction (no connection made)");
    DockerRuntime::new(Arc::new(docker), false, "test-network".to_string())
}

/// A runtime whose client never connects (port 1 refuses), so the
/// diagnosis tests run on hosts without a daemon instead of skipping.
pub(super) fn runtime_for_diagnosis(use_buildkit: bool) -> DockerRuntime {
    let unreachable =
        Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
            .expect("client construction makes no connection");
    DockerRuntime::new(
        Arc::new(unreachable),
        use_buildkit,
        "test-network".to_string(),
    )
    .with_build_limits(
        2,
        Some(BuildResourceLimits {
            cpu_cores: 1.0,
            memory_mb: 512,
        }),
    )
}
