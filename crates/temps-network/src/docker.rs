//! Docker network integration.
//!
//! We pin a Docker bridge network to the kernel bridge that the rest of this
//! crate manages. Docker handles per-container veth + IPAM within the
//! configured CIDR; we own everything *outside* the bridge (transport,
//! routes, firewall).

use crate::config::{NetworkConfig, NodeAlloc};
use crate::error::NetworkError;
use bollard::models::{Ipam, IpamConfig, NetworkCreateRequest};
use bollard::query_parameters::{InspectNetworkOptions, ListNetworksOptions};
use bollard::Docker;
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Ensure that a Docker network exists on this host with the right name,
/// driver, subnet, and bridge mapping. Idempotent.
///
/// Returns the Docker network id.
pub async fn ensure_network(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
) -> crate::Result<String> {
    // 1. Inspect existing networks to detect collisions and short-circuit
    //    when our network already exists in a compatible state.
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .map_err(|e| NetworkError::Docker {
            op: "list_networks",
            network: config.docker_network_name.clone(),
            reason: e.to_string(),
        })?;

    let mut existing_id: Option<String> = None;
    for net in networks {
        let Some(name) = net.name.clone() else {
            continue;
        };
        let cidrs: Vec<String> = net
            .ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .map(|cfgs| cfgs.iter().filter_map(|c| c.subnet.clone()).collect())
            .unwrap_or_default();

        if name == config.docker_network_name {
            existing_id = net.id.clone();
            continue;
        }

        for cidr in &cidrs {
            if cidr == &alloc.compute_cidr.to_string() {
                return Err(NetworkError::DockerCidrCollision {
                    cidr: alloc.compute_cidr,
                    existing_network: name,
                    desired_network: config.docker_network_name.clone(),
                });
            }
        }
    }

    if let Some(id) = existing_id {
        // Network already exists. Inspect it to confirm the subnet matches.
        let inspect = docker
            .inspect_network(&config.docker_network_name, None::<InspectNetworkOptions>)
            .await
            .map_err(|e| NetworkError::Docker {
                op: "inspect_network",
                network: config.docker_network_name.clone(),
                reason: e.to_string(),
            })?;

        let want_subnet = alloc.compute_cidr.to_string();
        let got_subnet = inspect
            .ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .and_then(|cfgs| cfgs.first())
            .and_then(|c| c.subnet.clone());

        if got_subnet.as_deref() != Some(want_subnet.as_str()) {
            return Err(NetworkError::InterfaceConflict {
                name: config.docker_network_name.clone(),
                reason: format!(
                    "existing docker network has subnet {:?}, want {}",
                    got_subnet, want_subnet
                ),
            });
        }

        debug!(
            network = %config.docker_network_name,
            id = %id,
            "docker network already exists with matching configuration"
        );
        return Ok(id);
    }

    // 2. Create a new bridge network pinned to our br-temps0 bridge.
    let mtu = config.transport.bridge_mtu(config.underlay_mtu);
    let mut driver_opts: HashMap<String, String> = HashMap::new();
    driver_opts.insert(
        "com.docker.network.bridge.name".into(),
        config.bridge_name.clone(),
    );
    driver_opts.insert("com.docker.network.driver.mtu".into(), mtu.to_string());
    // We handle masquerading ourselves via nftables so that the rules survive
    // a Docker daemon restart and we have a single source of truth.
    driver_opts.insert(
        "com.docker.network.bridge.enable_ip_masquerade".into(),
        "false".into(),
    );

    let request = NetworkCreateRequest {
        name: config.docker_network_name.clone(),
        driver: Some("bridge".into()),
        ipam: Some(Ipam {
            driver: Some("default".into()),
            config: Some(vec![IpamConfig {
                subnet: Some(alloc.compute_cidr.to_string()),
                gateway: Some(alloc.bridge_address.to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        options: Some(driver_opts),
        ..Default::default()
    };

    let resp = docker
        .create_network(request)
        .await
        .map_err(|e| NetworkError::Docker {
            op: "create_network",
            network: config.docker_network_name.clone(),
            reason: e.to_string(),
        })?;

    let id = resp.id;
    info!(
        network = %config.docker_network_name,
        id = %id,
        cidr = %alloc.compute_cidr,
        "created docker bridge network"
    );
    Ok(id)
}

/// Remove the Docker network we created. Idempotent — silently succeeds when
/// the network does not exist.
pub async fn remove_network(docker: &Docker, config: &NetworkConfig) -> crate::Result<()> {
    match docker
        .inspect_network(&config.docker_network_name, None::<InspectNetworkOptions>)
        .await
    {
        Ok(_) => {}
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => return Ok(()),
        Err(e) => {
            return Err(NetworkError::Docker {
                op: "inspect_network",
                network: config.docker_network_name.clone(),
                reason: e.to_string(),
            });
        }
    }

    if let Err(e) = docker.remove_network(&config.docker_network_name).await {
        match e {
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            } => return Ok(()),
            bollard::errors::Error::DockerResponseServerError {
                status_code: 403, ..
            } => {
                warn!(
                    network = %config.docker_network_name,
                    "docker network has attached containers; not forcing removal"
                );
                return Err(NetworkError::Docker {
                    op: "remove_network",
                    network: config.docker_network_name.clone(),
                    reason: "network still has attached containers".into(),
                });
            }
            other => {
                return Err(NetworkError::Docker {
                    op: "remove_network",
                    network: config.docker_network_name.clone(),
                    reason: other.to_string(),
                });
            }
        }
    }

    Ok(())
}
