// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Serving the agent API on this node's WireGuard mesh address.
//!
//! A control plane that can only reach its workers through the mesh (one
//! behind NAT, with outbound access limited to HTTPS) cannot call an agent at
//! its registered address. So when the agent serves mutual TLS and the mesh is
//! up, it also serves the same router, with the same TLS configuration and
//! the same authentication, on `<mesh address>:<agent port>`. The mesh
//! firewall admits that port from the control plane's mesh address only (see
//! [`mesh_agent_api`]), and the heartbeat reports the address once it is
//! served, which is what makes the control plane dial it
//! (`temps_core::node_address::node_agent_endpoint`). A change is reported at
//! once, not at the next heartbeat interval ([`MeshListenerReport`]).
//!
//! A plaintext agent is never exposed on another interface.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use ipnet::Ipv4Net;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::AgentConfig;

/// This node's mesh address, as the network-sync loop last determined it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeshAddress {
    /// Not determined yet: the agent just started.
    #[default]
    Unknown,
    /// The cluster has no mesh, or this node has no allocation.
    Off,
    /// This node's end of the mesh is up on this address.
    At(Ipv4Addr),
}

/// Published by the network-sync loop, read by the mesh listener.
pub type MeshAddressSender = watch::Sender<MeshAddress>;

/// Whether the agent API is served on the mesh, as the heartbeat reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeshListenerState {
    /// Not determined yet (the agent just started, or it is a plaintext
    /// agent, which never serves on the mesh): the key is left out of the
    /// heartbeat, and the control plane keeps what it last heard while the
    /// node's mesh address holds.
    #[default]
    Unknown,
    /// Not served on the mesh: no mesh, or the listener could not bind.
    Off,
    /// Served on this address.
    Serving(SocketAddr),
}

impl MeshListenerState {
    /// The heartbeat `capacity` value under
    /// `temps_core::node_address::AGENT_MESH_LISTENER_KEY`; `None` leaves the
    /// key out.
    pub fn heartbeat_value(self) -> Option<serde_json::Value> {
        match self {
            MeshListenerState::Unknown => None,
            MeshListenerState::Off => Some(serde_json::Value::Null),
            MeshListenerState::Serving(address) => {
                Some(serde_json::Value::String(address.to_string()))
            }
        }
    }
}

/// The mesh listener's state, shared with the heartbeat loop, which sends a
/// heartbeat as soon as it changes: a control plane that reaches this node
/// only through the mesh learns where at once, instead of up to a heartbeat
/// interval later with calls failing meanwhile.
#[derive(Debug, Default)]
pub struct MeshListenerReport {
    state: std::sync::RwLock<MeshListenerState>,
    changed: tokio::sync::Notify,
}

impl MeshListenerReport {
    pub fn get(&self) -> MeshListenerState {
        *self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record `state`, waking the heartbeat loop when it changed.
    pub fn set(&self, state: MeshListenerState) {
        let mut current = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *current != state {
            *current = state;
            // A stored permit: a heartbeat in flight is followed by another.
            self.changed.notify_one();
        }
    }

    /// Resolves once the state changed since the last call.
    pub async fn changed(&self) {
        self.changed.notified().await
    }
}

/// Shared between the mesh listener and the heartbeat loop.
pub type SharedMeshListener = Arc<MeshListenerReport>;

/// How long to wait before retrying a mesh address the listener could not
/// bind (the address may not be on the interface yet).
const BIND_RETRY: Duration = Duration::from_secs(30);

/// Whether the agent serves mutual TLS: it does exactly when it has a leaf,
/// its key and the cluster CA (see `server::start_agent_server`).
pub fn serves_mtls(config: &AgentConfig) -> bool {
    config.tls_cert_path.is_some()
        && config.tls_key_path.is_some()
        && config.cluster_ca_path.is_some()
}

/// The agent API port, from its listen address.
fn agent_port(listen_address: &str) -> Option<u16> {
    listen_address
        .trim()
        .parse::<SocketAddr>()
        .ok()
        .map(|address| address.port())
        .filter(|port| *port != 0)
}

/// The mesh firewall's opening for the agent API: the agent port, from the
/// control plane's mesh address only. `None` for a plaintext agent, which is
/// never reachable over the mesh.
pub fn mesh_agent_api(
    config: &AgentConfig,
    mesh: Ipv4Net,
) -> Option<temps_network::mesh::MeshAgentApi> {
    if !serves_mtls(config) {
        return None;
    }
    Some(temps_network::mesh::MeshAgentApi {
        from: temps_network::mesh::control_plane_mesh_address(mesh),
        port: agent_port(&config.listen_address)?,
    })
}

/// What serving the agent API on the mesh address takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshListen {
    /// Not served on the mesh: plaintext agent, no mesh, or an unusable
    /// listen address.
    Off,
    /// The main listener already accepts on the mesh address (it is bound to
    /// all interfaces, or to the mesh address itself).
    Covered(SocketAddr),
    /// A second listener on this address.
    Bind(SocketAddr),
}

impl MeshListen {
    /// The address the agent API is reachable at over the mesh.
    pub fn address(self) -> Option<SocketAddr> {
        match self {
            MeshListen::Off => None,
            MeshListen::Covered(address) | MeshListen::Bind(address) => Some(address),
        }
    }
}

/// Decide how the agent API is served on the mesh address `mesh`, given the
/// main listener's `listen_address` and whether the agent serves mutual TLS.
pub fn mesh_listen_plan(listen_address: &str, mtls: bool, mesh: Option<Ipv4Addr>) -> MeshListen {
    let (true, Some(mesh)) = (mtls, mesh) else {
        return MeshListen::Off;
    };
    let Ok(listen) = listen_address.trim().parse::<SocketAddr>() else {
        return MeshListen::Off;
    };
    if listen.port() == 0 {
        return MeshListen::Off;
    }
    let on_mesh = SocketAddr::new(IpAddr::V4(mesh), listen.port());
    if listen.ip().is_unspecified() || listen.ip() == IpAddr::V4(mesh) {
        MeshListen::Covered(on_mesh)
    } else {
        MeshListen::Bind(on_mesh)
    }
}

/// Keep the agent API served on the mesh address as it comes, changes and
/// goes. Runs for the agent's lifetime; only spawned for an mTLS agent.
pub fn spawn(
    router: Router,
    tls: Arc<rustls::ServerConfig>,
    listen_address: String,
    mut mesh: watch::Receiver<MeshAddress>,
    serving: SharedMeshListener,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // `None` until the mesh address is known: nothing is reported then.
        let mut applied: Option<MeshListen> = None;
        let mut listener: Option<tokio::task::JoinHandle<()>> = None;
        let mut retry = false;
        loop {
            let address = match *mesh.borrow_and_update() {
                MeshAddress::Unknown => None,
                MeshAddress::Off => Some(None),
                MeshAddress::At(address) => Some(Some(address)),
            };
            let desired = address.map(|address| mesh_listen_plan(&listen_address, true, address));
            if let Some(desired) = desired.filter(|desired| applied != Some(*desired) || retry) {
                if let Some(task) = listener.take() {
                    task.abort();
                    if let Some(previous) = applied.and_then(MeshListen::address) {
                        info!(
                            address = %previous,
                            "stopped serving the agent API on the previous WireGuard mesh address"
                        );
                    }
                }
                retry = false;
                match desired {
                    MeshListen::Off => {
                        if applied.is_some_and(|applied| applied != MeshListen::Off) {
                            info!("the WireGuard mesh is gone; the agent API is no longer served on it");
                        }
                        serving.set(MeshListenerState::Off);
                    }
                    MeshListen::Covered(address) => {
                        info!(
                            %address,
                            listen_address = %listen_address,
                            "the agent API is reachable on the WireGuard mesh address through its \
                             main listener"
                        );
                        serving.set(MeshListenerState::Serving(address));
                    }
                    MeshListen::Bind(address) => {
                        match tokio::net::TcpListener::bind(address).await {
                            Ok(bound) => {
                                info!(
                                    %address,
                                    "serving the agent API with mutual TLS on the WireGuard mesh \
                                     address too, for a control plane that reaches this node \
                                     only through the mesh"
                                );
                                let (router, tls) = (router.clone(), tls.clone());
                                listener = Some(tokio::spawn(async move {
                                    if let Err(error) =
                                        crate::server::serve_mtls(bound, router, tls).await
                                    {
                                        warn!(
                                            %address,
                                            error = %error,
                                            "the agent API listener on the mesh address stopped"
                                        );
                                    }
                                }));
                                serving.set(MeshListenerState::Serving(address));
                            }
                            Err(error) => {
                                warn!(
                                    %address,
                                    error = %error,
                                    retry_in_secs = BIND_RETRY.as_secs(),
                                    "could not serve the agent API on the WireGuard mesh address; \
                                     the control plane keeps calling the registered address \
                                     meanwhile"
                                );
                                serving.set(MeshListenerState::Off);
                                retry = true;
                            }
                        }
                    }
                }
                applied = Some(desired);
            }
            tokio::select! {
                changed = mesh.changed() => {
                    if changed.is_err() {
                        // The network-sync loop is gone; keep what is served.
                        return;
                    }
                }
                _ = tokio::time::sleep(BIND_RETRY), if retry => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh() -> Option<Ipv4Addr> {
        Some("10.201.0.5".parse().unwrap())
    }

    #[test]
    fn a_loopback_agent_adds_a_listener_on_its_mesh_address() {
        assert_eq!(
            mesh_listen_plan("127.0.0.1:3100", true, mesh()),
            MeshListen::Bind("10.201.0.5:3100".parse().unwrap())
        );
        assert_eq!(
            mesh_listen_plan("203.0.113.10:3443", true, mesh()),
            MeshListen::Bind("10.201.0.5:3443".parse().unwrap())
        );
    }

    #[test]
    fn a_listener_that_already_accepts_there_is_not_doubled() {
        for listen in ["0.0.0.0:3100", "[::]:3100", "10.201.0.5:3100"] {
            assert_eq!(
                mesh_listen_plan(listen, true, mesh()),
                MeshListen::Covered("10.201.0.5:3100".parse().unwrap()),
                "{listen}"
            );
        }
    }

    #[test]
    fn plaintext_agents_and_nodes_off_the_mesh_are_never_served_there() {
        assert_eq!(
            mesh_listen_plan("127.0.0.1:3100", false, mesh()),
            MeshListen::Off
        );
        assert_eq!(
            mesh_listen_plan("0.0.0.0:3100", false, mesh()),
            MeshListen::Off
        );
        assert_eq!(
            mesh_listen_plan("127.0.0.1:3100", true, None),
            MeshListen::Off
        );
        assert_eq!(
            mesh_listen_plan("not-an-address", true, mesh()),
            MeshListen::Off
        );
        assert_eq!(
            mesh_listen_plan("127.0.0.1:0", true, mesh()),
            MeshListen::Off
        );
    }

    #[test]
    fn only_an_mtls_agent_opens_the_mesh_firewall_and_only_to_the_control_plane() {
        let mut config = crate::server::tests::test_agent_config(false);
        let pool: Ipv4Net = "10.201.0.0/16".parse().unwrap();
        assert_eq!(mesh_agent_api(&config, pool), None);

        config.tls_cert_path = Some("node.pem".into());
        config.tls_key_path = Some("node.key".into());
        config.cluster_ca_path = Some("ca.pem".into());
        assert_eq!(
            mesh_agent_api(&config, pool),
            Some(temps_network::mesh::MeshAgentApi {
                from: "10.201.0.1".parse().unwrap(),
                port: 3100,
            })
        );

        config.listen_address = "garbage".into();
        assert_eq!(mesh_agent_api(&config, pool), None);
    }

    fn mtls_server_config(dir: &std::path::Path) -> Arc<rustls::ServerConfig> {
        // The agent binary installs the process provider at startup.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = temps_core::node_pki::generate_cluster_ca().unwrap();
        let csr = temps_core::node_pki::generate_node_keypair_csr("worker", &[]).unwrap();
        let leaf = temps_core::node_pki::sign_node_csr(
            &ca.cert_pem,
            &ca.key_pem,
            &csr.csr_pem,
            &["worker".to_string()],
        )
        .unwrap();
        let (cert, key, ca_path) = (
            dir.join("node.pem"),
            dir.join("node.key"),
            dir.join("ca.pem"),
        );
        std::fs::write(&cert, leaf.cert_pem).unwrap();
        std::fs::write(&key, csr.key_pem).unwrap();
        std::fs::write(&ca_path, ca.cert_pem).unwrap();
        Arc::new(crate::server::build_tls_server_config(&cert, &key, &ca_path).unwrap())
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    async fn accepts(address: SocketAddr) -> bool {
        tokio::net::TcpStream::connect(address).await.is_ok()
    }

    #[tokio::test]
    async fn the_listener_follows_the_mesh_address() {
        let dir = tempfile::tempdir().unwrap();
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let (sender, receiver) = watch::channel(MeshAddress::Unknown);
        let serving: SharedMeshListener = Arc::default();
        // The main listener is on another address, so the mesh address gets a
        // listener of its own; loopback stands in for the mesh address.
        let task = spawn(
            Router::new(),
            mtls_server_config(dir.path()),
            format!("192.0.2.1:{port}"),
            receiver,
            serving.clone(),
        );
        let on_mesh: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        settle().await;
        assert_eq!(
            serving.get(),
            MeshListenerState::Unknown,
            "nothing is reported before the mesh address is known"
        );
        assert!(!accepts(on_mesh).await);

        sender.send_replace(MeshAddress::At(Ipv4Addr::LOCALHOST));
        // The heartbeat is woken by the bind, not by its interval.
        tokio::time::timeout(Duration::from_secs(5), serving.changed())
            .await
            .expect("binding the mesh listener wakes the heartbeat");
        assert_eq!(serving.get(), MeshListenerState::Serving(on_mesh));
        assert!(accepts(on_mesh).await, "served once the mesh is up");

        sender.send_replace(MeshAddress::Off);
        tokio::time::timeout(Duration::from_secs(5), serving.changed())
            .await
            .expect("losing the mesh wakes the heartbeat");
        assert_eq!(serving.get(), MeshListenerState::Off);
        settle().await;
        assert!(!accepts(on_mesh).await, "stopped once the mesh is gone");

        sender.send_replace(MeshAddress::At(Ipv4Addr::LOCALHOST));
        tokio::time::timeout(Duration::from_secs(5), serving.changed())
            .await
            .expect("rebinding wakes the heartbeat");
        assert!(accepts(on_mesh).await, "served again when the mesh returns");
        task.abort();
    }

    #[tokio::test]
    async fn only_a_change_wakes_the_heartbeat() {
        let report = MeshListenerReport::default();
        let address: SocketAddr = "10.201.0.5:3100".parse().unwrap();
        report.set(MeshListenerState::Serving(address));
        tokio::time::timeout(Duration::from_secs(1), report.changed())
            .await
            .expect("a change set before the heartbeat waits is not lost");
        report.set(MeshListenerState::Serving(address));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), report.changed())
                .await
                .is_err(),
            "the same state again sends no extra heartbeat"
        );
    }

    #[test]
    fn the_heartbeat_tells_unknown_from_not_served() {
        assert_eq!(MeshListenerState::Unknown.heartbeat_value(), None);
        assert_eq!(
            MeshListenerState::Off.heartbeat_value(),
            Some(serde_json::Value::Null)
        );
        assert_eq!(
            MeshListenerState::Serving("10.201.0.5:3100".parse().unwrap()).heartbeat_value(),
            Some(serde_json::json!("10.201.0.5:3100"))
        );
    }
}
