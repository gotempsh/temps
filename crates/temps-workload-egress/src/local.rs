// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's egress recorder: the per-node egress proxy for the
//! workloads that run on this host and record their outbound connections.
//!
//! **One proxy, one table, one recording channel**, whatever the workload
//! kinds. What differs by kind is how its traffic gets here, which the kind
//! says through a [`LocalTraffic`]:
//!
//! - where on this host its workloads reach the recorder (a sandbox's
//!   sidecar on the sandbox egress network, an environment's containers on
//!   the deployment network);
//! - which workloads record, the policy each is held to and the source
//!   addresses that speak for it;
//! - what to do when the recorder starts or stops serving it (re-point a
//!   sidecar, stop handing traffic over).
//!
//! **Who is calling** comes from the source address alone: each source
//! names the addresses Docker assigned to its recorded workloads, and an
//! address no recorded workload holds is refused.
//!
//! **Linux only**, where the control plane runs its workloads' Docker; node
//! 0 reports that it can record only while the proxy serves somewhere.
//!
//! **Its port is stable** ([`RECORDING_PROXY_PORT`]): workloads carry the
//! address (a sidecar's hand-off, a container's proxy variables), so a port
//! that changed on every restart would mean replacing them on every
//! restart. Only when the port is taken does a listener take any free one,
//! saying so.
//!
//! **Cost.** Per source, each refresh is whatever the source reads (one
//! query for its opted-in workloads; only when there are some, one local
//! container list). Every [`REFRESH_INTERVAL`], and at once after an opt-in
//! changes. Memory is the proxy's fixed bound plus one table entry per
//! recorded source address.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use temps_workload::WorkloadKind;
use temps_workload_netpol::{
    recording_channel, EgressBatcher, EgressPolicy, EgressProxy, EgressProxyConfig,
    EgressRecordingCapability, EgressTable, IdentityMap,
};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::service::EgressRecordingService;
use crate::sink::LocalRecordSink;

/// How often the table follows the sources without being told.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// The port the recorder listens on, on each address a source names.
pub const RECORDING_PROXY_PORT: u16 = 13128;

/// One kind's recorded workloads on this host, as the proxy needs them.
#[derive(Debug, Clone, Default)]
pub struct RecordedTraffic {
    /// The policy each recorded workload is held to, with `record` set.
    pub policies: Vec<EgressPolicy>,
    /// The source addresses that speak for each of them.
    pub identities: IdentityMap,
}

/// How one workload kind's traffic reaches the recorder on this host.
#[async_trait]
pub trait LocalTraffic: Send + Sync {
    fn kind(&self) -> WorkloadKind;

    /// The address on this host this kind's workloads reach the recorder
    /// on (the host's address on a Docker network of theirs). `None` while
    /// that network does not exist yet.
    async fn listen_address(&self) -> Result<Option<IpAddr>, String>;

    /// Brings this kind's steering up to date with the recorder serving it
    /// at `address`, and returns its recorded workloads. Called on every
    /// refresh.
    async fn refresh(&self, address: SocketAddr) -> Result<RecordedTraffic, String>;

    /// The recorder began serving this kind at `address`.
    async fn started(&self, _address: SocketAddr) {}

    /// The recorder does not serve this kind (it stopped, could not start,
    /// or this host cannot record): stop sending it traffic.
    async fn stopped(&self) {}
}

/// The node-0 status setter: [`temps_network_policy::NetworkPolicyService`].
pub trait LocalCapability: Send + Sync {
    fn set(&self, capability: EgressRecordingCapability);
}

impl LocalCapability for temps_network_policy::NetworkPolicyService {
    fn set(&self, capability: EgressRecordingCapability) {
        self.set_local_egress_recording(capability);
    }
}

/// The proxy and its batcher, started with the first source.
struct Running {
    proxy: Arc<EgressProxy>,
}

/// The recorder, shared by its tasks and the kinds' opt-in hooks.
pub struct LocalEgressRecorder {
    service: Arc<EgressRecordingService>,
    capability: Arc<dyn LocalCapability>,
    table: Arc<ArcSwap<EgressTable>>,
    running: tokio::sync::OnceCell<Option<Running>>,
    sources: Mutex<Vec<Arc<dyn LocalTraffic>>>,
    /// Where each kind is served now.
    serving: Mutex<HashMap<WorkloadKind, SocketAddr>>,
    /// Each kind's last good answer, kept while a refresh of it fails.
    last: Mutex<HashMap<WorkloadKind, RecordedTraffic>>,
    /// What node 0 was last told about recording, to tell it only changes.
    reported: Mutex<Option<bool>>,
    wake: Notify,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl LocalEgressRecorder {
    /// A recorder that serves nothing until a source is added.
    pub fn new(
        service: Arc<EgressRecordingService>,
        capability: Arc<dyn LocalCapability>,
    ) -> Arc<Self> {
        Arc::new(Self {
            service,
            capability,
            table: Arc::new(ArcSwap::from_pointee(EgressTable::default())),
            running: tokio::sync::OnceCell::new(),
            sources: Mutex::new(Vec::new()),
            serving: Mutex::new(HashMap::new()),
            last: Mutex::new(HashMap::new()),
            reported: Mutex::new(None),
            wake: Notify::new(),
        })
    }

    /// Starts serving `source`'s kind in the background: it waits for the
    /// source's address to exist, then listens there. On a host that
    /// cannot record, the source is told so at once.
    pub async fn add_source(self: &Arc<Self>, source: Arc<dyn LocalTraffic>) {
        if !cfg!(target_os = "linux") {
            info!(
                os = std::env::consts::OS,
                kind = %source.kind(),
                "egress recording: the control plane records only on Linux"
            );
            source.stopped().await;
            return;
        }
        let Some(running) = self.running().await else {
            source.stopped().await;
            return;
        };
        let proxy = running.proxy.clone();
        let first = {
            let mut sources = lock(&self.sources);
            sources.push(source.clone());
            sources.len() == 1
        };
        if first {
            let recorder = self.clone();
            tokio::spawn(async move { recorder.refresh_loop().await });
        }
        let recorder = self.clone();
        tokio::spawn(async move { recorder.listen_loop(source, proxy).await });
    }

    /// The proxy and its batcher, created once. `None` when the proxy
    /// could not be built, which is logged.
    async fn running(self: &Arc<Self>) -> Option<&Running> {
        self.running
            .get_or_init(|| async {
                let (recorder, records) = recording_channel(
                    temps_workload_netpol::egress::record::RECORD_CHANNEL_CAPACITY,
                );
                // Listeners are bound per source; this address only names
                // the proxy in its own messages.
                let config = EgressProxyConfig::new(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    RECORDING_PROXY_PORT,
                ));
                match EgressProxy::with_recorder(config, self.table.clone(), Some(recorder)) {
                    Ok(proxy) => {
                        tokio::spawn(EgressBatcher::default().run(
                            records,
                            Arc::new(LocalRecordSink::new(self.service.clone())),
                            proxy.stats().clone(),
                        ));
                        Some(Running { proxy })
                    }
                    Err(error) => {
                        warn!(%error, "egress recording proxy could not be built; this host records nothing");
                        None
                    }
                }
            })
            .await
            .as_ref()
    }

    /// Where `kind` is served now, if it is.
    pub fn address(&self, kind: WorkloadKind) -> Option<SocketAddr> {
        lock(&self.serving).get(&kind).copied()
    }

    /// Asks for a refresh soon, without waiting for it.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Notes where `kind` is served, or that it no longer is.
    fn set_serving(&self, kind: WorkloadKind, address: Option<SocketAddr>) {
        match address {
            Some(address) => {
                lock(&self.serving).insert(kind, address);
            }
            None => {
                lock(&self.serving).remove(&kind);
                lock(&self.last).remove(&kind);
            }
        }
    }

    /// Tells node 0's capability report when the recorder starts or stops
    /// serving anything at all.
    fn report_capability(&self) {
        let serving = !lock(&self.serving).is_empty();
        let mut reported = lock(&self.reported);
        if *reported != Some(serving) {
            *reported = Some(serving);
            self.capability
                .set(EgressRecordingCapability::detect(serving));
        }
    }

    async fn listen_loop(self: Arc<Self>, source: Arc<dyn LocalTraffic>, proxy: Arc<EgressProxy>) {
        let kind = source.kind();
        let mut waiting_logged = false;
        loop {
            let host = match source.listen_address().await {
                Ok(Some(host)) => host,
                Ok(None) => {
                    if !waiting_logged {
                        info!(%kind, "egress recording waits for this kind's network on this host");
                        waiting_logged = true;
                    }
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                    continue;
                }
                Err(error) => {
                    warn!(%kind, %error, "egress recording: could not find where to listen");
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                    continue;
                }
            };
            match bind(host, RECORDING_PROXY_PORT).await {
                Ok(listener) => {
                    let address = listener
                        .local_addr()
                        .unwrap_or_else(|_| SocketAddr::new(host, RECORDING_PROXY_PORT));
                    // The table first, then the capability, then the
                    // source: nothing is steered here before it is known.
                    self.set_serving(kind, Some(address));
                    self.refresh().await;
                    self.report_capability();
                    info!(%kind, %address, "egress recording proxy serving the control plane's workloads");
                    // Accepting before the source is told: what it steers
                    // here must find the proxy already answering.
                    let serving = tokio::spawn(proxy.clone().serve(listener));
                    source.started(address).await;
                    self.wake();
                    match serving.await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            warn!(%kind, %address, %error, "egress recording proxy stopped accepting");
                        }
                        Err(error) => {
                            warn!(%kind, %address, %error, "egress recording proxy task ended");
                        }
                    }
                    warn!(%kind, %address, "egress recording proxy stopped; restarting it");
                    self.set_serving(kind, None);
                    self.report_capability();
                    source.stopped().await;
                    self.refresh().await;
                }
                Err(error) => {
                    warn!(%kind, %error, "egress recording proxy could not start");
                    // Workloads stop handing traffic to a proxy that is
                    // not there, so they keep their way out.
                    source.stopped().await;
                }
            }
            tokio::time::sleep(REFRESH_INTERVAL).await;
        }
    }

    async fn refresh_loop(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(REFRESH_INTERVAL) => {}
                _ = self.wake.notified() => {}
            }
            self.refresh().await;
        }
    }

    /// Rebuilds the table from every source that is being served. A source
    /// whose refresh fails keeps its last answer, so one failed read never
    /// drops its workloads from the table.
    pub async fn refresh(&self) {
        let sources: Vec<Arc<dyn LocalTraffic>> = lock(&self.sources).clone();
        let serving = lock(&self.serving).clone();
        let mut policies = Vec::new();
        let mut identities = IdentityMap::new();
        for source in sources {
            let kind = source.kind();
            let Some(address) = serving.get(&kind) else {
                continue;
            };
            let traffic = match source.refresh(*address).await {
                Ok(traffic) => {
                    lock(&self.last).insert(kind, traffic.clone());
                    traffic
                }
                Err(error) => {
                    warn!(%kind, %error, "egress recording: could not refresh this kind's recorded workloads");
                    lock(&self.last).get(&kind).cloned().unwrap_or_default()
                }
            };
            identities.extend(&traffic.identities);
            policies.extend(traffic.policies);
        }
        let table = EgressTable::build(&policies, &identities);
        debug!(
            recorded = policies.len(),
            sources = table.len(),
            "egress recording table refreshed"
        );
        self.table.store(Arc::new(table));
    }
}

/// A listener on `host`, on `port` unless it is taken, then on any free
/// port, saying so.
async fn bind(host: IpAddr, port: u16) -> Result<TcpListener, String> {
    let preferred = SocketAddr::new(host, port);
    match TcpListener::bind(preferred).await {
        Ok(listener) => Ok(listener),
        Err(error) => {
            warn!(
                address = %preferred,
                %error,
                "egress recording: the recorder's port is taken; listening on a free port, so \
                 recorded workloads are re-pointed on every restart"
            );
            TcpListener::bind(SocketAddr::new(host, 0))
                .await
                .map_err(|e| format!("could not listen on {host}: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use temps_workload::WorkloadRef;
    use temps_workload_netpol::rule::Action;
    use temps_workload_netpol::{EgressRule, EgressTarget, HostPattern};

    use crate::tests::{fixture, Fixture};

    #[derive(Default)]
    struct Capability(Mutex<Vec<bool>>);

    impl LocalCapability for Capability {
        fn set(&self, capability: EgressRecordingCapability) {
            self.0.lock().unwrap().push(capability.is_available());
        }
    }

    /// One recorded workload at one address, whose refresh can be failed.
    struct Source {
        kind: WorkloadKind,
        address: IpAddr,
        fail: AtomicBool,
    }

    impl Source {
        fn new(kind: WorkloadKind, address: &str) -> Arc<Self> {
            Arc::new(Self {
                kind,
                address: address.parse().unwrap(),
                fail: AtomicBool::new(false),
            })
        }
    }

    #[async_trait]
    impl LocalTraffic for Source {
        fn kind(&self) -> WorkloadKind {
            self.kind
        }

        async fn listen_address(&self) -> Result<Option<IpAddr>, String> {
            Ok(None)
        }

        async fn refresh(&self, _address: SocketAddr) -> Result<RecordedTraffic, String> {
            if self.fail.load(Ordering::SeqCst) {
                return Err("the container list timed out".into());
            }
            let subject = WorkloadRef::new(self.kind, 1);
            Ok(RecordedTraffic {
                policies: vec![EgressPolicy {
                    subject,
                    rules: vec![EgressRule {
                        id: 1,
                        target: EgressTarget::Host(HostPattern::parse("*").unwrap()),
                        ports: None,
                        action: Action::Allow,
                    }],
                    record: true,
                    public_only: true,
                }],
                identities: IdentityMap::new().with(subject, self.address),
            })
        }
    }

    fn recorder(f: &Fixture) -> (Arc<LocalEgressRecorder>, Arc<Capability>) {
        let (service, _) = f.service();
        let capability = Arc::new(Capability::default());
        (
            LocalEgressRecorder::new(service, capability.clone()),
            capability,
        )
    }

    /// One table for every kind: each served source contributes its
    /// workloads, a source that is not served contributes none, and a
    /// source whose refresh fails keeps the ones it had.
    #[tokio::test]
    async fn the_table_holds_every_served_kind_and_survives_a_failed_refresh() {
        let Some(f) = fixture().await else { return };
        let (recorder, capability) = recorder(&f);
        let sandboxes = Source::new(WorkloadKind::Sandbox, "172.30.0.5");
        let environments = Source::new(WorkloadKind::Deployment, "172.18.0.9");
        lock(&recorder.sources).extend([
            sandboxes.clone() as Arc<dyn LocalTraffic>,
            environments.clone() as Arc<dyn LocalTraffic>,
        ]);
        let listening: SocketAddr = "127.0.0.1:13128".parse().unwrap();
        let owner = |address: &str| {
            recorder
                .table
                .load()
                .lookup(address.parse().unwrap())
                .map(|rules| rules.workload.kind)
        };

        recorder.refresh().await;
        assert_eq!(owner("172.30.0.5"), None, "nothing is served yet");

        recorder.set_serving(WorkloadKind::Sandbox, Some(listening));
        recorder.refresh().await;
        recorder.report_capability();
        assert_eq!(owner("172.30.0.5"), Some(WorkloadKind::Sandbox));
        assert_eq!(owner("172.18.0.9"), None);

        recorder.set_serving(WorkloadKind::Deployment, Some(listening));
        recorder.refresh().await;
        recorder.report_capability();
        assert_eq!(owner("172.18.0.9"), Some(WorkloadKind::Deployment));

        sandboxes.fail.store(true, Ordering::SeqCst);
        recorder.refresh().await;
        assert_eq!(
            owner("172.30.0.5"),
            Some(WorkloadKind::Sandbox),
            "a failed refresh keeps the last answer"
        );

        recorder.set_serving(WorkloadKind::Sandbox, None);
        recorder.refresh().await;
        recorder.report_capability();
        assert_eq!(owner("172.30.0.5"), None, "a kind no longer served is gone");
        assert_eq!(owner("172.18.0.9"), Some(WorkloadKind::Deployment));
        recorder.set_serving(WorkloadKind::Deployment, None);
        recorder.report_capability();
        assert_eq!(
            *capability.0.lock().unwrap(),
            vec![true, false],
            "node 0 can record exactly while something is served"
        );
    }

    /// A listener keeps the recorder's port, so a restart keeps the address
    /// recorded workloads carry; only when that port is taken does it take
    /// a free one.
    #[tokio::test]
    async fn a_listener_keeps_the_port_unless_it_is_taken() {
        let loopback = IpAddr::from([127, 0, 0, 1]);
        let Ok(occupant) =
            std::net::TcpListener::bind(SocketAddr::new(loopback, RECORDING_PROXY_PORT))
        else {
            eprintln!("port {RECORDING_PROXY_PORT} is in use on this host; skipping");
            return;
        };
        let taken = bind(loopback, RECORDING_PROXY_PORT).await.expect("bind");
        assert_ne!(taken.local_addr().unwrap().port(), RECORDING_PROXY_PORT);
        drop(taken);
        drop(occupant);

        let free = bind(loopback, RECORDING_PROXY_PORT).await.expect("bind");
        assert_eq!(
            free.local_addr().unwrap(),
            SocketAddr::new(loopback, RECORDING_PROXY_PORT)
        );
    }
}
