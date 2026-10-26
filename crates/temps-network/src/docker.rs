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
use bollard::models::{
    EndpointIpamConfig, EndpointSettings, Ipam, IpamConfig, NetworkConnectRequest,
    NetworkCreateRequest, NetworkDisconnectRequest,
};
use bollard::query_parameters::{
    InspectContainerOptions, InspectNetworkOptions, ListContainersOptions, ListNetworksOptions,
};
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
///
/// Checks the subnets this node routes on the overlay (`routed`: its own and
/// every peer's), not the whole cluster pool (ADR 056). A Docker network
/// elsewhere in the pool cannot black-hole anything this node sends; a new
/// peer is allocated around the ranges this node reports as occupied.
pub async fn preflight_network_for_subnets(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    routed: &[Ipv4Net],
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
                        "existing Temps-owned Docker network does not use authoritative subnet {} \
                         and gateway {}: it was created for another allocation, often by another \
                         cluster this host belonged to. Temps never removes it. Re-run `sudo \
                         temps join` on this host: it sees the conflict and gives this node \
                         another overlay network name, so this cluster gets its own network \
                         beside it. Or, if nothing uses '{}' any more, remove it (`docker network rm {}` after disconnecting its \
                         containers) so it is recreated on the new subnet",
                        alloc.compute_cidr,
                        alloc.bridge_address,
                        config.docker_network_name,
                        config.docker_network_name
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
            if let Some(subnet) = routed_overlap(routed, &existing_cidr) {
                return Err(NetworkError::DockerCidrCollision {
                    cidr: subnet,
                    existing_cidr,
                    existing_network: name,
                    desired_network: config.docker_network_name.clone(),
                });
            }
        }
    }
    Ok(())
}

/// The first routed subnet `existing` overlaps, in either direction.
fn routed_overlap(routed: &[Ipv4Net], existing: &Ipv4Net) -> Option<Ipv4Net> {
    routed
        .iter()
        .copied()
        .find(|subnet| cidrs_overlap(subnet, existing))
}

/// Every Docker network's IPv4 subnets except `owned_network`'s, as occupied
/// ranges (ADR 056).
pub async fn occupied_docker_subnets(
    docker: &Docker,
    owned_network: &str,
) -> crate::Result<Vec<crate::occupied::OccupiedRange>> {
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .map_err(|error| NetworkError::Docker {
            op: "list_networks",
            network: owned_network.to_owned(),
            reason: error.to_string(),
        })?;
    let mut ranges = Vec::new();
    for network in networks {
        let Some(name) = network.name else {
            continue;
        };
        if name == owned_network {
            continue;
        }
        let subnets = network
            .ipam
            .and_then(|ipam| ipam.config)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| entry.subnet)
            .filter_map(|raw| Ipv4Net::from_str(&raw).ok());
        for cidr in subnets {
            ranges.push(crate::occupied::OccupiedRange::docker(cidr, name.clone()));
        }
    }
    Ok(ranges)
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
                "existing network is not this overlay's Temps-owned bridge (driver={:?}, \
                 owned={owned}, bridge_matches={bridge_matches}, \
                 masquerade_disabled={masquerade_disabled}). Temps never removes it: re-run \
                 `sudo temps join` on this host, which gives this node another overlay network \
                 name so the overlay gets its own network",
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
    ensure_network_for_subnets(docker, config, alloc, &[alloc.compute_cidr]).await
}

/// Ensure the local Docker network while fencing against every subnet this
/// node routes on the overlay (its own and its peers'). Use this in multi-node
/// setup/reconciliation; the narrower [`ensure_network`] wrapper remains for
/// single-allocation callers and backwards compatibility.
pub async fn ensure_network_for_subnets(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    routed: &[Ipv4Net],
) -> crate::Result<String> {
    preflight_network_for_subnets(docker, config, alloc, routed).await?;
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
            if let Some(subnet) = routed_overlap(routed, &existing_cidr) {
                return Err(NetworkError::DockerCidrCollision {
                    cidr: subnet,
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

        let want_mtu = config.transport.bridge_mtu(config.underlay_mtu).to_string();
        let got_mtu = inspect
            .options
            .as_ref()
            .and_then(|options| options.get(MTU_OPTION))
            .cloned();
        if got_mtu.as_deref() != Some(want_mtu.as_str()) {
            return recreate_for_new_mtu(docker, config, alloc, got_mtu).await;
        }

        debug!(
            network = %config.docker_network_name,
            id = %id,
            "docker network already exists with matching configuration"
        );
        return Ok(id);
    }

    create_owned_network(docker, config, alloc).await
}

const MTU_OPTION: &str = "com.docker.network.driver.mtu";

/// Create our bridge network, pinned to the overlay bridge.
async fn create_owned_network(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
) -> crate::Result<String> {
    let mtu = config.transport.bridge_mtu(config.underlay_mtu);
    let mut driver_opts: HashMap<String, String> = HashMap::new();
    driver_opts.insert(
        "com.docker.network.bridge.name".into(),
        config.bridge_name.clone(),
    );
    driver_opts.insert(MTU_OPTION.into(), mtu.to_string());
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

/// Docker can't change a network's MTU in place, and containers keep the MTU
/// they were attached with. After the overlay's MTU drops (its underlay moved
/// onto the WireGuard mesh, whose MTU is lower), the bridge drops every frame
/// above the new MTU without an ICMP error, so large transfers stall while
/// small ones work. Recreate the network with the new MTU and reattach every
/// member, running or stopped, with its address and aliases.
async fn recreate_for_new_mtu(
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    previous_mtu: Option<String>,
) -> crate::Result<String> {
    let name = config.docker_network_name.as_str();
    let attachments = owned_network_members(docker, name).await?;
    let want_mtu = config.transport.bridge_mtu(config.underlay_mtu);
    warn!(
        network = %name,
        previous_mtu = previous_mtu.as_deref().unwrap_or("unset"),
        mtu = want_mtu,
        containers = attachments.len(),
        "overlay MTU changed; recreating the Docker network and reattaching its containers"
    );
    detach_and_remove(docker, name, &attachments).await?;
    let id = create_owned_network(docker, config, alloc).await?;

    let mut failed = Vec::new();
    for (container, previous) in attachments {
        let address = previous
            .ipam_config
            .as_ref()
            .and_then(|ipam| ipam.ipv4_address.clone())
            .or(previous.ip_address.clone())
            .filter(|address| !address.is_empty());
        let request = NetworkConnectRequest {
            container: container.clone(),
            endpoint_config: Some(EndpointSettings {
                ipam_config: address.map(|address| EndpointIpamConfig {
                    ipv4_address: Some(address),
                    ..Default::default()
                }),
                aliases: previous.aliases.clone(),
                ..Default::default()
            }),
        };
        if let Err(error) = docker.connect_network(name, request).await {
            warn!(network = %name, container = %container, error = %error, "could not reattach container to the recreated overlay network");
            failed.push(container);
        }
    }
    if !failed.is_empty() {
        return Err(NetworkError::Docker {
            op: "connect_network",
            network: name.to_string(),
            reason: format!(
                "recreated the network for MTU {want_mtu} but could not reattach {} container(s): {}; redeploy them",
                failed.len(),
                failed.join(", ")
            ),
        });
    }
    info!(network = %name, mtu = want_mtu, "overlay Docker network recreated with the new MTU");
    Ok(id)
}

/// Every container attached to `name`, running or stopped, with the endpoint
/// it is attached with.
async fn owned_network_members(
    docker: &Docker,
    name: &str,
) -> crate::Result<Vec<(String, EndpointSettings)>> {
    let docker_error = |op: &'static str| {
        move |error: bollard::errors::Error| NetworkError::Docker {
            op,
            network: name.to_string(),
            reason: error.to_string(),
        }
    };
    let members = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "network".to_string(),
                vec![name.to_string()],
            )])),
            ..Default::default()
        }))
        .await
        .map_err(docker_error("list_containers"))?;
    let mut attachments = Vec::new();
    for member in members {
        let Some(id) = member.id else { continue };
        let endpoint = docker
            .inspect_container(&id, None::<InspectContainerOptions>)
            .await
            .map_err(docker_error("inspect_container"))?
            .network_settings
            .and_then(|settings| settings.networks)
            .and_then(|mut networks| networks.remove(name))
            .unwrap_or_default();
        attachments.push((id, endpoint));
    }
    Ok(attachments)
}

/// Detach `attachments` from `name`, then remove the network.
async fn detach_and_remove(
    docker: &Docker,
    name: &str,
    attachments: &[(String, EndpointSettings)],
) -> crate::Result<()> {
    let docker_error = |op: &'static str| {
        move |error: bollard::errors::Error| NetworkError::Docker {
            op,
            network: name.to_string(),
            reason: error.to_string(),
        }
    };
    for (container, _) in attachments {
        docker
            .disconnect_network(
                name,
                NetworkDisconnectRequest {
                    container: container.clone(),
                    force: Some(true),
                },
            )
            .await
            .map_err(docker_error("disconnect_network"))?;
    }
    docker
        .remove_network(name)
        .await
        .map_err(docker_error("remove_network"))
}

/// A container taken off the overlay network when this node's allocation
/// moved, with the aliases it rejoins with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedMember {
    pub container: String,
    pub aliases: Option<Vec<String>>,
}

/// Retire the overlay network because the control plane moved this node's
/// allocation (ADR 056). Docker cannot change a network's subnet, and the old
/// one is no longer this node's, so detach every member and remove the
/// network; the caller recreates it on the new subnet and passes `queue` to
/// [`reattach_members`]. Each member is added to `queue` before it is
/// detached, so one detached before a later failure is still reattached once
/// a retry succeeds. Refuses a network Temps does not own. A missing network
/// has no members.
pub async fn retire_network_for_move(
    docker: &Docker,
    config: &NetworkConfig,
    queue: &mut Vec<DetachedMember>,
) -> crate::Result<()> {
    let name = config.docker_network_name.as_str();
    match docker
        .inspect_network(name, None::<InspectNetworkOptions>)
        .await
    {
        Ok(_) => {}
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => return Ok(()),
        Err(error) => {
            return Err(NetworkError::Docker {
                op: "inspect_network",
                network: name.to_string(),
                reason: error.to_string(),
            })
        }
    }
    validate_owned_network(docker, config).await?;
    let attachments = owned_network_members(docker, name).await?;
    for (container, endpoint) in &attachments {
        if !queue.iter().any(|queued| &queued.container == container) {
            queue.push(DetachedMember {
                container: container.clone(),
                aliases: endpoint.aliases.clone(),
            });
        }
    }
    warn!(
        network = %name,
        containers = attachments.len(),
        "overlay allocation moved; removing the Docker network so it is recreated on the new subnet"
    );
    detach_and_remove(docker, name, &attachments).await
}

/// Attach containers removed by [`retire_network_for_move`] to the recreated
/// network with their aliases. Their old addresses belonged to the previous
/// subnet, so Docker assigns new ones. A container that no longer exists is
/// dropped. `members` keeps only the containers that could not rejoin, so a
/// retry does not reconnect the ones that did.
pub async fn reattach_members(
    docker: &Docker,
    config: &NetworkConfig,
    members: &mut Vec<DetachedMember>,
) -> crate::Result<()> {
    let name = config.docker_network_name.as_str();
    let total = members.len();
    let mut failed = Vec::new();
    for member in members.drain(..) {
        let request = NetworkConnectRequest {
            container: member.container.clone(),
            endpoint_config: Some(EndpointSettings {
                aliases: member.aliases.clone(),
                ..Default::default()
            }),
        };
        match docker.connect_network(name, request).await {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                debug!(network = %name, container = %member.container, "detached container is gone; nothing to reattach")
            }
            Err(error) => {
                warn!(network = %name, container = %member.container, error = %error, "could not reattach container to the moved overlay network");
                failed.push(member);
            }
        }
    }
    if failed.is_empty() {
        info!(network = %name, containers = total, "containers rejoined the overlay network on its new subnet");
        return Ok(());
    }
    let names = failed
        .iter()
        .map(|member| member.container.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let count = failed.len();
    *members = failed;
    Err(NetworkError::Docker {
        op: "connect_network",
        network: name.to_string(),
        reason: format!(
            "the overlay moved to a new subnet but {count} container(s) could not rejoin it yet: {names}; retrying, or redeploy them"
        ),
    })
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
    fn only_routed_subnets_are_checked_against_docker_networks() {
        let routed: [Ipv4Net; 2] = [
            "10.202.0.0/24".parse().unwrap(),
            "10.202.1.0/24".parse().unwrap(),
        ];
        // A Compose network elsewhere in the cluster pool is fine.
        assert_eq!(
            routed_overlap(&routed, &"10.202.9.0/24".parse().unwrap()),
            None
        );
        // One over a peer's subnet, or covering all of them, is not.
        assert_eq!(
            routed_overlap(&routed, &"10.202.1.0/24".parse().unwrap()),
            Some(routed[1])
        );
        assert_eq!(
            routed_overlap(&routed, &"10.0.0.0/8".parse().unwrap()),
            Some(routed[0])
        );
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
