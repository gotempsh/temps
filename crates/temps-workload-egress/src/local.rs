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
//! 0 reports that it can record only while the proxy serves somewhere, and
//! only the kinds it serves ([`LocalServing`]): one kind's listener may be
//! up while another's could not start.
//!
//! **Its port is fixed** ([`RECORDING_PROXY_PORT`]): workloads carry the
//! address (a sidecar's hand-off, a container's proxy variables), and a
//! container started with one cannot be re-pointed. When the port is taken
//! the kind is not served, saying why, rather than served on a port its
//! workloads would lose on the next restart.
//!
//! **One refresh at a time.** A refresh reads every source and replaces the
//! whole table, so two overlapping ones could let the older finish last and
//! put back what the newer had just removed or never had. Refreshes take a
//! turn; each reads what is current when its turn comes. Work that may
//! itself ask for a refresh (re-pointing a sidecar) runs outside the turn,
//! in [`LocalTraffic::maintain`].
//!
//! **Cost.** Per source, each refresh is whatever the source reads (one
//! query for its opted-in workloads; only when there are some, one local
//! container list). Every [`REFRESH_INTERVAL`], and at once after an opt-in
//! changes. Memory is the proxy's fixed bound plus one table entry per
//! recorded source address.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
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

    /// Follow-up work after a refresh, run outside the refresh's turn so it
    /// may ask for another refresh itself (retrying a sidecar that could
    /// not be replaced). Nothing by default.
    async fn maintain(&self) {}

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

/// Which workload kinds this host's recorder serves now, and why not when
/// it does not. Shared with [`EgressRecordingService`], which reports node 0
/// able to record only the kinds served here.
#[derive(Debug, Default)]
pub struct LocalServing {
    kinds: RwLock<HashMap<WorkloadKind, Result<SocketAddr, String>>>,
}

impl LocalServing {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Where `kind` is served now, if it is.
    pub fn address(&self, kind: WorkloadKind) -> Option<SocketAddr> {
        match self.read().get(&kind) {
            Some(Ok(address)) => Some(*address),
            _ => None,
        }
    }

    /// Why `kind` is not served here, in a sentence; `None` when it is.
    pub fn not_serving(&self, kind: WorkloadKind) -> Option<String> {
        match self.read().get(&kind) {
            Some(Ok(_)) => None,
            Some(Err(reason)) => Some(reason.clone()),
            None => Some("its listener has not started on this host".to_string()),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_for_tests(&self, kind: WorkloadKind, state: Result<SocketAddr, String>) {
        self.set(kind, state);
    }

    fn any(&self) -> bool {
        self.read().values().any(Result::is_ok)
    }

    fn set(&self, kind: WorkloadKind, state: Result<SocketAddr, String>) {
        self.kinds
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(kind, state);
    }

    fn read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<WorkloadKind, Result<SocketAddr, String>>> {
        self.kinds.read().unwrap_or_else(|e| e.into_inner())
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
    /// Where each kind is served now, and why not.
    serving: Arc<LocalServing>,
    /// Held for the whole of a refresh: one at a time.
    refreshing: tokio::sync::Mutex<()>,
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
    /// A recorder that serves nothing until a source is added. `serving` is
    /// where it says which kinds it serves; give the same one to the
    /// service ([`EgressRecordingService::set_local_serving`]).
    pub fn new(
        service: Arc<EgressRecordingService>,
        capability: Arc<dyn LocalCapability>,
        serving: Arc<LocalServing>,
    ) -> Arc<Self> {
        Arc::new(Self {
            service,
            capability,
            table: Arc::new(ArcSwap::from_pointee(EgressTable::default())),
            running: tokio::sync::OnceCell::new(),
            sources: Mutex::new(Vec::new()),
            serving,
            refreshing: tokio::sync::Mutex::new(()),
            last: Mutex::new(HashMap::new()),
            reported: Mutex::new(None),
            wake: Notify::new(),
        })
    }

    /// Starts serving `source`'s kind in the background: it waits for the
    /// source's address to exist, then listens there. On a host that
    /// cannot record, the source is told so at once.
    pub async fn add_source(self: &Arc<Self>, source: Arc<dyn LocalTraffic>) {
        let kind = source.kind();
        if !cfg!(target_os = "linux") {
            info!(
                os = std::env::consts::OS,
                %kind,
                "egress recording: the control plane records only on Linux"
            );
            self.serving.set(
                kind,
                Err(format!(
                    "the control plane records only on Linux, and this one runs {}",
                    std::env::consts::OS
                )),
            );
            source.stopped().await;
            return;
        }
        let Some(running) = self.running().await else {
            self.serving
                .set(kind, Err("the egress proxy could not be built".to_string()));
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
        self.serving.address(kind)
    }

    /// Asks for a refresh soon, without waiting for it.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Notes where `kind` is served, or why it no longer is.
    fn set_serving(&self, kind: WorkloadKind, state: Result<SocketAddr, String>) {
        if state.is_err() {
            lock(&self.last).remove(&kind);
        }
        self.serving.set(kind, state);
    }

    /// Tells node 0's capability report when the recorder starts or stops
    /// serving anything at all; which kinds it serves is in
    /// [`LocalServing`].
    fn report_capability(&self) {
        let serving = self.serving.any();
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
                    self.set_serving(
                        kind,
                        Err("its network does not exist on this host yet".to_string()),
                    );
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                    continue;
                }
                Err(error) => {
                    warn!(%kind, %error, "egress recording: could not find where to listen");
                    self.set_serving(kind, Err(error));
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
                    self.set_serving(kind, Ok(address));
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
                    self.set_serving(
                        kind,
                        Err(format!(
                            "its listener on {address} stopped and is restarting"
                        )),
                    );
                    self.report_capability();
                    self.refresh().await;
                    source.stopped().await;
                }
                Err(error) => {
                    warn!(%kind, %error, "egress recording proxy could not start");
                    self.set_serving(kind, Err(error));
                    self.report_capability();
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
            let sources: Vec<Arc<dyn LocalTraffic>> = lock(&self.sources).clone();
            for source in sources {
                if self.serving.address(source.kind()).is_some() {
                    source.maintain().await;
                }
            }
        }
    }

    /// Rebuilds the table from every source that is being served. A source
    /// whose refresh fails keeps its last answer, so one failed read never
    /// drops its workloads from the table.
    ///
    /// One refresh at a time: a caller waits its turn, then reads the
    /// sources and what is served as they are then, so a refresh that
    /// started earlier can never replace the table after a later one.
    pub async fn refresh(&self) {
        let _turn = self.refreshing.lock().await;
        let sources: Vec<Arc<dyn LocalTraffic>> = lock(&self.sources).clone();
        let mut policies = Vec::new();
        let mut identities = IdentityMap::new();
        for source in sources {
            let kind = source.kind();
            let Some(address) = self.serving.address(kind) else {
                continue;
            };
            let traffic = match source.refresh(address).await {
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

/// A listener on `host` and `port`, or why there is none. Never another
/// port: a workload started with an address keeps it, so a listener that
/// moved would strand it on the next restart.
async fn bind(host: IpAddr, port: u16) -> Result<TcpListener, String> {
    let address = SocketAddr::new(host, port);
    TcpListener::bind(address).await.map_err(|error| {
        format!(
            "it cannot listen on {address} ({error}); free port {port} on that address and \
             it starts on its own"
        )
    })
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
            LocalEgressRecorder::new(service, capability.clone(), LocalServing::new()),
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

        recorder.set_serving(WorkloadKind::Sandbox, Ok(listening));
        recorder.refresh().await;
        recorder.report_capability();
        assert_eq!(owner("172.30.0.5"), Some(WorkloadKind::Sandbox));
        assert_eq!(owner("172.18.0.9"), None);

        recorder.set_serving(WorkloadKind::Deployment, Ok(listening));
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

        recorder.set_serving(WorkloadKind::Sandbox, Err("stopped".into()));
        recorder.refresh().await;
        recorder.report_capability();
        assert_eq!(owner("172.30.0.5"), None, "a kind no longer served is gone");
        assert_eq!(owner("172.18.0.9"), Some(WorkloadKind::Deployment));
        recorder.set_serving(WorkloadKind::Deployment, Err("stopped".into()));
        recorder.report_capability();
        assert_eq!(
            *capability.0.lock().unwrap(),
            vec![true, false],
            "node 0 can record exactly while something is served"
        );
    }

    /// The recorder serves only its own port: when it is taken there is no
    /// listener, and the reason says what to free. A workload started with
    /// an address keeps it, so a listener that moved would strand it.
    #[tokio::test]
    async fn a_taken_port_is_reported_not_replaced_by_another() {
        let loopback = IpAddr::from([127, 0, 0, 1]);
        let Ok(occupant) =
            std::net::TcpListener::bind(SocketAddr::new(loopback, RECORDING_PROXY_PORT))
        else {
            eprintln!("port {RECORDING_PROXY_PORT} is in use on this host; skipping");
            return;
        };
        let reason = bind(loopback, RECORDING_PROXY_PORT)
            .await
            .expect_err("the port is taken");
        assert!(
            reason.contains("127.0.0.1:13128") && reason.contains("free port 13128"),
            "{reason}"
        );
        drop(occupant);

        let free = bind(loopback, RECORDING_PROXY_PORT).await.expect("bind");
        assert_eq!(
            free.local_addr().unwrap(),
            SocketAddr::new(loopback, RECORDING_PROXY_PORT)
        );
    }

    /// Which kinds are served, and why one is not: what the service reports
    /// for node 0, kind by kind.
    #[test]
    fn serving_is_known_kind_by_kind_with_a_reason() {
        let serving = LocalServing::new();
        let address: SocketAddr = "172.30.0.1:13128".parse().unwrap();
        assert!(!serving.any());
        assert_eq!(
            serving.not_serving(WorkloadKind::Deployment).as_deref(),
            Some("its listener has not started on this host")
        );
        serving.set(WorkloadKind::Sandbox, Ok(address));
        serving.set(
            WorkloadKind::Deployment,
            Err("it cannot listen on 172.18.0.1:13128".into()),
        );
        assert!(serving.any());
        assert_eq!(serving.address(WorkloadKind::Sandbox), Some(address));
        assert_eq!(serving.not_serving(WorkloadKind::Sandbox), None);
        assert_eq!(serving.address(WorkloadKind::Deployment), None);
        assert_eq!(
            serving.not_serving(WorkloadKind::Deployment).as_deref(),
            Some("it cannot listen on 172.18.0.1:13128")
        );
    }

    /// A source whose answer is read when its refresh starts and handed
    /// back only when the gate opens: a slow container list.
    struct SlowSource {
        generation: std::sync::atomic::AtomicU32,
        calls: std::sync::atomic::AtomicU32,
        gate: tokio::sync::Semaphore,
    }

    #[async_trait]
    impl LocalTraffic for SlowSource {
        fn kind(&self) -> WorkloadKind {
            WorkloadKind::Sandbox
        }

        async fn listen_address(&self) -> Result<Option<IpAddr>, String> {
            Ok(None)
        }

        async fn refresh(&self, _address: SocketAddr) -> Result<RecordedTraffic, String> {
            let generation = self.generation.load(Ordering::SeqCst);
            let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                // Held until the test lets the first refresh finish.
                let _permit = self.gate.acquire().await.map_err(|e| e.to_string())?;
            }
            let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(generation));
            Ok(RecordedTraffic {
                policies: vec![EgressPolicy {
                    subject,
                    rules: Vec::new(),
                    record: true,
                    public_only: true,
                }],
                identities: IdentityMap::new().with(subject, "172.30.0.5".parse().unwrap()),
            })
        }
    }

    /// A refresh that started before a change and finishes after a refresh
    /// that saw the change must not put the old table back: refreshes take
    /// turns, and each reads its sources when its turn comes.
    #[tokio::test]
    async fn an_older_refresh_never_replaces_a_newer_table() {
        let Some(f) = fixture().await else { return };
        let (recorder, _) = recorder(&f);
        let source = Arc::new(SlowSource {
            generation: std::sync::atomic::AtomicU32::new(1),
            calls: std::sync::atomic::AtomicU32::new(0),
            gate: tokio::sync::Semaphore::new(0),
        });
        lock(&recorder.sources).push(source.clone());
        recorder.set_serving(
            WorkloadKind::Sandbox,
            Ok("127.0.0.1:13128".parse().unwrap()),
        );

        let older = {
            let recorder = recorder.clone();
            tokio::spawn(async move { recorder.refresh().await })
        };
        while source.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        // The change lands while the older refresh is still reading.
        source.generation.store(2, Ordering::SeqCst);
        let newer = {
            let recorder = recorder.clone();
            tokio::spawn(async move { recorder.refresh().await })
        };
        tokio::task::yield_now().await;
        source.gate.add_permits(1);
        older.await.unwrap();
        newer.await.unwrap();

        let owner = recorder
            .table
            .load()
            .lookup("172.30.0.5".parse().unwrap())
            .map(|rules| rules.workload);
        assert_eq!(
            owner,
            Some(WorkloadRef::new(WorkloadKind::Sandbox, 2)),
            "the table is the newer refresh's"
        );
    }
}
