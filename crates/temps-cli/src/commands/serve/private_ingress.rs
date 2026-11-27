// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Private addresses the control plane serves itself (ADR-115).
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

/// How long listing Docker's networks may take. A listing that fails or
/// times out is reported as unknown, never as no networks: the loop then
/// serves nothing until a listing succeeds.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(5);

/// Every address on this machine, with its interface and prefix.
struct HostAddresses;

impl LocalAddresses for HostAddresses {
    fn addresses(&self) -> Vec<LocalNetwork> {
        temps_node_agent::public_ingress_manager::local_networks()
    }
}

/// Makes a Docker client, or says it cannot.
type DockerConnect = Box<dyn Fn() -> Option<bollard::Docker> + Send + Sync>;

/// Docker's networks on this machine.
enum DockerNetworks {
    /// No container runs on this machine (`--profile control-plane`), so
    /// there is none to overlap.
    NotUsed,
    /// Containers run here. The client is the one startup connected, or
    /// one made on a later pass when Docker did not answer at boot: until
    /// one lists, the inventory is unknown, never empty.
    Used {
        client: tokio::sync::Mutex<Option<Arc<bollard::Docker>>>,
        connect: DockerConnect,
    },
}

impl DockerNetworks {
    fn new(used: bool, client: Option<Arc<bollard::Docker>>) -> Self {
        if !used {
            return Self::NotUsed;
        }
        Self::Used {
            client: tokio::sync::Mutex::new(client),
            connect: Box::new(|| bollard::Docker::connect_with_defaults().ok()),
        }
    }
}

#[async_trait]
impl ContainerNetworks for DockerNetworks {
    async fn networks(&self) -> Option<Vec<(String, IpNet)>> {
        let docker = match self {
            Self::NotUsed => return Some(Vec::new()),
            Self::Used { client, connect } => {
                let mut client = client.lock().await;
                if client.is_none() {
                    *client = connect().map(Arc::new);
                }
                let Some(docker) = client.clone() else {
                    debug!("no Docker client for private ingress; its networks are unknown");
                    return None;
                };
                docker
            }
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
                return None;
            }
            Err(_) => {
                debug!("listing Docker networks for private ingress timed out");
                return None;
            }
        };
        let networks = networks
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
            .collect();
        Some(networks)
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
    docker_used: bool,
    docker: Option<Arc<bollard::Docker>>,
) {
    let private = ControlPlanePrivateIngressLoop::new(
        db,
        runtime,
        route_table,
        Arc::new(HostAddresses),
        Arc::new(DockerNetworks::new(docker_used, docker)),
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
    info!("control-plane private ingress runs in this process (ADR-115)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn docker_that_did_not_answer_at_boot_is_unknown_not_empty() {
        // No containers on this machine: nothing can overlap.
        assert_eq!(
            DockerNetworks::new(false, None).networks().await,
            Some(Vec::new())
        );
        // Containers run here but no client could be made: unknown.
        let unreachable = DockerNetworks::Used {
            client: tokio::sync::Mutex::new(None),
            connect: Box::new(|| None),
        };
        assert_eq!(unreachable.networks().await, None);
        // A client to a daemon that does not answer: unknown on every pass.
        let down = DockerNetworks::Used {
            client: tokio::sync::Mutex::new(None),
            connect: Box::new(|| {
                bollard::Docker::connect_with_unix(
                    "/nonexistent/temps-test/docker.sock",
                    1,
                    bollard::API_DEFAULT_VERSION,
                )
                .ok()
            }),
        };
        assert_eq!(down.networks().await, None);
        assert_eq!(down.networks().await, None);
    }

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
