// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker scaffolding for the engines' end-to-end import tests: an isolated
//! network, containers on it, exec, and cleanup that runs even when an
//! assertion fails.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, InspectContainerOptions, RemoveContainerOptions,
    StartContainerOptions,
};
use bollard::Docker;

use crate::externalsvc::exec_util::run_exec;

/// Containers and a network created for one test.
pub(crate) struct TestDocker {
    pub docker: Docker,
    pub network: String,
    containers: Vec<String>,
    suffix: String,
}

/// A container started by [`TestDocker::run`].
pub(crate) struct TestContainer {
    pub name: String,
    /// Host port the container's `publish` port is bound to on 127.0.0.1.
    pub host_port: Option<u16>,
}

impl TestDocker {
    /// `None` when Docker is unavailable: the caller skips the test.
    pub async fn connect() -> Option<Self> {
        let docker = Docker::connect_with_local_defaults().ok()?;
        docker.ping().await.ok()?;
        let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let network = format!("temps-import-test-{suffix}");
        docker
            .create_network(bollard::models::NetworkCreateRequest {
                name: network.clone(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Self {
            docker,
            network,
            containers: Vec::new(),
            suffix,
        })
    }

    /// Remove everything this test created, then re-raise the scenario's
    /// assertion failure, if any. Call with the result of
    /// `AssertUnwindSafe(scenario(&mut docker)).catch_unwind().await`.
    pub async fn finish(mut self, result: std::thread::Result<()>) {
        self.cleanup().await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    /// Start `image` on the test network.
    pub async fn run(
        &mut self,
        prefix: &str,
        image: &str,
        env: Vec<String>,
        publish: Option<&str>,
    ) -> TestContainer {
        let name = format!("{prefix}-{}", self.suffix);
        let port_bindings = publish.map(|port| crate::utils::local_port_binding(port, ""));
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    env: Some(env),
                    exposed_ports: publish.map(|port| vec![port.to_string()]),
                    host_config: Some(HostConfig {
                        network_mode: Some(self.network.clone()),
                        port_bindings,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("create {name}: {e}"));
        self.containers.push(name.clone());
        self.docker
            .start_container(&name, None::<StartContainerOptions>)
            .await
            .unwrap_or_else(|e| panic!("start {name}: {e}"));

        let host_port = match publish {
            None => None,
            Some(port) => {
                let inspect = self
                    .docker
                    .inspect_container(&name, None::<InspectContainerOptions>)
                    .await
                    .unwrap_or_else(|e| panic!("inspect {name}: {e}"));
                inspect
                    .network_settings
                    .and_then(|s| s.ports)
                    .and_then(|ports| ports.get(port).cloned().flatten())
                    .and_then(|bindings| bindings.first().cloned())
                    .and_then(|binding| binding.host_port)
                    .and_then(|p| p.parse().ok())
            }
        };
        TestContainer { name, host_port }
    }

    /// Run a shell command in `container`; returns (succeeded, output).
    pub async fn sh(&self, container: &str, script: &str, env: Vec<String>) -> (bool, String) {
        match run_exec(
            &self.docker,
            container,
            vec!["sh".to_string(), "-c".to_string(), script.to_string()],
            Some(env),
            Duration::from_secs(120),
        )
        .await
        {
            Ok(result) => (true, result.output),
            Err(e) => (false, e.to_string()),
        }
    }

    /// Repeat `script` in `container` until it succeeds or `timeout` passes.
    pub async fn wait_for(
        &self,
        container: &str,
        script: &str,
        env: Vec<String>,
        timeout: Duration,
    ) {
        let started = Instant::now();
        loop {
            if self.sh(container, script, env.clone()).await.0 {
                return;
            }
            assert!(
                started.elapsed() < timeout,
                "{container} not ready after {timeout:?}: {script}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn cleanup(&mut self) {
        for name in self.containers.drain(..) {
            let _ = self
                .docker
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
        let _ = self.docker.remove_network(&self.network).await;
    }
}

/// Labels of every data import helper still present for `run_id` — a test
/// asserts there are none once a run is over.
pub(crate) async fn leftover_helpers(docker: &Docker, run_id: i32) -> usize {
    let filters = HashMap::from([(
        "label".to_string(),
        vec![format!("{}={}", super::runner::RUN_LABEL, run_id)],
    )]);
    docker
        .list_containers(Some(
            bollard::query_parameters::ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await
        .map(|c| c.len())
        .unwrap_or(0)
}
