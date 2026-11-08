// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The engine operations a worker's agent runs, behind the interface the agent
//! depends on, so `temps-node-agent` does not have to build this crate.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bollard::Docker;
use temps_service_protocol::{
    AdmittedWorkload, AfterCreate, ContainerSnapshot, HealthProbeResult,
    MariadbBinlogArchiveRequest, MariadbBinlogArchiveResult, MariadbDumpBackupRequest,
    MariadbDumpBackupResult, MariadbPhysicalBackupRequest, MariadbPhysicalBackupResult,
    MariadbRestoreRequest, MariadbRestoreResult, MongodbBackupRequest, MongodbBackupResult,
    MongodbRestoreRequest, MongodbRestoreResult, NodeAttachment, NodeServiceError,
    NodeServiceRuntime, PostgresWalgArchiveRequest, PostgresWalgArchiveResult,
    PostgresWalgBackupRequest, PostgresWalgBackupResult, PostgresWalgCredentialsRequest,
    PostgresWalgCredentialsResult, PostgresWalgRestoreRequest, PostgresWalgRestoreResult,
    RecreateHook, RedisRdbBackupRequest, RedisRdbBackupResult, RedisRestoreRequest,
    RedisRestoreResult, RedisWalgBackupRequest, RedisWalgBackupResult, RemoteServiceBackupRequest,
    RemoteServiceBackupResponse, RemoteServiceRestoreRequest, RemoteServiceRestoreResponse,
    RemoteWalHealthResponse, ServiceConfig, ServiceType,
};

use crate::externalsvc;
use crate::workload::{runtime, DatabaseWorkloadPlan, ServiceWorkloadError};

/// [`NodeServiceRuntime`] over this crate's engines.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProvidersNodeRuntime;

fn engine_error(error: impl std::fmt::Display) -> NodeServiceError {
    NodeServiceError::Engine {
        message: error.to_string(),
    }
}

#[async_trait]
impl NodeServiceRuntime for ProvidersNodeRuntime {
    fn admit_workload(
        &self,
        plan: serde_json::Value,
    ) -> Result<Box<dyn AdmittedWorkload>, NodeServiceError> {
        let plan: DatabaseWorkloadPlan =
            serde_json::from_value(plan).map_err(|error| NodeServiceError::MalformedPlan {
                message: format!("Invalid database workload plan: {error}"),
            })?;
        // Admission runs on both sides: a request is never trusted to have
        // been admitted by its sender.
        let admitted = plan.readmit().map_err(|error| NodeServiceError::Refused {
            container: plan.container_name.clone(),
            message: error.to_string(),
        })?;
        Ok(Box::new(AdmittedPlan(admitted)))
    }

    async fn container_snapshot(
        &self,
        docker: &Docker,
        container_name: &str,
    ) -> Result<ContainerSnapshot, NodeServiceError> {
        runtime::snapshot(docker, container_name)
            .await
            .map_err(engine_error)
    }

    async fn provision_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<HashMap<String, String>, NodeServiceError> {
        externalsvc::provision_runtime_environment(
            name,
            service_type,
            config,
            project_slug,
            environment_slug,
            docker,
            published_address,
        )
        .await
        .map_err(engine_error)
    }

    async fn preview_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<HashMap<String, String>, NodeServiceError> {
        externalsvc::preview_runtime_environment(
            name,
            service_type,
            config,
            project_slug,
            environment_slug,
            docker,
            published_address,
        )
        .await
        .map_err(engine_error)
    }

    async fn revoke_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
    ) -> Result<(), NodeServiceError> {
        externalsvc::revoke_runtime_environment(
            name,
            service_type,
            config,
            project_slug,
            environment_slug,
            docker,
        )
        .await
        .map_err(engine_error)
    }

    async fn probe_service_health(
        &self,
        config: ServiceConfig,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<HealthProbeResult, NodeServiceError> {
        externalsvc::probe_service_health(config, docker, published_address)
            .await
            .map_err(engine_error)
    }

    async fn probe_postgres_wal(
        &self,
        config: ServiceConfig,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<RemoteWalHealthResponse, NodeServiceError> {
        externalsvc::probe_postgres_wal_health(config, docker, published_address)
            .await
            .map_err(engine_error)
    }

    async fn backup_service(
        &self,
        request: RemoteServiceBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<RemoteServiceBackupResponse, NodeServiceError> {
        externalsvc::worker_data::backup_service_on_node(request, docker, published_address)
            .await
            .map_err(engine_error)
    }

    async fn restore_service(
        &self,
        request: RemoteServiceRestoreRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<RemoteServiceRestoreResponse, NodeServiceError> {
        externalsvc::worker_data::restore_service_on_node(request, docker, published_address)
            .await
            .map_err(engine_error)
    }

    async fn postgres_walg_backup(
        &self,
        request: PostgresWalgBackupRequest,
        docker: Arc<Docker>,
        node: NodeAttachment,
        after_create: RecreateHook,
        published_address: Option<IpAddr>,
    ) -> Result<PostgresWalgBackupResult, NodeServiceError> {
        let recreate = externalsvc::postgres::NodeRecreate {
            service_id: request.service_id,
            node_id: request.node_id,
            attachment: node,
            after_create,
        };
        Ok(externalsvc::postgres_walg_backup::walg_base_backup_on_node(
            request,
            docker,
            recreate,
            published_address,
        )
        .await)
    }

    async fn postgres_walg_archive(
        &self,
        request: PostgresWalgArchiveRequest,
        docker: Arc<Docker>,
        node: NodeAttachment,
        after_create: RecreateHook,
        published_address: Option<IpAddr>,
    ) -> Result<PostgresWalgArchiveResult, NodeServiceError> {
        let recreate = externalsvc::postgres::NodeRecreate {
            service_id: request.service_id,
            node_id: request.node_id,
            attachment: node,
            after_create,
        };
        Ok(externalsvc::postgres_walg_backup::walg_archive_on_node(
            request,
            docker,
            recreate,
            published_address,
        )
        .await)
    }

    async fn postgres_walg_credentials(
        &self,
        request: PostgresWalgCredentialsRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<PostgresWalgCredentialsResult, NodeServiceError> {
        Ok(externalsvc::postgres_walg_backup::walg_credentials_on_node(
            request,
            docker,
            published_address,
        )
        .await)
    }

    async fn postgres_walg_restore(
        &self,
        request: PostgresWalgRestoreRequest,
        docker: Arc<Docker>,
        node: NodeAttachment,
        after_create: RecreateHook,
        published_address: Option<IpAddr>,
    ) -> Result<PostgresWalgRestoreResult, NodeServiceError> {
        let recreate = externalsvc::postgres::NodeRecreate {
            service_id: request.service_id,
            node_id: request.node_id,
            attachment: node,
            after_create,
        };
        Ok(externalsvc::postgres_walg_restore::walg_restore_on_node(
            request,
            docker,
            recreate,
            published_address,
        )
        .await)
    }

    async fn mongodb_backup(
        &self,
        request: MongodbBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MongodbBackupResult, NodeServiceError> {
        Ok(
            externalsvc::mongodb_backup::mongodb_backup_on_node(request, docker, published_address)
                .await,
        )
    }

    async fn mongodb_restore(
        &self,
        request: MongodbRestoreRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MongodbRestoreResult, NodeServiceError> {
        Ok(
            externalsvc::mongodb_backup::mongodb_restore_on_node(
                request,
                docker,
                published_address,
            )
            .await,
        )
    }

    async fn redis_walg_backup(
        &self,
        request: RedisWalgBackupRequest,
        docker: Arc<Docker>,
    ) -> Result<RedisWalgBackupResult, NodeServiceError> {
        Ok(externalsvc::redis_node::walg_backup_on_node(request, docker).await)
    }

    async fn redis_rdb_backup(
        &self,
        request: RedisRdbBackupRequest,
        docker: Arc<Docker>,
    ) -> Result<RedisRdbBackupResult, NodeServiceError> {
        Ok(externalsvc::redis_node::rdb_backup_on_node(request, docker).await)
    }

    async fn redis_restore(
        &self,
        request: RedisRestoreRequest,
        docker: Arc<Docker>,
    ) -> Result<RedisRestoreResult, NodeServiceError> {
        Ok(externalsvc::redis_node::restore_on_node(request, docker).await)
    }

    async fn mariadb_physical_backup(
        &self,
        request: MariadbPhysicalBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MariadbPhysicalBackupResult, NodeServiceError> {
        Ok(
            externalsvc::mariadb_node::physical_backup_on_node(request, docker, published_address)
                .await,
        )
    }

    async fn mariadb_dump_backup(
        &self,
        request: MariadbDumpBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MariadbDumpBackupResult, NodeServiceError> {
        Ok(
            externalsvc::mariadb_node::dump_backup_on_node(request, docker, published_address)
                .await,
        )
    }

    async fn mariadb_restore(
        &self,
        request: MariadbRestoreRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MariadbRestoreResult, NodeServiceError> {
        Ok(externalsvc::mariadb_node::restore_on_node(request, docker, published_address).await)
    }

    async fn mariadb_binlog_archive(
        &self,
        request: MariadbBinlogArchiveRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MariadbBinlogArchiveResult, NodeServiceError> {
        Ok(
            externalsvc::mariadb_node::binlog_archive_on_node(request, docker, published_address)
                .await,
        )
    }
}

struct AdmittedPlan(DatabaseWorkloadPlan);

#[async_trait]
impl AdmittedWorkload for AdmittedPlan {
    fn container_name(&self) -> &str {
        &self.0.container_name
    }

    fn engine(&self) -> String {
        self.0.engine.to_string()
    }

    fn volumes(&self) -> Vec<String> {
        self.0
            .spec
            .volumes
            .iter()
            .map(|mount| mount.volume.clone())
            .collect()
    }

    fn image(&self) -> Option<&str> {
        self.0.image()
    }

    fn host_port(&self) -> Option<u16> {
        self.0.host_port
    }

    async fn create(
        &self,
        docker: &Docker,
        secrets: &BTreeMap<String, String>,
        node: &NodeAttachment,
        after_create: AfterCreate<'_>,
    ) -> Result<(), NodeServiceError> {
        runtime::create_and_start(docker, &self.0, secrets, node, after_create)
            .await
            .map(|_| ())
            .map_err(engine_error)
    }

    async fn replace(
        &self,
        docker: &Docker,
        service_name: &str,
        secrets: &BTreeMap<String, String>,
        node: &NodeAttachment,
        after_create: AfterCreate<'_>,
    ) -> Result<(), NodeServiceError> {
        let plan = &self.0;
        match runtime::snapshot(docker, &plan.container_name).await {
            Ok(ContainerSnapshot {
                found: true,
                inspect: Some(existing),
                ..
            }) => {
                runtime::recreate_from_plan(
                    docker,
                    plan,
                    service_name,
                    &existing,
                    secrets,
                    node,
                    after_create,
                )
                .await
            }
            Ok(_) => Err(ServiceWorkloadError::ContainerMissing {
                service_id: plan.service_id,
                container: plan.container_name.clone(),
                node: "this node".to_string(),
            }),
            Err(error) => Err(error),
        }
        .map_err(engine_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_service_client::{RemotePortMapping, RemoteServiceCreateParams};
    use crate::workload::{DatabaseEngine, DatabaseIdentity, DatabaseProcess};
    use std::collections::HashMap as Map;

    /// The node's published address has to reach the provider's probe, which
    /// checks the caller's host first and only then retargets it. Against an
    /// unreachable daemon, a loopback host fails at the container lookup and a
    /// caller-supplied address is refused before it.
    #[tokio::test]
    async fn probe_passes_the_published_address_to_the_provider() {
        let docker = Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("an HTTP Docker client is built without connecting"),
        );
        let published = "10.42.0.22".parse().ok();
        let config = |host: &str| ServiceConfig {
            name: "orders-cache".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({"host": host, "port": "6379"}),
        };

        let loopback = ProvidersNodeRuntime
            .probe_service_health(config("localhost"), docker.clone(), published)
            .await
            .expect_err("the daemon is unreachable");
        let refused = ProvidersNodeRuntime
            .probe_service_health(config("10.42.0.22"), docker, published)
            .await
            .expect_err("a caller-supplied address is refused");

        assert!(!loopback.to_string().contains("localhost"), "{loopback}");
        assert!(refused.to_string().contains("10.42.0.22"), "{refused}");
    }

    fn admitted_redis_plan() -> DatabaseWorkloadPlan {
        let params = RemoteServiceCreateParams {
            name: "redis-orders".into(),
            service_type: "redis".into(),
            image: "gotempsh/redis-walg:8-bookworm".into(),
            environment: Map::new(),
            port_mappings: vec![RemotePortMapping {
                host_port: 16379,
                container_port: 6379,
            }],
            volumes: Map::from([("redis-orders_data".into(), "/data".into())]),
            network: Some("temps-app-network".into()),
            command: Some(vec![
                "redis-server".into(),
                "--requirepass".into(),
                "seeded-secret-1".into(),
            ]),
            resource_limits: None,
        };
        let process = DatabaseProcess::from_remote_params(DatabaseEngine::Redis, &params);
        DatabaseWorkloadPlan::admit(
            &DatabaseIdentity {
                service_id: 4,
                service_name: "orders".into(),
                engine: DatabaseEngine::Redis,
                node_id: Some(3),
                container_name: "redis-orders".into(),
            },
            &process,
        )
        .expect("admit the plan")
    }

    #[test]
    fn a_control_plane_plan_passes_admission_on_the_worker() {
        let plan = serde_json::to_value(admitted_redis_plan()).expect("serialize the plan");
        let admitted = ProvidersNodeRuntime
            .admit_workload(plan)
            .map_err(|error| error.to_string())
            .expect("admit the plan again");
        assert_eq!(admitted.container_name(), "redis-orders");
        assert_eq!(admitted.engine(), "redis");
        assert_eq!(admitted.volumes(), vec!["redis-orders_data".to_string()]);
        assert_eq!(admitted.image(), Some("gotempsh/redis-walg:8-bookworm"));
    }

    /// Admission runs on the worker too: a plan edited after the control plane
    /// admitted it is refused, here one claiming another service's spec.
    #[test]
    fn a_plan_edited_in_transit_is_refused_on_the_worker() {
        let mut plan = serde_json::to_value(admitted_redis_plan()).expect("serialize the plan");
        plan["service_id"] = serde_json::json!(999);
        match ProvidersNodeRuntime.admit_workload(plan) {
            Err(NodeServiceError::Refused { container, message }) => {
                assert_eq!(container, "redis-orders");
                assert!(message.contains("999"), "{message}");
            }
            Err(other) => panic!("expected a refusal, got {other}"),
            Ok(_) => panic!("a tampered plan must not be admitted"),
        }
    }

    #[test]
    fn something_that_is_not_a_plan_is_malformed() {
        let result = ProvidersNodeRuntime.admit_workload(serde_json::json!({ "plan": true }));
        assert!(matches!(
            result,
            Err(NodeServiceError::MalformedPlan { .. })
        ));
    }
}
