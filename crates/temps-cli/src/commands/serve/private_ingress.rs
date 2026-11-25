// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Private addresses the control plane serves itself (ADR-114).
//!
//! `temps-ingress` decides what node `0` serves privately and records it;
//! the inside checks are `temps-node-agent`'s own code, exactly as on a
//! worker. The two crates do not depend on each other, so this binary joins
//! them, as it does for the control plane's tunnel connectors (ADR-107).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use temps_core::private_ingress::ControlPlanePrivateIngress;
use temps_core::private_network::{IpNet, LocalNetwork};
use temps_core::worker_ingress::PrivateCheck;
use temps_ingress::control_plane_private::{
    ContainerNetworks, ControlPlanePrivateIngressLoop, InsideChecker, LocalAddresses,
};
use temps_node_agent::route_store::{PrivateRouteEntry, RouteEntry};
use temps_routes::route_table::PrivateAddress;
use tokio::sync::Notify;
use tracing::{debug, info};

/// How long listing Docker's networks may take before the pass goes on
/// with the container bridge interfaces alone.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(5);

/// Every address on this machine, with its interface and prefix.
struct HostAddresses;

impl LocalAddresses for HostAddresses {
    fn addresses(&self) -> Vec<LocalNetwork> {
        temps_node_agent::public_ingress_manager::local_networks()
    }
}

/// Docker's networks on this machine, when this process has a Docker
/// client (not with `--profile control-plane`).
struct DockerNetworks(Option<Arc<bollard::Docker>>);

#[async_trait]
impl ContainerNetworks for DockerNetworks {
    async fn networks(&self) -> Vec<(String, IpNet)> {
        let Some(docker) = &self.0 else {
            return Vec::new();
        };
        let listed = tokio::time::timeout(
            DOCKER_TIMEOUT,
            docker.list_networks(None::<bollard::query_parameters::ListNetworksOptions>),
        )
        .await;
        let networks = match listed {
            Ok(Ok(networks)) => networks,
            Ok(Err(error)) => {
                debug!(%error, "could not list Docker networks for private ingress");
                return Vec::new();
            }
            Err(_) => {
                debug!("listing Docker networks for private ingress timed out");
                return Vec::new();
            }
        };
        networks
            .into_iter()
            .flat_map(|network| {
                let name = network.name.unwrap_or_default();
                network
                    .ipam
                    .and_then(|ipam| ipam.config)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(move |config| {
                        let subnet = config.subnet?.parse::<IpNet>().ok()?;
                        Some((name.clone(), subnet))
                    })
            })
            .collect()
    }
}

/// The agent's inside check, on node `0`'s private addresses.
struct AgentInsideCheck;

/// The agent's view of one private address: the check reads only its host,
/// id and listener.
fn agent_entry(host: String, address: PrivateAddress) -> Option<PrivateRouteEntry> {
    Some(PrivateRouteEntry {
        route: RouteEntry {
            host,
            backends: Vec::new(),
            deployment_id: None,
            project_id: None,
            environment_id: None,
            gate: None,
            shield: None,
            allowed_callers: Vec::new(),
            allowed_sandboxes: Vec::new(),
        },
        address_id: address.address_id,
        range: address.range?,
        listener: address.listener?,
        principals: None,
    })
}

#[async_trait]
impl InsideChecker for AgentInsideCheck {
    async fn check(&self, addresses: Vec<(String, PrivateAddress)>) -> Vec<PrivateCheck> {
        let entries: Vec<PrivateRouteEntry> = addresses
            .into_iter()
            .filter_map(|(host, address)| agent_entry(host, address))
            .collect();
        temps_node_agent::private_check::check_all(&entries, None).await
    }
}

/// Run node `0`'s private ingress in this process, beside the proxy that
/// serves it. Idle until the operator consents.
pub(super) fn start_control_plane_private_ingress(
    db: Arc<sea_orm::DatabaseConnection>,
    runtime: Arc<ControlPlanePrivateIngress>,
    route_table: Arc<temps_routes::CachedPeerTable>,
    docker: Option<Arc<bollard::Docker>>,
) {
    let private = ControlPlanePrivateIngressLoop::new(
        db,
        runtime,
        route_table,
        Arc::new(HostAddresses),
        Arc::new(DockerNetworks(docker)),
        Arc::new(AgentInsideCheck),
    );
    let shutdown = Arc::new(Notify::new());
    let on_signal = Arc::clone(&shutdown);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_signal.notify_one();
        }
    });
    private.start(shutdown);
    info!("control-plane private ingress runs in this process (ADR-114)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_without_a_range_or_listener_is_not_checked() {
        let address = PrivateAddress {
            address_id: "environment:4".into(),
            range: Some("100.64.0.0/10".into()),
            node_id: Some(0),
            listener: Some("100.101.102.103".into()),
            via: Some("listener".into()),
            principals: None,
        };
        let entry = agent_entry("app.internal.example.com".into(), address.clone()).unwrap();
        assert_eq!(entry.route.host, "app.internal.example.com");
        assert_eq!(entry.address_id, "environment:4");
        assert_eq!(entry.listener, "100.101.102.103");
        assert!(agent_entry(
            "a.example.com".into(),
            PrivateAddress {
                listener: None,
                ..address.clone()
            }
        )
        .is_none());
        assert!(agent_entry(
            "a.example.com".into(),
            PrivateAddress {
                range: None,
                ..address
            }
        )
        .is_none());
    }
}
