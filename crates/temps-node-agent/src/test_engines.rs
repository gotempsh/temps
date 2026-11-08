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
    RemoteWalHealthResponse, ServiceConfig, ServiceType,
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
    /// The published address each runtime provisioning was given.
    pub(crate) provisioned_through: Mutex<Vec<Option<std::net::IpAddr>>>,
    /// The WAL-G base backups asked for, with the attachment a recreate
    /// would use and the published address.
    pub(crate) walg_backups: Mutex<
        Vec<(
            temps_service_protocol::PostgresWalgBackupRequest,
            temps_service_protocol::NodeAttachment,
            Option<std::net::IpAddr>,
        )>,
    >,
    /// The WAL-G archive re-points asked for, with the attachment.
    pub(crate) walg_archives: Mutex<
        Vec<(
            temps_service_protocol::PostgresWalgArchiveRequest,
            temps_service_protocol::NodeAttachment,
        )>,
    >,
    /// The MongoDB backups asked for, with the published address.
    pub(crate) mongodb_backups: Mutex<
        Vec<(
            temps_service_protocol::MongodbBackupRequest,
            Option<std::net::IpAddr>,
        )>,
    >,
    /// The MongoDB restores asked for, with the published address.
    pub(crate) mongodb_restores: Mutex<
        Vec<(
            temps_service_protocol::MongodbRestoreRequest,
            Option<std::net::IpAddr>,
        )>,
    >,
    /// The WAL-G restores asked for, with the attachment and published
    /// address.
    pub(crate) walg_restores: Mutex<
        Vec<(
            temps_service_protocol::PostgresWalgRestoreRequest,
            temps_service_protocol::NodeAttachment,
            Option<std::net::IpAddr>,
        )>,
    >,
    /// The WAL-G archiving credential refreshes asked for.
    pub(crate) walg_credentials: Mutex<Vec<temps_service_protocol::PostgresWalgCredentialsRequest>>,
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
        published_address: Option<std::net::IpAddr>,
    ) -> Result<HashMap<String, String>, NodeServiceError> {
        self.provisioned_through
            .lock()
            .expect("provisioned_through")
            .push(published_address);
        Err(not_available("provision"))
    }

    async fn preview_runtime_environment(
        &self,
        _name: String,
        _service_type: ServiceType,
        _config: ServiceConfig,
        _project_slug: &str,
        _environment_slug: &str,
        _docker: Arc<Docker>,
        published_address: Option<std::net::IpAddr>,
    ) -> Result<HashMap<String, String>, NodeServiceError> {
        self.provisioned_through
            .lock()
            .expect("provisioned_through")
            .push(published_address);
        Err(not_available("preview"))
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

    async fn probe_postgres_wal(
        &self,
        _config: ServiceConfig,
        _docker: Arc<Docker>,
        _published_address: Option<std::net::IpAddr>,
    ) -> Result<RemoteWalHealthResponse, NodeServiceError> {
        Err(not_available("WAL probe"))
    }

    async fn backup_service(
        &self,
        _request: temps_service_protocol::RemoteServiceBackupRequest,
        _docker: Arc<Docker>,
        _published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::RemoteServiceBackupResponse, NodeServiceError> {
        Err(not_available("backup_service"))
    }

    async fn restore_service(
        &self,
        _request: temps_service_protocol::RemoteServiceRestoreRequest,
        _docker: Arc<Docker>,
        _published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::RemoteServiceRestoreResponse, NodeServiceError> {
        Err(not_available("restore_service"))
    }

    async fn postgres_walg_backup(
        &self,
        request: temps_service_protocol::PostgresWalgBackupRequest,
        _docker: Arc<Docker>,
        node: temps_service_protocol::NodeAttachment,
        _after_create: temps_service_protocol::RecreateHook,
        published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::PostgresWalgBackupResult, NodeServiceError> {
        self.walg_backups
            .lock()
            .expect("walg_backups")
            .push((request, node, published_address));
        Err(not_available("postgres_walg_backup"))
    }

    async fn postgres_walg_archive(
        &self,
        request: temps_service_protocol::PostgresWalgArchiveRequest,
        _docker: Arc<Docker>,
        node: temps_service_protocol::NodeAttachment,
        _after_create: temps_service_protocol::RecreateHook,
        _published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::PostgresWalgArchiveResult, NodeServiceError> {
        self.walg_archives
            .lock()
            .expect("walg_archives")
            .push((request, node));
        Err(not_available("postgres_walg_archive"))
    }

    async fn postgres_walg_credentials(
        &self,
        request: temps_service_protocol::PostgresWalgCredentialsRequest,
        _docker: Arc<Docker>,
        _published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::PostgresWalgCredentialsResult, NodeServiceError> {
        self.walg_credentials
            .lock()
            .expect("walg_credentials")
            .push(request);
        Ok(temps_service_protocol::PostgresWalgCredentialsResult::Refreshed)
    }

    async fn postgres_walg_restore(
        &self,
        request: temps_service_protocol::PostgresWalgRestoreRequest,
        _docker: Arc<Docker>,
        node: temps_service_protocol::NodeAttachment,
        _after_create: temps_service_protocol::RecreateHook,
        published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::PostgresWalgRestoreResult, NodeServiceError> {
        self.walg_restores
            .lock()
            .expect("walg_restores")
            .push((request, node, published_address));
        Err(not_available("postgres_walg_restore"))
    }

    async fn mongodb_backup(
        &self,
        request: temps_service_protocol::MongodbBackupRequest,
        _docker: Arc<Docker>,
        published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::MongodbBackupResult, NodeServiceError> {
        self.mongodb_backups
            .lock()
            .expect("mongodb_backups")
            .push((request, published_address));
        Err(not_available("mongodb_backup"))
    }

    async fn mongodb_restore(
        &self,
        request: temps_service_protocol::MongodbRestoreRequest,
        _docker: Arc<Docker>,
        published_address: Option<std::net::IpAddr>,
    ) -> Result<temps_service_protocol::MongodbRestoreResult, NodeServiceError> {
        self.mongodb_restores
            .lock()
            .expect("mongodb_restores")
            .push((request, published_address));
        Err(not_available("mongodb_restore"))
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
    agent_state_with_docker(
        service_runtime,
        plugin_builder,
        Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
            .expect("build an unused Docker client"),
    )
}

/// Agent state whose Docker client is `docker` (a stub daemon, for a test
/// that drives the service paths). The deployer points at nothing.
pub(crate) fn agent_state_with_docker(
    service_runtime: Arc<dyn NodeServiceRuntime>,
    plugin_builder: Arc<dyn PluginContainerBuilder>,
    docker: Docker,
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
        docker: Some(docker),
        overlay_bridge_address: Default::default(),
        overlay_peers: Default::default(),
        platform: Default::default(),
        host_bind_address: Arc::new(std::sync::RwLock::new("127.0.0.1".into())),
    })
}
