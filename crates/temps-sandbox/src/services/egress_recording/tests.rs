// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The sandbox side of recording against a real, migrated database: who
//! may see and change it, and what the adapter tells the shared service.
//! The flow itself is tested once, in `temps-workload-egress`. Skipped,
//! saying why, when no TimescaleDB container can be started.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseBackend, EntityTrait, Statement};
use temps_entities::sandboxes;
use temps_network_policy::NodeRecordingStatus;
use temps_otel::types::{LogQuery, LogRecord};
use temps_otel::OtelError;
use temps_test_utils::TestDatabase;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::{EgressLogStore, EgressRecordingService, NodeRecordingCapabilities};
use temps_workload_netpol::{
    ConnectionKind, ConnectionOutcome, ConnectionRecord, EgressReason, EgressRecordBatch,
    RecordingUnavailable,
};

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
        // Only a missing Docker skips: a migration or schema check that
        // fails is a failure, never a test that passes without running.
        Err(error) if !temps_test_utils::docker_available().await => {
            eprintln!("Docker unavailable, skipping egress recording test: {error}");
            return None;
        }
        Err(error) => panic!("the test database could not be prepared: {error}"),
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

/// The shared recording service with the sandbox adapter registered, and
/// the sandbox API over it, as the plugin wires them.
pub(crate) struct Wired {
    pub sandboxes: Arc<SandboxEgress>,
    pub recording: Arc<EgressRecordingService>,
    pub adapter: Arc<SandboxEgressAdapter>,
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

    pub fn wired(&self, store: Option<Arc<dyn EgressLogStore>>) -> Wired {
        let recording = Arc::new(EgressRecordingService::new(
            self.db.connection_arc(),
            self.capabilities(),
            store,
        ));
        let adapter = Arc::new(SandboxEgressAdapter::new(self.db.connection_arc()));
        recording.register(adapter.clone());
        Wired {
            sandboxes: Arc::new(SandboxEgress::new(
                self.db.connection_arc(),
                recording.clone(),
            )),
            recording,
            adapter,
        }
    }

    /// Over real OpenTelemetry storage in the fixture's database, as
    /// production wires it.
    pub fn with_otel(&self) -> Wired {
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
        self.wired(Some(otel as Arc<dyn EgressLogStore>))
    }

    /// Over a store that only collects.
    pub fn collecting(&self) -> (Wired, Arc<CollectStore>) {
        let store = Arc::new(CollectStore::default());
        (self.wired(Some(store.clone())), store)
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

/// A sandbox is one workload on one node, in its project or in none, and
/// the opt-in is its own column.
#[tokio::test]
async fn the_adapter_describes_sandboxes_from_their_rows_and_stores_the_opt_in() {
    let Some(f) = fixture().await else { return };
    let (wired, _) = f.collecting();
    let adapter = &wired.adapter;
    assert_eq!(adapter.kind(), WorkloadKind::Sandbox);
    assert_eq!(adapter.id_attribute(), "temps.sandbox.id");

    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE sandboxes SET status = 'destroyed' WHERE id = {}",
            ON_OTHER_WORKER.0
        ))
        .await
        .unwrap();
    let mut rows = adapter
        .load(&[
            i64::from(ON_WORKER.0),
            i64::from(PROJECTLESS.0),
            i64::from(ON_CONTROL_PLANE.0),
            i64::from(ON_OTHER_WORKER.0),
            999_999,
            i64::MAX,
        ])
        .await
        .unwrap();
    rows.sort_by_key(|row| row.workload.id);
    let seen: Vec<(&str, Option<i32>, &[i32], bool)> = rows
        .iter()
        .map(|row| {
            (
                row.public_id.as_str(),
                row.project_id,
                row.nodes.as_slice(),
                row.live,
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (ON_WORKER.1, Some(PROJECT), &[f.worker_a][..], true),
            (PROJECTLESS.1, None, &[f.worker_a][..], true),
            (
                ON_CONTROL_PLANE.1,
                Some(PROJECT),
                &[CONTROL_PLANE_NODE_ID][..],
                true
            ),
            (ON_OTHER_WORKER.1, Some(PROJECT), &[f.worker_b][..], false),
        ],
        "unknown ids are left out; a destroyed sandbox is not live"
    );

    let on_worker = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(ON_WORKER.0));
    adapter.set_enabled(on_worker, true).await.unwrap();
    assert!(f.recording(ON_WORKER.0).await);
    assert!(adapter.load(&[on_worker.id]).await.unwrap()[0].enabled);
    adapter.set_enabled(on_worker, false).await.unwrap();
    assert!(!f.recording(ON_WORKER.0).await);
}

#[tokio::test]
async fn an_owner_turns_recording_on_and_off_for_a_sandbox_with_or_without_a_project() {
    let Some(f) = fixture().await else { return };
    let (wired, _) = f.collecting();
    let sandboxes = &wired.sandboxes;
    let status = sandboxes.status(ON_WORKER.1, OWNER).await.unwrap();
    assert_eq!(
        (status.workload_kind.as_str(), status.workload_id.as_str()),
        ("sandbox", ON_WORKER.1)
    );
    assert!(!status.enabled && status.configured);
    assert_eq!(status.nodes[0].node_name, "worker-a");
    assert_eq!(
        status.logs_path.as_deref(),
        Some(
            "/projects/sample-agent/telemetry-logs?attribute_key=temps.sandbox.id&attribute_value=sbx_00000000000a4101"
        )
    );

    let (previous, status) = sandboxes
        .set_enabled(ON_WORKER.1, OWNER, true)
        .await
        .unwrap();
    assert!(!previous && status.enabled);
    assert!(f.recording(ON_WORKER.0).await);
    let (previous, status) = sandboxes
        .set_enabled(ON_WORKER.1, OWNER, false)
        .await
        .unwrap();
    assert!(previous && !status.enabled);
    assert!(!f.recording(ON_WORKER.0).await);

    // No project: the records are the sandbox's own, read from its page.
    let (_, projectless) = sandboxes
        .set_enabled(PROJECTLESS.1, OWNER, true)
        .await
        .unwrap();
    assert!(projectless.enabled && projectless.configured);
    assert_eq!(
        (projectless.project_id, projectless.logs_path),
        (None, None)
    );
    assert!(f.recording(PROJECTLESS.0).await);
}

#[tokio::test]
async fn a_node_that_cannot_record_refuses_naming_the_sandbox() {
    let Some(f) = fixture().await else { return };
    let (wired, _) = f.collecting();
    let error = wired
        .sandboxes
        .set_enabled(ON_OTHER_WORKER.1, OWNER, true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            EgressRecordingError::Unavailable { noun: "sandbox", id, node_name, code: "unsupported_platform", .. }
                if id == ON_OTHER_WORKER.1 && node_name == "worker-b"
        ),
        "{error:?}"
    );
    assert!(!f.recording(ON_OTHER_WORKER.0).await, "nothing was written");
}

#[tokio::test]
async fn only_the_owner_sees_or_changes_recording() {
    let Some(f) = fixture().await else { return };
    let (wired, _) = f.collecting();
    let sandboxes = &wired.sandboxes;
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE sandboxes SET status = 'destroyed' WHERE id = {}",
            ON_CONTROL_PLANE.0
        ))
        .await
        .unwrap();
    for result in [
        sandboxes.status(ON_WORKER.1, STRANGER).await.map(|_| ()),
        sandboxes
            .set_enabled(ON_WORKER.1, STRANGER, true)
            .await
            .map(|_| ()),
        sandboxes
            .activity(ON_WORKER.1, STRANGER, None, None, None)
            .await
            .map(|_| ()),
        sandboxes.status("sbx_not-an-id", OWNER).await.map(|_| ()),
        // A destroyed sandbox is gone, even for its owner.
        sandboxes
            .status(ON_CONTROL_PLANE.1, OWNER)
            .await
            .map(|_| ()),
    ] {
        assert!(
            matches!(
                result,
                Err(EgressRecordingError::NotFound {
                    noun: "sandbox",
                    ..
                })
            ),
            "{result:?}"
        );
    }
    assert!(!f.recording(ON_WORKER.0).await);
}

/// The shared ingest, with sandboxes as the kind: a sandbox's records are
/// taken only from its own node, stored under the sandbox, and read back
/// by its owner from its own page, with or without a project.
#[tokio::test]
async fn a_sandboxs_records_come_from_its_node_and_are_read_by_its_owner() {
    let Some(f) = fixture().await else { return };
    let wired = f.with_otel();
    for id in [ON_WORKER.1, PROJECTLESS.1] {
        wired.sandboxes.set_enabled(id, OWNER, true).await.unwrap();
    }
    let foreign = wired
        .recording
        .ingest(
            f.worker_b,
            batch(vec![record(
                ON_WORKER.0,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )]),
        )
        .await
        .unwrap();
    assert_eq!(
        (foreign.stored, foreign.not_placed),
        (0, 1),
        "a node that does not host the sandbox stores nothing for it"
    );

    let outcome = wired
        .recording
        .ingest(
            f.worker_a,
            batch(vec![
                record(ON_WORKER.0, "api.example.com", ConnectionOutcome::Allowed),
                record(
                    PROJECTLESS.0,
                    "blocked.example.com",
                    ConnectionOutcome::Refused,
                ),
            ]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (2, 0));

    let page = wired
        .sandboxes
        .activity(ON_WORKER.1, OWNER, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        (page.workload_kind.as_str(), page.workload_id.as_str()),
        ("sandbox", ON_WORKER.1)
    );
    assert_eq!(page.total, 1);
    assert_eq!(page.records[0].host.as_deref(), Some("api.example.com"));
    assert_eq!(page.records[0].node_name.as_deref(), Some("worker-a"));

    let projectless = wired
        .sandboxes
        .activity(PROJECTLESS.1, OWNER, None, None, None)
        .await
        .unwrap();
    assert_eq!(projectless.total, 1);
    assert_eq!(projectless.records[0].outcome, "refused");
    assert_eq!(
        projectless.records[0].reason.as_deref(),
        Some("no_allow_rule")
    );
}

#[tokio::test]
async fn the_node_snapshot_tells_the_proxy_which_sandboxes_record() {
    let Some(f) = fixture().await else { return };
    let (wired, _) = f.collecting();
    for id in [ON_WORKER.1, PROJECTLESS.1] {
        wired.sandboxes.set_enabled(id, OWNER, true).await.unwrap();
    }
    let policy = temps_network_policy::NetworkPolicyService::new(f.db.connection_arc());
    let snapshot = policy.node_snapshot(f.worker_a).await.unwrap();
    let mut recorded: Vec<WorkloadRef> = snapshot
        .egress
        .iter()
        .filter(|p| p.record)
        .map(|p| p.subject)
        .collect();
    recorded.sort();
    assert_eq!(
        recorded,
        vec![
            WorkloadRef::new(WorkloadKind::Sandbox, i64::from(ON_WORKER.0)),
            WorkloadRef::new(WorkloadKind::Sandbox, i64::from(PROJECTLESS.0)),
        ],
        "every opted-in sandbox, with or without a project"
    );
}

/// Only a Docker sandbox has the sidecar that hands its connections to the
/// recorder: one on another backend says so and cannot turn recording on,
/// wherever it runs.
#[tokio::test]
async fn a_sandbox_on_another_backend_cannot_record() {
    let Some(f) = fixture().await else { return };
    let (wired, _) = f.collecting();
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE sandboxes SET backend = 'firecracker' WHERE id = {}",
            ON_CONTROL_PLANE.0
        ))
        .await
        .unwrap();
    let status = wired
        .sandboxes
        .status(ON_CONTROL_PLANE.1, OWNER)
        .await
        .unwrap();
    assert!(!status.configured);
    assert_eq!(status.reason_code.as_deref(), Some("not_recordable"));
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("firecracker") && r.contains("only Docker sandboxes")),
        "{status:?}"
    );
    let refused = wired
        .sandboxes
        .set_enabled(ON_CONTROL_PLANE.1, OWNER, true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            EgressRecordingError::NotRecordable {
                noun: "sandbox",
                ..
            }
        ),
        "{refused:?}"
    );
    assert!(
        !f.recording(ON_CONTROL_PLANE.0).await,
        "nothing was written"
    );
    assert!(
        records_on_backend(None) && records_on_backend(Some("docker")),
        "a row from before the column is a Docker sandbox"
    );
}

struct NoCapability;

impl temps_workload_egress::local::LocalCapability for NoCapability {
    fn set(&self, _capability: temps_workload_netpol::EgressRecordingCapability) {}
}

/// The recording service holds the sandbox adapter and the recorder holds
/// the service: the adapter's steering must not close that loop, or
/// dropping the plugin graph would never drop the recorder (its tasks, its
/// listener and its port).
#[tokio::test]
async fn the_adapter_never_keeps_the_recorder_alive() {
    let Some(f) = fixture().await else { return };
    let (wired, _) = f.collecting();
    let recorder = temps_workload_egress::local::LocalEgressRecorder::new(
        wired.recording.clone(),
        Arc::new(NoCapability),
        temps_workload_egress::local::LocalServing::new(),
        temps_workload_egress::steering::InstallationId::from_bytes([7; 8]),
    );
    let traffic = Arc::new(super::local::SandboxTraffic::new(
        f.db.connection_arc(),
        bollard::Docker::connect_with_local_defaults().expect("docker client"),
        None,
    ));
    wired.adapter.set_local(super::LocalSteering {
        recorder: Arc::downgrade(&recorder),
        traffic,
    });
    let weak = Arc::downgrade(&recorder);
    drop(recorder);
    assert!(
        weak.upgrade().is_none(),
        "freed with its last outside reference"
    );
}
