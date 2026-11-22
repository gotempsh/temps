// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where this control plane connects to a standalone managed service.
//!
//! The rule itself is [`temps_core::managed_service_address`]. This module
//! supplies what the rule needs from the providers layer: the container a
//! service runs in and the port its engine listens on there, both derived
//! from the service's type and stored backend without a Docker handle.

use super::{
    service_execution_route, EngineSelection, ExternalServiceManager, ServiceExecutionRoute,
};
use crate::externalsvc::{ServiceConfig, ServiceType};
use temps_core::managed_service_address::{
    retarget_parameters, ContainerEndpoint, ContainerEndpointResolver, DialTargetSource, HostForm,
};
use temps_entities::external_services;

/// Topology of a service with one container and one stored address. A
/// cluster's stored host and port describe no listening member: its primary
/// is resolved through its monitor instead.
const STANDALONE_TOPOLOGY: &str = "standalone";

/// The container behind a standalone service called `service_name`: its name
/// (an imported container's own name wins over the derived one) and the port
/// its engine listens on inside it.
///
/// `None` when the parameters select no engine, or the engine's port is not a
/// TCP port: the caller then keeps the stored address.
pub(crate) fn standalone_container_endpoint(
    service_name: &str,
    service_type: ServiceType,
    parameters: &serde_json::Value,
) -> Option<ContainerEndpoint> {
    // The service id only labels a validation error, which is dropped here.
    let selection =
        EngineSelection::for_parameters(0, service_name, service_type, parameters).ok()?;
    let name = parameters
        .get("container_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| selection.container_name());
    let port = selection.internal_port().parse::<u16>().ok()?;
    Some(ContainerEndpoint { name, port })
}

/// How a host is embedded by the clients of `service_type`: every engine but
/// Postgres puts it in a URL authority.
fn host_form(service_type: ServiceType) -> HostForm {
    match service_type {
        ServiceType::Postgres => HostForm::Bare,
        _ => HostForm::UrlAuthority,
    }
}

/// [`ContainerEndpointResolver`] backed by the engines this build ships.
///
/// Handed to crates below the providers layer (the metrics scraper) that
/// connect to managed services but cannot name their containers themselves.
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineContainerEndpoints;

impl ContainerEndpointResolver for EngineContainerEndpoints {
    fn container_endpoint(
        &self,
        service_type: &str,
        service_name: &str,
        parameters: &serde_json::Value,
    ) -> Option<ContainerEndpoint> {
        let service_type = ServiceType::from_str(service_type).ok()?;
        standalone_container_endpoint(service_name, service_type, parameters)
    }
}

impl ExternalServiceManager {
    /// Point the decrypted configuration of a standalone service on this
    /// control plane's own Docker daemon at the address this process can
    /// connect to.
    ///
    /// For connections only: the result must not be stored, nor handed to an
    /// operation that creates or recreates the container. A cluster, and a
    /// service placed on a worker node, are left as stored: the first resolves
    /// its primary, the second is reached through its node's agent.
    pub(crate) fn retarget_local_standalone_dial(
        service: &external_services::Model,
        config: &mut ServiceConfig,
    ) -> DialTargetSource {
        let local = matches!(
            service_execution_route(service.node_id),
            ServiceExecutionRoute::Local
        );
        if !local || service.topology != STANDALONE_TOPOLOGY {
            return DialTargetSource::Stored;
        }
        let container =
            standalone_container_endpoint(&service.name, config.service_type, &config.parameters);
        retarget_parameters(
            temps_core::runtime::execution_environment_compatibility(),
            &mut config.parameters,
            None,
            container.as_ref(),
            host_form(config.service_type),
        )
    }
}

/// A managed container's address on the Temps network, as the Docker daemon
/// reported it, and the port its engine listens on there.
///
/// What a containerized control plane dials when a connection must not leave
/// this host: the address was not resolved from a name nor read from stored
/// parameters, so it can only be the service's own container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalContainerAddress {
    pub address: std::net::IpAddr,
    pub port: u16,
}

/// Why the address of a managed container could not be confirmed.
#[derive(Debug, thiserror::Error)]
pub enum LocalContainerAddressError {
    #[error(
        "Cannot look up container '{container}' of service {service_id}: Docker is unavailable \
         ({reason})"
    )]
    DockerUnavailable {
        service_id: i32,
        container: String,
        reason: String,
    },

    #[error("Failed to inspect container '{container}' of service {service_id}: {reason}")]
    InspectFailed {
        service_id: i32,
        container: String,
        reason: String,
    },

    #[error(
        "Container '{container}' of service {service_id} is not attached to the '{network}' \
         network, which this containerized Temps reaches managed services through. Restart the \
         service so Temps reattaches it"
    )]
    NotOnNetwork {
        service_id: i32,
        container: String,
        network: String,
    },
}

/// The container to look up for `service`, when this control plane reaches
/// it over the network it shares with it: the same decision as
/// [`ExternalServiceManager::retarget_local_standalone_dial`].
pub(crate) fn container_to_look_up(
    execution_environment: temps_core::ExecutionEnvironment,
    service: &external_services::Model,
    config: &ServiceConfig,
) -> Option<ContainerEndpoint> {
    let local = matches!(
        service_execution_route(service.node_id),
        ServiceExecutionRoute::Local
    );
    if !local || service.topology != STANDALONE_TOPOLOGY {
        return None;
    }
    let stored_host = config
        .parameters
        .get("host")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let stored_port = match config.parameters.get("port") {
        Some(serde_json::Value::String(port)) => port.trim().parse::<u16>().ok(),
        Some(serde_json::Value::Number(port)) => {
            port.as_u64().and_then(|port| u16::try_from(port).ok())
        }
        _ => None,
    }
    .unwrap_or(0);
    let container =
        standalone_container_endpoint(&service.name, config.service_type, &config.parameters)?;
    let target = temps_core::managed_service_address::control_plane_dial_target(
        execution_environment,
        stored_host,
        stored_port,
        None,
        Some(&container),
    );
    (target.source == DialTargetSource::ContainerNetwork).then_some(container)
}

/// The address an inspected container has on `network`.
pub(crate) fn address_on_network(
    info: &bollard::models::ContainerInspectResponse,
    network: &str,
) -> Option<std::net::IpAddr> {
    let endpoint = info
        .network_settings
        .as_ref()?
        .networks
        .as_ref()?
        .get(network)?;
    [&endpoint.ip_address, &endpoint.global_ipv6_address]
        .into_iter()
        .flatten()
        .find_map(|address| address.trim().parse().ok())
}

impl ExternalServiceManager {
    /// The daemon-confirmed address of `service`'s container on the Temps
    /// network, when this control plane runs in a container and reaches the
    /// service over that network; `None` when it dials the stored address
    /// (a native control plane, a worker, a cluster, an explicit host).
    pub async fn local_container_address(
        &self,
        service: &external_services::Model,
        config: &ServiceConfig,
    ) -> Result<Option<LocalContainerAddress>, LocalContainerAddressError> {
        let Some(container) = container_to_look_up(
            temps_core::runtime::execution_environment_compatibility(),
            service,
            config,
        ) else {
            return Ok(None);
        };
        let docker = self.docker.require().map_err(|error| {
            LocalContainerAddressError::DockerUnavailable {
                service_id: service.id,
                container: container.name.clone(),
                reason: error.to_string(),
            }
        })?;
        let info = docker
            .inspect_container(
                &container.name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|error| LocalContainerAddressError::InspectFailed {
                service_id: service.id,
                container: container.name.clone(),
                reason: error.to_string(),
            })?;
        let network = temps_core::NETWORK_NAME.as_str();
        let address = address_on_network(&info, network).ok_or_else(|| {
            LocalContainerAddressError::NotOnNetwork {
                service_id: service.id,
                container: container.name.clone(),
                network: network.to_string(),
            }
        })?;
        Ok(Some(LocalContainerAddress {
            address,
            port: container.port,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn service_row(node_id: Option<i32>, topology: &str) -> external_services::Model {
        let now = chrono::Utc::now();
        external_services::Model {
            id: 7,
            name: "orders".to_string(),
            service_type: "postgres".to_string(),
            topology: topology.to_string(),
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            node_id,
            version: None,
            slug: None,
            config: None,
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
            healthcheck_interval_secs: None,
            healthcheck_start_interval_secs: None,
            healthcheck_start_period_secs: None,
            healthcheck_timeout_secs: None,
            healthcheck_retries: None,
        }
    }

    fn postgres_config(parameters: serde_json::Value) -> ServiceConfig {
        ServiceConfig {
            name: "orders".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters,
        }
    }

    #[test]
    fn test_containerized_control_plane_looks_up_its_own_standalone_service() {
        let container = container_to_look_up(
            temps_core::ExecutionEnvironment::Docker,
            &service_row(None, "standalone"),
            &postgres_config(json!({"host": "localhost", "port": "6097"})),
        );

        assert_eq!(
            container,
            Some(ContainerEndpoint {
                name: "postgres-orders".to_string(),
                port: 5432,
            })
        );
    }

    #[test]
    fn test_no_lookup_where_the_stored_address_is_dialled() {
        let loopback = postgres_config(json!({"host": "localhost", "port": "6097"}));
        let docker = temps_core::ExecutionEnvironment::Docker;

        // A native control plane reaches the published port on its own host.
        assert_eq!(
            container_to_look_up(
                temps_core::ExecutionEnvironment::Host,
                &service_row(None, "standalone"),
                &loopback
            ),
            None
        );
        // An explicit host names a real address and is never replaced.
        assert_eq!(
            container_to_look_up(
                docker,
                &service_row(None, "standalone"),
                &postgres_config(json!({"host": "db.example.internal", "port": "5432"}))
            ),
            None
        );
        // A service on a worker is not on this daemon.
        assert_eq!(
            container_to_look_up(docker, &service_row(Some(3), "standalone"), &loopback),
            None
        );
        // A cluster resolves its primary through its monitor instead.
        assert_eq!(
            container_to_look_up(docker, &service_row(None, "cluster"), &loopback),
            None
        );
    }

    fn inspected(networks: serde_json::Value) -> bollard::models::ContainerInspectResponse {
        serde_json::from_value(json!({"NetworkSettings": {"Networks": networks}})).unwrap()
    }

    #[test]
    fn test_address_on_network_reads_the_daemon_reported_address() {
        let info = inspected(json!({
            "bridge": {"IPAddress": "172.17.0.4"},
            "temps-docker-workloads": {"IPAddress": "198.20.255.7"}
        }));

        assert_eq!(
            address_on_network(&info, "temps-docker-workloads"),
            Some("198.20.255.7".parse().unwrap())
        );
    }

    #[test]
    fn test_address_on_network_falls_back_to_ipv6_and_rejects_garbage() {
        let ipv6_only = inspected(json!({
            "temps-docker-workloads": {"IPAddress": "", "GlobalIPv6Address": "fd00::7"}
        }));
        assert_eq!(
            address_on_network(&ipv6_only, "temps-docker-workloads"),
            Some("fd00::7".parse().unwrap())
        );

        let garbage = inspected(json!({
            "temps-docker-workloads": {"IPAddress": "evil.example", "GlobalIPv6Address": ""}
        }));
        assert_eq!(address_on_network(&garbage, "temps-docker-workloads"), None);
    }

    #[test]
    fn test_address_on_network_is_none_off_the_network() {
        let info = inspected(json!({"bridge": {"IPAddress": "172.17.0.4"}}));

        assert_eq!(address_on_network(&info, "temps-docker-workloads"), None);
        assert_eq!(
            address_on_network(
                &bollard::models::ContainerInspectResponse::default(),
                "temps-docker-workloads"
            ),
            None
        );
    }

    #[test]
    fn test_container_endpoint_uses_the_engine_name_and_port() {
        let endpoint = standalone_container_endpoint(
            "orders",
            ServiceType::Postgres,
            &json!({"host": "localhost", "port": "6097"}),
        );

        assert_eq!(
            endpoint,
            Some(ContainerEndpoint {
                name: "postgres-orders".to_string(),
                port: 5432,
            })
        );
    }

    #[test]
    fn test_container_endpoint_prefers_an_imported_container_name() {
        let endpoint = standalone_container_endpoint(
            "cache",
            ServiceType::Redis,
            &json!({"port": "7043", "container_name": "legacy-redis-1"}),
        );

        assert_eq!(
            endpoint,
            Some(ContainerEndpoint {
                name: "legacy-redis-1".to_string(),
                port: 6379,
            })
        );
    }

    #[test]
    fn test_container_endpoint_ignores_a_blank_imported_name() {
        let endpoint = standalone_container_endpoint(
            "cache",
            ServiceType::Redis,
            &json!({"port": "7043", "container_name": "  "}),
        );

        assert_eq!(
            endpoint.map(|endpoint| endpoint.name),
            Some("redis-cache".to_string())
        );
    }

    #[test]
    fn test_resolver_answers_none_for_an_unknown_service_type() {
        let endpoint =
            EngineContainerEndpoints.container_endpoint("not-an-engine", "cache", &json!({}));

        assert_eq!(endpoint, None);
    }

    #[test]
    fn test_resolver_names_each_database_engine_container() {
        let cases = [
            ("postgres", "postgres-app", 5432),
            ("redis", "redis-app", 6379),
            ("mariadb", "mariadb-app", 3306),
            ("mongodb", "temps-mongodb-app", 27017),
        ];
        for (service_type, name, port) in cases {
            let endpoint = EngineContainerEndpoints.container_endpoint(
                service_type,
                "app",
                &json!({"host": "localhost"}),
            );
            assert_eq!(
                endpoint,
                Some(ContainerEndpoint {
                    name: name.to_string(),
                    port,
                }),
                "{service_type}"
            );
        }
    }
}
