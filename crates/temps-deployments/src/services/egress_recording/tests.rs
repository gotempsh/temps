// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The environment side of recording against a real, migrated database:
//! what the adapter tells the shared service, who may see and change an
//! environment's recording, and what a deployment does with it. The flow
//! itself is tested once, in `temps-workload-egress`. Skipped, saying why,
//! when no TimescaleDB container can be started.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseBackend, EntityTrait, Statement};
use temps_entities::environments;
use temps_network_policy::NodeRecordingStatus;
use temps_otel::types::{LogQuery, LogRecord};
use temps_otel::OtelError;
use temps_test_utils::TestDatabase;
use temps_workload_egress::{EgressLogStore, NodeRecordingCapabilities};
use temps_workload_netpol::{
    ConnectionKind, ConnectionOutcome, ConnectionRecord, EgressRecordBatch, RecordingUnavailable,
};

use super::*;

pub(crate) const PROJECT: i32 = 6101;
pub(crate) const OTHER_PROJECT: i32 = 6102;
/// One container on the control plane and one on the worker.
pub(crate) const PRODUCTION: i32 = 6101;
/// Deployed once, its container removed since.
pub(crate) const STAGING: i32 = 6102;
/// Of the other project, on the worker that cannot record.
pub(crate) const ELSEWHERE: i32 = 6103;
/// Deleted.
pub(crate) const DELETED: i32 = 6104;

pub(crate) struct Fixture {
    pub db: TestDatabase,
    pub worker: i32,
    pub mac: i32,
}

/// A migrated database with two projects, two workers and four
/// environments, or `None` (with the reason printed) without Docker.
pub(crate) async fn fixture() -> Option<Fixture> {
    let db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        // Only a missing Docker skips: a migration or schema check that
        // fails is a failure, never a test that passes without running.
        Err(error) if !temps_test_utils::docker_available().await => {
            eprintln!("Docker unavailable, skipping environment egress test: {error}");
            return None;
        }
        Err(error) => panic!("the test database could not be prepared: {error}"),
    };
    let conn = db.connection();
    conn.execute_unprepared(&format!(
        "INSERT INTO projects (id, name, slug, repo_name, repo_owner, directory, main_branch, \
           preset, created_at, updated_at) VALUES \
           ({PROJECT}, 'Shop', 'shop', 'shop', 'example', '/', 'main', 'nodejs', now(), now()), \
           ({OTHER_PROJECT}, 'Blog', 'blog', 'blog', 'example', '/', 'main', 'nodejs', now(), \
            now());"
    ))
    .await
    .expect("seed projects");
    let mut nodes = Vec::new();
    for name in ["worker-a", "worker-mac"] {
        let row = conn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
                 labels, capacity) VALUES ($1, $2, '127.0.0.1', '10.0.0.1', 'worker', 'active', \
                 '{}', '{}') RETURNING id",
                [name.into(), temps_core::crypto::sha256_hex(name).into()],
            ))
            .await
            .expect("insert node")
            .expect("node id");
        nodes.push(row.try_get::<i32>("", "id").expect("id"));
    }
    let (worker, mac) = (nodes[0], nodes[1]);
    for (id, project, slug, deleted) in [
        (PRODUCTION, PROJECT, "production", false),
        (STAGING, PROJECT, "staging", false),
        (ELSEWHERE, OTHER_PROJECT, "production", false),
        (DELETED, PROJECT, "old", true),
    ] {
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO environments (id, name, slug, subdomain, host, upstreams, created_at, \
             updated_at, project_id, deleted_at) VALUES ($1, $2, $2, $3, $4, '[]', now(), \
             now(), $5, CASE WHEN $6 THEN now() END)",
            [
                id.into(),
                slug.into(),
                format!("env-{id}").into(),
                format!("env-{id}.example.com").into(),
                project.into(),
                deleted.into(),
            ],
        ))
        .await
        .expect("insert environment");
    }
    // (environment, node, removed)
    for (index, (environment, node, removed)) in [
        (PRODUCTION, None, false),
        (PRODUCTION, Some(worker), false),
        (PRODUCTION, Some(worker), false),
        (STAGING, Some(worker), true),
        (ELSEWHERE, Some(mac), false),
    ]
    .into_iter()
    .enumerate()
    {
        let project = if environment == ELSEWHERE {
            OTHER_PROJECT
        } else {
            PROJECT
        };
        let deployment = conn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO deployments (project_id, environment_id, slug, state, created_at, \
                 updated_at, metadata) VALUES ($1, $2, $3, 'running', now(), now(), '{}') \
                 RETURNING id",
                [
                    project.into(),
                    environment.into(),
                    format!("deploy-{index}").into(),
                ],
            ))
            .await
            .expect("insert deployment")
            .expect("deployment id")
            .try_get::<i32>("", "id")
            .expect("id");
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO deployment_containers (deployment_id, container_id, container_name, \
             status, node_id, deployed_at, created_at, deleted_at) VALUES ($1, $2, $2, \
             'running', $3, now(), now(), CASE WHEN $4 THEN now() END)",
            [
                deployment.into(),
                format!("container-{index}").into(),
                node.into(),
                removed.into(),
            ],
        ))
        .await
        .expect("insert container");
    }
    Some(Fixture { db, worker, mac })
}

struct FakeCapabilities(HashMap<i32, NodeRecordingStatus>);

impl NodeRecordingCapabilities for FakeCapabilities {
    fn status(&self, node_id: i32) -> NodeRecordingStatus {
        self.0
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
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|log| query.workload.is_some() && log.workload == query.workload)
            .cloned()
            .collect())
    }

    async fn count(&self, query: LogQuery) -> Result<u64, OtelError> {
        Ok(self.query(query).await?.len() as u64)
    }
}

/// The shared recording service with the environment adapter registered,
/// and the environment API over it, as the plugin wires them.
pub(crate) struct Wired {
    pub environments: Arc<EnvironmentEgress>,
    pub recording: Arc<EgressRecordingService>,
    pub adapter: Arc<EnvironmentEgressAdapter>,
    pub store: Arc<CollectStore>,
}

impl Fixture {
    /// The control plane records; the worker's proxy is not serving yet and
    /// the Mac never will.
    pub fn wired(&self) -> Wired {
        let capabilities = FakeCapabilities(HashMap::from([
            (CONTROL_PLANE_NODE_ID, NodeRecordingStatus::Available),
            (
                self.worker,
                NodeRecordingStatus::Unavailable(RecordingUnavailable::ProxyNotServing),
            ),
            (
                self.mac,
                NodeRecordingStatus::Unavailable(RecordingUnavailable::UnsupportedPlatform {
                    os: "macos".into(),
                }),
            ),
        ]));
        let store = Arc::new(CollectStore::default());
        let recording = Arc::new(EgressRecordingService::new(
            self.db.connection_arc(),
            Arc::new(capabilities),
            Some(store.clone()),
        ));
        let adapter = Arc::new(EnvironmentEgressAdapter::new(self.db.connection_arc()));
        recording.register(adapter.clone());
        Wired {
            environments: Arc::new(EnvironmentEgress::new(
                self.db.connection_arc(),
                recording.clone(),
            )),
            recording,
            adapter,
            store,
        }
    }

    async fn recording(&self, id: i32) -> bool {
        environments::Entity::find_by_id(id)
            .one(self.db.connection())
            .await
            .unwrap()
            .unwrap()
            .egress_recording
    }
}

fn record(environment: i32, host: &str) -> ConnectionRecord {
    ConnectionRecord {
        workload: workload(environment),
        started_at_ms: 1_700_000_000_000,
        last_started_at_ms: 1_700_000_000_000,
        duration_ms: 12,
        kind: ConnectionKind::Tunnel,
        outcome: ConnectionOutcome::Allowed,
        reason: None,
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

fn batch(records: Vec<ConnectionRecord>) -> EgressRecordBatch {
    EgressRecordBatch {
        records,
        dropped: 0,
    }
}

/// An environment is the workload `deployment/<id>`, always in a project,
/// on every node that hosts a live container of it, and the opt-in is its
/// own column.
#[tokio::test]
async fn the_adapter_describes_environments_from_their_rows_and_containers() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    let adapter = &wired.adapter;
    assert_eq!(adapter.kind(), WorkloadKind::Deployment);
    assert_eq!(adapter.id_attribute(), "temps.environment.id");

    let mut rows = adapter
        .load(&[
            i64::from(PRODUCTION),
            i64::from(STAGING),
            i64::from(ELSEWHERE),
            i64::from(DELETED),
            999_999,
            i64::MAX,
        ])
        .await
        .unwrap();
    rows.sort_by_key(|row| row.workload.id);
    let seen: Vec<(&str, Option<i32>, Vec<i32>, bool)> = rows
        .iter()
        .map(|row| {
            (
                row.public_id.as_str(),
                row.project_id,
                row.nodes.clone(),
                row.live,
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                "6101",
                Some(PROJECT),
                vec![CONTROL_PLANE_NODE_ID, f.worker],
                true
            ),
            ("6102", Some(PROJECT), vec![], true),
            ("6103", Some(OTHER_PROJECT), vec![f.mac], true),
            ("6104", Some(PROJECT), vec![], false),
        ],
        "each node once, removed containers and unknown ids left out, a deleted \
         environment not live"
    );
    assert_eq!(rows[0].workload, workload(PRODUCTION));

    adapter
        .set_enabled(workload(PRODUCTION), true)
        .await
        .unwrap();
    assert!(f.recording(PRODUCTION).await);
    assert!(
        adapter.load(&[i64::from(PRODUCTION)]).await.unwrap()[0].enabled,
        "the switch is read back from the row"
    );
    adapter
        .set_enabled(workload(PRODUCTION), false)
        .await
        .unwrap();
    assert!(!f.recording(PRODUCTION).await);
}

/// The same flow sandboxes use: an environment records where one of its
/// nodes can, one that runs nowhere yet can be turned on, and one whose
/// only node cannot record is refused, naming the environment and the node.
#[tokio::test]
async fn an_environment_records_where_one_of_its_nodes_can() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    let environments = &wired.environments;

    let status = environments.status(PROJECT, PRODUCTION).await.unwrap();
    assert_eq!(
        (status.workload_kind.as_str(), status.workload_id.as_str()),
        ("deployment", "6101")
    );
    assert!(!status.enabled && status.configured);
    assert_eq!(status.project_id, Some(PROJECT));
    assert_eq!(
        status.logs_path.as_deref(),
        Some(
            "/projects/shop/telemetry-logs?attribute_key=temps.environment.id&attribute_value=6101"
        )
    );
    let nodes: Vec<(&str, bool, Option<&str>)> = status
        .nodes
        .iter()
        .map(|n| (n.node_name.as_str(), n.recording, n.reason_code.as_deref()))
        .collect();
    assert_eq!(
        nodes,
        vec![
            ("control-plane", true, None),
            ("worker-a", false, Some("proxy_not_serving")),
        ]
    );

    let (previous, on) = environments
        .set_enabled(PROJECT, PRODUCTION, true)
        .await
        .unwrap();
    assert!(!previous && on.enabled && on.configured);
    assert!(f.recording(PRODUCTION).await);

    let (_, idle) = environments
        .set_enabled(PROJECT, STAGING, true)
        .await
        .unwrap();
    assert!(idle.enabled && idle.configured && idle.nodes.is_empty());

    let refused = environments
        .set_enabled(OTHER_PROJECT, ELSEWHERE, true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &refused,
            EgressRecordingError::Unavailable { noun: "environment", id, node_name, code: "unsupported_platform", .. }
                if id == "6103" && node_name == "worker-mac"
        ),
        "{refused:?}"
    );
    assert!(!f.recording(ELSEWHERE).await, "nothing was written");
}

/// An environment is reached only through its own project: another
/// project's id, a deleted environment and an unknown one are all not
/// found, and nothing is changed.
#[tokio::test]
async fn an_environment_is_found_only_in_its_own_project() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    let environments = &wired.environments;
    for result in [
        environments
            .status(OTHER_PROJECT, PRODUCTION)
            .await
            .map(|_| ()),
        environments
            .set_enabled(OTHER_PROJECT, PRODUCTION, true)
            .await
            .map(|_| ()),
        environments
            .activity(OTHER_PROJECT, PRODUCTION, None, None, None)
            .await
            .map(|_| ()),
        environments.status(PROJECT, DELETED).await.map(|_| ()),
        environments.status(PROJECT, 999_999).await.map(|_| ()),
    ] {
        assert!(
            matches!(
                result,
                Err(EgressRecordingError::NotFound {
                    noun: "environment",
                    ..
                })
            ),
            "{result:?}"
        );
    }
    assert!(!f.recording(PRODUCTION).await);
}

/// The shared ingest, with environments as the kind: records are taken
/// from a node that hosts the environment, stored under the environment
/// and its project, and read back from the environment's own page.
#[tokio::test]
async fn an_environments_records_come_from_its_nodes_and_are_read_from_its_page() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    wired
        .environments
        .set_enabled(PROJECT, PRODUCTION, true)
        .await
        .unwrap();

    let foreign = wired
        .recording
        .ingest(f.mac, batch(vec![record(PRODUCTION, "api.example.com")]))
        .await
        .unwrap();
    assert_eq!(
        (foreign.stored, foreign.not_placed),
        (0, 1),
        "a node that does not host the environment stores nothing for it"
    );

    let outcome = wired
        .recording
        .ingest(
            CONTROL_PLANE_NODE_ID,
            batch(vec![
                record(PRODUCTION, "api.example.com"),
                // Staging never opted in: dropped, not refused... but it is
                // not placed here either, so it is not in this batch.
            ]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (1, 0));
    let stored = wired.store.0.lock().unwrap().clone();
    assert_eq!(stored[0].project_id, Some(PROJECT));
    assert_eq!(stored[0].workload, Some(workload(PRODUCTION)));
    assert_eq!(stored[0].attributes["temps.environment.id"], "6101");
    assert_eq!(stored[0].attributes["temps.workload.kind"], "deployment");
    assert_eq!(stored[0].attributes["temps.node.name"], "control-plane");

    let page = wired
        .environments
        .activity(PROJECT, PRODUCTION, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        (page.workload_kind.as_str(), page.workload_id.as_str()),
        ("deployment", "6101")
    );
    assert_eq!(page.total, 1);
    assert_eq!(page.records[0].host.as_deref(), Some("api.example.com"));

    // Turned off: what its proxy still sends is dropped.
    wired
        .environments
        .set_enabled(PROJECT, PRODUCTION, false)
        .await
        .unwrap();
    let outcome = wired
        .recording
        .ingest(
            CONTROL_PLANE_NODE_ID,
            batch(vec![record(PRODUCTION, "api.example.com")]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.stored, outcome.not_recording), (0, 1));
}

fn environment(egress_recording: bool) -> environments::Model {
    serde_json::from_value(serde_json::json!({
        "id": PRODUCTION,
        "name": "production",
        "slug": "production",
        "subdomain": "shop-production",
        "host": "shop.example.com",
        "upstreams": [],
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "project_id": PROJECT,
        "is_preview": false,
        "protected": false,
        "sleeping": false,
        "shield_exempt_paths": [],
        "egress_recording": egress_recording,
    }))
    .expect("an environment row")
}

/// A deployment is steered only when its environment records and this
/// host's recorder serves deployments; otherwise it starts as it always
/// did, and the plan says which case it is so the deployment log can.
#[tokio::test]
async fn a_deployment_goes_through_the_recorder_only_when_its_environment_records() {
    assert_eq!(
        RecordedEgress::plan(&environment(false), None),
        RecordedEgress::Off
    );
    assert_eq!(
        RecordedEgress::plan(&environment(true), None),
        RecordedEgress::NotServing,
        "recording is on, but nothing serves it here"
    );

    let existing: HashMap<String, String> = [
        ("DATABASE_URL", "postgres://app:pw@postgres-main:5432/app"),
        ("TEMPS_API_URL", "https://console.example.com/api"),
        ("SENTRY_DSN", "https://key@errors.example.com/7"),
        ("PAYMENTS_API", "https://api.example.com"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect();
    assert_eq!(RecordedEgress::Off.variables(&existing), None);
    assert_eq!(RecordedEgress::NotServing.variables(&existing), None);

    let steering = RecordedEgress::Proxy(
        "172.18.0.1:13128".parse().unwrap(),
        temps_workload_egress::steering::InstallationId::from_bytes([7; 8]),
    )
    .variables(&existing)
    .expect("steered");
    assert!(steering.steers());
    assert_eq!(steering.variables["HTTPS_PROXY"], "http://172.18.0.1:13128");
    let direct: Vec<&str> = steering.variables["NO_PROXY"].split(',').collect();
    for host in ["postgres-main", "console.example.com", "errors.example.com"] {
        assert!(
            direct.contains(&host),
            "{host} stays direct: a linked service or the platform's own endpoint"
        );
    }
    assert!(
        !direct.contains(&"api.example.com"),
        "a public API the application calls is what gets recorded"
    );
}

/// A Compose environment's containers are started by Compose, which nothing
/// steers to the recorder: it says so and cannot turn recording on.
#[tokio::test]
async fn a_compose_environment_cannot_record() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE projects SET preset = 'docker-compose' WHERE id = {PROJECT}"
        ))
        .await
        .unwrap();
    let status = wired
        .environments
        .status(PROJECT, PRODUCTION)
        .await
        .unwrap();
    assert!(!status.configured);
    assert_eq!(status.reason_code.as_deref(), Some("not_recordable"));
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("Compose")),
        "{status:?}"
    );
    let refused = wired
        .environments
        .set_enabled(PROJECT, PRODUCTION, true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            EgressRecordingError::NotRecordable {
                noun: "environment",
                ..
            }
        ),
        "{refused:?}"
    );
    assert!(!f.recording(PRODUCTION).await, "nothing was written");
    // Another project's image deployment is unaffected.
    assert_eq!(
        wired
            .environments
            .status(OTHER_PROJECT, ELSEWHERE)
            .await
            .unwrap()
            .reason_code
            .as_deref(),
        Some("unsupported_platform")
    );
}

/// An environment whose replicas run in microVMs (workload mode enforce)
/// is never steered to the recorder: it says so instead of being turned on
/// and recording nothing. The same isolation outside enforce is ignored by
/// the deploy path, so it does not stop recording there.
#[tokio::test]
async fn a_microvm_environment_says_it_cannot_record() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE environments SET workload_isolation = 'micro_vm' WHERE id = {PRODUCTION}"
        ))
        .await
        .unwrap();
    let before = wired
        .environments
        .status(PROJECT, PRODUCTION)
        .await
        .unwrap();
    assert_ne!(
        before.reason_code.as_deref(),
        Some("not_recordable"),
        "isolation is ignored outside enforce: {before:?}"
    );
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE environments SET workload_mode = 'enforce' WHERE id = {PRODUCTION}"
        ))
        .await
        .unwrap();
    let status = wired
        .environments
        .status(PROJECT, PRODUCTION)
        .await
        .unwrap();
    assert!(!status.configured);
    assert_eq!(status.reason_code.as_deref(), Some("not_recordable"));
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("micro_vm")),
        "{status:?}"
    );
    let refused = wired
        .environments
        .set_enabled(PROJECT, PRODUCTION, true)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, EgressRecordingError::NotRecordable { .. }),
        "{refused:?}"
    );
    assert!(!f.recording(PRODUCTION).await, "nothing was written");
}

/// A replica stopped from the console keeps its row, but no longer places
/// its environment on that node: an environment whose only replica was
/// stopped on a node that cannot record is no longer refused for it, and
/// can opt in for its next deployment.
#[tokio::test]
async fn a_stopped_replica_no_longer_places_its_environment() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    let before = wired
        .environments
        .status(OTHER_PROJECT, ELSEWHERE)
        .await
        .unwrap();
    assert_eq!(before.reason_code.as_deref(), Some("unsupported_platform"));
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE deployment_containers SET status = 'stopped', finished_at = now() \
             WHERE node_id = {}",
            f.mac
        ))
        .await
        .unwrap();
    let after = wired
        .environments
        .status(OTHER_PROJECT, ELSEWHERE)
        .await
        .unwrap();
    assert!(after.nodes.is_empty(), "{after:?}");
    assert_ne!(after.reason_code.as_deref(), Some("unsupported_platform"));
}

/// A static site's deployments publish files and start no container, so
/// nothing is ever steered: it says it cannot record instead of accepting
/// the setting and recording nothing. The same project deployed as a Docker
/// image runs a replica, and records like any other.
#[tokio::test]
async fn a_static_site_says_it_cannot_record() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    f.db.connection()
        .execute_unprepared(&format!(
            "UPDATE projects SET preset = 'vite' WHERE id = {PROJECT}"
        ))
        .await
        .unwrap();
    let with_replica = wired
        .environments
        .status(PROJECT, PRODUCTION)
        .await
        .unwrap();
    assert_ne!(
        with_replica.reason_code.as_deref(),
        Some("not_recordable"),
        "an image deployment of a static project runs a replica: {with_replica:?}"
    );
    // Files alone: no running replica.
    f.db.connection()
        .execute_unprepared("UPDATE deployment_containers SET status = 'stopped'")
        .await
        .unwrap();
    // A preset that builds static output: published as files, no replica.
    for preset in ["vite"] {
        f.db.connection()
            .execute_unprepared(&format!(
                "UPDATE projects SET preset = '{preset}' WHERE id = {PROJECT}"
            ))
            .await
            .unwrap();
        let status = wired
            .environments
            .status(PROJECT, PRODUCTION)
            .await
            .unwrap();
        assert_eq!(
            status.reason_code.as_deref(),
            Some("not_recordable"),
            "{preset}: {status:?}"
        );
        assert!(
            status
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("static site")),
            "{preset}: {status:?}"
        );
    }
    let refused = wired
        .environments
        .set_enabled(PROJECT, PRODUCTION, true)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, EgressRecordingError::NotRecordable { .. }),
        "{refused:?}"
    );
}

struct NoCapability;

impl temps_workload_egress::local::LocalCapability for NoCapability {
    fn set(&self, _capability: temps_workload_netpol::EgressRecordingCapability) {}
}

/// The recording service holds the environment adapter and the recorder
/// holds the service: the adapter's own reference to the recorder must not
/// close that loop, or dropping the plugin graph would never drop the
/// recorder (its tasks, its listener and its port).
#[tokio::test]
async fn the_adapter_never_keeps_the_recorder_alive() {
    let Some(f) = fixture().await else { return };
    let wired = f.wired();
    let recorder = temps_workload_egress::local::LocalEgressRecorder::new(
        wired.recording.clone(),
        Arc::new(NoCapability),
        temps_workload_egress::local::LocalServing::new(),
        temps_workload_egress::steering::InstallationId::from_bytes([7; 8]),
    );
    wired.adapter.set_recorder(&recorder);
    let weak = Arc::downgrade(&recorder);
    drop(recorder);
    assert!(
        weak.upgrade().is_none(),
        "freed with its last outside reference"
    );
}
