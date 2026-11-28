// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reaching a container the operator imports as a managed service.
//!
//! An import names a running container and the port the operator connects to
//! it on, which is the port it publishes on the host. A control plane on the
//! host verifies the credentials at `localhost:<that port>`. A containerized
//! control plane cannot: its loopback is its own. It reaches the container by
//! name on the Temps network, at the port the engine listens on inside it, so
//! the container has to join that network before the credentials are checked.
//!
//! [`attach_imported_container`] joins it, and [`import_dial_address`] says
//! where to connect afterwards. The stored parameters keep the published port,
//! as for a service Temps created; [`super::dial`] maps them the same way.

use bollard::models::{NetworkConnectRequest, PortMap};
use bollard::Docker;
use temps_core::ExecutionEnvironment;
use tracing::{debug, info, warn};

use super::error::{EngineError, Result};

/// The port the engine listens on inside an imported container, given the
/// port the operator entered.
///
/// The entered port is usually one the container publishes; then it is the
/// container port bound to it. A container on the Temps network may publish
/// nothing, and the operator enters the port it listens on: that is a port the
/// container exposes. Anything else falls back to the engine's default port.
pub fn imported_internal_port(
    ports: Option<&PortMap>,
    entered_port: &str,
    default_internal_port: &str,
) -> String {
    let entered = entered_port.trim();
    let Some(ports) = ports else {
        return default_internal_port.to_string();
    };
    let container_port = |key: &str| -> Option<String> {
        let (port, protocol) = key.split_once('/').unwrap_or((key, "tcp"));
        (protocol == "tcp").then(|| port.to_string())
    };

    let mut published: Vec<String> = ports
        .iter()
        .filter(|(_, bindings)| {
            bindings
                .iter()
                .flatten()
                .any(|binding| binding.host_port.as_deref().map(str::trim) == Some(entered))
        })
        .filter_map(|(key, _)| container_port(key))
        .collect();
    // A host port is bound to one container port; sort so the answer does not
    // depend on map order if a malformed inspect ever lists two.
    published.sort();
    if let Some(port) = published.into_iter().next() {
        return port;
    }

    if ports
        .keys()
        .filter_map(|key| container_port(key))
        .any(|port| port == entered)
    {
        return entered.to_string();
    }
    default_internal_port.to_string()
}

/// Host and port to verify an imported service on, for `execution_environment`.
///
/// On the host this is `localhost` and the entered port, as before. In a
/// container it is the container name and [`imported_internal_port`].
pub fn import_dial_address_for_environment(
    execution_environment: ExecutionEnvironment,
    ports: Option<&PortMap>,
    entered_port: &str,
    container_name: &str,
    default_internal_port: &str,
) -> (String, String) {
    let internal_port = imported_internal_port(ports, entered_port, default_internal_port);
    super::dial::engine_dial_address_for_environment(
        execution_environment,
        "localhost",
        entered_port,
        container_name,
        &internal_port,
    )
}

/// [`import_dial_address_for_environment`] for the environment this process
/// was started in.
pub fn import_dial_address(
    ports: Option<&PortMap>,
    entered_port: &str,
    container_name: &str,
    default_internal_port: &str,
) -> (String, String) {
    import_dial_address_for_environment(
        temps_core::runtime::execution_environment_compatibility(),
        ports,
        entered_port,
        container_name,
        default_internal_port,
    )
}

/// Attach an imported container to the Temps network.
///
/// Projects reach a managed service by container name on that network, so an
/// import always attaches. Where the control plane itself only reaches the
/// container through that network (it runs in a container), a failure is an
/// error that names the network; on the host it is logged and the import goes
/// on, as projects can still be linked once the operator attaches it.
pub async fn attach_imported_container(
    docker: &Docker,
    service_name: &str,
    engine: &str,
    container_id: &str,
    container_name: &str,
) -> Result<()> {
    attach_imported_container_for_environment(
        docker,
        temps_core::runtime::execution_environment_compatibility(),
        service_name,
        engine,
        container_id,
        container_name,
    )
    .await
}

/// [`attach_imported_container`] for an explicit execution environment.
pub async fn attach_imported_container_for_environment(
    docker: &Docker,
    execution_environment: ExecutionEnvironment,
    service_name: &str,
    engine: &str,
    container_id: &str,
    container_name: &str,
) -> Result<()> {
    let network_name = temps_core::NETWORK_NAME.as_str();
    // `ensure_network_exists` fails with a non-`Send` error; turn it into text
    // before the next await.
    let ensured = crate::utils::ensure_network_exists(docker)
        .await
        .map_err(|e| format!("the network could not be created: {}", e));
    let outcome = match ensured {
        Err(cause) => Err(cause),
        Ok(()) => {
            let request = NetworkConnectRequest {
                container: container_id.to_string(),
                ..Default::default()
            };
            match docker.connect_network(network_name, request).await {
                Ok(()) => {
                    info!(
                        "Attached imported {} container '{}' to {}",
                        engine, container_name, network_name
                    );
                    Ok(())
                }
                // Docker answers 403 when the container is already attached.
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 403, ..
                }) => {
                    debug!(
                        "Imported {} container '{}' is already attached to {}",
                        engine, container_name, network_name
                    );
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
    };

    match outcome {
        Ok(()) => Ok(()),
        Err(cause) if needs_network_to_verify(execution_environment) => {
            Err(EngineError::connection(
                service_name,
                format!(
                    "Cannot import {} container '{}': Temps runs in a container and reaches \
                     imported services by container name on the '{}' network, but attaching \
                     the container to it failed: {}",
                    engine, container_name, network_name, cause
                ),
            ))
        }
        Err(cause) => {
            warn!(
                "Failed to attach imported {} container '{}' to {}: {}",
                engine, container_name, network_name, cause
            );
            Ok(())
        }
    }
}

/// Whether this control plane can only verify an import through the Temps
/// network, so an import that cannot join it cannot proceed.
fn needs_network_to_verify(execution_environment: ExecutionEnvironment) -> bool {
    !matches!(execution_environment, ExecutionEnvironment::Host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::PortBinding;
    use std::collections::HashMap;

    fn binding(host_port: &str) -> PortBinding {
        PortBinding {
            host_ip: Some("0.0.0.0".to_string()),
            host_port: Some(host_port.to_string()),
        }
    }

    fn ports(entries: &[(&str, Option<Vec<PortBinding>>)]) -> PortMap {
        entries
            .iter()
            .map(|(key, bindings)| (key.to_string(), bindings.clone()))
            .collect::<HashMap<_, _>>()
    }

    #[test]
    fn test_entered_published_port_maps_to_the_bound_container_port() {
        let map = ports(&[("5432/tcp", Some(vec![binding("15432")]))]);

        assert_eq!(imported_internal_port(Some(&map), "15432", "5432"), "5432");
    }

    #[test]
    fn test_published_port_bound_to_a_non_default_container_port() {
        let map = ports(&[
            ("6380/tcp", Some(vec![binding("16380")])),
            ("6379/tcp", None),
        ]);

        assert_eq!(imported_internal_port(Some(&map), "16380", "6379"), "6380");
    }

    #[test]
    fn test_entered_exposed_port_of_an_unpublished_container_is_used_as_is() {
        let map = ports(&[("27018/tcp", None)]);

        assert_eq!(
            imported_internal_port(Some(&map), "27018", "27017"),
            "27018"
        );
    }

    #[test]
    fn test_udp_binding_on_the_entered_port_is_not_the_engine_port() {
        let map = ports(&[
            ("9999/udp", Some(vec![binding("15432")])),
            ("5432/tcp", None),
        ]);

        assert_eq!(imported_internal_port(Some(&map), "15432", "5432"), "5432");
    }

    #[test]
    fn test_unknown_port_or_no_port_data_falls_back_to_engine_default() {
        let map = ports(&[("5432/tcp", Some(vec![binding("15432")]))]);

        assert_eq!(imported_internal_port(Some(&map), "7000", "5432"), "5432");
        assert_eq!(imported_internal_port(None, "15432", "5432"), "5432");
        assert_eq!(imported_internal_port(Some(&map), " ", "5432"), "5432");
    }

    #[test]
    fn test_host_control_plane_verifies_on_localhost_and_entered_port() {
        let map = ports(&[("5432/tcp", Some(vec![binding("15432")]))]);

        let address = import_dial_address_for_environment(
            ExecutionEnvironment::Host,
            Some(&map),
            "15432",
            "legacy-db",
            "5432",
        );

        assert_eq!(address, ("localhost".to_string(), "15432".to_string()));
    }

    #[test]
    fn test_containerized_control_plane_verifies_on_container_name() {
        let map = ports(&[("6380/tcp", Some(vec![binding("16380")]))]);

        let address = import_dial_address_for_environment(
            ExecutionEnvironment::Docker,
            Some(&map),
            "16380",
            "legacy-cache",
            "6379",
        );

        assert_eq!(address, ("legacy-cache".to_string(), "6380".to_string()));
    }

    #[test]
    fn test_only_the_host_environment_can_import_without_the_network() {
        assert!(!needs_network_to_verify(ExecutionEnvironment::Host));
        assert!(needs_network_to_verify(ExecutionEnvironment::Docker));
    }

    #[tokio::test]
    async fn test_attach_failure_is_an_error_naming_the_network_in_a_container() {
        // Nothing listens on port 1, so every Docker call fails.
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .unwrap();

        let error = attach_imported_container_for_environment(
            &docker,
            ExecutionEnvironment::Docker,
            "imported-pg",
            "PostgreSQL",
            "abc123",
            "legacy-db",
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("legacy-db"), "{error}");
        assert!(error.contains(temps_core::NETWORK_NAME.as_str()), "{error}");
        assert!(error.contains("container name"), "{error}");
    }

    #[tokio::test]
    async fn test_attach_failure_on_the_host_does_not_block_the_import() {
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .unwrap();

        attach_imported_container_for_environment(
            &docker,
            ExecutionEnvironment::Host,
            "imported-pg",
            "PostgreSQL",
            "abc123",
            "legacy-db",
        )
        .await
        .unwrap();
    }
}
