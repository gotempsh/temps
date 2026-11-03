// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stand-ins for the engines a worker is given at startup, for unit tests of
//! the handlers around them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bollard::Docker;
use temps_service_protocol::{
    AdmittedWorkload, ContainerSnapshot, HealthProbeResult, NodeServiceError, NodeServiceRuntime,
    ServiceConfig, ServiceType,
};

use crate::handlers::AgentState;
use crate::plugin_build_handler::{PluginBuildError, PluginContainerBuilder};

/// What [`StubRuntime::admit_workload`] answers.
#[derive(Clone, Copy, Default)]
pub(crate) enum Admission {
    Malformed,
    #[default]
    Refused,
}

/// Refuses every plan and fails every engine call.
#[derive(Default)]
pub(crate) struct StubRuntime {
    pub(crate) admission: Admission,
    /// The plans handed to admission.
    pub(crate) admitted: Mutex<Vec<serde_json::Value>>,
}

fn not_available(operation: &str) -> NodeServiceError {
    NodeServiceError::Engine {
        message: format!("{operation} failed in the test engine"),
    }
}

#[async_trait]
impl NodeServiceRuntime for StubRuntime {
    fn admit_workload(
        &self,
        plan: serde_json::Value,
    ) -> Result<Box<dyn AdmittedWorkload>, NodeServiceError> {
        self.admitted.lock().expect("admitted").push(plan);
        Err(match self.admission {
            Admission::Malformed => NodeServiceError::MalformedPlan {
                message: "Invalid database workload plan: missing field".to_string(),
            },
            Admission::Refused => NodeServiceError::Refused {
                container: "temps-db".to_string(),
                message: "refused: privileged".to_string(),
            },
        })
    }

    async fn container_snapshot(
        &self,
        _docker: &Docker,
        _container_name: &str,
    ) -> Result<ContainerSnapshot, NodeServiceError> {
        Err(not_available("snapshot"))
    }

    async fn provision_runtime_environment(
        &self,
        _name: String,
        _service_type: ServiceType,
        _config: ServiceConfig,
        _project_slug: &str,
        _environment_slug: &str,
        _docker: Arc<Docker>,
    ) -> Result<HashMap<String, String>, NodeServiceError> {
        Err(not_available("provision"))
    }

    async fn revoke_runtime_environment(
        &self,
        _name: String,
        _service_type: ServiceType,
        _config: ServiceConfig,
        _project_slug: &str,
        _environment_slug: &str,
        _docker: Arc<Docker>,
    ) -> Result<(), NodeServiceError> {
        Err(not_available("revoke"))
    }

    async fn probe_service_health(
        &self,
        _config: ServiceConfig,
        _docker: Arc<Docker>,
        _published_address: Option<std::net::IpAddr>,
    ) -> Result<HealthProbeResult, NodeServiceError> {
        Err(not_available("probe"))
    }
}

/// What [`StubPluginBuilder`] does with a build.
#[derive(Clone, Copy, Default)]
pub(crate) enum PluginBuild {
    #[default]
    RuntimeMissing,
    Fails,
    WritesBinary,
}

#[derive(Default)]
pub(crate) struct StubPluginBuilder {
    pub(crate) outcome: PluginBuild,
}

#[async_trait]
impl PluginContainerBuilder for StubPluginBuilder {
    async fn build_from_archive(
        &self,
        _archive: &Path,
        _target: &str,
        output: &Path,
    ) -> Result<(), PluginBuildError> {
        match self.outcome {
            PluginBuild::RuntimeMissing => Err(PluginBuildError::RuntimeMissing),
            PluginBuild::Fails => Err(PluginBuildError::Failed {
                reason: "bun exited 1".to_string(),
            }),
            PluginBuild::WritesBinary => {
                std::fs::write(output, b"compiled").expect("write the test binary");
                Ok(())
            }
        }
    }
}

pub(crate) fn service_runtime() -> Arc<dyn NodeServiceRuntime> {
    Arc::new(StubRuntime::default())
}

pub(crate) fn plugin_builder() -> Arc<dyn PluginContainerBuilder> {
    Arc::new(StubPluginBuilder::default())
}

/// Agent state around the given engines. The deployer and the Docker client
/// point at nothing; the paths under test must not reach them.
pub(crate) fn agent_state(
    service_runtime: Arc<dyn NodeServiceRuntime>,
    plugin_builder: Arc<dyn PluginContainerBuilder>,
) -> Arc<AgentState> {
    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(
            "http://127.0.0.1:1".into(),
            "no-io".into(),
            "no-io".into(),
        )
        .expect("build an unused remote deployer"),
    );
    Arc::new(AgentState {
        service_runtime,
        plugin_builder,
        container_deployer: remote.clone(),
        image_builder: remote,
        docker: Some(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("build an unused Docker client"),
        ),
        overlay_bridge_address: Default::default(),
        overlay_peers: Default::default(),
        platform: Default::default(),
        host_bind_address: Arc::new(std::sync::RwLock::new("127.0.0.1".into())),
    })
}
