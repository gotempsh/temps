// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
use ipnet::Ipv4Net;
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{debug, info, warn};

const NETWORK_OWNER_LABEL: &str = "sh.temps.network";
const NETWORK_OWNER_VALUE: &str = "multi-node-overlay";

/// Refuse a setup before any kernel state is changed when Docker already owns
/// an overlapping address range. Docker rejects these late with a generic
/// `invalid pool request`; this turns it into an actionable, named conflict.
pub async fn preflight_network(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
) -> crate::Result<()> {
    preflight_network_for_pool(docker, config, alloc, alloc.compute_cidr).await
}

/// Preflight the complete cluster pool, not only this node's allocation.
/// Every node installs routes for peer subnets, so a local Docker network
/// overlapping any part of the pool can black-hole a future peer.
pub async fn preflight_network_for_pool(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    compute_pool: Ipv4Net,
) -> crate::Result<()> {
    let desired_subnet = alloc.compute_cidr.to_string();
    let desired_gateway = alloc.bridge_address.to_string();
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .map_err(|error| NetworkError::Docker {
            op: "list_networks",
            network: config.docker_network_name.clone(),
            reason: error.to_string(),
        })?;
    for network in networks {
        let Some(name) = network.name else {
            continue;
        };
        if name == config.docker_network_name {
            validate_owned_network(docker, config).await?;
            let allocation = network
                .ipam
                .and_then(|ipam| ipam.config)
                .and_then(|configs| {
                    configs.into_iter().find(|entry| {
                        entry.subnet.as_deref() == Some(desired_subnet.as_str())
                            && entry.gateway.as_deref() == Some(desired_gateway.as_str())
                    })
                });
            if allocation.is_none() {
                return Err(NetworkError::InterfaceConflict {
                    name,
                    reason: format!(
                        "existing Temps-owned Docker network does not use authoritative subnet {} and gateway {}",
                        alloc.compute_cidr, alloc.bridge_address
                    ),
                });
            }
            continue;
        }
        let cidrs = network
            .ipam
            .and_then(|ipam| ipam.config)
            .unwrap_or_default();
        for raw in cidrs.into_iter().filter_map(|entry| entry.subnet) {
            let Ok(existing_cidr) = Ipv4Net::from_str(&raw) else {
                continue;
            };
            if cidrs_overlap(&compute_pool, &existing_cidr) {
                return Err(NetworkError::DockerCidrCollision {
                    cidr: compute_pool,
                    existing_cidr,
                    existing_network: name,
                    desired_network: config.docker_network_name.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Verify that a pre-existing Docker network is the overlay created and
/// owned by Temps before privileged code attaches a container to it.
///
/// A network name is not an ownership boundary: another local actor can
/// create `temps0` first. Callers which do not know this node's allocation
/// can still verify the immutable ownership label and bridge mapping; the
/// setup path below additionally verifies subnet and gateway.
pub async fn validate_owned_network(docker: &Docker, config: &NetworkConfig) -> crate::Result<()> {
    let inspect = docker
        .inspect_network(&config.docker_network_name, None::<InspectNetworkOptions>)
        .await
        .map_err(|error| NetworkError::Docker {
            op: "inspect_network",
            network: config.docker_network_name.clone(),
            reason: error.to_string(),
        })?;
    let owned = inspect
        .labels
        .as_ref()
        .and_then(|labels| labels.get(NETWORK_OWNER_LABEL))
        .is_some_and(|value| value == NETWORK_OWNER_VALUE);
    let bridge_matches = inspect
        .options
        .as_ref()
        .and_then(|options| options.get("com.docker.network.bridge.name"))
        .is_some_and(|value| value == &config.bridge_name);
    let masquerade_disabled = inspect
        .options
        .as_ref()
        .and_then(|options| options.get("com.docker.network.bridge.enable_ip_masquerade"))
        .is_some_and(|value| value == "false");

    if inspect.driver.as_deref() != Some("bridge")
        || !owned
        || !bridge_matches
        || !masquerade_disabled
    {
        return Err(NetworkError::InterfaceConflict {
            name: config.docker_network_name.clone(),
            reason: format!(
                "existing network is not the Temps-owned bridge (driver={:?}, owned={owned}, bridge_matches={bridge_matches}, masquerade_disabled={masquerade_disabled})",
                inspect.driver
            ),
        });
    }
    Ok(())
}

/// Ensure that a Docker network exists on this host with the right name,
/// driver, subnet, and bridge mapping. Idempotent.
///
/// Returns the Docker network id.
pub async fn ensure_network(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
) -> crate::Result<String> {
    ensure_network_for_pool(docker, config, alloc, alloc.compute_cidr).await
}

/// Ensure the local Docker network while fencing against every subnet in the
/// authoritative cluster pool. Use this in multi-node setup/reconciliation;
/// the narrower [`ensure_network`] wrapper remains for single-allocation
/// callers and backwards compatibility.
pub async fn ensure_network_for_pool(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    compute_pool: Ipv4Net,
) -> crate::Result<String> {
    preflight_network_for_pool(docker, config, alloc, compute_pool).await?;
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
            let Ok(existing_cidr) = Ipv4Net::from_str(cidr) else {
                continue;
            };
            if cidrs_overlap(&compute_pool, &existing_cidr) {
                return Err(NetworkError::DockerCidrCollision {
                    cidr: compute_pool,
                    existing_cidr,
                    existing_network: name,
                    desired_network: config.docker_network_name.clone(),
                });
            }
        }
    }

    if let Some(id) = existing_id {
        // Network already exists. Inspect it to confirm the subnet matches.
        validate_owned_network(docker, config).await?;
        let inspect = docker
            .inspect_network(&config.docker_network_name, None::<InspectNetworkOptions>)
            .await
            .map_err(|e| NetworkError::Docker {
                op: "inspect_network",
                network: config.docker_network_name.clone(),
                reason: e.to_string(),
            })?;

        let want_subnet = alloc.compute_cidr.to_string();
        let want_gateway = alloc.bridge_address.to_string();
        let got_subnet = inspect
            .ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .and_then(|cfgs| cfgs.first())
            .and_then(|c| c.subnet.clone());

        let got_gateway = inspect
            .ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .and_then(|cfgs| cfgs.first())
            .and_then(|config| config.gateway.as_deref());
        if got_subnet.as_deref() != Some(want_subnet.as_str())
            || got_gateway != Some(want_gateway.as_str())
        {
            return Err(NetworkError::InterfaceConflict {
                name: config.docker_network_name.clone(),
                reason: format!(
                    "existing Temps-owned network allocation differs (subnet={got_subnet:?}, gateway={got_gateway:?})"
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
        labels: Some(HashMap::from([(
            NETWORK_OWNER_LABEL.to_string(),
            NETWORK_OWNER_VALUE.to_string(),
        )])),
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

fn cidrs_overlap(left: &Ipv4Net, right: &Ipv4Net) -> bool {
    left.contains(&right.network()) || right.contains(&left.network())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_detects_parent_docker_pool_containing_node_subnet() {
        let existing: Ipv4Net = "172.20.0.0/16".parse().expect("valid parent pool");
        let requested: Ipv4Net = "172.20.255.0/24".parse().expect("valid node subnet");
        assert!(cidrs_overlap(&requested, &existing));
        assert!(cidrs_overlap(&existing, &requested));
    }

    #[test]
    fn overlap_rejects_disjoint_private_networks() {
        let existing: Ipv4Net = "172.18.0.0/16".parse().expect("valid app pool");
        let requested: Ipv4Net = "172.20.0.0/24".parse().expect("valid node subnet");
        assert!(!cidrs_overlap(&requested, &existing));
    }

    #[test]
    fn full_pool_detects_future_peer_collision_outside_local_allocation() {
        let cluster_pool: Ipv4Net = "10.240.0.0/16".parse().unwrap();
        let local_allocation: Ipv4Net = "10.240.255.0/24".parse().unwrap();
        let existing_docker_pool: Ipv4Net = "10.240.1.0/24".parse().unwrap();
        assert!(!cidrs_overlap(&local_allocation, &existing_docker_pool));
        assert!(cidrs_overlap(&cluster_pool, &existing_docker_pool));
    }
}
