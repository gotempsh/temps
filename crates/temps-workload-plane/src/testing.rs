// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test helpers: a scripted driver, an isolated database with the plane's
//! tables, and recording audit and finalizer doubles.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::ConnectionTrait;
use temps_core::{AuditLogger, AuditOperation};
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_workload::{
    Artifact, ArtifactKinds, AttachmentKind, Attachments, DriverCapabilities, DriverError, EnvSpec,
    InstanceHandle, InstanceSpec, InstanceState, InstanceStats, InstanceStatus, Isolation,
    Lifetime, LimitSupport, ListedInstance, LogLine, LogQuery, LogStream, PreparedArtifact,
    Process, RuntimeDriver, VolumeKinds, WorkloadKind, WorkloadRef, WorkloadSpec,
};

use crate::{
    ApplyRequest, Finalizer, NoEnvSource, NodeDriver, Owner, PlaneConfig, WorkloadPlane,
    WorkloadView,
};

pub(crate) fn job_spec_with(
    image: &str,
    timeout_secs: u32,
    retry_limit: u32,
    ttl_secs_after_finished: Option<u32>,
) -> WorkloadSpec {
    WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Job, 7),
        artifact: Artifact::oci(image),
        isolation: Isolation::Process,
        process: Process {
            command: Some(vec!["bin/cleanup".into(), "--all".into()]),
            ..Default::default()
        },
        env: EnvSpec::default(),
        resources: Default::default(),
        volumes: vec![],
        ports: vec![],
        network: Default::default(),
        probes: Default::default(),
        placement: Default::default(),
        lifetime: Lifetime::RunToCompletion {
            timeout_secs,
            retry_limit,
            ttl_secs_after_finished,
        },
        security: None,
        labels: BTreeMap::from([("sh.temps.test".to_string(), "adr055-p2".to_string())]),
    }
}

/// One replica of a deployment revision (Phase 5): long-running, one port.
pub(crate) fn deployment_spec(image: &str) -> WorkloadSpec {
    let mut spec = job_spec(image);
    spec.owner = WorkloadRef::new(WorkloadKind::Deployment, 42);
    spec.process = Process::default();
    spec.lifetime = Lifetime::LongRunning {
        restart: temps_workload::RestartPolicy::UnlessStopped,
    };
    spec.ports.push(temps_workload::PortSpec {
        name: "http".into(),
        container_port: 3000,
        protocol: Default::default(),
        app_protocol: Default::default(),
    });
    spec
}

pub(crate) fn job_spec(image: &str) -> WorkloadSpec {
    job_spec_with(image, 600, 0, None)
}

pub(crate) fn apply_request(owner_id: i64, name: &str, spec: WorkloadSpec) -> ApplyRequest {
    ApplyRequest {
        owner: Owner::new("cron", owner_id),
        name: name.to_string(),
        spec,
        project_slug: Some("shop".to_string()),
        finalizers: Vec::new(),
    }
}

/// An isolated schema holding the plane's tables, or `None` (the test
/// skips) when no container runtime is available.
pub(crate) async fn test_db() -> Option<TestDatabase> {
    let db = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) => {
            let text = format!("{error:#}");
            if is_container_runtime_unavailable(&text) {
                println!("Docker not available, skipping: {text}");
                return None;
            }
            panic!("test database could not be created: {text}");
        }
    };
    for statement in temps_migrations::WORKLOAD_TABLES_SQL {
        db.db
            .execute_unprepared(statement)
            .await
            .unwrap_or_else(|e| panic!("creating the plane's tables failed at {statement}: {e}"));
    }
    Some(db)
}

pub(crate) async fn insert_workload_row(db: &TestDatabase, name: &str) -> i64 {
    let row = db
        .db
        .query_one(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO workloads (owner_kind, owner_id, name, spec) VALUES ('test', 1, $1, '{}') \
             RETURNING id",
            vec![name.into()],
        ))
        .await
        .expect("inserted")
        .expect("a row");
    row.try_get::<i64>("", "id").expect("an id")
}

#[derive(Default)]
pub(crate) struct RecordingAudit {
    operations: Mutex<Vec<String>>,
}

impl RecordingAudit {
    pub(crate) fn operations(&self) -> Vec<String> {
        self.operations.lock().expect("lock").clone()
    }
}

#[async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::anyhow::Result<()> {
        let line = format!("{} {}", operation.operation_type(), operation.serialize()?);
        self.operations.lock().expect("lock").push(line);
        Ok(())
    }
}

/// Records every view it is called with; fails its first `failures` calls.
pub(crate) struct RecordingFinalizer {
    name: String,
    failures: Mutex<u32>,
    seen: Mutex<Vec<WorkloadView>>,
}

impl RecordingFinalizer {
    pub(crate) fn new(name: &str, failures: u32) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            failures: Mutex::new(failures),
            seen: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn seen(&self) -> Vec<WorkloadView> {
        self.seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Finalizer for RecordingFinalizer {
    fn name(&self) -> &str {
        &self.name
    }

    async fn finalize(&self, workload: &WorkloadView) -> Result<(), String> {
        self.seen.lock().expect("lock").push(workload.clone());
        let mut failures = self.failures.lock().expect("lock");
        if *failures > 0 {
            *failures -= 1;
            return Err("scripted failure".to_string());
        }
        Ok(())
    }
}

pub(crate) fn plane(
    db: &TestDatabase,
    driver: NodeDriver,
    finalizers: Vec<Arc<RecordingFinalizer>>,
) -> (Arc<WorkloadPlane>, Arc<RecordingAudit>) {
    let audit = Arc::new(RecordingAudit::default());
    let scope = format!("t{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let mut plane = WorkloadPlane::new(
        db.db.clone(),
        driver,
        Arc::new(NoEnvSource),
        audit.clone(),
        PlaneConfig::new(scope),
    );
    for finalizer in finalizers {
        plane = plane.with_finalizer(finalizer);
    }
    (Arc::new(plane), audit)
}

pub(crate) fn plane_with(
    db: &TestDatabase,
) -> (Arc<WorkloadPlane>, Arc<FakeDriver>, Arc<RecordingAudit>) {
    let driver = FakeDriver::new();
    let (plane, audit) = plane(db, NodeDriver(Some(driver.clone())), Vec::new());
    (plane, driver, audit)
}

struct FakeInstance {
    state: InstanceState,
    exit_code: Option<i64>,
    labels: BTreeMap<String, String>,
    finished_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Default)]
struct FakeState {
    instances: BTreeMap<String, FakeInstance>,
    created: Vec<(InstanceSpec, Attachments)>,
    fail_prepare: bool,
    output: String,
}

/// A driver whose instances run until the test says they exit.
pub(crate) struct FakeDriver {
    capabilities: DriverCapabilities,
    state: Mutex<FakeState>,
}

impl FakeDriver {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            capabilities: DriverCapabilities {
                name: "fake".into(),
                artifacts: ArtifactKinds {
                    oci: true,
                    ..Default::default()
                },
                isolation: Isolation::Process,
                network_attachment: AttachmentKind::DockerNetwork,
                volumes: VolumeKinds::default(),
                exec: false,
                snapshot: false,
                resize_disk: false,
                limits: LimitSupport::default(),
            },
            state: Mutex::new(FakeState::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().expect("lock")
    }

    pub(crate) fn fail_prepare(&self, fail: bool) {
        self.state().fail_prepare = fail;
    }

    pub(crate) fn set_output(&self, output: &str) {
        self.state().output = output.to_string();
    }

    pub(crate) fn exit(&self, name: &str, code: i64) {
        let mut state = self.state();
        let instance = state.instances.get_mut(name).expect("a known instance");
        instance.state = InstanceState::Exited;
        instance.exit_code = Some(code);
        instance.finished_at = Some(Utc::now());
    }

    pub(crate) fn forget(&self, name: &str) {
        self.state().instances.remove(name);
    }

    pub(crate) fn running(&self) -> Vec<String> {
        self.state()
            .instances
            .iter()
            .filter(|(_, i)| i.state == InstanceState::Running)
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub(crate) fn exists(&self, name: &str) -> bool {
        self.state().instances.contains_key(name)
    }

    pub(crate) fn created(&self) -> Vec<(InstanceSpec, Attachments)> {
        self.state().created.clone()
    }

    pub(crate) fn insert_listed(&self, name: &str, labels: &[(&str, &str)]) {
        self.state().instances.insert(
            name.to_string(),
            FakeInstance {
                state: InstanceState::Exited,
                exit_code: Some(0),
                labels: labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                finished_at: Some(Utc::now()),
            },
        );
    }

    fn handle(name: &str) -> InstanceHandle {
        InstanceHandle {
            driver: "fake".into(),
            id: name.to_string(),
            name: name.to_string(),
        }
    }
}

#[async_trait]
impl RuntimeDriver for FakeDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        let Artifact::OciImage { reference, .. } = artifact else {
            return Err(DriverError::Unsupported {
                driver: "fake".into(),
                operation: "non-OCI artifacts",
                instance: String::new(),
            });
        };
        if self.state().fail_prepare {
            return Err(DriverError::Artifact {
                driver: "fake".into(),
                artifact: reference.clone(),
                reason: "manifest unknown".into(),
            });
        }
        Ok(PreparedArtifact {
            artifact: artifact.clone(),
            local_ref: reference.clone(),
        })
    }

    async fn create(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError> {
        let mut state = self.state();
        if state.instances.contains_key(&spec.name) {
            return Err(DriverError::Runtime {
                driver: "fake".into(),
                operation: "create",
                instance: spec.name.clone(),
                reason: "name already in use".into(),
            });
        }
        state.instances.insert(
            spec.name.clone(),
            FakeInstance {
                state: InstanceState::Created,
                exit_code: None,
                labels: spec.labels.clone(),
                finished_at: None,
            },
        );
        state.created.push((spec.clone(), attach.clone()));
        Ok(Self::handle(&spec.name))
    }

    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        let mut state = self.state();
        let instance =
            state
                .instances
                .get_mut(&handle.id)
                .ok_or_else(|| DriverError::NotFound {
                    driver: "fake".into(),
                    instance: handle.name.clone(),
                })?;
        instance.state = InstanceState::Running;
        Ok(())
    }

    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        if let Some(instance) = self.state().instances.get_mut(&handle.id) {
            if instance.state == InstanceState::Running {
                instance.state = InstanceState::Exited;
                instance.exit_code = Some(137);
                instance.finished_at = Some(Utc::now());
            }
        }
        Ok(())
    }

    async fn remove(&self, handle: &InstanceHandle, _purge: bool) -> Result<(), DriverError> {
        self.state().instances.remove(&handle.id);
        Ok(())
    }

    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        let state = self.state();
        let (state, exit_code, finished_at) = match state.instances.get(&handle.id) {
            Some(i) => (i.state, i.exit_code, i.finished_at),
            None => (InstanceState::Missing, None, None),
        };
        Ok(InstanceStatus {
            state,
            exit_code,
            started_at: None,
            finished_at,
            address: None,
            published_ports: vec![],
        })
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        _query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        let state = self.state();
        if !state.instances.contains_key(&handle.id) {
            return Err(DriverError::NotFound {
                driver: "fake".into(),
                instance: handle.name.clone(),
            });
        }
        let lines: Vec<Result<LogLine, DriverError>> = state
            .output
            .split_inclusive('\n')
            .map(|line| {
                Ok(LogLine {
                    stderr: false,
                    line: bytes::Bytes::from(line.to_string()),
                })
            })
            .collect();
        Ok(Box::pin(futures::stream::iter(lines)))
    }

    async fn stats(&self, _handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        Ok(InstanceStats::default())
    }

    async fn list(
        &self,
        labels: &BTreeMap<String, String>,
    ) -> Result<Vec<ListedInstance>, DriverError> {
        Ok(self
            .state()
            .instances
            .iter()
            .filter(|(_, i)| labels.iter().all(|(k, v)| i.labels.get(k) == Some(v)))
            .map(|(name, i)| ListedInstance {
                handle: Self::handle(name),
                labels: i.labels.clone(),
            })
            .collect())
    }
}
