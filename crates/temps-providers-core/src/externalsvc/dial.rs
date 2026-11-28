// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where an engine opens a connection to the service it manages.
//!
//! An engine holds the service's stored configuration: a loopback `host` and
//! the host `port` the container publishes. Those are what a container is
//! created with. They are not always what this process can connect to (see
//! [`temps_core::managed_service_address`]), so every connection an engine
//! opens to its own service takes its address from here.
//!
//! Publishing a port and dialling the service are different operations that
//! happen to share one stored field. Code that creates or recreates a
//! container keeps reading the stored port directly; only code that connects
//! calls [`engine_dial_address`].

use temps_core::managed_service_address::{control_plane_dial_target, ContainerEndpoint};
use temps_core::ExecutionEnvironment;

/// Host and port this process connects to for a service stored at
/// `stored_host:stored_port`, whose container is `container_name` and whose
/// engine listens on `internal_port` inside it.
///
/// A port that is not a plain TCP port is returned as stored, with the stored
/// host: the connection then fails naming the value the operator can see.
pub fn engine_dial_address_for_environment(
    execution_environment: ExecutionEnvironment,
    stored_host: &str,
    stored_port: &str,
    container_name: &str,
    internal_port: &str,
) -> (String, String) {
    let (Ok(published_port), Ok(internal_port)) = (
        stored_port.trim().parse::<u16>(),
        internal_port.trim().parse::<u16>(),
    ) else {
        return (stored_host.to_string(), stored_port.to_string());
    };
    let container = ContainerEndpoint {
        name: container_name.to_string(),
        port: internal_port,
    };
    // An engine only ever runs against its own Docker daemon. A service on a
    // worker is handled by that worker's agent, which is given the node's
    // published address as the stored host, so no worker address applies.
    let target = control_plane_dial_target(
        execution_environment,
        stored_host,
        published_port,
        None,
        Some(&container),
    );
    (target.host, target.port.to_string())
}

/// [`engine_dial_address_for_environment`] for the environment this process
/// was started in.
pub fn engine_dial_address(
    stored_host: &str,
    stored_port: &str,
    container_name: &str,
    internal_port: &str,
) -> (String, String) {
    engine_dial_address_for_environment(
        temps_core::runtime::execution_environment_compatibility(),
        stored_host,
        stored_port,
        container_name,
        internal_port,
    )
}

/// [`engine_dial_address`] for callers that hold ports as numbers.
pub fn control_plane_endpoint(
    stored_host: &str,
    stored_port: u16,
    container_name: &str,
    internal_port: u16,
) -> (String, u16) {
    control_plane_endpoint_for_environment(
        temps_core::runtime::execution_environment_compatibility(),
        stored_host,
        stored_port,
        container_name,
        internal_port,
    )
}

/// [`control_plane_endpoint`] for an explicit execution environment.
pub fn control_plane_endpoint_for_environment(
    execution_environment: ExecutionEnvironment,
    stored_host: &str,
    stored_port: u16,
    container_name: &str,
    internal_port: u16,
) -> (String, u16) {
    let container = ContainerEndpoint {
        name: container_name.to_string(),
        port: internal_port,
    };
    let target = control_plane_dial_target(
        execution_environment,
        stored_host,
        stored_port,
        None,
        Some(&container),
    );
    (target.host, target.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_process_dials_the_stored_loopback_address() {
        let address = engine_dial_address_for_environment(
            ExecutionEnvironment::Host,
            "localhost",
            "6097",
            "postgres-app",
            "5432",
        );

        assert_eq!(address, ("localhost".to_string(), "6097".to_string()));
    }

    #[test]
    fn test_containerized_process_dials_container_name_and_engine_port() {
        let address = engine_dial_address_for_environment(
            ExecutionEnvironment::Docker,
            "localhost",
            "6097",
            "postgres-app",
            "5432",
        );

        assert_eq!(address, ("postgres-app".to_string(), "5432".to_string()));
    }

    #[test]
    fn test_published_address_supplied_by_a_worker_is_kept() {
        // A worker's agent replaces the stored loopback host with the node's
        // published address before the engine sees the configuration.
        let address = engine_dial_address_for_environment(
            ExecutionEnvironment::Docker,
            "10.0.0.7",
            "6097",
            "postgres-app",
            "5432",
        );

        assert_eq!(address, ("10.0.0.7".to_string(), "6097".to_string()));
    }

    #[test]
    fn test_unparseable_stored_port_is_returned_as_stored() {
        let address = engine_dial_address_for_environment(
            ExecutionEnvironment::Docker,
            "localhost",
            "not-a-port",
            "postgres-app",
            "5432",
        );

        assert_eq!(address, ("localhost".to_string(), "not-a-port".to_string()));
    }

    #[test]
    fn numeric_ports_follow_the_same_rule() {
        assert_eq!(
            control_plane_endpoint_for_environment(
                ExecutionEnvironment::Docker,
                "localhost",
                39000,
                "garage-media",
                3900,
            ),
            ("garage-media".to_string(), 3900)
        );
        assert_eq!(
            control_plane_endpoint_for_environment(
                ExecutionEnvironment::Host,
                "localhost",
                39000,
                "garage-media",
                3900,
            ),
            ("localhost".to_string(), 39000)
        );
        // An address the operator set explicitly is never replaced.
        assert_eq!(
            control_plane_endpoint_for_environment(
                ExecutionEnvironment::Docker,
                "storage.internal.example",
                39000,
                "garage-media",
                3900,
            ),
            ("storage.internal.example".to_string(), 39000)
        );
    }
}
