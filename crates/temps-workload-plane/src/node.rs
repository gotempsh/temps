// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where instances run (ADR-055 §7, §8), and the control plane's half of
//! the node runtime protocol.
//!
//! - [`Placement`] chooses the node of a new attempt. Without one, every
//!   instance runs on the control plane (node 0) through the plane's own
//!   driver, exactly as in phase 2.
//! - An instance on a worker is driven through the [`RuntimeDriver`] the
//!   placement returns for it:
//!   - a worker that advertises the `state_stream` capability gets a
//!     [`StreamNodeDriver`]: creating, stopping and removing are desired
//!     state (the instance row, which the worker reads from its stream),
//!     and what the worker observed comes back on its heartbeat
//!     ([`WorkloadPlane::ingest_node_reports`]);
//!   - any other worker gets the product's `/agent/*` driver, as before.
//! - [`WorkloadPlane::instance_env`] gives a worker the environment of one
//!   of its instances, at create time. It is the only call that carries
//!   secret values, and it is never stored in the stream.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{SubsecRound, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_core::docker_socket_grant::DOCKER_SOCKET_BIND;
use temps_entities::{workload_instances, workloads};
use temps_workload::stream::{InstanceEnv, InstanceReport, InstanceTemplate};
use temps_workload::{
    labels as workload_labels, Artifact, Attachments, ConditionKind, ConditionStatus,
    DriverCapabilities, DriverError, InstanceHandle, InstanceSpec, InstanceState, InstanceStats,
    InstanceStatus, Lifetime, LogQuery, LogRotation, LogStream, NetworkAttachment, PortBinding,
    PreparedArtifact, RestartPolicy, RuntimeDriver, SecurityProfile, WorkloadSpec,
};

use crate::env;
use crate::error::PlaneError;
use crate::events;
use crate::labels;
use crate::plane::{WorkloadPlane, WorkloadView};
use crate::reconciler::limits_from;

/// The driver name of instances run through a node's stream.
pub const STREAM_DRIVER: &str = temps_workload::stream::STREAM_DRIVER;

/// Where a new attempt runs.
pub enum Placed {
    /// The control plane (node 0), through the plane's own driver.
    ControlPlane,
    /// A worker, through `driver`.
    Node {
        node_id: i32,
        node_name: String,
        driver: Arc<dyn RuntimeDriver>,
    },
}

/// Chooses nodes for new attempts and gives the driver of an instance
/// already placed. Implemented by the product that owns the node registry
/// and its scheduler.
#[async_trait]
pub trait Placement: Send + Sync {
    /// The node of `workload`'s next attempt, or why none is eligible
    /// (the `NoEligibleNode` message).
    async fn place(&self, workload: &WorkloadView) -> Result<Placed, String>;

    /// `workload`'s next attempt on worker `node_id`, which its owner
    /// already chose (a deployment replica the deploy job placed): only
    /// the driver is decided here, or why the node cannot run it.
    async fn place_on(&self, workload: &WorkloadView, node_id: i32) -> Result<Placed, String> {
        Err(format!(
            "workload {} is pinned to node {node_id}, and this plane's placement does not \
             place pinned workloads on workers",
            workload.id
        ))
    }

    /// The driver of an instance on worker `node_id` created by the driver
    /// named `driver`, or why it is unavailable.
    async fn driver_for(
        &self,
        node_id: i32,
        driver: &str,
    ) -> Result<Arc<dyn RuntimeDriver>, String>;

    /// Whether worker `node_id` is still registered. The reconciler keeps
    /// retrying the removal of an instance whose driver acts only when
    /// called (not [`RuntimeDriver::reconciled_by_node`]) until it
    /// succeeds, except on a node that no longer exists, where nothing can
    /// remove it any more. The default never gives up.
    async fn node_exists(&self, _node_id: i32) -> Result<bool, String> {
        Ok(true)
    }
}

fn stream_capabilities() -> DriverCapabilities {
    DriverCapabilities {
        name: STREAM_DRIVER.to_string(),
        artifacts: temps_workload::ArtifactKinds {
            oci: true,
            wasm: false,
            rootfs: false,
        },
        isolation: temps_workload::Isolation::Process,
        network_attachment: temps_workload::AttachmentKind::DockerNetwork,
        volumes: Default::default(),
        exec: false,
        snapshot: false,
        resize_disk: false,
        limits: temps_workload::LimitSupport {
            cpu: true,
            memory: true,
            pids: true,
            disk: false,
        },
    }
}

/// `{scope}-{kind}-{workload_id}-a{attempt}` → `(workload_id, attempt)`.
pub(crate) fn parse_instance_name(name: &str) -> Option<(i64, i32)> {
    let (rest, attempt) = name.rsplit_once("-a")?;
    let attempt = attempt.parse().ok()?;
    let (_, workload) = rest.rsplit_once('-')?;
    Some((workload.parse().ok()?, attempt))
}

/// The driver of instances on a worker that reads its stream. The control
/// plane only records desired state; the worker's node runtime creates,
/// stops and removes, and reports what it observed on its heartbeat.
pub struct StreamNodeDriver {
    db: Arc<DatabaseConnection>,
    node_id: i32,
    /// The worker's `/agent/*` surface, for what is not desired state:
    /// moving the artifact onto the node and reading output.
    rpc: Arc<dyn RuntimeDriver>,
    capabilities: DriverCapabilities,
    poll: StdDuration,
}

impl StreamNodeDriver {
    pub fn new(db: Arc<DatabaseConnection>, node_id: i32, rpc: Arc<dyn RuntimeDriver>) -> Self {
        Self {
            db,
            node_id,
            rpc,
            capabilities: stream_capabilities(),
            poll: StdDuration::from_secs(10),
        }
    }

    pub fn with_poll_interval(mut self, poll: StdDuration) -> Self {
        self.poll = poll;
        self
    }

    async fn row(
        &self,
        handle: &InstanceHandle,
    ) -> Result<Option<workload_instances::Model>, DriverError> {
        let Some((workload_id, attempt)) = parse_instance_name(&handle.name) else {
            return Err(DriverError::NotFound {
                driver: STREAM_DRIVER.to_string(),
                instance: handle.name.clone(),
            });
        };
        workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(workload_id))
            .filter(workload_instances::Column::Attempt.eq(attempt))
            .filter(workload_instances::Column::NodeId.eq(self.node_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DriverError::Unavailable {
                driver: STREAM_DRIVER.to_string(),
                reason: format!(
                    "reading instance '{}' of node {}: {e}",
                    handle.name, self.node_id
                ),
            })
    }
}

#[async_trait]
impl RuntimeDriver for StreamNodeDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    /// The worker's node runtime creates, stops and removes from its
    /// stream, and only its report moves an instance to running.
    fn reconciled_by_node(&self) -> bool {
        true
    }

    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        self.rpc.prepare(artifact).await
    }

    /// Desired state: the instance row already says what to run. The
    /// worker creates the container when its stream delivers the row.
    async fn create(
        &self,
        spec: &InstanceSpec,
        _attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError> {
        Ok(InstanceHandle {
            driver: STREAM_DRIVER.to_string(),
            id: spec.name.clone(),
            name: spec.name.clone(),
        })
    }

    async fn start(&self, _handle: &InstanceHandle) -> Result<(), DriverError> {
        Ok(())
    }

    /// Desired state: the plane fails the instance row, and the worker
    /// stops what its stream says is finished.
    async fn stop(&self, _handle: &InstanceHandle) -> Result<(), DriverError> {
        Ok(())
    }

    /// Desired state: the worker removes an instance when its row goes, or
    /// when a later attempt of the same workload replaces it.
    async fn remove(&self, _handle: &InstanceHandle, _purge: bool) -> Result<(), DriverError> {
        Ok(())
    }

    /// What the worker last reported, as the instance row records it.
    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        let Some(row) = self.row(handle).await? else {
            return Ok(InstanceStatus {
                state: InstanceState::Missing,
                exit_code: None,
                started_at: None,
                finished_at: None,
                address: None,
                published_ports: Vec::new(),
            });
        };
        let conditions: Vec<temps_workload::Condition> =
            serde_json::from_value(row.conditions.clone()).unwrap_or_default();
        // What the node reported of a long-running instance it keeps as
        // the same attempt: stopped by its owner, or removed after it ran.
        let not_running = conditions
            .iter()
            .find(|c| c.kind == ConditionKind::Running && c.status == ConditionStatus::False)
            .map(|c| c.reason.as_str());
        let state = match (row.phase.as_str(), not_running) {
            ("pending", _) => InstanceState::Created,
            ("running", Some("InstanceRemoved")) => InstanceState::Missing,
            ("running", Some("Stopped")) => InstanceState::Exited,
            ("running", _) => InstanceState::Running,
            _ => InstanceState::Exited,
        };
        Ok(InstanceStatus {
            state,
            exit_code: row.exit_code,
            started_at: row.started_at,
            finished_at: row.finished_at,
            address: row.address,
            published_ports: serde_json::from_value(row.published_ports.clone())
                .unwrap_or_default(),
        })
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        self.rpc.logs(handle, query).await
    }

    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        self.rpc.stats(handle).await
    }

    /// Returns when the row is finished or gone. The heartbeat ingest wakes
    /// the reconciler directly; this bounds a report the ingest missed.
    async fn wait(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        loop {
            match self.row(handle).await? {
                Some(row) if row.phase == "pending" || row.phase == "running" => {}
                _ => return Ok(()),
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

/// The binds of an instance: the ADR-045 socket for `HostPrivileged`, then
/// each resolved volume mount (`source:path[:ro]`). A mount without a
/// source is not bound; admission refuses one for the kinds that mount.
pub(crate) fn volume_binds(spec: &WorkloadSpec, security: SecurityProfile) -> Vec<String> {
    let socket =
        (security == SecurityProfile::HostPrivileged).then(|| DOCKER_SOCKET_BIND.to_string());
    socket
        .into_iter()
        .chain(spec.volumes.iter().filter_map(|mount| {
            mount.source.as_ref().map(|source| {
                let mut bind = format!("{source}:{}", mount.mount_path);
                if mount.read_only {
                    bind.push_str(":ro");
                }
                bind
            })
        }))
        .collect()
}

const NODE_PHASES: [&str; 4] = ["pending", "running", "succeeded", "failed"];

impl WorkloadPlane {
    /// The non-secret part of an instance's spec. The direct path adds the
    /// environment for its create call; the stream sends it as is.
    pub(crate) fn instance_template(
        &self,
        spec: &WorkloadSpec,
        workload_id: i64,
        revision: i64,
        attempt: u32,
        name: &str,
    ) -> InstanceTemplate {
        let security = spec.security.unwrap_or(SecurityProfile::Trusted);
        let mut instance_labels = spec.labels.clone();
        instance_labels.extend(workload_labels::instance_labels(
            spec.owner,
            workload_id,
            revision,
        ));
        instance_labels.insert(labels::PLANE.to_string(), self.config.scope.clone());
        instance_labels.insert(
            labels::MANAGED_BY.to_string(),
            labels::MANAGED_BY_VALUE.to_string(),
        );
        instance_labels.insert(labels::ATTEMPT.to_string(), attempt.to_string());
        InstanceTemplate {
            owner: spec.owner,
            name: name.to_string(),
            artifact: spec.artifact.clone(),
            platform: None,
            command: spec.process.command.clone(),
            entrypoint: spec.process.entrypoint.clone(),
            workdir: spec.process.workdir.clone(),
            user: spec.process.user.clone(),
            ports: match spec.lifetime {
                Lifetime::LongRunning { .. } | Lifetime::ScaleToZero { .. } => spec
                    .ports
                    .iter()
                    .map(|port| PortBinding {
                        container_port: port.container_port,
                        protocol: port.protocol,
                        host_ip: self.config.publish_host_ip.clone(),
                        host_port: None,
                    })
                    .collect(),
                Lifetime::RunToCompletion { .. } => Vec::new(),
            },
            restart: match spec.lifetime {
                // The runtime restarts a long-running instance in place, as
                // the legacy deployer's containers are.
                Lifetime::LongRunning { restart } => restart,
                // The plane retries, not the runtime: a restarted container
                // would hide the exit the retry limit counts.
                Lifetime::RunToCompletion { .. } | Lifetime::ScaleToZero { .. } => {
                    RestartPolicy::Never
                }
            },
            limits: limits_from(&spec.resources),
            log_rotation: Some(LogRotation {
                max_size: "10m".to_string(),
                max_file: 3,
            }),
            labels: instance_labels,
            security,
            attachments: Attachments {
                network: NetworkAttachment {
                    primary: self.config.network.clone(),
                    required: spec.network.extra_networks.clone(),
                    optional: Vec::new(),
                    dns: None,
                },
                // Admission gave this profile only to an ADR-045 granted
                // project; every other workload never gets the socket. Then
                // the volumes, whose sources the plane resolved (§12).
                volumes: volume_binds(spec, security),
            },
            long_running: matches!(spec.lifetime, Lifetime::LongRunning { .. }),
        }
    }

    /// The environment of instance `instance_id`, for node `node_id` to
    /// create it. Only an unfinished instance assigned to that node gets
    /// one. The values are never logged.
    pub async fn instance_env(
        &self,
        node_id: i32,
        instance_id: i64,
    ) -> Result<InstanceEnv, PlaneError> {
        let found = workload_instances::Entity::find_by_id(instance_id)
            .find_also_related(workloads::Entity)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "reading the instance whose environment a node asked for",
                format!("instance {instance_id}"),
            ))?;
        let (instance, workload) = match found {
            Some((instance, Some(workload)))
                if instance.node_id == Some(node_id)
                    && (instance.phase == "pending" || instance.phase == "running")
                    && workload.deleted_at.is_none() =>
            {
                (instance, workload)
            }
            _ => {
                return Err(PlaneError::InstanceNotOnNode {
                    instance_id,
                    node_id,
                })
            }
        };
        let spec: WorkloadSpec =
            serde_json::from_value(workload.spec.clone()).map_err(|e| PlaneError::Corrupt {
                workload_id: workload.id,
                field: "spec",
                reason: e.to_string(),
            })?;
        let resolved =
            env::resolve_with_staged(&spec.env, self.env.as_ref(), &|import| self.staged(import))
                .await
                .map_err(|reason| PlaneError::EnvironmentUnavailable {
                    workload_id: workload.id,
                    instance_id,
                    reason,
                })?;
        self.redactors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(workload.id, resolved.redactor);
        tracing::info!(
            node_id,
            instance_id = instance.id,
            workload_id = workload.id,
            variables = resolved.env.len(),
            "gave a node the environment of its instance"
        );
        Ok(InstanceEnv {
            instance_id,
            env: resolved.env,
            secret_files: resolved.secret_files,
        })
    }

    /// Record what node `node_id` observed of its instances. Only its own
    /// unfinished instances are written, only when something changed, and
    /// never out of a finished phase. Returns how many rows changed; the
    /// reconciler is woken when any did.
    pub async fn ingest_node_reports(
        &self,
        node_id: i32,
        reports: &[InstanceReport],
    ) -> Result<usize, PlaneError> {
        if reports.is_empty() {
            return Ok(0);
        }
        let ids: Vec<i64> = reports.iter().map(|r| r.instance_id).collect();
        let rows: BTreeMap<i64, workload_instances::Model> = workload_instances::Entity::find()
            .filter(workload_instances::Column::Id.is_in(ids))
            .filter(workload_instances::Column::NodeId.eq(node_id))
            .all(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "reading the instances a node reported",
                format!("node {node_id}"),
            ))?
            .into_iter()
            .map(|row| (row.id, row))
            .collect();
        let now = Utc::now();
        let mut changed = 0;
        for report in reports {
            let Some(row) = rows.get(&report.instance_id) else {
                tracing::debug!(
                    node_id,
                    instance_id = report.instance_id,
                    "node reported an instance that is not assigned to it; ignored"
                );
                continue;
            };
            if !NODE_PHASES.contains(&report.phase.as_str()) {
                tracing::warn!(
                    node_id,
                    instance_id = row.id,
                    phase = %report.phase,
                    "node reported an unknown instance phase; ignored"
                );
                continue;
            }
            if row.phase == "succeeded" || row.phase == "failed" {
                continue;
            }
            // `pending` from a node never moves a running row back.
            let phase = if report.phase == "pending" {
                row.phase.clone()
            } else {
                report.phase.clone()
            };
            let conditions =
                serde_json::to_value(&report.conditions).map_err(|e| PlaneError::Encode {
                    workload: format!("instance {}", row.id),
                    field: "instance conditions",
                    reason: e.to_string(),
                })?;
            let published_ports =
                serde_json::to_value(&report.published_ports).map_err(|e| PlaneError::Encode {
                    workload: format!("instance {}", row.id),
                    field: "instance published ports",
                    reason: e.to_string(),
                })?;
            // Running, and not reported stopped or removed.
            let ready = phase == "running"
                && !report.conditions.iter().any(|c| {
                    c.kind == ConditionKind::Running && c.status == ConditionStatus::False
                });
            // Stored at PostgreSQL's microsecond precision: a runtime's
            // nanosecond timestamps compared as sent would differ on every
            // heartbeat and turn "written only when changed" into a write
            // per beat.
            let started_at = report
                .started_at
                .map(|t| t.trunc_subsecs(6))
                .or(row.started_at);
            let finished_at = report
                .finished_at
                .map(|t| t.trunc_subsecs(6))
                .or(row.finished_at);
            if phase == row.phase
                && report.exit_code == row.exit_code
                && conditions == row.conditions
                && published_ports == row.published_ports
                && ready == row.ready
                && started_at == row.started_at
                && finished_at == row.finished_at
            {
                continue;
            }
            let finished = phase == "succeeded" || phase == "failed";
            workload_instances::Entity::update_many()
                .col_expr(
                    workload_instances::Column::Phase,
                    Expr::value(phase.clone()),
                )
                .col_expr(workload_instances::Column::Ready, Expr::value(ready))
                .col_expr(
                    workload_instances::Column::PublishedPorts,
                    Expr::value(published_ports.clone()),
                )
                .col_expr(
                    workload_instances::Column::ExitCode,
                    Expr::value(report.exit_code),
                )
                .col_expr(
                    workload_instances::Column::StartedAt,
                    Expr::value(started_at),
                )
                .col_expr(
                    workload_instances::Column::FinishedAt,
                    Expr::value(if finished {
                        finished_at.or(Some(now))
                    } else {
                        finished_at
                    }),
                )
                .col_expr(
                    workload_instances::Column::Conditions,
                    Expr::value(conditions.clone()),
                )
                .filter(workload_instances::Column::Id.eq(row.id))
                .filter(workload_instances::Column::Phase.is_in(["pending", "running"]))
                .exec(self.db.as_ref())
                .await
                .map_err(PlaneError::db(
                    "recording a node's instance report",
                    format!("instance {} on node {node_id}", row.id),
                ))?;
            changed += 1;
            // A condition the node newly reports False is an event on the
            // workload: `SecretsUnavailable` names the instance.
            for condition in &report.conditions {
                let before: Vec<temps_workload::Condition> =
                    serde_json::from_value(row.conditions.clone()).unwrap_or_default();
                let known = before.iter().any(|c| {
                    c.kind == condition.kind
                        && c.status == condition.status
                        && c.reason == condition.reason
                });
                if condition.status == ConditionStatus::False && !known {
                    events::record(
                        self.db.as_ref(),
                        row.workload_id,
                        Some(row.id),
                        &condition.reason,
                        &format!("node {node_id}: {}", condition.message),
                        now,
                    )
                    .await?;
                }
            }
        }
        if changed > 0 {
            self.wake();
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{apply_request, job_spec_with, test_db, FakeDriver, RecordingAudit};
    use crate::{NoEnvSource, NodeDriver, PlaneConfig};
    use chrono::Timelike;
    use temps_workload::stream::{ObjectKind, StateChange, StateObject};
    use temps_workload::{ConditionKind, WorkloadPhase};

    /// Places every run on worker 5, read through its stream.
    struct OnWorker {
        db: Arc<DatabaseConnection>,
        rpc: Arc<FakeDriver>,
    }

    #[async_trait]
    impl Placement for OnWorker {
        async fn place(&self, _: &WorkloadView) -> Result<Placed, String> {
            Ok(Placed::Node {
                node_id: 5,
                node_name: "worker-a".into(),
                driver: self.driver_for(5, STREAM_DRIVER).await?,
            })
        }
        async fn place_on(&self, _: &WorkloadView, node_id: i32) -> Result<Placed, String> {
            Ok(Placed::Node {
                node_id,
                node_name: format!("worker-{node_id}"),
                driver: self.driver_for(node_id, STREAM_DRIVER).await?,
            })
        }
        async fn driver_for(
            &self,
            node_id: i32,
            _: &str,
        ) -> Result<Arc<dyn RuntimeDriver>, String> {
            Ok(Arc::new(
                StreamNodeDriver::new(self.db.clone(), node_id, self.rpc.clone())
                    .with_poll_interval(StdDuration::from_millis(100)),
            ))
        }
    }

    /// ADR-055 §7/§8: a job run placed on a worker that reads its stream is
    /// desired state only. The control plane writes the instance row for the
    /// worker; the worker's stream delivers it; what the worker observed
    /// comes back as a heartbeat report and finishes the run. The
    /// environment goes only to the node the instance is assigned to.
    #[tokio::test]
    async fn a_run_placed_on_a_stream_worker_is_driven_by_its_reports() {
        let Some(db) = test_db().await else { return };
        let control_plane = FakeDriver::new();
        let rpc = FakeDriver::new();
        let plane = Arc::new(
            WorkloadPlane::new(
                db.db.clone(),
                NodeDriver(Some(control_plane.clone())),
                Arc::new(NoEnvSource),
                Arc::new(RecordingAudit::default()),
                PlaneConfig::new(format!(
                    "t{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                )),
            )
            .with_placement(Arc::new(OnWorker {
                db: db.db.clone(),
                rpc: rpc.clone(),
            })),
        );
        let id = plane
            .apply(apply_request(
                21,
                "on-worker",
                job_spec_with("app:1", 600, 0, Some(30)),
            ))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(control_plane.running().is_empty(), "nothing runs on node 0");
        assert!(
            rpc.running().is_empty(),
            "the stream driver creates nothing itself"
        );

        let row = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .one(db.db.as_ref())
            .await
            .expect("read")
            .expect("an instance row");
        assert_eq!(row.node_id, Some(5));
        assert_eq!(row.driver, STREAM_DRIVER);
        assert_eq!(row.phase, "pending", "running only once the worker says so");
        let view = plane.get(id).await.expect("read");
        let scheduled = view
            .status
            .condition(ConditionKind::Scheduled)
            .expect("scheduled");
        assert_eq!(scheduled.reason, "Node");

        // The worker's whole view carries the instance and its workload.
        let state = plane.read_node_state(5, 0).await.expect("read");
        assert!(state.changes.iter().any(|c| matches!(
            c,
            StateChange::Upsert { object: StateObject::Instance(i) } if i.id == row.id
        )));
        assert!(state
            .changes
            .iter()
            .any(|c| c.kind() == ObjectKind::Workload));

        // Only its node gets the environment.
        assert!(plane.instance_env(5, row.id).await.is_ok());
        assert!(matches!(
            plane.instance_env(6, row.id).await,
            Err(PlaneError::InstanceNotOnNode { node_id: 6, .. })
        ));

        // The worker reports a running instance, then its exit.
        // A runtime's timestamp at nanosecond precision, as Docker reports
        // it: repeating it must still compare equal to the stored value.
        let started = Utc::now()
            .with_nanosecond(123_456_789)
            .expect("valid nanosecond");
        let report = |phase: &str, exit_code: Option<i64>| InstanceReport {
            instance_id: row.id,
            phase: phase.to_string(),
            exit_code,
            started_at: Some(started),
            finished_at: None,
            conditions: Vec::new(),
            published_ports: Vec::new(),
        };
        assert_eq!(
            plane
                .ingest_node_reports(5, &[report("running", None)])
                .await
                .expect("ingested"),
            1
        );
        assert_eq!(
            plane
                .ingest_node_reports(5, &[report("running", None)])
                .await
                .expect("ingested"),
            0,
            "an unchanged report writes nothing"
        );
        plane.reconcile_at(Utc::now(), false).await;
        assert_eq!(
            plane.get(id).await.expect("read").status.phase,
            WorkloadPhase::Running,
            "the worker's report made it running"
        );
        assert_eq!(
            plane
                .ingest_node_reports(6, &[report("succeeded", Some(0))])
                .await
                .expect("ingested"),
            0,
            "another node cannot report this instance"
        );
        plane
            .ingest_node_reports(5, &[report("succeeded", Some(0))])
            .await
            .expect("ingested");
        plane.reconcile_at(Utc::now(), false).await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Succeeded);
        assert_eq!(view.status.exit_code, Some(0));
    }

    /// A deployment replica pinned to one worker runs there through its
    /// stream, and the host port the worker published comes back with its
    /// report: that is the address the control-plane proxy dials.
    #[tokio::test]
    async fn a_replica_pinned_to_a_worker_runs_there_and_reports_its_published_port() {
        let Some(db) = test_db().await else { return };
        let control_plane = FakeDriver::new();
        let rpc = FakeDriver::new();
        let plane = Arc::new(
            WorkloadPlane::new(
                db.db.clone(),
                NodeDriver(Some(control_plane.clone())),
                Arc::new(NoEnvSource),
                Arc::new(RecordingAudit::default()),
                PlaneConfig::new(format!(
                    "t{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                )),
            )
            .with_placement(Arc::new(OnWorker {
                db: db.db.clone(),
                rpc: rpc.clone(),
            })),
        );
        let mut spec = crate::testing::deployment_spec("app:1");
        spec.placement.nodes = Some(vec![7]);
        let id = plane
            .apply(apply_request(42, "app-on-worker", spec))
            .await
            .expect("applied");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(control_plane.running().is_empty(), "nothing runs on node 0");

        let row = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .one(db.db.as_ref())
            .await
            .expect("read")
            .expect("an instance row");
        assert_eq!(row.node_id, Some(7), "on the pinned worker");
        assert_eq!(row.driver, STREAM_DRIVER);

        // Nothing has run yet: the worker has not even read the row. It
        // stays pending, so the worker creates it when its stream delivers
        // it, and nobody waiting for the replica is told it runs.
        assert_eq!(row.phase, "pending");
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Pending);
        let running = view
            .status
            .condition(ConditionKind::Running)
            .expect("running condition");
        assert_eq!(running.status, ConditionStatus::False);
        assert_eq!(running.reason, "WaitingForNode");
        assert!(matches!(
            plane
                .wait_for(id, ConditionKind::Running, StdDuration::from_millis(200))
                .await,
            Err(PlaneError::WaitTimedOut { .. })
        ));
        // A second pass before the report changes nothing.
        plane.reconcile_at(Utc::now(), false).await;
        let state = plane.read_node_state(7, 0).await.expect("read");
        assert!(state.changes.iter().any(|c| matches!(
            c,
            StateChange::Upsert { object: StateObject::Instance(i) }
                if i.id == row.id && i.phase == "pending"
        )));
        // What the worker fetches when it creates the container.
        assert!(plane.instance_env(7, row.id).await.is_ok());

        let published = temps_workload::PortBinding {
            container_port: 3000,
            protocol: temps_workload::PortProtocol::Tcp,
            host_ip: "10.0.0.7".into(),
            host_port: Some(40000),
        };
        let report = InstanceReport {
            instance_id: row.id,
            phase: "running".into(),
            exit_code: None,
            started_at: Some(Utc::now()),
            finished_at: None,
            conditions: Vec::new(),
            published_ports: vec![published.clone()],
        };
        assert_eq!(
            plane
                .ingest_node_reports(7, &[report])
                .await
                .expect("ingested"),
            1
        );
        // The report, not the create, makes it running.
        plane.reconcile_at(Utc::now(), false).await;
        let view = plane
            .wait_for(id, ConditionKind::Running, StdDuration::from_secs(1))
            .await
            .expect("running after the report");
        assert_eq!(view.status.phase, WorkloadPhase::Running);
        let (_, status) = plane
            .instance_status(id)
            .await
            .expect("status")
            .expect("an instance");
        assert_eq!(status.state, temps_workload::InstanceState::Running);
        assert_eq!(status.published_ports, vec![published]);
    }

    #[test]
    fn instance_names_parse_back_to_their_workload_and_attempt() {
        assert_eq!(parse_instance_name("temps-cron-42-a3"), Some((42, 3)));
        assert_eq!(parse_instance_name("t1a2b-cron-7-a1"), Some((7, 1)));
        assert_eq!(parse_instance_name("not-an-instance"), None);
    }
}
