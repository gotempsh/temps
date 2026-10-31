// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The overlay address of every running Temps container on this node, for
//! the heartbeat.
//!
//! Docker hands out overlay addresses dynamically, so a daemon restart can
//! give a service container a different address than the one the control
//! plane recorded when it created it. `service_members.compute_ip` and the
//! cluster DNS records built from it would then point at a neighbour (or at
//! nothing), and network policy, which fails closed, blocks allowed
//! traffic. The heartbeat carries what the containers hold now, and the
//! control plane moves the records that no longer match.

use bollard::query_parameters::ListContainersOptionsBuilder;
use bollard::Docker;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Most addresses one heartbeat carries. A node runs at most its
/// `max_workloads` containers (110 by default); this bound only protects
/// the control plane from a misbehaving daemon.
pub const MAX_REPORTED_OVERLAY_ADDRESSES: usize = 4096;

/// One running container's address on the workload overlay.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OverlayAddress {
    pub container_name: String,
    pub ip: String,
}

/// The overlay addresses of the running Temps-managed containers, sorted so
/// an unchanged set compares equal between beats. One Docker list call; no
/// per-container inspect.
pub async fn collect(docker: &Docker) -> Result<Vec<OverlayAddress>, bollard::errors::Error> {
    let filters: HashMap<&str, Vec<&str>> = HashMap::from([
        ("label", vec!["sh.temps.managed=true"]),
        ("status", vec!["running"]),
    ]);
    let containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::new()
                .filters(&filters)
                .build(),
        ))
        .await?;
    let overlay = temps_network::overlay_name::current().docker_network;
    let mut addresses: Vec<OverlayAddress> = containers
        .into_iter()
        .filter_map(|container| {
            let name = container
                .names
                .as_ref()?
                .first()?
                .trim_start_matches('/')
                .to_string();
            let ip = overlay_ip(
                container.network_settings.as_ref()?.networks.as_ref()?,
                &overlay,
            )?;
            Some(OverlayAddress {
                container_name: name,
                ip,
            })
        })
        .take(MAX_REPORTED_OVERLAY_ADDRESSES)
        .collect();
    addresses.sort();
    Ok(addresses)
}

fn overlay_ip(
    networks: &HashMap<String, bollard::models::EndpointSettings>,
    overlay: &str,
) -> Option<String> {
    let ip = networks.get(overlay)?.ip_address.as_deref()?.trim();
    (!ip.is_empty() && ip.parse::<std::net::IpAddr>().is_ok()).then(|| ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::EndpointSettings;

    fn endpoint(ip: &str) -> EndpointSettings {
        EndpointSettings {
            ip_address: Some(ip.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn only_a_valid_address_on_the_overlay_counts() {
        let networks = HashMap::from([
            ("bridge".to_string(), endpoint("172.17.0.4")),
            ("temps-overlay".to_string(), endpoint("10.20.30.7")),
        ]);
        assert_eq!(
            overlay_ip(&networks, "temps-overlay").as_deref(),
            Some("10.20.30.7")
        );
        assert_eq!(overlay_ip(&networks, "missing"), None);

        let blank = HashMap::from([("temps-overlay".to_string(), endpoint(""))]);
        assert_eq!(overlay_ip(&blank, "temps-overlay"), None);
        let junk = HashMap::from([("temps-overlay".to_string(), endpoint("not-an-ip"))]);
        assert_eq!(overlay_ip(&junk, "temps-overlay"), None);
    }
}
