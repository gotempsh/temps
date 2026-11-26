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
use std::sync::atomic::Ordering;
use temps_workload::WorkloadKind;
use temps_workload_netpol::egress::record::MAX_BATCH_RECORDS;
use temps_workload_netpol::{
    recording_channel, ConnectionRecord, Delivered, EgressBatcher, EgressPolicy, EgressProxy,
    EgressProxyConfig, EgressRecordBatch, EgressRecordSink, EgressRecordingCapability, EgressStats,
    EgressTable, IdentityMap, RecordSinkError, UnknownSource,
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

/// The least time between two refreshes asked for by what any caller can
/// trigger: a connection from an address the table does not know, and a
/// batch whose sources must be confirmed. Callers in between wait for the
/// next one and share it.
pub const ASKED_REFRESH_SPACING: Duration = Duration::from_secs(1);

/// The longest records wait for the refresh that confirms their sources.
/// Past it they are not stored, and counted as dropped.
pub const CONFIRM_WAIT: Duration = Duration::from_secs(15);

/// The most records waiting for confirmation at once. One refresh confirms
/// every record made before it started, so this bounds what arrives in one
/// [`ASKED_REFRESH_SPACING`] plus a refresh: about 4,000 recorded
/// connections a second, in a few megabytes. Past it, records are discarded
/// and counted, never queued without bound.
pub const MAX_AWAITING_CONFIRMATION: usize =
    8 * temps_workload_netpol::egress::record::MAX_BATCH_RECORDS;

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
    /// refresh. With `discover`, a workload of this kind may hold an address
    /// the last refresh did not see: nothing may be skipped for looking
    /// unchanged. Part of the call, not a flag set beforehand, so a refresh
    /// already under way can never clear it.
    async fn refresh(&self, address: SocketAddr, discover: bool)
        -> Result<RecordedTraffic, String>;

    /// The recorder began serving this kind at `address`.
    async fn started(&self, _address: SocketAddr) {}

    /// Follow-up work after a refresh, run outside the refresh's turn so it
    /// may ask for another refresh itself (retrying a sidecar that could
    /// not be replaced), whether this kind is served or not. Nothing by
    /// default.
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

/// When the finished refreshes started, on the monotonic clock: a step of
/// the wall clock can neither make an old refresh look newer than a
/// connection nor a new one older.
#[derive(Debug, Clone, Default)]
struct Refreshed {
    /// The last refresh.
    any: Option<tokio::time::Instant>,
    /// The last refresh that discovered ([`LocalTraffic::refresh`]).
    discovered: Option<tokio::time::Instant>,
    /// For each kind, the last refresh that read it successfully: what the
    /// table holds for a kind is that refresh's answer, also while later
    /// ones fail.
    read: HashMap<WorkloadKind, tokio::time::Instant>,
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
    /// When the finished refreshes started.
    refreshed: Mutex<Refreshed>,
    /// This installation, named in what it steers here.
    installation: crate::steering::InstallationId,
    wake: Arc<Notify>,
    /// Every task this recorder started, aborted when it is dropped: its
    /// loops hold it only weakly, so dropping the last reference stops them
    /// and frees its listeners.
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
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
        installation: crate::steering::InstallationId,
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
            refreshed: Mutex::new(Refreshed::default()),
            wake: Arc::new(Notify::new()),
            tasks: Mutex::new(Vec::new()),
            installation,
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
            self.own(tokio::spawn(Self::refresh_loop(
                Arc::downgrade(self),
                self.wake.clone(),
            )));
        }
        self.own(tokio::spawn(Self::listen_loop(
            Arc::downgrade(self),
            source,
            proxy,
        )));
    }

    /// Keeps `task` to abort when this recorder is dropped.
    fn own<T>(&self, task: tokio::task::JoinHandle<T>) {
        let mut tasks = lock(&self.tasks);
        tasks.retain(|task| !task.is_finished());
        tasks.push(task.abort_handle());
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
                        proxy.set_unknown_source(Arc::new(LearnSource {
                            recorder: Arc::downgrade(self),
                        }));
                        let confirmation = Confirmation::new(
                            Arc::downgrade(self),
                            Arc::new(LocalRecordSink::new(self.service.clone())),
                            proxy.stats().clone(),
                        );
                        self.own(tokio::spawn(confirmation.clone().run()));
                        self.own(tokio::spawn(EgressBatcher::default().run(
                            records,
                            confirmation,
                            proxy.stats().clone(),
                        )));
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

    /// The installation this recorder belongs to, named in the label of
    /// what it steers ([`crate::steering::steered_label`]).
    pub fn installation(&self) -> crate::steering::InstallationId {
        self.installation
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

    /// Listens for `source`'s kind, restarting when the listener stops.
    /// Holds the recorder only for each step, never while it waits: a
    /// recorder dropped meanwhile ends the loop at its next step (and its
    /// `Drop` aborts it, and the listener, at once).
    async fn listen_loop(
        recorder: std::sync::Weak<Self>,
        source: Arc<dyn LocalTraffic>,
        proxy: Arc<EgressProxy>,
    ) {
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
                    let Some(this) = recorder.upgrade() else {
                        return;
                    };
                    this.set_serving(
                        kind,
                        Err("its network does not exist on this host yet".to_string()),
                    );
                    drop(this);
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                    continue;
                }
                Err(error) => {
                    warn!(%kind, %error, "egress recording: could not find where to listen");
                    let Some(this) = recorder.upgrade() else {
                        return;
                    };
                    this.set_serving(kind, Err(error));
                    drop(this);
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                    continue;
                }
            };
            match bind(host, RECORDING_PROXY_PORT).await {
                Ok(listener) => {
                    let address = listener
                        .local_addr()
                        .unwrap_or_else(|_| SocketAddr::new(host, RECORDING_PROXY_PORT));
                    let serving = {
                        let Some(this) = recorder.upgrade() else {
                            return;
                        };
                        // The table first, then the capability, then the
                        // source: nothing is steered here before it is known.
                        this.set_serving(kind, Ok(address));
                        this.refresh().await;
                        this.report_capability();
                        info!(%kind, %address, "egress recording proxy serving the control plane's workloads");
                        // Accepting before the source is told: what it steers
                        // here must find the proxy already answering.
                        let serving = tokio::spawn(proxy.clone().serve(listener));
                        this.own_abort(serving.abort_handle());
                        source.started(address).await;
                        this.wake();
                        serving
                    };
                    match serving.await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            warn!(%kind, %address, %error, "egress recording proxy stopped accepting");
                        }
                        Err(error) => {
                            warn!(%kind, %address, %error, "egress recording proxy task ended");
                        }
                    }
                    let Some(this) = recorder.upgrade() else {
                        return;
                    };
                    warn!(%kind, %address, "egress recording proxy stopped; restarting it");
                    this.set_serving(
                        kind,
                        Err(format!(
                            "its listener on {address} stopped and is restarting"
                        )),
                    );
                    this.report_capability();
                    this.refresh().await;
                    drop(this);
                    source.stopped().await;
                }
                Err(error) => {
                    warn!(%kind, %error, "egress recording proxy could not start");
                    let Some(this) = recorder.upgrade() else {
                        return;
                    };
                    this.set_serving(kind, Err(error));
                    this.report_capability();
                    drop(this);
                    // Workloads stop handing traffic to a proxy that is
                    // not there, so they keep their way out.
                    source.stopped().await;
                }
            }
            tokio::time::sleep(REFRESH_INTERVAL).await;
        }
    }

    /// Keeps `task` to abort when this recorder is dropped.
    fn own_abort(&self, task: tokio::task::AbortHandle) {
        let mut tasks = lock(&self.tasks);
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    async fn refresh_loop(recorder: std::sync::Weak<Self>, wake: Arc<Notify>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(REFRESH_INTERVAL) => {}
                _ = wake.notified() => {}
            }
            let Some(this) = recorder.upgrade() else {
                return;
            };
            this.refresh().await;
            let sources: Vec<Arc<dyn LocalTraffic>> = lock(&this.sources).clone();
            // Served or not: a source that was told to stop may still have
            // work to finish (a sidecar to re-point without the hand-off).
            for source in sources {
                source.maintain().await;
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
        self.refresh_in_turn(false).await;
    }

    /// A refresh in which no source skips anything for looking unchanged:
    /// for when a workload has just been given an address.
    pub async fn rediscover(&self) {
        let _turn = self.refreshing.lock().await;
        self.refresh_in_turn(true).await;
    }

    /// Returns once a refresh that started after `since` has finished (one
    /// that discovered, with `discover`), running one if none has. Never
    /// more than one per [`ASKED_REFRESH_SPACING`]: callers in between wait
    /// for it and share it, so what any caller can trigger stays bounded.
    /// An ordinary refresh never answers a caller that needs discovery.
    pub async fn refreshed_since(&self, since: tokio::time::Instant, discover: bool) {
        let done = |refreshed: &Refreshed| {
            let last = if discover {
                refreshed.discovered
            } else {
                refreshed.any
            };
            last.is_some_and(|at| at > since)
        };
        if done(&lock(&self.refreshed)) {
            return;
        }
        let _turn = self.refreshing.lock().await;
        let last = {
            let refreshed = lock(&self.refreshed);
            if done(&refreshed) {
                return;
            }
            refreshed.any
        };
        if let Some(last) = last {
            tokio::time::sleep_until(last + ASKED_REFRESH_SPACING).await;
        }
        self.refresh_in_turn(discover).await;
    }

    /// Whether `workload` holds `source` according to a successful read of
    /// its kind that started after `since`. A kind whose read failed since
    /// confirms nothing: the table then holds an older answer for it.
    fn holds_since(
        &self,
        source: IpAddr,
        workload: &temps_workload::WorkloadRef,
        since: tokio::time::Instant,
    ) -> bool {
        let read = lock(&self.refreshed).read.get(&workload.kind).copied();
        read.is_some_and(|at| at > since)
            && self
                .table
                .load()
                .lookup(source)
                .is_some_and(|rules| rules.workload == *workload)
    }

    async fn refresh_in_turn(&self, discover: bool) {
        let started = tokio::time::Instant::now();
        let sources: Vec<Arc<dyn LocalTraffic>> = lock(&self.sources).clone();
        let mut policies = Vec::new();
        let mut identities = IdentityMap::new();
        let mut read = Vec::new();
        for source in sources {
            let kind = source.kind();
            let Some(address) = self.serving.address(kind) else {
                continue;
            };
            let traffic = match source.refresh(address, discover).await {
                Ok(traffic) => {
                    lock(&self.last).insert(kind, traffic.clone());
                    read.push(kind);
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
        let mut refreshed = lock(&self.refreshed);
        refreshed.any = Some(started);
        if discover {
            refreshed.discovered = Some(started);
        }
        for kind in read {
            refreshed.read.insert(kind, started);
        }
    }
}

impl Drop for LocalEgressRecorder {
    /// Stops everything it started: its loops, its listeners (so the port
    /// is free for a replacement) and its batcher.
    fn drop(&mut self) {
        for task in lock(&self.tasks).drain(..) {
            task.abort();
        }
    }
}

/// Asked by the proxy before it refuses a caller its table does not know:
/// a workload given its address just now may be calling before the
/// refresh that names it. Rediscovers, spaced like every asked refresh.
struct LearnSource {
    recorder: std::sync::Weak<LocalEgressRecorder>,
}

#[async_trait]
impl UnknownSource for LearnSource {
    async fn unknown_source(&self, source: IpAddr) {
        let Some(recorder) = self.recorder.upgrade() else {
            return;
        };
        debug!(%source, "egress recording: an unknown caller; looking for new workloads");
        recorder
            .refreshed_since(tokio::time::Instant::now(), true)
            .await;
    }
}

/// Node 0's sink, confirming each record's source before it is stored:
/// the proxy names a caller by its address, and an address can pass from a
/// workload that stopped to another before the table follows.
///
/// A record is stored only if a successful read of its kind, started after
/// the record was made (so after its connection was accepted, on the
/// monotonic clock), still gives its address to its workload. The rest are
/// discarded and counted ([`EgressStats::records_discarded`], and as
/// dropped in what ingest sees), never attributed. A kind whose read is
/// failing confirms nothing, and the last connections of a workload that
/// stopped in between are discarded too.
///
/// **Pipelined.** The batcher hands batches over at once: they wait here,
/// at most [`MAX_AWAITING_CONFIRMATION`] records, and one refresh confirms
/// every record made before it started, whatever batch it came in. So the
/// refreshes this costs, at most one per [`ASKED_REFRESH_SPACING`], do not
/// limit how many records get through.
struct Confirmation {
    recorder: std::sync::Weak<LocalEgressRecorder>,
    inner: Arc<dyn EgressRecordSink>,
    stats: Arc<EgressStats>,
    awaiting: Mutex<Awaiting>,
    wake: Notify,
}

/// Records waiting for confirmation, and losses ingest has not heard of.
#[derive(Default)]
struct Awaiting {
    records: Vec<ConnectionRecord>,
    dropped: u64,
}

impl Confirmation {
    fn new(
        recorder: std::sync::Weak<LocalEgressRecorder>,
        inner: Arc<dyn EgressRecordSink>,
        stats: Arc<EgressStats>,
    ) -> Arc<Self> {
        Arc::new(Self {
            recorder,
            inner,
            stats,
            awaiting: Mutex::new(Awaiting::default()),
            wake: Notify::new(),
        })
    }

    async fn run(self: Arc<Self>) {
        loop {
            self.wake.notified().await;
            if !self.confirm_awaiting().await {
                return;
            }
        }
    }

    /// Confirms and delivers what is waiting, until nothing is. `false`
    /// once the recorder is gone.
    async fn confirm_awaiting(&self) -> bool {
        loop {
            let newest = {
                let awaiting = lock(&self.awaiting);
                if awaiting.records.is_empty() && awaiting.dropped == 0 {
                    return true;
                }
                awaiting
                    .records
                    .iter()
                    .filter_map(|record| record.recorded_at)
                    .max()
            };
            let Some(recorder) = self.recorder.upgrade() else {
                return false;
            };
            let refreshed = match newest {
                Some(newest) => {
                    tokio::time::timeout(CONFIRM_WAIT, recorder.refreshed_since(newest, false))
                        .await
                        .is_ok()
                }
                None => true,
            };
            // Every record made by then: one refresh started after it.
            let (taken, mut dropped) = {
                let mut awaiting = lock(&self.awaiting);
                let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut awaiting.records)
                    .into_iter()
                    .partition(|record| {
                        newest
                            .is_some_and(|newest| record.recorded_at.is_some_and(|at| at <= newest))
                    });
                awaiting.records = kept;
                (taken, std::mem::take(&mut awaiting.dropped))
            };
            if !refreshed && !taken.is_empty() {
                warn!(
                    records = taken.len(),
                    "egress recording: no refresh confirmed these records' sources within {}s; \
                     discarded",
                    CONFIRM_WAIT.as_secs()
                );
            }
            let before = taken.len();
            let confirmed: Vec<ConnectionRecord> = taken
                .into_iter()
                .filter(|record| {
                    refreshed
                        && match (record.source, record.recorded_at) {
                            (Some(source), Some(at)) => {
                                recorder.holds_since(source, &record.workload, at)
                            }
                            _ => false,
                        }
                })
                .collect();
            // A record ingest would refuse is discarded here, by itself:
            // ingest refuses a whole batch for one, and a batch carries
            // other workloads' records.
            let confirmed: Vec<ConnectionRecord> = confirmed
                .into_iter()
                .filter(|record| match crate::records::validate(record) {
                    Ok(()) => true,
                    Err(reason) => {
                        warn!(workload = %record.workload, %reason, "egress recording: discarded a record ingest would refuse");
                        false
                    }
                })
                .collect();
            let unconfirmed = (before - confirmed.len()) as u64;
            if unconfirmed > 0 && refreshed {
                warn!(
                    unconfirmed,
                    "egress recording: discarded records whose source address could not be \
                     confirmed as still belonging to the workload it was attributed to"
                );
            }
            self.stats
                .records_discarded
                .fetch_add(unconfirmed, Ordering::Relaxed);
            dropped = dropped.saturating_add(unconfirmed);
            drop(recorder);
            self.deliver_confirmed(confirmed, dropped).await;
        }
    }

    /// Hands `records` to ingest in batches it accepts, telling it of
    /// `dropped` with the first, and counts what it stored.
    async fn deliver_confirmed(&self, records: Vec<ConnectionRecord>, mut dropped: u64) {
        let mut chunks: Vec<Vec<ConnectionRecord>> = Vec::new();
        let mut records = records.into_iter().peekable();
        while records.peek().is_some() {
            chunks.push(records.by_ref().take(MAX_BATCH_RECORDS).collect());
        }
        if chunks.is_empty() && dropped > 0 {
            chunks.push(Vec::new());
        }
        for chunk in chunks {
            let count = chunk.len() as u64;
            let batch = EgressRecordBatch {
                records: chunk,
                dropped: std::mem::take(&mut dropped),
            };
            match self.inner.deliver(batch).await {
                Ok(delivered) => {
                    let discarded = delivered.discarded.min(count);
                    self.stats
                        .records_delivered
                        .fetch_add(count - discarded, Ordering::Relaxed);
                    self.stats
                        .records_discarded
                        .fetch_add(discarded, Ordering::Relaxed);
                }
                Err(error) => {
                    // Counted as lost: the batcher reports it to ingest with
                    // its next batch.
                    self.stats
                        .records_undelivered
                        .fetch_add(count, Ordering::Relaxed);
                    warn!(records = count, %error, "egress records not delivered; they are dropped and counted");
                }
            }
        }
    }
}

#[async_trait]
impl EgressRecordSink for Confirmation {
    /// Takes the batch to confirm, at once. Records past
    /// [`MAX_AWAITING_CONFIRMATION`] are discarded.
    async fn deliver(&self, batch: EgressRecordBatch) -> Result<Delivered, RecordSinkError> {
        let now = tokio::time::Instant::now();
        let (deferred, overflow) = {
            let mut awaiting = lock(&self.awaiting);
            let room = MAX_AWAITING_CONFIRMATION.saturating_sub(awaiting.records.len());
            let total = batch.records.len();
            let taken = total.min(room);
            awaiting
                .records
                .extend(batch.records.into_iter().take(taken).map(|mut record| {
                    // Made before it got here, if the proxy did not say.
                    record.recorded_at.get_or_insert(now);
                    record
                }));
            let overflow = (total - taken) as u64;
            awaiting.dropped = awaiting
                .dropped
                .saturating_add(batch.dropped)
                .saturating_add(overflow);
            (taken as u64, overflow)
        };
        if overflow > 0 {
            warn!(
                overflow,
                "egress recording: too many records waiting for confirmation; discarded"
            );
        }
        self.wake.notify_one();
        Ok(Delivered {
            discarded: overflow,
            deferred,
        })
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

        async fn refresh(
            &self,
            _address: SocketAddr,
            _discover: bool,
        ) -> Result<RecordedTraffic, String> {
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
            LocalEgressRecorder::new(
                service,
                capability.clone(),
                LocalServing::new(),
                crate::steering::InstallationId::from_bytes([7; 8]),
            ),
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

    /// Dropping the last reference to a recorder stops what it started:
    /// its loops hold it only weakly, and its `Drop` aborts them (the
    /// listener with them), so nothing keeps running against the old
    /// service and nothing keeps its port.
    #[tokio::test]
    async fn a_dropped_recorder_stops_its_tasks() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let Some(f) = fixture().await else { return };
        let (recorder, _) = recorder(&f);
        let source = Source::new(WorkloadKind::Sandbox, "172.30.0.5");
        recorder.add_source(source.clone()).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!lock(&recorder.tasks).is_empty(), "it started tasks");
        assert!(Arc::strong_count(&source) > 1, "a task holds the source");
        let weak = Arc::downgrade(&recorder);
        drop(recorder);
        assert!(weak.upgrade().is_none(), "no task kept the recorder alive");
        for _ in 0..50 {
            if Arc::strong_count(&source) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(Arc::strong_count(&source), 1, "its listen loop was aborted");
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

        async fn refresh(
            &self,
            _address: SocketAddr,
            _discover: bool,
        ) -> Result<RecordedTraffic, String> {
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

    /// Sandbox `owner` at 172.30.0.5, counting refreshes and discoveries:
    /// an address that passes from one workload to another. Its read can be
    /// failed, and like the deployment source it can skip its listing when
    /// not asked to discover (`lists_only_on_discover`).
    #[derive(Default)]
    struct Moving {
        owner: std::sync::atomic::AtomicI64,
        refreshes: std::sync::atomic::AtomicU32,
        discovered: std::sync::atomic::AtomicU32,
        fail: AtomicBool,
        lists_only_on_discover: AtomicBool,
    }

    #[async_trait]
    impl LocalTraffic for Moving {
        fn kind(&self) -> WorkloadKind {
            WorkloadKind::Sandbox
        }

        async fn listen_address(&self) -> Result<Option<IpAddr>, String> {
            Ok(None)
        }

        async fn refresh(
            &self,
            _address: SocketAddr,
            discover: bool,
        ) -> Result<RecordedTraffic, String> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            if discover {
                self.discovered.fetch_add(1, Ordering::SeqCst);
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err("the container list timed out".into());
            }
            if !discover && self.lists_only_on_discover.load(Ordering::SeqCst) {
                return Ok(RecordedTraffic::default());
            }
            let subject =
                WorkloadRef::new(WorkloadKind::Sandbox, self.owner.load(Ordering::SeqCst));
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

    /// Collects what it is given; says it discarded `discard` of each batch,
    /// as ingest does for a workload that no longer records.
    #[derive(Default)]
    struct Collect(Mutex<Vec<EgressRecordBatch>>, u64);

    #[async_trait]
    impl EgressRecordSink for Collect {
        async fn deliver(&self, batch: EgressRecordBatch) -> Result<Delivered, RecordSinkError> {
            self.0.lock().unwrap().push(batch);
            Ok(Delivered {
                discarded: self.1,
                ..Default::default()
            })
        }
    }

    /// The confirmation stage of `recorder`, delivering into `collect`.
    fn confirmation(
        recorder: &Arc<LocalEgressRecorder>,
        collect: &Arc<Collect>,
    ) -> (Arc<Confirmation>, Arc<EgressStats>) {
        let stats = Arc::new(EgressStats::default());
        (
            Confirmation::new(Arc::downgrade(recorder), collect.clone(), stats.clone()),
            stats,
        )
    }

    fn moving_recorder(f: &Fixture, owner: i64) -> (Arc<LocalEgressRecorder>, Arc<Moving>) {
        let (recorder, _) = recorder(f);
        let source = Arc::new(Moving::default());
        source.owner.store(owner, Ordering::SeqCst);
        lock(&recorder.sources).push(source.clone());
        recorder.set_serving(
            WorkloadKind::Sandbox,
            Ok("127.0.0.1:13128".parse().unwrap()),
        );
        (recorder, source)
    }

    fn record_of(owner: i64) -> temps_workload_netpol::ConnectionRecord {
        let now = temps_workload_netpol::egress::record::unix_millis();
        temps_workload_netpol::ConnectionRecord {
            workload: WorkloadRef::new(WorkloadKind::Sandbox, owner),
            started_at_ms: now,
            last_started_at_ms: now,
            duration_ms: 1,
            kind: temps_workload_netpol::ConnectionKind::Tunnel,
            outcome: temps_workload_netpol::ConnectionOutcome::Allowed,
            reason: None,
            host: Some("api.example.com".into()),
            port: Some(443),
            peer: None,
            method: None,
            path: None,
            relay: None,
            bytes_sent: 0,
            bytes_received: 0,
            count: 1,
            source: None,
            recorded_at: None,
        }
        .from_source("172.30.0.5".parse().unwrap())
    }

    /// The proxy named sandbox 1 from a table that had not followed its
    /// address to sandbox 2: the record is confirmed against a refresh that
    /// started after it was made, and discarded (counted), never stored
    /// under sandbox 1. The new holder's own records are kept. The wall
    /// clock plays no part: both records claim a start long before the last
    /// refresh, as after a backward clock step, and a refresh still runs.
    #[tokio::test]
    async fn a_record_from_an_address_that_moved_is_dropped_not_misattributed() {
        let Some(f) = fixture().await else { return };
        let (recorder, source) = moving_recorder(&f, 1);
        recorder.refresh().await;
        let mut stale = record_of(1);
        source.owner.store(2, Ordering::SeqCst);
        let mut fresh = record_of(2);
        for record in [&mut stale, &mut fresh] {
            record.started_at_ms = 1;
            record.last_started_at_ms = 1;
        }
        let collect = Arc::new(Collect::default());
        let (stage, stats) = confirmation(&recorder, &collect);
        let delivered = stage
            .deliver(EgressRecordBatch {
                records: vec![stale, fresh],
                dropped: 0,
            })
            .await
            .unwrap();
        assert_eq!(delivered.deferred, 2, "counted by the stage itself");
        assert!(stage.confirm_awaiting().await);
        assert_eq!(source.refreshes.load(Ordering::SeqCst), 2, "a refresh ran");
        let batches = collect.0.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].records.len(), 1);
        assert_eq!(
            batches[0].records[0].workload,
            WorkloadRef::new(WorkloadKind::Sandbox, 2)
        );
        assert_eq!(batches[0].dropped, 1, "ingest hears of the discarded one");
        let stats = stats.snapshot();
        assert_eq!((stats.records_delivered, stats.records_discarded), (1, 1));
    }

    /// Records with no source address cannot be confirmed and are dropped.
    #[tokio::test]
    async fn a_record_without_a_source_is_dropped() {
        let Some(f) = fixture().await else { return };
        let (recorder, _) = moving_recorder(&f, 1);
        recorder.refresh().await;
        let mut unsourced = record_of(1);
        unsourced.source = None;
        let collect = Arc::new(Collect::default());
        let (stage, _) = confirmation(&recorder, &collect);
        stage
            .deliver(EgressRecordBatch {
                records: vec![unsourced],
                dropped: 0,
            })
            .await
            .unwrap();
        assert!(stage.confirm_awaiting().await);
        let batches = collect.0.lock().unwrap();
        assert_eq!(batches[0].records.len(), 0);
        assert_eq!(batches[0].dropped, 1);
    }

    /// A read that fails while records are confirmed confirms nothing: the
    /// table still holds the kind's last good answer, in which the address
    /// may belong to a workload that no longer holds it.
    #[tokio::test]
    async fn a_failed_read_confirms_no_record_of_its_kind() {
        let Some(f) = fixture().await else { return };
        let (recorder, source) = moving_recorder(&f, 1);
        recorder.refresh().await;
        source.fail.store(true, Ordering::SeqCst);
        let collect = Arc::new(Collect::default());
        let (stage, stats) = confirmation(&recorder, &collect);
        stage
            .deliver(EgressRecordBatch {
                records: vec![record_of(1)],
                dropped: 0,
            })
            .await
            .unwrap();
        assert!(stage.confirm_awaiting().await);
        assert_eq!(stats.snapshot().records_discarded, 1);
        let batches = collect.0.lock().unwrap();
        assert_eq!(batches.len(), 1, "ingest still hears of the loss");
        assert!(batches[0].records.is_empty());
        assert_eq!(batches[0].dropped, 1);
        assert_eq!(
            recorder
                .table
                .load()
                .lookup("172.30.0.5".parse().unwrap())
                .map(|rules| rules.workload),
            Some(WorkloadRef::new(WorkloadKind::Sandbox, 1)),
            "the table keeps serving the last good answer meanwhile"
        );
    }

    /// Batches that queued up while one refresh ran are all confirmed by
    /// the next one: refreshes do not cap how many records get through, and
    /// ingest gets them in batches it accepts.
    #[tokio::test]
    async fn one_refresh_confirms_every_batch_waiting() {
        let Some(f) = fixture().await else { return };
        let (recorder, source) = moving_recorder(&f, 1);
        recorder.refresh().await;
        let collect = Arc::new(Collect::default());
        let (stage, stats) = confirmation(&recorder, &collect);
        for batch in 0..3 {
            let records = (0..MAX_BATCH_RECORDS)
                .map(|i| {
                    let mut record = record_of(1);
                    record.host = Some(format!("h{batch}-{i}.example.com"));
                    record
                })
                .collect();
            stage
                .deliver(EgressRecordBatch {
                    records,
                    dropped: 0,
                })
                .await
                .unwrap();
        }
        assert!(stage.confirm_awaiting().await);
        assert_eq!(
            source.refreshes.load(Ordering::SeqCst),
            2,
            "one refresh for all three batches"
        );
        let batches = collect.0.lock().unwrap();
        assert_eq!(batches.len(), 3);
        assert!(batches.iter().all(|b| b.records.len() == MAX_BATCH_RECORDS));
        assert_eq!(
            stats.snapshot().records_delivered,
            3 * MAX_BATCH_RECORDS as u64
        );
    }

    /// Past [`MAX_AWAITING_CONFIRMATION`] records are discarded at once and
    /// counted, never queued without bound.
    #[tokio::test]
    async fn records_past_the_waiting_bound_are_discarded() {
        let Some(f) = fixture().await else { return };
        let (recorder, _) = moving_recorder(&f, 1);
        let collect = Arc::new(Collect::default());
        let (stage, _) = confirmation(&recorder, &collect);
        let records = (0..MAX_AWAITING_CONFIRMATION + 3)
            .map(|_| record_of(1))
            .collect();
        let delivered = stage
            .deliver(EgressRecordBatch {
                records,
                dropped: 0,
            })
            .await
            .unwrap();
        assert_eq!(delivered.discarded, 3);
        assert_eq!(delivered.deferred, MAX_AWAITING_CONFIRMATION as u64);
    }

    /// A record ingest would refuse (an empty destination) is discarded by
    /// itself; the valid records delivered with it are stored.
    #[tokio::test]
    async fn an_invalid_record_is_discarded_without_its_batch() {
        let Some(f) = fixture().await else { return };
        let (recorder, _) = moving_recorder(&f, 1);
        recorder.refresh().await;
        let collect = Arc::new(Collect::default());
        let (stage, stats) = confirmation(&recorder, &collect);
        let mut empty_host = record_of(1);
        empty_host.host = Some(String::new());
        stage
            .deliver(EgressRecordBatch {
                records: vec![empty_host, record_of(1)],
                dropped: 0,
            })
            .await
            .unwrap();
        assert!(stage.confirm_awaiting().await);
        let batches = collect.0.lock().unwrap();
        assert_eq!(batches[0].records.len(), 1);
        assert_eq!(batches[0].dropped, 1);
        let stats = stats.snapshot();
        assert_eq!((stats.records_delivered, stats.records_discarded), (1, 1));
    }

    /// What ingest takes but does not store (the workload stopped recording
    /// in between) is counted as discarded, not delivered.
    #[tokio::test]
    async fn records_ingest_does_not_store_are_counted_as_discarded() {
        let Some(f) = fixture().await else { return };
        let (recorder, _) = moving_recorder(&f, 1);
        recorder.refresh().await;
        let collect = Arc::new(Collect(Mutex::default(), 1));
        let (stage, stats) = confirmation(&recorder, &collect);
        stage
            .deliver(EgressRecordBatch {
                records: vec![record_of(1), record_of(1)],
                dropped: 0,
            })
            .await
            .unwrap();
        assert!(stage.confirm_awaiting().await);
        let stats = stats.snapshot();
        assert_eq!((stats.records_delivered, stats.records_discarded), (1, 1));
    }

    /// A caller that needs discovery is not answered by an ordinary refresh
    /// that started after it (one that skipped its listing): it waits for,
    /// or runs, one that discovered. `rediscover` always discovers, whatever
    /// a refresh under way or before it found.
    #[tokio::test]
    async fn only_a_discovering_refresh_answers_a_caller_that_needs_one() {
        let Some(f) = fixture().await else { return };
        let (recorder, source) = moving_recorder(&f, 1);
        source.lists_only_on_discover.store(true, Ordering::SeqCst);
        let since = tokio::time::Instant::now();
        recorder.refresh().await;
        assert!(recorder.table.load().is_empty(), "the listing was skipped");
        recorder.refreshed_since(since, true).await;
        assert_eq!(source.discovered.load(Ordering::SeqCst), 1);
        assert!(
            recorder
                .table
                .load()
                .lookup("172.30.0.5".parse().unwrap())
                .is_some(),
            "found by the discovering refresh"
        );

        recorder.refresh().await;
        recorder.rediscover().await;
        assert_eq!(source.discovered.load(Ordering::SeqCst), 2);
        assert!(!recorder.table.load().is_empty());
    }

    /// A caller the table does not know makes the recorder rediscover (a
    /// deployment source lists again even if it last saw nothing), and any
    /// number of them at once share one refresh; the next is spaced.
    #[tokio::test]
    async fn unknown_callers_share_one_spaced_rediscovery() {
        let Some(f) = fixture().await else { return };
        let (recorder, source) = moving_recorder(&f, 1);
        recorder.refresh().await;
        let learn = Arc::new(LearnSource {
            recorder: Arc::downgrade(&recorder),
        });
        let started = tokio::time::Instant::now();
        let mut callers = Vec::new();
        for _ in 0..8 {
            let learn = learn.clone();
            callers.push(tokio::spawn(async move {
                learn.unknown_source("172.30.0.9".parse().unwrap()).await
            }));
        }
        for caller in callers {
            caller.await.unwrap();
        }
        assert_eq!(
            source.refreshes.load(Ordering::SeqCst),
            2,
            "one shared refresh"
        );
        assert_eq!(source.discovered.load(Ordering::SeqCst), 1);
        assert!(
            started.elapsed() >= ASKED_REFRESH_SPACING - Duration::from_millis(50),
            "spaced from the refresh before it"
        );
    }
}
