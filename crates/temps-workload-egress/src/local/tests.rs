// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests of the local egress recorder (`super`).

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

    async fn listen_address(&self) -> Result<Option<IpAddr>, LocalTrafficError> {
        Ok(None)
    }

    async fn refresh(
        &self,
        _address: SocketAddr,
        _discover: bool,
    ) -> Result<RecordedTraffic, LocalTrafficError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(LocalTrafficError::docker(
                self.kind(),
                "listing the containers",
            )(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the container list timed out",
            )));
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

/// A refresh that reads exactly what the table was built from rebuilds
/// nothing: the published table stays the same allocation. A change is
/// published at once.
#[tokio::test]
async fn an_unchanged_refresh_does_not_rebuild_the_table() {
    let Some(f) = fixture().await else { return };
    let (recorder, _) = recorder(&f);
    let sandboxes = Source::new(WorkloadKind::Sandbox, "172.30.0.5");
    lock(&recorder.sources).push(sandboxes.clone() as Arc<dyn LocalTraffic>);
    recorder.set_serving(
        WorkloadKind::Sandbox,
        Ok("127.0.0.1:13128".parse().unwrap()),
    );
    recorder.refresh().await;
    let first = recorder.table.load_full();
    assert!(first.lookup("172.30.0.5".parse().unwrap()).is_some());
    recorder.refresh().await;
    assert!(
        Arc::ptr_eq(&first, &recorder.table.load_full()),
        "the same table, not rebuilt"
    );
    recorder.set_serving(WorkloadKind::Sandbox, Err("stopped".into()));
    recorder.refresh().await;
    assert!(!Arc::ptr_eq(&first, &recorder.table.load_full()));
    assert!(recorder
        .table
        .load()
        .lookup("172.30.0.5".parse().unwrap())
        .is_none());
}

/// A policy update puts its new rules in force at once: amended, the
/// table holds them even when the refresh that follows fails and keeps
/// the last answer. A workload the recorder does not hold is ignored.
#[tokio::test]
async fn an_amended_policy_holds_through_a_failed_refresh() {
    let Some(f) = fixture().await else { return };
    let (recorder, _) = recorder(&f);
    let sandboxes = Source::new(WorkloadKind::Sandbox, "172.30.0.5");
    lock(&recorder.sources).push(sandboxes.clone() as Arc<dyn LocalTraffic>);
    recorder.set_serving(
        WorkloadKind::Sandbox,
        Ok("127.0.0.1:13128".parse().unwrap()),
    );
    recorder.refresh().await;
    let admits = |host: &str| {
        recorder
            .table
            .load()
            .lookup("172.30.0.5".parse().unwrap())
            .is_some_and(|rules| rules.check_host(host, 443).is_ok())
    };
    assert!(admits("api.example.com"), "the source allows everything");

    let subject = WorkloadRef::new(WorkloadKind::Sandbox, 1);
    let narrowed = EgressPolicy {
        subject,
        rules: vec![EgressRule {
            id: 1,
            target: EgressTarget::Host(HostPattern::parse("api.example.com").unwrap()),
            ports: None,
            action: Action::Allow,
        }],
        record: true,
        public_only: true,
    };
    sandboxes.fail.store(true, Ordering::SeqCst);
    recorder.amend_policy(narrowed.clone()).await;
    recorder.rediscover().await;
    assert!(admits("api.example.com"));
    assert!(
        !admits("other.example.com"),
        "the stored policy holds though the refresh failed"
    );

    recorder
        .amend_policy(EgressPolicy {
            subject: WorkloadRef::new(WorkloadKind::Sandbox, 99),
            ..narrowed
        })
        .await;
    assert!(
        recorder
            .table
            .load()
            .lookup("172.30.0.5".parse().unwrap())
            .is_some_and(|rules| rules.workload == subject),
        "a workload it does not hold changes nothing"
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
    let Ok(occupant) = std::net::TcpListener::bind(SocketAddr::new(loopback, RECORDING_PROXY_PORT))
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

    async fn listen_address(&self) -> Result<Option<IpAddr>, LocalTrafficError> {
        Ok(None)
    }

    async fn refresh(
        &self,
        _address: SocketAddr,
        _discover: bool,
    ) -> Result<RecordedTraffic, LocalTrafficError> {
        let generation = self.generation.load(Ordering::SeqCst);
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            // Held until the test lets the first refresh finish.
            let _permit = self
                .gate
                .acquire()
                .await
                .map_err(LocalTrafficError::docker(
                    self.kind(),
                    "waiting for the gate",
                ))?;
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

    async fn listen_address(&self) -> Result<Option<IpAddr>, LocalTrafficError> {
        Ok(None)
    }

    async fn refresh(
        &self,
        _address: SocketAddr,
        discover: bool,
    ) -> Result<RecordedTraffic, LocalTrafficError> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        if discover {
            self.discovered.fetch_add(1, Ordering::SeqCst);
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(LocalTrafficError::docker(
                self.kind(),
                "listing the containers",
            )(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the container list timed out",
            )));
        }
        if !discover && self.lists_only_on_discover.load(Ordering::SeqCst) {
            return Ok(RecordedTraffic::default());
        }
        let subject = WorkloadRef::new(WorkloadKind::Sandbox, self.owner.load(Ordering::SeqCst));
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

/// A source whose listen address is set by the test; a failed check
/// when `failing`.
struct Gateway {
    address: Mutex<Option<IpAddr>>,
    failing: AtomicBool,
}

#[async_trait]
impl LocalTraffic for Gateway {
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Sandbox
    }

    async fn listen_address(&self) -> Result<Option<IpAddr>, LocalTrafficError> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(LocalTrafficError::docker(
                WorkloadKind::Sandbox,
                "inspecting the sandbox network",
            )(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "docker unreachable",
            )));
        }
        Ok(*self.address.lock().unwrap())
    }

    async fn refresh(&self, _: SocketAddr, _: bool) -> Result<RecordedTraffic, LocalTrafficError> {
        unreachable!("never refreshed: only its listen address is read")
    }
}

/// A listener bound to a gateway the network no longer has is stopped
/// once the source says it moved (or is gone), not left blocked in
/// `accept` while the old address is advertised; a check that fails
/// keeps it.
#[tokio::test]
async fn a_listener_on_a_vanished_gateway_is_stopped() {
    let old: IpAddr = "172.30.0.1".parse().unwrap();
    let new: IpAddr = "172.31.0.1".parse().unwrap();
    for (now, expected) in [(Some(new), Some(new)), (None, None)] {
        let source = Gateway {
            address: Mutex::new(Some(old)),
            failing: AtomicBool::new(true),
        };
        // A listener that never ends on its own, as one blocked in
        // accept on a lost address.
        let serving = tokio::spawn(std::future::pending::<Result<(), EgressError>>());
        let abort = serving.abort_handle();
        let watched = serve_while_current(serving, &source, old, Duration::from_millis(10));
        tokio::pin!(watched);
        // Failing checks, then the same address: still serving.
        assert!(
            tokio::time::timeout(Duration::from_millis(60), &mut watched)
                .await
                .is_err()
        );
        source.failing.store(false, Ordering::SeqCst);
        assert!(
            tokio::time::timeout(Duration::from_millis(60), &mut watched)
                .await
                .is_err()
        );
        *source.address.lock().unwrap() = now;
        let ended = tokio::time::timeout(Duration::from_secs(5), &mut watched)
            .await
            .expect("stopped once the address moved");
        assert!(
            matches!(ended, ListenerEnd::Moved(at) if at == expected),
            "{ended:?}"
        );
        tokio::task::yield_now().await;
        assert!(abort.is_finished(), "the old listener is aborted");
    }
}
