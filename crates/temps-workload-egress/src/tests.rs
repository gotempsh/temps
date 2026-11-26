// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The opt-in, the ingest and the activity query against a real, migrated
//! database, with a workload kind that exists only here: nothing in this
//! crate knows what a sandbox or an environment is. Skipped, saying why,
//! when no TimescaleDB container can be started.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_network_policy::NodeRecordingStatus;
use temps_otel::types::{LogQuery, LogRecord};
use temps_otel::OtelError;
use temps_test_utils::TestDatabase;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::{
    ConnectionKind, ConnectionOutcome, ConnectionRecord, EgressReason, EgressRecordBatch,
    EgressRecordSink, RecordingUnavailable,
};

use crate::activity::EgressOutcomeFilter;
use crate::adapter::{RecordedWorkload, WorkloadEgressAdapter};
use crate::error::EgressRecordingError;
use crate::service::{
    EgressLogStore, EgressRecordingService, NodeRecordingCapabilities, CONTROL_PLANE_NODE_ID,
};
use crate::sink::LocalRecordSink;

pub(crate) const PROJECT: i32 = 4101;
/// In the project, on worker A.
pub(crate) const ON_WORKER: i64 = 4101;
/// In no project, on worker A.
pub(crate) const PROJECTLESS: i64 = 4102;
/// In the project, on the control plane.
pub(crate) const ON_CONTROL_PLANE: i64 = 4103;
/// In the project, on worker B, which cannot record.
pub(crate) const ON_OTHER_WORKER: i64 = 4104;
/// In the project, on worker A and worker B.
pub(crate) const ON_BOTH_WORKERS: i64 = 4105;
/// In the project, running nowhere yet.
pub(crate) const NOT_RUNNING: i64 = 4106;
/// The token worker A authenticates with.
pub(crate) const NODE_TOKEN: &str = "test-node-token-a";

/// The kind the fake adapter answers for.
pub(crate) const KIND: WorkloadKind = WorkloadKind::Sandbox;

pub(crate) fn workload(id: i64) -> WorkloadRef {
    WorkloadRef::new(KIND, id)
}

pub(crate) fn public_id(id: i64) -> String {
    format!("box_{id}")
}

/// A workload kind kept in memory: its rows, and what it was told.
#[derive(Default)]
pub(crate) struct FakeAdapter {
    pub rows: Mutex<HashMap<i64, RecordedWorkload>>,
    pub changed: Mutex<Vec<(i64, bool)>>,
}

impl FakeAdapter {
    pub fn row(&self, id: i64) -> RecordedWorkload {
        self.rows.lock().unwrap()[&id].clone()
    }
}

#[async_trait]
impl WorkloadEgressAdapter for FakeAdapter {
    fn kind(&self) -> WorkloadKind {
        KIND
    }

    fn noun(&self) -> &'static str {
        "box"
    }

    fn id_attribute(&self) -> &'static str {
        "temps.box.id"
    }

    async fn load(&self, ids: &[i64]) -> Result<Vec<RecordedWorkload>, EgressRecordingError> {
        let rows = self.rows.lock().unwrap();
        Ok(ids.iter().filter_map(|id| rows.get(id).cloned()).collect())
    }

    async fn set_enabled(
        &self,
        workload: WorkloadRef,
        enabled: bool,
    ) -> Result<(), EgressRecordingError> {
        if let Some(row) = self.rows.lock().unwrap().get_mut(&workload.id) {
            row.enabled = enabled;
        }
        Ok(())
    }

    async fn changed(&self, workload: &RecordedWorkload) {
        self.changed
            .lock()
            .unwrap()
            .push((workload.workload.id, workload.enabled));
    }
}

pub(crate) struct Fixture {
    pub db: TestDatabase,
    pub worker_a: i32,
    pub worker_b: i32,
    pub adapter: Arc<FakeAdapter>,
}

/// A migrated database with one project and two workers, and a fake kind
/// with six workloads, or `None` (with the reason printed) without Docker.
pub(crate) async fn fixture() -> Option<Fixture> {
    let db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            eprintln!("TimescaleDB unavailable, skipping egress recording test: {error}");
            return None;
        }
    };
    let conn = db.connection();
    conn.execute_unprepared(&format!(
        "INSERT INTO projects (id, name, slug, repo_name, repo_owner, directory, main_branch, \
           preset, created_at, updated_at) \
         VALUES ({PROJECT}, 'Sample', 'sample-agent', 'sample', 'example', '/', 'main', \
           'static', now(), now());"
    ))
    .await
    .expect("seed project");
    let mut workers = Vec::new();
    for (name, token) in [("worker-a", NODE_TOKEN), ("worker-b", "test-node-token-b")] {
        let row = conn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
                 labels, capacity) VALUES ($1, $2, '127.0.0.1', '10.0.0.1', 'worker', 'active', \
                 '{}', '{}') RETURNING id",
                [name.into(), temps_core::crypto::sha256_hex(token).into()],
            ))
            .await
            .expect("insert node")
            .expect("node id");
        workers.push(row.try_get::<i32>("", "id").expect("id"));
    }
    let (worker_a, worker_b) = (workers[0], workers[1]);
    let adapter = Arc::new(FakeAdapter::default());
    for (id, project_id, nodes) in [
        (ON_WORKER, Some(PROJECT), vec![worker_a]),
        (PROJECTLESS, None, vec![worker_a]),
        (ON_CONTROL_PLANE, Some(PROJECT), vec![CONTROL_PLANE_NODE_ID]),
        (ON_OTHER_WORKER, Some(PROJECT), vec![worker_b]),
        (ON_BOTH_WORKERS, Some(PROJECT), vec![worker_b, worker_a]),
        (NOT_RUNNING, Some(PROJECT), vec![]),
    ] {
        adapter.rows.lock().unwrap().insert(
            id,
            RecordedWorkload {
                workload: workload(id),
                public_id: public_id(id),
                project_id,
                nodes,
                enabled: false,
                live: true,
                unrecordable: None,
            },
        );
    }
    Some(Fixture {
        db,
        worker_a,
        worker_b,
        adapter,
    })
}

#[derive(Default)]
pub(crate) struct FakeCapabilities(pub Mutex<HashMap<i32, NodeRecordingStatus>>);

impl FakeCapabilities {
    pub fn with(entries: &[(i32, NodeRecordingStatus)]) -> Arc<Self> {
        Arc::new(Self(Mutex::new(entries.iter().cloned().collect())))
    }
}

impl NodeRecordingCapabilities for FakeCapabilities {
    fn status(&self, node_id: i32) -> NodeRecordingStatus {
        self.0
            .lock()
            .unwrap()
            .get(&node_id)
            .cloned()
            .unwrap_or(NodeRecordingStatus::NotReporting { within_secs: 120 })
    }
}

#[derive(Default)]
pub(crate) struct CollectStore(pub Mutex<Vec<LogRecord>>);

#[async_trait]
impl EgressLogStore for CollectStore {
    async fn store(&self, records: Vec<LogRecord>) -> Result<u64, OtelError> {
        let count = records.len() as u64;
        self.0.lock().unwrap().extend(records);
        Ok(count)
    }

    async fn query(&self, query: LogQuery) -> Result<Vec<LogRecord>, OtelError> {
        let mut found: Vec<LogRecord> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|log| match query.workload {
                Some(workload) => log.workload == Some(workload),
                None => log.project_id == Some(query.project_id),
            })
            .cloned()
            .collect();
        found.sort_by_key(|log| std::cmp::Reverse(log.timestamp));
        Ok(found)
    }

    async fn count(&self, query: LogQuery) -> Result<u64, OtelError> {
        Ok(self.query(query).await?.len() as u64)
    }
}

impl Fixture {
    /// Worker A and the control plane can record; worker B is a Mac.
    pub fn capabilities(&self) -> Arc<FakeCapabilities> {
        FakeCapabilities::with(&[
            (self.worker_a, NodeRecordingStatus::Available),
            (CONTROL_PLANE_NODE_ID, NodeRecordingStatus::Available),
            (
                self.worker_b,
                NodeRecordingStatus::Unavailable(RecordingUnavailable::UnsupportedPlatform {
                    os: "macos".into(),
                }),
            ),
        ])
    }

    fn with_store(&self, store: Option<Arc<dyn EgressLogStore>>) -> Arc<EgressRecordingService> {
        let service = Arc::new(EgressRecordingService::new(
            self.db.connection_arc(),
            self.capabilities(),
            store,
        ));
        service.register(self.adapter.clone());
        service
    }

    /// The service over real OpenTelemetry storage in the fixture's
    /// database, as production wires it.
    pub fn service_with_otel(&self) -> Arc<EgressRecordingService> {
        let otel = Arc::new(temps_otel::OtelService::new(
            Arc::new(temps_otel::storage::timescaledb::TimescaleDbStorage::new(
                self.db.connection_arc(),
                None,
            )),
            Arc::new(temps_otel::ingest::auth::OtelAuthService::new(
                self.db.connection_arc(),
            )),
            Arc::new(temps_otel::ingest::rate_limit::RateLimiter::new(
                10_000,
                std::time::Duration::from_secs(60),
            )),
            temps_otel::services::otel_service::DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS,
        ));
        self.with_store(Some(otel as Arc<dyn EgressLogStore>))
    }

    pub fn service(&self) -> (Arc<EgressRecordingService>, Arc<CollectStore>) {
        let store = Arc::new(CollectStore::default());
        (self.with_store(Some(store.clone())), store)
    }

    pub fn row(&self, id: i64) -> RecordedWorkload {
        self.adapter.row(id)
    }
}

pub(crate) fn record(id: i64, host: &str, outcome: ConnectionOutcome) -> ConnectionRecord {
    ConnectionRecord {
        workload: workload(id),
        started_at_ms: 1_700_000_000_000,
        last_started_at_ms: 1_700_000_000_000,
        duration_ms: 12,
        kind: ConnectionKind::Tunnel,
        outcome,
        reason: (outcome == ConnectionOutcome::Refused).then_some(EgressReason::NoAllowRule),
        host: Some(host.to_string()),
        port: Some(443),
        peer: None,
        method: None,
        path: None,
        relay: None,
        bytes_sent: 10,
        bytes_received: 20,
        count: 1,
        source: None,
        recorded_at: None,
    }
}

pub(crate) fn batch(records: Vec<ConnectionRecord>) -> EgressRecordBatch {
    EgressRecordBatch {
        records,
        dropped: 0,
    }
}

#[tokio::test]
async fn recording_is_off_by_default_and_reports_whether_it_can_work() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let status = service.status(&f.row(ON_WORKER)).await.unwrap();
    assert_eq!(
        (status.workload_kind.as_str(), status.workload_id.as_str()),
        ("sandbox", "box_4101")
    );
    assert!(!status.enabled);
    assert!(status.configured);
    assert_eq!(status.reason, None);
    assert_eq!(status.nodes.len(), 1);
    assert_eq!(status.nodes[0].node_name, "worker-a");
    assert!(status.nodes[0].recording);
    assert_eq!(
        status.logs_path.as_deref(),
        Some(
            "/projects/sample-agent/telemetry-logs?attribute_key=temps.box.id&attribute_value=box_4101"
        )
    );

    let mac = service.status(&f.row(ON_OTHER_WORKER)).await.unwrap();
    assert!(!mac.configured);
    assert_eq!(mac.reason_code.as_deref(), Some("unsupported_platform"));
    assert!(mac.reason.unwrap().contains("macos"));
    assert_eq!(
        mac.setup_path, None,
        "nothing on the node page fixes a platform"
    );

    let control_plane = service.status(&f.row(ON_CONTROL_PLANE)).await.unwrap();
    assert_eq!(control_plane.nodes[0].node_id, CONTROL_PLANE_NODE_ID);
    assert_eq!(control_plane.nodes[0].node_name, "control-plane");
}

/// A workload needs no project to record: its records are its own, read
/// from the workload, and no project's logs link to them.
#[tokio::test]
async fn a_workload_without_a_project_records_and_reads_its_own_activity() {
    let Some(f) = fixture().await else { return };
    let service = f.service_with_otel();
    let status = service.status(&f.row(PROJECTLESS)).await.unwrap();
    assert!(status.configured, "{status:?}");
    assert_eq!((status.project_id, status.logs_path), (None, None));

    service
        .set_enabled(&f.row(PROJECTLESS), true)
        .await
        .unwrap();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                PROJECTLESS,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (1, 0));
    let page = service
        .activity(&f.row(PROJECTLESS), None, None, None)
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.records[0].host.as_deref(), Some("api.example.com"));
}

#[tokio::test]
async fn an_owner_turns_recording_on_and_off_and_the_kind_is_told() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let (previous, status) = service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    assert!(!previous);
    assert!(status.enabled && status.configured);
    assert!(f.row(ON_WORKER).enabled);
    // Setting the same value again changes nothing and still succeeds.
    let (previous, _) = service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    assert!(previous);
    let (previous, status) = service.set_enabled(&f.row(ON_WORKER), false).await.unwrap();
    assert!(previous);
    assert!(!status.enabled);
    assert!(!f.row(ON_WORKER).enabled);
    assert_eq!(
        *f.adapter.changed.lock().unwrap(),
        vec![(ON_WORKER, true), (ON_WORKER, false)],
        "the kind steers traffic once per change, not per call"
    );
}

#[tokio::test]
async fn enabling_on_a_node_that_cannot_record_names_the_workload_and_the_node() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let error = service
        .set_enabled(&f.row(ON_OTHER_WORKER), true)
        .await
        .unwrap_err();
    let EgressRecordingError::Unavailable {
        noun,
        id,
        node_id,
        node_name,
        code,
        ..
    } = &error
    else {
        panic!("expected Unavailable, got {error:?}");
    };
    assert_eq!((*noun, id.as_str()), ("box", "box_4104"));
    assert_eq!(*node_id, f.worker_b);
    assert_eq!(node_name, "worker-b");
    assert_eq!(*code, "unsupported_platform");
    let message = error.to_string();
    assert!(message.contains("box box_4104") && message.contains("worker-b"));
    assert!(!f.row(ON_OTHER_WORKER).enabled, "nothing was written");
    // Turning it off is never refused.
    service
        .set_enabled(&f.row(ON_OTHER_WORKER), false)
        .await
        .unwrap();
}

/// A workload on several nodes records on those that can; one that runs
/// nowhere yet can be turned on and records wherever it starts.
#[tokio::test]
async fn a_workload_on_several_nodes_records_where_a_node_can() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    let (_, status) = service
        .set_enabled(&f.row(ON_BOTH_WORKERS), true)
        .await
        .unwrap();
    assert!(status.configured);
    let by_node: Vec<(&str, bool, Option<&str>)> = status
        .nodes
        .iter()
        .map(|n| (n.node_name.as_str(), n.recording, n.reason_code.as_deref()))
        .collect();
    assert_eq!(
        by_node,
        vec![
            ("worker-b", false, Some("unsupported_platform")),
            ("worker-a", true, None)
        ]
    );
    for node in [f.worker_a, f.worker_b] {
        service
            .ingest(
                node,
                batch(vec![record(
                    ON_BOTH_WORKERS,
                    "api.example.com",
                    ConnectionOutcome::Allowed,
                )]),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        store.0.lock().unwrap().len(),
        2,
        "either node may report it"
    );

    let (_, idle) = service
        .set_enabled(&f.row(NOT_RUNNING), true)
        .await
        .unwrap();
    assert!(idle.enabled && idle.configured && idle.nodes.is_empty());
}

#[tokio::test]
async fn a_linux_node_whose_proxy_is_not_serving_cannot_record_yet() {
    let Some(f) = fixture().await else { return };
    let service = EgressRecordingService::new(
        f.db.connection_arc(),
        FakeCapabilities::with(&[(
            f.worker_a,
            NodeRecordingStatus::Unavailable(RecordingUnavailable::ProxyNotServing),
        )]),
        Some(Arc::new(CollectStore::default())),
    );
    service.register(f.adapter.clone());
    let error = service
        .set_enabled(&f.row(ON_WORKER), true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            EgressRecordingError::Unavailable {
                code: "proxy_not_serving",
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn without_otel_storage_recording_says_so() {
    let Some(f) = fixture().await else { return };
    let service = f.with_store(None);
    let status = service.status(&f.row(ON_WORKER)).await.unwrap();
    assert_eq!(status.reason_code.as_deref(), Some("otel_unavailable"));
    assert!(matches!(
        service.set_enabled(&f.row(ON_WORKER), true).await,
        Err(EgressRecordingError::OtelUnavailable { noun: "box", .. })
    ));
    assert!(matches!(
        service.activity(&f.row(ON_WORKER), None, None, None).await,
        Err(EgressRecordingError::OtelUnavailable { .. })
    ));
}

/// A kind no adapter was registered for has nothing to turn on, and its
/// records are refused at ingest.
#[tokio::test]
async fn a_kind_without_an_adapter_is_not_recorded() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    let job = RecordedWorkload {
        workload: WorkloadRef::new(WorkloadKind::Job, 1),
        ..f.row(ON_WORKER)
    };
    assert!(matches!(
        service.status(&job).await,
        Err(EgressRecordingError::KindNotRecorded {
            kind: WorkloadKind::Job
        })
    ));
    let mut foreign = record(ON_WORKER, "api.example.com", ConnectionOutcome::Allowed);
    foreign.workload = job.workload;
    assert!(matches!(
        service.ingest(f.worker_a, batch(vec![foreign])).await,
        Err(EgressRecordingError::InvalidRecord { .. })
    ));
    assert!(store.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ingest_drops_records_of_a_workload_that_does_not_record() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                ON_WORKER,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (0, 1));
    assert!(store.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ingest_stores_records_attributed_from_the_row() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![
                record(ON_WORKER, "api.example.com", ConnectionOutcome::Allowed),
                record(ON_WORKER, "blocked.example.com", ConnectionOutcome::Refused),
                // Opted out: dropped, not refused.
                record(PROJECTLESS, "api.example.com", ConnectionOutcome::Allowed),
            ]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (2, 1));
    let stored = store.0.lock().unwrap().clone();
    assert!(stored.iter().all(|log| log.project_id == Some(PROJECT)));
    assert!(stored
        .iter()
        .all(|log| log.workload == Some(workload(ON_WORKER))));
    assert!(stored
        .iter()
        .all(|log| log.attributes["temps.box.id"] == "box_4101"));
    assert!(stored
        .iter()
        .all(|log| log.attributes["temps.node.name"] == "worker-a"));
    assert_eq!(stored[1].attributes["temps.egress.reason"], "no_allow_rule");

    // Turned off after the records were taken: the next delivery is dropped.
    service.set_enabled(&f.row(ON_WORKER), false).await.unwrap();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                ON_WORKER,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (0, 1));

    // On, but destroyed since: nothing is recorded for it.
    service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    f.adapter
        .rows
        .lock()
        .unwrap()
        .get_mut(&ON_WORKER)
        .unwrap()
        .live = false;
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                ON_WORKER,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (0, 1));
}

#[tokio::test]
async fn ingest_drops_only_the_records_of_workloads_not_placed_on_the_worker() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    for foreign in [ON_OTHER_WORKER, ON_CONTROL_PLANE] {
        let outcome = service
            .ingest(
                f.worker_a,
                batch(vec![
                    record(ON_WORKER, "api.example.com", ConnectionOutcome::Allowed),
                    record(foreign, "api.example.com", ConnectionOutcome::Allowed),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(
            (outcome.stored, outcome.not_placed),
            (1, 1),
            "the node's own workload is stored, the foreign one dropped"
        );
    }
    let outcome = service
        .ingest(
            f.worker_b,
            batch(vec![record(
                ON_WORKER,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_placed), (0, 1));
    {
        let stored = store.0.lock().unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored
            .iter()
            .all(|log| log.workload == Some(workload(ON_WORKER))));
    }
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                999_999,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!(
        (outcome.stored, outcome.not_recording, outcome.not_placed),
        (0, 1, 0),
        "a deleted workload's record is dropped as no longer recording"
    );
}

const MAX_BATCH: usize = temps_workload_netpol::egress::record::MAX_BATCH_RECORDS;

#[tokio::test]
async fn ingest_refuses_malformed_batches() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let too_many = (0..=MAX_BATCH)
        .map(|_| record(ON_WORKER, "api.example.com", ConnectionOutcome::Allowed))
        .collect();
    assert!(matches!(
        service.ingest(f.worker_a, batch(too_many)).await,
        Err(EgressRecordingError::BatchTooLarge { .. })
    ));
    let mut query = record(ON_WORKER, "api.example.com", ConnectionOutcome::Allowed);
    query.path = Some("/a?token=secret".into());
    assert!(matches!(
        service.ingest(f.worker_a, batch(vec![query])).await,
        Err(EgressRecordingError::InvalidRecord { .. })
    ));
}

#[tokio::test]
async fn node_zero_delivers_through_the_local_sink() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    service
        .set_enabled(&f.row(ON_CONTROL_PLANE), true)
        .await
        .unwrap();
    let sink = LocalRecordSink::new(service.clone());
    sink.deliver(batch(vec![record(
        ON_CONTROL_PLANE,
        "api.example.com",
        ConnectionOutcome::Allowed,
    )]))
    .await
    .unwrap();
    assert_eq!(store.0.lock().unwrap().len(), 1);
    // Node 0 cannot write for a worker's workload either: discarded, and
    // said so, so it is not counted as delivered.
    let delivered = sink
        .deliver(batch(vec![record(
            ON_WORKER,
            "api.example.com",
            ConnectionOutcome::Allowed,
        )]))
        .await
        .unwrap();
    assert_eq!(delivered.discarded, 1);
    assert_eq!(store.0.lock().unwrap().len(), 1, "nothing stored for it");
}

/// A connection of workload `id` to `host`, `seconds` after a fixed start,
/// so the newest-first order is known.
fn at(id: i64, host: &str, outcome: ConnectionOutcome, seconds: u64) -> ConnectionRecord {
    let mut record = record(id, host, outcome);
    record.started_at_ms += seconds * 1000;
    record.last_started_at_ms = record.started_at_ms;
    record
}

/// A workload's page reads its own records from storage, newest first, and
/// none of another workload's in the same project.
#[tokio::test]
async fn a_workload_reads_its_own_recorded_connections_newest_first() {
    let Some(f) = fixture().await else { return };
    let service = f.service_with_otel();
    service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    service
        .set_enabled(&f.row(ON_CONTROL_PLANE), true)
        .await
        .unwrap();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![
                at(
                    ON_WORKER,
                    "first.example.com",
                    ConnectionOutcome::Allowed,
                    0,
                ),
                at(
                    ON_WORKER,
                    "blocked.example.com",
                    ConnectionOutcome::Refused,
                    1,
                ),
                at(ON_WORKER, "last.example.com", ConnectionOutcome::Allowed, 2),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(outcome.stored, 3);

    let row = f.row(ON_WORKER);
    let page = service.activity(&row, None, None, None).await.unwrap();
    assert_eq!(
        (page.workload_kind.as_str(), page.workload_id.as_str()),
        ("sandbox", "box_4101")
    );
    assert_eq!(
        (page.page, page.per_page, page.total, page.has_more),
        (1, 20, 3, false)
    );
    let hosts: Vec<_> = page
        .records
        .iter()
        .map(|r| r.host.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(
        hosts,
        [
            "last.example.com",
            "blocked.example.com",
            "first.example.com"
        ]
    );
    let refused = &page.records[1];
    assert_eq!(refused.outcome, "refused");
    assert_eq!(refused.reason.as_deref(), Some("no_allow_rule"));
    assert_eq!(refused.kind, "tunnel");
    assert_eq!(refused.port, Some(443));
    assert_eq!((refused.bytes_sent, refused.bytes_received), (10, 20));
    assert_eq!(refused.count, 1);
    assert_eq!(refused.node_name.as_deref(), Some("worker-a"));

    let refused_only = service
        .activity(&row, None, None, Some(EgressOutcomeFilter::Refused))
        .await
        .unwrap();
    assert_eq!((refused_only.records.len(), refused_only.total), (1, 1));
    assert_eq!(
        refused_only.records[0].host.as_deref(),
        Some("blocked.example.com")
    );

    let first = service
        .activity(&row, Some(1), Some(2), None)
        .await
        .unwrap();
    assert_eq!(
        (first.records.len(), first.total, first.has_more),
        (2, 3, true)
    );
    let second = service
        .activity(&row, Some(2), Some(2), None)
        .await
        .unwrap();
    assert_eq!((second.records.len(), second.has_more), (1, false));
    assert_eq!(second.records[0].host.as_deref(), Some("first.example.com"));

    // Same project, records of its own: none of the other workload's.
    let other = service
        .activity(&f.row(ON_CONTROL_PLANE), None, None, None)
        .await
        .unwrap();
    assert!(other.records.is_empty());

    // Out-of-range paging is clamped, not an error.
    let clamped = service
        .activity(&row, Some(0), Some(1_000), None)
        .await
        .unwrap();
    assert_eq!((clamped.page, clamped.per_page), (1, 100));
}

/// The node endpoint authenticates the node before it reads the batch, and
/// answers what was stored.
#[tokio::test]
async fn the_node_endpoint_takes_batches_only_with_the_node_token() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    let app = crate::handlers::node_routes(Arc::new(crate::handlers::EgressRecordingNodeState {
        service,
    }));
    let post = |token: Option<&str>, body: String| {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/internal/nodes/{}/egress-records", f.worker_a))
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        app.clone().oneshot(request.body(Body::from(body)).unwrap())
    };
    let body = serde_json::to_string(&batch(vec![record(
        ON_WORKER,
        "api.example.com",
        ConnectionOutcome::Allowed,
    )]))
    .unwrap();

    let unauthenticated = post(None, body.clone()).await.unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let wrong = post(Some("not-the-token"), body.clone()).await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(store.0.lock().unwrap().is_empty());

    let garbage = post(Some(NODE_TOKEN), "{not json".to_string())
        .await
        .unwrap();
    assert_eq!(garbage.status(), StatusCode::BAD_REQUEST);

    let accepted = post(Some(NODE_TOKEN), body).await.unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(store.0.lock().unwrap().len(), 1);
}

/// Node 0 records only the kinds this host's recorder serves: a node-wide
/// "can record" says nothing about a kind whose listener could not start.
#[tokio::test]
async fn the_control_plane_records_only_the_kinds_its_recorder_serves() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let serving = crate::local::LocalServing::new();
    service.set_local_serving(serving.clone());

    let status = service.status(&f.row(ON_CONTROL_PLANE)).await.unwrap();
    assert!(!status.configured, "{status:?}");
    assert_eq!(status.reason_code.as_deref(), Some("kind_not_served"));
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("does not serve boxs") && r.contains("not started")),
        "{status:?}"
    );
    assert_eq!(
        status.nodes[0].reason_code.as_deref(),
        Some("kind_not_served")
    );
    let refused = service
        .set_enabled(&f.row(ON_CONTROL_PLANE), true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            EgressRecordingError::Unavailable {
                code: "kind_not_served",
                node_id: CONTROL_PLANE_NODE_ID,
                ..
            }
        ),
        "{refused:?}"
    );
    assert!(!f.row(ON_CONTROL_PLANE).enabled, "nothing was written");

    // A worker is not this host: its own report decides.
    assert!(service.status(&f.row(ON_WORKER)).await.unwrap().configured);

    serving.set_for_tests(KIND, Ok("127.0.0.1:13128".parse().unwrap()));
    let status = service.status(&f.row(ON_CONTROL_PLANE)).await.unwrap();
    assert!(status.configured, "{status:?}");
    service
        .set_enabled(&f.row(ON_CONTROL_PLANE), true)
        .await
        .unwrap();
}

/// A workload nothing routes to a recorder is refused with the kind's own
/// reason, wherever it runs, and says so in its status.
#[tokio::test]
async fn a_workload_the_recorder_cannot_see_cannot_record() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let reason = "boxes on the vm backend have no route to the egress proxy";
    f.adapter
        .rows
        .lock()
        .unwrap()
        .get_mut(&ON_WORKER)
        .unwrap()
        .unrecordable = Some(reason.to_string());
    let status = service.status(&f.row(ON_WORKER)).await.unwrap();
    assert!(!status.configured);
    assert_eq!(status.reason_code.as_deref(), Some("not_recordable"));
    assert_eq!(status.reason.as_deref(), Some(reason));
    let refused = service
        .set_enabled(&f.row(ON_WORKER), true)
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, EgressRecordingError::NotRecordable { noun: "box", reason: r, .. } if r == reason),
        "{refused:?}"
    );
    assert!(!f.row(ON_WORKER).enabled);
    // Turning it off is never refused.
    service.set_enabled(&f.row(ON_WORKER), false).await.unwrap();
}

/// Everything the answer needs is read before the change is written: when
/// that read fails, the request fails and recording is left as it was, so
/// no change is ever committed behind an error the audit log never sees.
#[tokio::test]
async fn a_failed_status_read_leaves_recording_unchanged() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    assert!(f.row(ON_WORKER).enabled);
    f.adapter.changed.lock().unwrap().clear();

    // The project lookup the answer needs now fails.
    f.db.connection()
        .execute_unprepared("ALTER TABLE projects RENAME TO projects_unavailable")
        .await
        .unwrap();
    let error = service
        .set_enabled(&f.row(ON_WORKER), false)
        .await
        .unwrap_err();
    assert!(
        matches!(error, EgressRecordingError::Database { .. }),
        "{error:?}"
    );
    assert!(f.row(ON_WORKER).enabled, "the change was not written");
    assert!(
        f.adapter.changed.lock().unwrap().is_empty(),
        "nothing was steered"
    );
}

/// A page past what a database offset can address is empty, with the real
/// total, instead of an error from a negative offset.
#[tokio::test]
async fn a_page_past_any_offset_is_empty_not_an_error() {
    let Some(f) = fixture().await else { return };
    let service = f.service_with_otel();
    service.set_enabled(&f.row(ON_WORKER), true).await.unwrap();
    service
        .ingest(
            f.worker_a,
            batch(vec![record(
                ON_WORKER,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    for page in [u64::MAX, u64::MAX / 20 + 2] {
        let result = service
            .activity(&f.row(ON_WORKER), Some(page), None, None)
            .await
            .unwrap();
        assert!(result.records.is_empty() && !result.has_more, "{page}");
        assert_eq!(result.total, 1, "{page}");
    }
}
