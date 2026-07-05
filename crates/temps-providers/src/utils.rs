use bollard::{models::NetworkCreateRequest, query_parameters::ListNetworksOptions, Docker};
use std::collections::HashMap;
use tracing::{error, info};

pub(crate) async fn ensure_network_exists(
    docker: &Docker,
) -> Result<(), Box<dyn std::error::Error>> {
    let network_name = temps_core::NETWORK_NAME.as_str();

    // Check if network exists
    let networks = docker.list_networks(None::<ListNetworksOptions>).await?;
    let network_exists = networks
        .iter()
        .any(|n| n.name.as_deref() == Some(network_name));

    if !network_exists {
        info!("Creating network: {}", network_name);
        let options = NetworkCreateRequest {
            name: network_name.to_string(),
            driver: Some("bridge".to_string()),
            ..Default::default()
        };

        match docker.create_network(options).await {
            Ok(_) => info!("Successfully created network: {}", network_name),
            Err(e) => {
                error!("Failed to create network: {}", e);
                return Err(Box::new(e));
            }
        }
    }

    Ok(())
}

/// Create a Docker log configuration for external service containers.
/// Uses `json-file` driver with configurable size limits to prevent unbounded log growth.
///
/// Default: 20MB max per file, 3 rotated files = 60MB max total per container.
pub(crate) fn service_log_config(
    max_size: &str,
    max_file: u32,
) -> bollard::models::HostConfigLogConfig {
    let mut config = HashMap::new();
    config.insert("max-size".to_string(), max_size.to_string());
    config.insert("max-file".to_string(), max_file.to_string());

    bollard::models::HostConfigLogConfig {
        typ: Some("json-file".to_string()),
        config: Some(config),
    }
}

/// Create default Docker log configuration for external service containers.
/// 20MB max per file, 3 rotated files = 60MB max total.
pub(crate) fn default_service_log_config() -> bollard::models::HostConfigLogConfig {
    service_log_config("20m", 3)
}

/// Build a Docker port-binding map that maps `container_port_key`
/// (e.g. `"5432/tcp"`) to `host_port` on the loopback interface only.
///
/// This is the single source of truth for how managed-service containers
/// (Postgres, Redis, MongoDB, S3/RustFS) publish their ports to the host.
/// It must always bind to `127.0.0.1` — never `0.0.0.0` — so services are
/// reachable from the host and from other containers on the Docker network,
/// but never from outside the server without an explicit reverse proxy or
/// port-forward. This matches the documented behavior in
/// `docs/howto/set-up-managed-services`. See bherila/temps#29.
pub(crate) fn local_port_binding(
    container_port_key: &str,
    host_port: &str,
) -> HashMap<String, Option<Vec<bollard::models::PortBinding>>> {
    HashMap::from([(
        container_port_key.to_string(),
        Some(vec![bollard::models::PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(host_port.to_string()),
        }]),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_port_binding_binds_to_loopback_only() {
        let bindings = local_port_binding("5432/tcp", "15432");

        let port_binding = bindings
            .get("5432/tcp")
            .expect("port bindings should contain the requested container port key")
            .as_ref()
            .expect("port binding list should be present")
            .first()
            .expect("port binding list should have one entry");

        assert_eq!(
            port_binding.host_ip.as_deref(),
            Some("127.0.0.1"),
            "managed service ports must bind to loopback only, never 0.0.0.0"
        );
        assert_eq!(port_binding.host_port.as_deref(), Some("15432"));
    }

    #[test]
    fn test_local_port_binding_never_binds_to_all_interfaces() {
        for (container_port, host_port) in [
            ("5432/tcp", "5432"),
            ("6379/tcp", "6379"),
            ("9000/tcp", "9000"),
        ] {
            let bindings = local_port_binding(container_port, host_port);
            let host_ip = bindings
                .get(container_port)
                .and_then(|b| b.as_ref())
                .and_then(|v| v.first())
                .and_then(|pb| pb.host_ip.as_deref());
            assert_ne!(
                host_ip,
                Some("0.0.0.0"),
                "port {container_port} must never bind to 0.0.0.0"
            );
        }
    }
}
