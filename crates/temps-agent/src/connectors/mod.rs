// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress connectors on the node (ADR 058).
//!
//! The control plane says *what* to run ([`wire::NodeConnectorSpec`]); this
//! module decides *how*. A [`ConnectorDriver`] knows one way of running a
//! connector: a pinned binary as a child process, an image as a container, or
//! a connector compiled into the agent. The [`ConnectorSupervisor`] is the
//! same for all of them: it picks the first driver in the spec's preference
//! list this node supports, restarts with backoff, and reports status. No
//! driver reimplements any of that.
//!
//! Traffic reaches the node's edge through an [`EdgeHandoff`]: process and
//! container drivers forward to the loopback tunnel origin listener, which
//! trusts the provider's client-address header from loopback only.

pub mod process;
pub mod sync;
pub mod wire;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::route_store::SharedRouteStore;
use wire::{ConnectorStatus, DriverChoice, NodeConnectorSpec};

/// How a driver runs a connector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverKind {
    Process,
    Container,
    Library(String),
}

impl DriverKind {
    pub fn label(&self) -> String {
        match self {
            DriverKind::Process => "process".into(),
            DriverKind::Container => "container".into(),
            DriverKind::Library(name) => format!("library:{name}"),
        }
    }
}

/// How visitor traffic reaches the node's edge from a running connector.
pub enum EdgeHandoff {
    /// The connector forwards plain HTTP to the loopback tunnel origin
    /// listener; `client_address_header` is trusted from loopback only.
    Loopback {
        addr: SocketAddr,
        client_address_header: Option<String>,
    },
    /// A compiled-in connector hands accepted streams over directly, with the
    /// visitor's real address. Reserved for the first library driver.
    Direct(tokio::sync::mpsc::Sender<DirectConnection>),
}

pub struct DirectConnection {
    pub stream: tokio::net::TcpStream,
    pub client_address: Option<IpAddr>,
}

/// Everything a driver needs to start one connector.
pub struct ResolvedSpec {
    pub connector_id: i32,
    pub provider: String,
    pub choice: DriverChoice,
    pub secrets: std::collections::BTreeMap<String, String>,
    /// Per-connector directory for secret files and state.
    pub work_dir: PathBuf,
    /// Where downloaded artifacts are cached, by sha256.
    pub bin_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum ConnectorHealth {
    Starting,
    Ready {
        connections: Option<i32>,
        connector_ref: Option<String>,
    },
    Degraded(String),
    Exited(String),
}

#[async_trait]
pub trait ConnectorDriver: Send + Sync {
    fn kind(&self) -> DriverKind;

    /// Whether this driver handles `choice` at all.
    fn accepts(&self, choice: &DriverChoice) -> bool;

    /// Whether this node can run `choice` with this driver; the reason when
    /// it cannot, for the operator.
    fn available(&self, choice: &DriverChoice) -> Result<(), String>;

    async fn start(
        &self,
        spec: &ResolvedSpec,
        edge: &EdgeHandoff,
    ) -> Result<Box<dyn RunningConnector>, String>;
}

#[async_trait]
pub trait RunningConnector: Send {
    async fn health(&mut self) -> ConnectorHealth;
    async fn stop(self: Box<Self>, grace: Duration);
}

struct Running {
    revision: String,
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
    status: Arc<Mutex<ConnectorStatus>>,
}

pub struct ConnectorSupervisor {
    drivers: Vec<Arc<dyn ConnectorDriver>>,
    data_dir: PathBuf,
    store: SharedRouteStore,
    shutdown: Arc<Notify>,
    running: tokio::sync::Mutex<HashMap<i32, Running>>,
    /// Connectors held back because their tunnel origin could not bind,
    /// with the status that says why. Retried on every [`Self::apply`].
    blocked: Mutex<HashMap<i32, ConnectorStatus>>,
    listeners: tokio::sync::Mutex<HashSet<u16>>,
}

impl ConnectorSupervisor {
    pub fn new(data_dir: PathBuf, store: SharedRouteStore, shutdown: Arc<Notify>) -> Arc<Self> {
        let drivers: Vec<Arc<dyn ConnectorDriver>> = vec![Arc::new(process::ProcessDriver::new())];
        Self::with_drivers(drivers, data_dir, store, shutdown)
    }

    fn with_drivers(
        drivers: Vec<Arc<dyn ConnectorDriver>>,
        data_dir: PathBuf,
        store: SharedRouteStore,
        shutdown: Arc<Notify>,
    ) -> Arc<Self> {
        Arc::new(Self {
            drivers,
            data_dir,
            store,
            shutdown,
            running: tokio::sync::Mutex::new(HashMap::new()),
            blocked: Mutex::new(HashMap::new()),
            listeners: tokio::sync::Mutex::new(HashSet::new()),
        })
    }

    /// Converge on `specs`: start new connectors, restart changed ones and
    /// stop the ones no longer assigned.
    ///
    /// A connector starts only once its tunnel origin listener is bound: a
    /// provider connector with nowhere to forward would report ready while
    /// every request fails. A bind failure is reported as the connector's
    /// status and retried on the next call, so freeing the port recovers
    /// without restarting the agent.
    pub async fn apply(self: &Arc<Self>, specs: Vec<NodeConnectorSpec>) {
        let wanted: HashMap<i32, NodeConnectorSpec> =
            specs.into_iter().map(|spec| (spec.id, spec)).collect();
        let mut running = self.running.lock().await;

        let stale: Vec<i32> = running
            .iter()
            .filter(|(id, run)| {
                wanted
                    .get(id)
                    .is_none_or(|spec| spec.revision != run.revision)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(run) = running.remove(&id) {
                info!(connector_id = id, "stopping ingress connector");
                run.stop.notify_one();
                let _ = run.task.await;
            }
        }

        self.blocked.lock().retain(|id, _| wanted.contains_key(id));

        for (id, spec) in wanted {
            if let Err(error) = self.ensure_listener(&spec).await {
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), spec.edge_port);
                warn!(connector_id = id, %addr, %error, "tunnel origin listener failed to bind");
                if let Some(run) = running.remove(&id) {
                    run.stop.notify_one();
                    let _ = run.task.await;
                }
                let mut blocked = self.blocked.lock();
                let restarts = blocked.get(&id).map_or(0, |s| s.restarts + 1);
                blocked.insert(
                    id,
                    ConnectorStatus {
                        connector_id: id,
                        driver: None,
                        state: "down".into(),
                        ready_connections: None,
                        connector_ref: None,
                        last_error: Some(format!(
                            "The tunnel origin could not listen on {addr} ({error}), so the \
                             connector was not started. Another process on this worker is \
                             probably using port {port}; stop it (`ss -ltnp 'sport = :{port}'` \
                             shows which). The agent retries on its next sync.",
                            port = spec.edge_port,
                        )),
                        restarts,
                    },
                );
                continue;
            }
            if self.blocked.lock().remove(&id).is_some() {
                info!(
                    connector_id = id,
                    "tunnel origin listener bound; starting the connector"
                );
            }
            if running.contains_key(&id) {
                continue;
            }
            let status = Arc::new(Mutex::new(ConnectorStatus {
                connector_id: id,
                driver: None,
                state: "starting".into(),
                ready_connections: None,
                connector_ref: None,
                last_error: None,
                restarts: 0,
            }));
            let stop = Arc::new(Notify::new());
            let revision = spec.revision.clone();
            let task = tokio::spawn(Arc::clone(self).run_connector(
                spec,
                Arc::clone(&stop),
                Arc::clone(&status),
            ));
            running.insert(
                id,
                Running {
                    revision,
                    stop,
                    task,
                    status,
                },
            );
        }
    }

    pub async fn statuses(&self) -> Vec<ConnectorStatus> {
        let mut statuses: Vec<ConnectorStatus> = self
            .running
            .lock()
            .await
            .values()
            .map(|run| run.status.lock().clone())
            .collect();
        statuses.extend(self.blocked.lock().values().cloned());
        statuses
    }

    async fn ensure_listener(&self, spec: &NodeConnectorSpec) -> std::io::Result<()> {
        let mut listeners = self.listeners.lock().await;
        if listeners.contains(&spec.edge_port) {
            return Ok(());
        }
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), spec.edge_port);
        crate::internal_proxy::spawn_tunnel_origin(
            addr,
            self.store.clone(),
            self.shutdown.clone(),
            spec.client_address_header.clone(),
        )
        .await?;
        info!(%addr, "tunnel origin listener started");
        listeners.insert(spec.edge_port);
        Ok(())
    }

    fn choose(
        &self,
        spec: &NodeConnectorSpec,
    ) -> Result<(Arc<dyn ConnectorDriver>, DriverChoice), String> {
        let mut reasons = Vec::new();
        for choice in &spec.drivers {
            for driver in &self.drivers {
                if !driver.accepts(choice) {
                    continue;
                }
                match driver.available(choice) {
                    Ok(()) => return Ok((Arc::clone(driver), choice.clone())),
                    Err(reason) => reasons.push(format!("{}: {reason}", driver.kind().label())),
                }
            }
        }
        if reasons.is_empty() {
            reasons.push("this agent has no driver for any of the offered kinds".into());
        }
        Err(format!(
            "No driver on this node can run the {} connector ({}). Upgrade the agent.",
            spec.provider,
            reasons.join("; ")
        ))
    }

    async fn run_connector(
        self: Arc<Self>,
        spec: NodeConnectorSpec,
        stop: Arc<Notify>,
        status: Arc<Mutex<ConnectorStatus>>,
    ) {
        let id = spec.id;
        let set = |state: &str, error: Option<String>| {
            let mut s = status.lock();
            s.state = state.into();
            if error.is_some() {
                s.last_error = error;
            }
        };
        let edge = EdgeHandoff::Loopback {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), spec.edge_port),
            client_address_header: spec.client_address_header.clone(),
        };
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = match self.choose(&spec) {
                Err(reason) => Err(reason),
                Ok((driver, choice)) => {
                    status.lock().driver = Some(driver.kind().label());
                    let resolved = ResolvedSpec {
                        connector_id: id,
                        provider: spec.provider.clone(),
                        choice,
                        secrets: spec.secrets.clone(),
                        work_dir: self.data_dir.join("connectors").join(id.to_string()),
                        bin_dir: self.data_dir.join("connectors").join("bin"),
                    };
                    set("starting", None);
                    driver.start(&resolved, &edge).await
                }
            };
            match started {
                Err(reason) => {
                    warn!(connector_id = id, %reason, "ingress connector could not start");
                    set("down", Some(reason));
                }
                Ok(mut running) => {
                    info!(connector_id = id, "ingress connector started");
                    loop {
                        tokio::select! {
                            _ = stop.notified() => {
                                running.stop(Duration::from_secs(35)).await;
                                return;
                            }
                            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                        }
                        match running.health().await {
                            ConnectorHealth::Starting => set("starting", None),
                            ConnectorHealth::Ready {
                                connections,
                                connector_ref,
                            } => {
                                let mut s = status.lock();
                                if s.state != "ready" {
                                    info!(
                                        connector_id = id,
                                        ?connections,
                                        "ingress connector ready"
                                    );
                                }
                                s.state = "ready".into();
                                s.ready_connections = connections;
                                s.connector_ref = connector_ref;
                                s.last_error = None;
                                backoff = Duration::from_secs(1);
                            }
                            ConnectorHealth::Degraded(reason) => set("degraded", Some(reason)),
                            ConnectorHealth::Exited(reason) => {
                                warn!(connector_id = id, %reason, "ingress connector exited");
                                set("down", Some(reason));
                                break;
                            }
                        }
                    }
                }
            }
            status.lock().restarts += 1;
            tokio::select! {
                _ = stop.notified() => return,
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_store::RouteStore;

    fn spec(id: i32, edge_port: u16) -> NodeConnectorSpec {
        NodeConnectorSpec {
            id,
            provider: "cloudflare".into(),
            revision: "r1".into(),
            edge_port,
            client_address_header: None,
            drivers: Vec::new(),
            secrets: Default::default(),
        }
    }

    #[tokio::test]
    async fn failed_origin_bind_holds_the_connector_and_recovers_once_the_port_is_free() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(RouteStore::new(dir.path().join("routes.json")));
        let shutdown = Arc::new(Notify::new());
        // No drivers: once started, the connector reports "no driver" rather
        // than launching anything, which is all this test needs.
        let supervisor = ConnectorSupervisor::with_drivers(
            Vec::new(),
            dir.path().to_path_buf(),
            store,
            Arc::clone(&shutdown),
        );

        let squatter = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = squatter.local_addr().unwrap().port();

        supervisor.apply(vec![spec(7, port)]).await;
        assert!(supervisor.running.lock().await.is_empty());
        let statuses = supervisor.statuses().await;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].connector_id, 7);
        assert_eq!(statuses[0].state, "down");
        let error = statuses[0].last_error.as_deref().unwrap();
        assert!(error.contains(&format!("127.0.0.1:{port}")), "{error}");

        // Still taken: still held, and counted as another failed attempt.
        supervisor.apply(vec![spec(7, port)]).await;
        assert!(supervisor.running.lock().await.is_empty());
        assert_eq!(supervisor.statuses().await[0].restarts, 1);

        drop(squatter);
        supervisor.apply(vec![spec(7, port)]).await;
        assert!(supervisor.blocked.lock().is_empty());
        assert!(supervisor.running.lock().await.contains_key(&7));
        assert!(supervisor.listeners.lock().await.contains(&port));
        let statuses = supervisor.statuses().await;
        assert_eq!(statuses.len(), 1);
        assert!(statuses[0]
            .last_error
            .as_deref()
            .is_none_or(|e| !e.contains("could not listen")));

        supervisor.apply(Vec::new()).await;
        assert!(supervisor.statuses().await.is_empty());
        shutdown.notify_waiters();
    }

    #[tokio::test]
    async fn unassigned_blocked_connector_stops_being_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(RouteStore::new(dir.path().join("routes.json")));
        let supervisor = ConnectorSupervisor::with_drivers(
            Vec::new(),
            dir.path().to_path_buf(),
            store,
            Arc::new(Notify::new()),
        );
        let squatter = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = squatter.local_addr().unwrap().port();

        supervisor.apply(vec![spec(9, port)]).await;
        assert_eq!(supervisor.statuses().await.len(), 1);
        supervisor.apply(Vec::new()).await;
        assert!(supervisor.statuses().await.is_empty());
    }
}
