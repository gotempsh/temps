// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The opt-in and the ingest against a real, migrated database. Skipped,
//! saying why, when no TimescaleDB container can be started.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sea_orm::{ConnectionTrait, DatabaseBackend, EntityTrait, Statement};
use temps_entities::sandboxes;
use temps_network_policy::NodeRecordingStatus;
use temps_otel::types::LogRecord;
use temps_test_utils::TestDatabase;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::{
    ConnectionKind, ConnectionOutcome, ConnectionRecord, EgressReason, EgressRecordBatch,
    EgressRecordSink, RecordingUnavailable,
};

use super::sink::LocalRecordSink;
use super::*;

pub(crate) const OWNER: i32 = 4101;
pub(crate) const STRANGER: i32 = 4102;
pub(crate) const PROJECT: i32 = 4101;
/// Recording off, project, on worker A.
pub(crate) const ON_WORKER: (i32, &str) = (4101, "sbx_00000000000a4101");
/// No project, on worker A.
pub(crate) const PROJECTLESS: (i32, &str) = (4102, "sbx_00000000000a4102");
/// Project, on the control plane.
pub(crate) const ON_CONTROL_PLANE: (i32, &str) = (4103, "sbx_00000000000a4103");
/// Project, on worker B.
pub(crate) const ON_OTHER_WORKER: (i32, &str) = (4104, "sbx_00000000000a4104");
/// The token worker A authenticates with.
pub(crate) const NODE_TOKEN: &str = "test-node-token-a";

pub(crate) struct Fixture {
    pub db: TestDatabase,
    pub worker_a: i32,
    pub worker_b: i32,
}

/// A migrated database with one owner, one project, two workers and four
/// sandboxes, or `None` (with the reason printed) without Docker.
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
        "INSERT INTO users (id, name, email) VALUES ({OWNER}, 'Owner', 'owner@example.com'), \
           ({STRANGER}, 'Stranger', 'stranger@example.com'); \
         INSERT INTO projects (id, name, slug, repo_name, repo_owner, directory, main_branch, \
           preset, created_at, updated_at) \
         VALUES ({PROJECT}, 'Sample', 'sample-agent', 'sample', 'example', '/', 'main', \
           'static', now(), now());"
    ))
    .await
    .expect("seed users and project");
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
    for ((id, public_id), project, node) in [
        (ON_WORKER, Some(PROJECT), Some(worker_a)),
        (PROJECTLESS, None, Some(worker_a)),
        (ON_CONTROL_PLANE, Some(PROJECT), None),
        (ON_OTHER_WORKER, Some(PROJECT), Some(worker_b)),
    ] {
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO sandboxes (id, public_id, name, user_id, project_id, status, backend, \
             node_id) VALUES ($1, $2, $2, $3, $4, 'running', 'docker', $5)",
            [
                id.into(),
                public_id.into(),
                OWNER.into(),
                project.into(),
                node.into(),
            ],
        ))
        .await
        .expect("insert sandbox");
    }
    Some(Fixture {
        db,
        worker_a,
        worker_b,
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

    pub fn service(&self) -> (Arc<EgressRecordingService>, Arc<CollectStore>) {
        let store = Arc::new(CollectStore::default());
        let service = Arc::new(EgressRecordingService::new(
            self.db.connection_arc(),
            self.capabilities(),
            Some(store.clone()),
        ));
        (service, store)
    }

    async fn recording(&self, id: i32) -> bool {
        sandboxes::Entity::find_by_id(id)
            .one(self.db.connection())
            .await
            .unwrap()
            .unwrap()
            .egress_recording
    }
}

pub(crate) fn record(sandbox: i32, host: &str, outcome: ConnectionOutcome) -> ConnectionRecord {
    ConnectionRecord {
        workload: WorkloadRef::new(WorkloadKind::Sandbox, i64::from(sandbox)),
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
    }
}

fn batch(records: Vec<ConnectionRecord>) -> EgressRecordBatch {
    EgressRecordBatch {
        records,
        dropped: 0,
    }
}

#[tokio::test]
async fn recording_is_off_by_default_and_reports_whether_it_can_work() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let status = service.status(ON_WORKER.1, OWNER).await.unwrap();
    assert!(!status.enabled);
    assert!(status.configured);
    assert_eq!(status.reason, None);
    assert_eq!(status.node_name, "worker-a");
    assert_eq!(
        status.logs_path.as_deref(),
        Some(
            "/projects/sample-agent/telemetry-logs?attribute_key=temps.sandbox.id&attribute_value=sbx_00000000000a4101"
        )
    );

    let mac = service.status(ON_OTHER_WORKER.1, OWNER).await.unwrap();
    assert!(!mac.configured);
    assert_eq!(mac.reason_code.as_deref(), Some("unsupported_platform"));
    assert!(mac.reason.unwrap().contains("macos"));
    assert_eq!(
        mac.setup_path, None,
        "nothing on the node page fixes a platform"
    );

    let projectless = service.status(PROJECTLESS.1, OWNER).await.unwrap();
    assert_eq!(projectless.reason_code.as_deref(), Some("no_project"));
    assert_eq!(projectless.logs_path, None);
}

#[tokio::test]
async fn an_owner_turns_recording_on_and_off() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let (previous, status) = service.set_enabled(ON_WORKER.1, OWNER, true).await.unwrap();
    assert!(!previous);
    assert!(status.enabled && status.configured);
    assert!(f.recording(ON_WORKER.0).await);
    // Setting the same value again changes nothing and still succeeds.
    let (previous, _) = service.set_enabled(ON_WORKER.1, OWNER, true).await.unwrap();
    assert!(previous);
    let (previous, status) = service
        .set_enabled(ON_WORKER.1, OWNER, false)
        .await
        .unwrap();
    assert!(previous);
    assert!(!status.enabled);
    assert!(!f.recording(ON_WORKER.0).await);
}

#[tokio::test]
async fn enabling_on_a_node_that_cannot_record_names_the_sandbox_and_the_node() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let error = service
        .set_enabled(ON_OTHER_WORKER.1, OWNER, true)
        .await
        .unwrap_err();
    let EgressRecordingError::Unavailable {
        sandbox_id,
        node_id,
        node_name,
        code,
        ..
    } = &error
    else {
        panic!("expected Unavailable, got {error:?}");
    };
    assert_eq!(sandbox_id, ON_OTHER_WORKER.1);
    assert_eq!(*node_id, f.worker_b);
    assert_eq!(node_name, "worker-b");
    assert_eq!(*code, "unsupported_platform");
    let message = error.to_string();
    assert!(message.contains(ON_OTHER_WORKER.1) && message.contains("worker-b"));
    assert!(!f.recording(ON_OTHER_WORKER.0).await, "nothing was written");
    // Turning it off is never refused.
    service
        .set_enabled(ON_OTHER_WORKER.1, OWNER, false)
        .await
        .unwrap();
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
    let error = service
        .set_enabled(ON_WORKER.1, OWNER, true)
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
async fn a_projectless_sandbox_cannot_record() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let error = service
        .set_enabled(PROJECTLESS.1, OWNER, true)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, EgressRecordingError::NoProject { sandbox_id } if sandbox_id == PROJECTLESS.1),
        "{error:?}"
    );
}

#[tokio::test]
async fn without_otel_storage_recording_says_so() {
    let Some(f) = fixture().await else { return };
    let service = EgressRecordingService::new(f.db.connection_arc(), f.capabilities(), None);
    let status = service.status(ON_WORKER.1, OWNER).await.unwrap();
    assert_eq!(status.reason_code.as_deref(), Some("otel_unavailable"));
    assert!(matches!(
        service.set_enabled(ON_WORKER.1, OWNER, true).await,
        Err(EgressRecordingError::OtelUnavailable { .. })
    ));
}

#[tokio::test]
async fn only_the_owner_sees_or_changes_recording() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    for result in [
        service.status(ON_WORKER.1, STRANGER).await.map(|_| ()),
        service
            .set_enabled(ON_WORKER.1, STRANGER, true)
            .await
            .map(|_| ()),
        service.status("sbx_not-an-id", OWNER).await.map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(EgressRecordingError::SandboxNotFound { .. })
        ));
    }
}

#[tokio::test]
async fn ingest_drops_records_of_a_sandbox_that_does_not_record() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                ON_WORKER.0,
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
    service.set_enabled(ON_WORKER.1, OWNER, true).await.unwrap();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![
                record(ON_WORKER.0, "api.example.com", ConnectionOutcome::Allowed),
                record(
                    ON_WORKER.0,
                    "blocked.example.com",
                    ConnectionOutcome::Refused,
                ),
                // Opted out and projectless: dropped, not refused.
                record(PROJECTLESS.0, "api.example.com", ConnectionOutcome::Allowed),
            ]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (2, 1));
    let stored = store.0.lock().unwrap().clone();
    assert!(stored.iter().all(|log| log.project_id == PROJECT));
    assert!(stored
        .iter()
        .all(|log| log.attributes["temps.sandbox.id"] == ON_WORKER.1));
    assert!(stored
        .iter()
        .all(|log| log.attributes["temps.node.name"] == "worker-a"));
    assert_eq!(stored[1].attributes["temps.egress.reason"], "no_allow_rule");

    // Turned off after the records were taken: the next delivery is dropped.
    service
        .set_enabled(ON_WORKER.1, OWNER, false)
        .await
        .unwrap();
    let outcome = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                ON_WORKER.0,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (0, 1));
}

#[tokio::test]
async fn ingest_refuses_a_worker_reporting_a_sandbox_not_placed_on_it() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    service.set_enabled(ON_WORKER.1, OWNER, true).await.unwrap();
    for (node, foreign) in [
        (f.worker_a, ON_OTHER_WORKER.0),
        (f.worker_a, ON_CONTROL_PLANE.0),
        (f.worker_b, ON_WORKER.0),
    ] {
        let error = service
            .ingest(
                node,
                batch(vec![
                    record(ON_WORKER.0, "api.example.com", ConnectionOutcome::Allowed),
                    record(foreign, "api.example.com", ConnectionOutcome::Allowed),
                ]),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, EgressRecordingError::NotPlacedOnNode { node_id, .. } if node_id == node),
            "{error:?}"
        );
    }
    assert!(
        store.0.lock().unwrap().is_empty(),
        "a refused batch stores nothing, not even its legitimate records"
    );
    let error = service
        .ingest(
            f.worker_a,
            batch(vec![record(
                999_999,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        EgressRecordingError::UnknownSandbox {
            sandbox: 999_999,
            ..
        }
    ));
}

#[tokio::test]
async fn ingest_refuses_malformed_batches() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    let too_many = (0..=MAX_BATCH)
        .map(|_| record(ON_WORKER.0, "api.example.com", ConnectionOutcome::Allowed))
        .collect();
    assert!(matches!(
        service.ingest(f.worker_a, batch(too_many)).await,
        Err(EgressRecordingError::BatchTooLarge { .. })
    ));
    let mut deployment = record(ON_WORKER.0, "api.example.com", ConnectionOutcome::Allowed);
    deployment.workload = WorkloadRef::new(WorkloadKind::Deployment, 1);
    let mut query = record(ON_WORKER.0, "api.example.com", ConnectionOutcome::Allowed);
    query.path = Some("/a?token=secret".into());
    for bad in [deployment, query] {
        assert!(matches!(
            service.ingest(f.worker_a, batch(vec![bad])).await,
            Err(EgressRecordingError::InvalidRecord { .. })
        ));
    }
}

const MAX_BATCH: usize = temps_workload_netpol::egress::record::MAX_BATCH_RECORDS;

#[tokio::test]
async fn node_zero_delivers_through_the_local_sink() {
    let Some(f) = fixture().await else { return };
    let (service, store) = f.service();
    service
        .set_enabled(ON_CONTROL_PLANE.1, OWNER, true)
        .await
        .unwrap();
    let sink = LocalRecordSink::new(service.clone());
    sink.deliver(batch(vec![record(
        ON_CONTROL_PLANE.0,
        "api.example.com",
        ConnectionOutcome::Allowed,
    )]))
    .await
    .unwrap();
    assert_eq!(store.0.lock().unwrap().len(), 1);
    // Node 0 cannot write for a worker's sandbox either.
    assert!(sink
        .deliver(batch(vec![record(
            ON_WORKER.0,
            "api.example.com",
            ConnectionOutcome::Allowed
        )]))
        .await
        .is_err());
}

#[tokio::test]
async fn the_node_snapshot_tells_the_proxy_which_sandboxes_record() {
    let Some(f) = fixture().await else { return };
    let (service, _) = f.service();
    service.set_enabled(ON_WORKER.1, OWNER, true).await.unwrap();
    // A projectless sandbox whose column was set anyway is never recorded.
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE sandboxes SET egress_recording = true WHERE id = {}",
            PROJECTLESS.0
        ))
        .await
        .unwrap();
    let policy = temps_network_policy::NetworkPolicyService::new(f.db.connection_arc());
    let snapshot = policy.node_snapshot(f.worker_a).await.unwrap();
    let recorded: Vec<WorkloadRef> = snapshot
        .egress
        .iter()
        .filter(|p| p.record)
        .map(|p| p.subject)
        .collect();
    assert_eq!(
        recorded,
        vec![WorkloadRef::new(
            WorkloadKind::Sandbox,
            i64::from(ON_WORKER.0)
        )]
    );
}
