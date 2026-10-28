// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a Docker Compose stack runs: the control plane's own Docker daemon
//! or a worker node's agent.
//!
//! [`DeployComposeJob`](super::DeployComposeJob) keeps one ordering for both
//! (prepare while the old stack serves, tear it down, start the new one,
//! retain or remove a failed candidate) and routes each step here. On the
//! control plane a step is a direct [`ComposeExecutor`] call; on a worker it
//! is the matching agent request (see [`temps_deployer::compose_protocol`]),
//! which runs the same executor there.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use temps_deployer::compose::{
    ComposeDeployFailure, ComposeDeployRequest, ComposeError, ComposeExecutor,
    ComposeServiceResult, PreparedComposeDeploy,
};
use temps_deployer::compose_protocol::{
    ComposePrepareSpec, ComposeRemoveVerifiedRequest, ComposeStackRequest,
};
use temps_deployer::remote::RemoteNodeDeployer;
use temps_entities::compose_security::ComposeSecurityPolicy;

/// The host a Compose stack is deployed to.
#[derive(Clone)]
pub enum ComposeStackHost {
    /// The control plane's own Docker daemon.
    ControlPlane,
    /// A worker node, reached through its agent.
    Worker {
        node_id: i32,
        node_name: String,
        deployer: Arc<RemoteNodeDeployer>,
        /// The project's Compose security policy, enforced by the worker.
        policy: ComposeSecurityPolicy,
    },
}

impl std::fmt::Debug for ComposeStackHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// A generation that has been built and pulled on its host.
pub enum PreparedStack {
    /// Prepared by the control plane's executor; carries its local paths.
    Local(PreparedComposeDeploy),
    /// Prepared on a worker, which holds the paths under the generation name.
    Remote,
}

impl ComposeStackHost {
    /// The `deployment_containers.node_id` for containers on this host.
    pub fn node_id(&self) -> Option<i32> {
        match self {
            Self::ControlPlane => None,
            Self::Worker { node_id, .. } => Some(*node_id),
        }
    }

    /// The host, for deployment logs and errors.
    pub fn describe(&self) -> String {
        match self {
            Self::ControlPlane => "the control plane".to_string(),
            Self::Worker {
                node_id, node_name, ..
            } => format!("worker node '{node_name}' (id {node_id})"),
        }
    }

    pub fn is_control_plane(&self) -> bool {
        matches!(self, Self::ControlPlane)
    }

    pub async fn prepare(
        &self,
        executor: &ComposeExecutor,
        request: &ComposeDeployRequest,
        generation: &str,
    ) -> Result<PreparedStack, ComposeError> {
        match self {
            Self::ControlPlane => executor
                .prepare_and_pull_for_generation(request, generation)
                .await
                .map(PreparedStack::Local),
            Self::Worker {
                deployer, policy, ..
            } => {
                let spec = ComposePrepareSpec::from_request(request, generation, policy.clone());
                deployer
                    .compose_prepare(
                        &spec,
                        request.repo_dir.as_deref(),
                        request.output_sink.as_ref(),
                    )
                    .await
                    .map(|()| PreparedStack::Remote)
            }
        }
    }

    pub async fn deploy(
        &self,
        executor: &ComposeExecutor,
        prepared: PreparedStack,
        request: &ComposeDeployRequest,
        generation: &str,
    ) -> Result<Vec<ComposeServiceResult>, Box<ComposeDeployFailure>> {
        match (self, prepared) {
            (Self::ControlPlane, PreparedStack::Local(prepared)) => {
                executor.deploy_prepared(prepared, request).await
            }
            (Self::Worker { deployer, .. }, PreparedStack::Remote) => {
                deployer
                    .compose_deploy(
                        &request.project_name,
                        generation,
                        request.output_sink.as_ref(),
                    )
                    .await
            }
            (host, _) => Err(Box::new(ComposeDeployFailure {
                error: ComposeError::CommandFailed {
                    project: request.project_name.clone(),
                    reason: format!(
                        "generation '{generation}' was not prepared on {}",
                        host.describe()
                    ),
                },
                containers: Vec::new(),
                cleanup_containers: Vec::new(),
                retention_error: None,
            })),
        }
    }

    /// `docker compose down`, keeping volumes. `generation` names the
    /// candidate whose files a worker should run it from, when there is one.
    #[allow(clippy::too_many_arguments)]
    pub async fn teardown(
        &self,
        executor: &ComposeExecutor,
        project_name: &str,
        repo_dir: Option<&Path>,
        compose_path: Option<&str>,
        environment_vars: &HashMap<String, String>,
        remove_secrets: bool,
        generation: Option<&str>,
    ) -> Result<(), ComposeError> {
        match self {
            Self::ControlPlane => {
                executor
                    .teardown_at(
                        project_name,
                        repo_dir,
                        compose_path,
                        environment_vars,
                        remove_secrets,
                    )
                    .await
            }
            Self::Worker { deployer, .. } => {
                deployer
                    .compose_teardown(&ComposeStackRequest {
                        project_name: project_name.to_string(),
                        generation: generation.map(str::to_string),
                        compose_path: compose_path.map(str::to_string),
                        remove_secrets,
                    })
                    .await
            }
        }
    }

    pub async fn stop(
        &self,
        executor: &ComposeExecutor,
        project_name: &str,
        repo_dir: Option<&Path>,
        compose_path: Option<&str>,
        generation: Option<&str>,
    ) -> Result<(), ComposeError> {
        match self {
            Self::ControlPlane => executor.stop_at(project_name, repo_dir, compose_path).await,
            Self::Worker { deployer, .. } => {
                deployer
                    .compose_stop(&ComposeStackRequest {
                        project_name: project_name.to_string(),
                        generation: generation.map(str::to_string),
                        compose_path: compose_path.map(str::to_string),
                        remove_secrets: false,
                    })
                    .await
            }
        }
    }

    /// Remove a candidate generation that will not start. On a worker this
    /// also forgets the prepared generation and its checkout.
    pub async fn discard_generation(
        &self,
        executor: &ComposeExecutor,
        project_name: &str,
        generation: &str,
    ) -> Result<(), ComposeError> {
        match self {
            Self::ControlPlane => {
                executor
                    .cleanup_secret_generation(project_name, generation)
                    .await
            }
            Self::Worker { deployer, .. } => {
                deployer
                    .compose_discard_generation(project_name, generation)
                    .await
            }
        }
    }

    pub async fn prune_secret_generations(
        &self,
        executor: &ComposeExecutor,
        project_name: &str,
        keep_generation: Option<&str>,
    ) -> Result<(), ComposeError> {
        match self {
            Self::ControlPlane => {
                executor
                    .prune_secret_generations(project_name, keep_generation)
                    .await
            }
            Self::Worker { deployer, .. } => {
                deployer
                    .compose_prune_secret_generations(project_name, keep_generation)
                    .await
            }
        }
    }

    pub async fn remove_verified_containers(
        &self,
        executor: &ComposeExecutor,
        containers: &[ComposeServiceResult],
        expected_labels: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        match self {
            Self::ControlPlane => {
                executor
                    .remove_verified_containers(containers, expected_labels)
                    .await
            }
            Self::Worker { deployer, .. } => {
                deployer
                    .compose_remove_verified_containers(&ComposeRemoveVerifiedRequest {
                        containers: containers.to_vec(),
                        expected_labels: expected_labels.clone(),
                    })
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker() -> ComposeStackHost {
        ComposeStackHost::Worker {
            node_id: 7,
            node_name: "worker-a".to_string(),
            deployer: Arc::new(
                RemoteNodeDeployer::new(
                    "http://127.0.0.1:9".to_string(),
                    "token".to_string(),
                    "worker-a".to_string(),
                )
                .unwrap(),
            ),
            policy: ComposeSecurityPolicy::default(),
        }
    }

    #[test]
    fn hosts_name_themselves_and_their_container_node() {
        assert_eq!(ComposeStackHost::ControlPlane.node_id(), None);
        assert_eq!(
            ComposeStackHost::ControlPlane.describe(),
            "the control plane"
        );
        assert_eq!(worker().node_id(), Some(7));
        assert_eq!(worker().describe(), "worker node 'worker-a' (id 7)");
        assert!(!worker().is_control_plane());
    }

    /// A generation prepared on one host is never started on another.
    #[tokio::test]
    async fn a_generation_cannot_start_on_a_host_that_did_not_prepare_it() {
        let executor = ComposeExecutor::new_with_handle(
            Arc::new(temps_core::DockerHandle::disabled(
                temps_core::PROFILE_CONTROL_PLANE,
                temps_core::CONTROL_PLANE_DOCKER_REASON,
            )),
            std::env::temp_dir(),
        );
        let request = ComposeDeployRequest {
            project_name: "temps-1-2".into(),
            compose_content: "services: {}\n".into(),
            env_content: None,
            work_dir: std::env::temp_dir(),
            compose_path: None,
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: Vec::new(),
            ready_timeout: None,
            routed_services: Vec::new(),
            output_sink: None,
        };
        let failure = ComposeStackHost::ControlPlane
            .deploy(&executor, PreparedStack::Remote, &request, "deployment-1-a")
            .await
            .unwrap_err();
        let message = failure.error.to_string();
        assert!(message.contains("deployment-1-a"), "{message}");
        assert!(message.contains("the control plane"), "{message}");
    }
}
