// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Connectors the control plane runs itself (ADR-107 D2).
//!
//! A worker's agent pulls its connector specs over HTTP, runs them with its
//! supervisor and posts their status back. The control plane does the same
//! against the service directly: [`ControlPlaneConnectors::sync`] reads node
//! `0`'s specs, hands the ones its proxy can carry to a
//! [`LocalConnectorRuntime`] (the node agent's supervisor, wired in by the
//! binary) and records what it reports in the same `ingress_connector_nodes`
//! rows the console reads for workers.
//!
//! The connector forwards to the proxy's tunnel origin, which the proxy
//! binds at startup ([`ControlPlaneTunnelOrigin`]) on the first free port
//! from the default `edge_port` (a worker agent on the same machine holds
//! that one). A tunnel placed only on the control plane is moved to that
//! port first ([`IngressConnectorService::align_control_plane_edge_ports`]).
//! A spec is only started once the listener is bound on the spec's
//! `edge_port`; otherwise its status says why, so a `cloudflared` never
//! reports ready while every request through it fails.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use temps_core::tunnel_origin::ControlPlaneTunnelOrigin;
use temps_core::CONTROL_PLANE_NODE_ID;
use tokio::sync::Notify;
use tracing::{debug, warn};

use crate::error::{ControlPlaneTunnelsUnavailable, IngressError};
use crate::service::IngressConnectorService;
use crate::wire::{ConnectorStatus, NodeConnectorSpec, NodeConnectorStatusReport};

/// How often the control plane re-reads its specs and reports status when
/// nothing wakes it: the agent's interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Runs connector specs on this host.
#[async_trait]
pub trait LocalConnectorRuntime: Send + Sync {
    /// Converge on `specs`: start new connectors, restart changed ones and
    /// stop the rest. Returns a status for every spec it could not take.
    async fn apply(&self, specs: Vec<NodeConnectorSpec>) -> Vec<ConnectorStatus>;

    /// How each connector it runs is doing.
    async fn statuses(&self) -> Vec<ConnectorStatus>;
}

/// What the service keeps of the control plane's connectors: whether it can
/// run them, and how to make it look again now.
#[derive(Clone)]
pub struct ControlPlaneHandle {
    origin: Arc<ControlPlaneTunnelOrigin>,
    wake: Arc<Notify>,
}

impl ControlPlaneHandle {
    pub fn new(origin: Arc<ControlPlaneTunnelOrigin>, wake: Arc<Notify>) -> Self {
        Self { origin, wake }
    }

    /// A handle on `origin` with its own wake-up: one per process, shared
    /// by every service instance that places connectors on node 0.
    pub fn for_origin(origin: Arc<ControlPlaneTunnelOrigin>) -> Self {
        Self::new(origin, Arc::new(Notify::new()))
    }

    /// `Err` with why, and what to do, when the proxy cannot carry a tunnel.
    pub fn ready(&self) -> Result<(), ControlPlaneTunnelsUnavailable> {
        match self.origin.bound_port() {
            Some(_) => Ok(()),
            None => Err(origin_unbound()),
        }
    }

    /// The loopback port the proxy takes tunnel traffic on, once bound.
    pub fn bound_port(&self) -> Option<u16> {
        self.origin.bound_port()
    }

    /// Look at node 0's specs now rather than at the next poll.
    pub fn wake(&self) {
        self.wake.notify_one();
    }
}

pub struct ControlPlaneConnectors {
    service: Arc<IngressConnectorService>,
    runtime: Arc<dyn LocalConnectorRuntime>,
    origin: Arc<ControlPlaneTunnelOrigin>,
    wake: Arc<Notify>,
}

impl ControlPlaneConnectors {
    /// Attach to `service`, which from now on accepts node 0 in placements
    /// and wakes this loop when they change. Attach `handle` to any other
    /// instance that places connectors (the console's) for the same.
    pub fn attach(
        service: Arc<IngressConnectorService>,
        runtime: Arc<dyn LocalConnectorRuntime>,
        handle: ControlPlaneHandle,
    ) -> Arc<Self> {
        service.attach_control_plane(handle.clone());
        Arc::new(Self {
            service,
            runtime,
            origin: handle.origin,
            wake: handle.wake,
        })
    }

    /// One pass: run what is assigned to node 0 and record how it runs.
    pub async fn sync(&self) -> Result<(), IngressError> {
        if let Some(port) = self.origin.bound_port() {
            self.service.align_control_plane_edge_ports(port).await?;
        }
        let specs = self
            .service
            .node_specs(CONTROL_PLANE_NODE_ID)
            .await?
            .connectors;
        let (runnable, mut reports) = split_by_origin(specs, self.origin.bound_port());
        if let Some(header) = client_address_header(&runnable) {
            if let Err(error) = self.origin.set_client_address_header(header) {
                warn!(%error, "keeping the tunnel origin's client-address header");
            }
        }
        reports.extend(self.runtime.apply(runnable).await);
        reports.extend(self.runtime.statuses().await);
        if reports.is_empty() {
            return Ok(());
        }
        self.service
            .record_status(CONTROL_PLANE_NODE_ID, NodeConnectorStatusReport { reports })
            .await
    }

    /// Sync now, every [`POLL_INTERVAL`] and whenever a placement changes,
    /// until `shutdown`.
    pub fn start(self: &Arc<Self>, shutdown: Arc<Notify>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut last_error: Option<String> = None;
            loop {
                match this.sync().await {
                    Ok(()) => {
                        if last_error.take().is_some() {
                            debug!("control-plane connectors in sync again");
                        }
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if last_error.as_deref() != Some(error.as_str()) {
                            warn!(%error, "could not sync the control plane's ingress connectors; retrying");
                        }
                        last_error = Some(error);
                    }
                }
                tokio::select! {
                    _ = shutdown.notified() => {
                        this.runtime.apply(Vec::new()).await;
                        return;
                    }
                    _ = this.wake.notified() => {}
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                }
            }
        })
    }
}

/// Specs the proxy can carry now, and a `down` status for each other one.
fn split_by_origin(
    specs: Vec<NodeConnectorSpec>,
    bound: Option<u16>,
) -> (Vec<NodeConnectorSpec>, Vec<ConnectorStatus>) {
    let mut runnable = Vec::new();
    let mut held = Vec::new();
    for spec in specs {
        let reason = match bound {
            None => Some(origin_unbound().to_string()),
            Some(port) if port != spec.edge_port => Some(format!(
                "This tunnel sends visitors to port {}, but the control plane's proxy takes \
                 tunnel traffic on port {port} (port {} was taken when Temps started, often by \
                 a worker agent on this machine). A tunnel placed only on the control plane \
                 moves to that port by itself; one also placed on workers keeps theirs. Place \
                 it only on the control plane, or only on workers.",
                spec.edge_port, spec.edge_port
            )),
            Some(_) => None,
        };
        match reason {
            None => runnable.push(spec),
            Some(reason) => held.push(ConnectorStatus {
                connector_id: spec.id,
                driver: None,
                state: "down".into(),
                ready_connections: None,
                connector_ref: None,
                last_error: Some(reason),
                restarts: 0,
            }),
        }
    }
    (runnable, held)
}

fn origin_unbound() -> ControlPlaneTunnelsUnavailable {
    ControlPlaneTunnelsUnavailable::OriginUnbound {
        first: SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            temps_core::tunnel_origin::TUNNEL_ORIGIN_PORT,
        ),
        last_port: temps_core::tunnel_origin::TUNNEL_ORIGIN_LAST_PORT,
    }
}

/// The header the runnable connectors carry the visitor's address in. One
/// listener serves them all, so they must agree; the first one wins and a
/// disagreement is logged.
fn client_address_header(specs: &[NodeConnectorSpec]) -> Option<Option<&str>> {
    let first = specs.first()?.client_address_header.as_deref();
    if let Some(other) = specs
        .iter()
        .find(|s| s.client_address_header.as_deref() != first)
    {
        warn!(
            connector_id = other.id,
            header = ?other.client_address_header,
            kept = ?first,
            "control-plane connectors disagree on the client-address header; keeping the first"
        );
    }
    Some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn spec(id: i32, edge_port: u16, header: Option<&str>) -> NodeConnectorSpec {
        NodeConnectorSpec {
            id,
            provider: "cloudflare_tunnel".into(),
            revision: "r1".into(),
            edge_port,
            client_address_header: header.map(String::from),
            client_ja4: None,
            drivers: Vec::new(),
            secrets: BTreeMap::new(),
        }
    }

    #[test]
    fn nothing_starts_while_the_proxy_has_no_tunnel_origin() {
        let (runnable, held) = split_by_origin(vec![spec(3, 7480, None)], None);
        assert!(runnable.is_empty());
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].connector_id, 3);
        assert_eq!(held[0].state, "down");
        let why = held[0].last_error.as_deref().unwrap();
        assert!(why.contains("127.0.0.1:7480"), "{why}");
        assert!(why.contains("--role console"), "{why}");
    }

    #[test]
    fn a_spec_for_another_port_is_held_with_the_port_the_proxy_listens_on() {
        let (runnable, held) =
            split_by_origin(vec![spec(3, 7480, None), spec(4, 8443, None)], Some(7480));
        assert_eq!(runnable.iter().map(|s| s.id).collect::<Vec<_>>(), vec![3]);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].connector_id, 4);
        let why = held[0].last_error.as_deref().unwrap();
        assert!(
            why.contains("port 8443") && why.contains("port 7480"),
            "{why}"
        );
    }

    #[test]
    fn the_first_runnable_spec_names_the_client_address_header() {
        assert_eq!(client_address_header(&[]), None);
        assert_eq!(
            client_address_header(&[
                spec(1, 7480, Some("CF-Connecting-IP")),
                spec(2, 7480, Some("X-Other"))
            ]),
            Some(Some("CF-Connecting-IP"))
        );
        assert_eq!(client_address_header(&[spec(1, 7480, None)]), Some(None));
    }

    #[test]
    fn the_handle_is_ready_once_the_proxy_bound_its_origin() {
        let origin = ControlPlaneTunnelOrigin::new();
        let handle = ControlPlaneHandle::new(origin.clone(), Arc::new(Notify::new()));
        let why = handle.ready().unwrap_err();
        assert_eq!(
            why,
            ControlPlaneTunnelsUnavailable::OriginUnbound {
                first: "127.0.0.1:7480".parse().unwrap(),
                last_port: 7489,
            }
        );
        assert!(
            why.to_string().contains("from 127.0.0.1:7480 to 7489"),
            "{why}"
        );
        assert_eq!(handle.bound_port(), None);
        // Any port of the range will do: a worker agent on this machine
        // may hold the first.
        origin.mark_bound(7481);
        handle.ready().expect("bound");
        assert_eq!(handle.bound_port(), Some(7481));
    }
}
