// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node runtime's reconciler.
//!
//! Desired state is the instance row the control plane streams:
//!
//! - `pending`/`running`: the instance must run. A missing container is
//!   created (environment from the control plane, else from the instance's
//!   kept secret file, else `SecretsUnavailable`) and started; one that
//!   exits is reported `succeeded`/`failed` with its exit code.
//! - `succeeded`/`failed`: the instance must not run. A running container
//!   is stopped and kept, for its output, until the instance is removed.
//! - removed, or replaced by a later attempt of the same workload: the
//!   container is removed, with its secret file.
//!
//! Containers carrying the plane's labels that no instance names are
//! removed once the node holds a whole view.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use temps_workload::stream::{
    InstanceEnv, InstanceObject, InstanceReport, ObjectKind, StateAck, StateChange, StateObject,
    StateResponse, STREAM_DRIVER,
};
use temps_workload::{
    Condition, ConditionKind, ConditionStatus, DriverError, InstanceHandle, InstanceState,
    RuntimeDriver, SecurityProfile,
};

use crate::store::{NodeState, SecretStore, SnapshotStore, StoreError};

/// Why the control plane could not be reached or answered with an error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LinkError {
    #[error("the control plane is unreachable: {reason}")]
    Unreachable { reason: String },
    #[error("the control plane refused the {operation} call with status {status}: {detail}")]
    Refused {
        operation: &'static str,
        status: u16,
        detail: String,
    },
}

/// The node's connection to the control plane.
#[async_trait]
pub trait ControlPlaneLink: Send + Sync {
    /// `GET /internal/nodes/{id}/state?since=W`.
    async fn fetch_state(&self, since: &str) -> Result<StateResponse, LinkError>;
    /// `POST /internal/nodes/{id}/state/ack`.
    async fn ack(&self, ack: &StateAck) -> Result<(), LinkError>;
    /// `GET /internal/nodes/{id}/instances/{instance_id}/env`.
    async fn instance_env(&self, instance_id: i64) -> Result<InstanceEnv, LinkError>;
}

#[derive(Debug, thiserror::Error)]
pub enum NodeRuntimeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("node {node_id}: the stream answered for node {answered}")]
    WrongNode { node_id: i32, answered: i32 },
}

/// What one reconcile pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub created: usize,
    pub stopped: usize,
    pub removed: usize,
    pub waiting_for_secrets: usize,
    pub errors: usize,
}

pub struct NodeRuntime {
    node_id: i32,
    driver: Arc<dyn RuntimeDriver>,
    link: Arc<dyn ControlPlaneLink>,
    snapshots: SnapshotStore,
    secrets: SecretStore,
    /// The plane's scope label value; only containers carrying it are ever
    /// removed as orphans.
    scope: String,
    state: tokio::sync::Mutex<NodeState>,
    /// Containers of removed instances still to remove.
    doomed: Mutex<BTreeSet<String>>,
    /// Whether the state holds a whole view (after a full snapshot, in
    /// this process or the one that saved it).
    synced: std::sync::atomic::AtomicBool,
    /// The last report sent per instance, so only changes are sent.
    sent: Mutex<BTreeMap<i64, InstanceReport>>,
    /// Wakes the heartbeat early when a report changed.
    pub reports_changed: Arc<tokio::sync::Notify>,
}

fn condition(kind: ConditionKind, reason: &str, message: String) -> Condition {
    Condition {
        kind,
        status: ConditionStatus::False,
        reason: reason.to_string(),
        message,
        last_transition_at: Utc::now(),
    }
}

/// The handle the local driver knows the instance by: the container name.
fn local_handle(driver: &dyn RuntimeDriver, instance: &InstanceObject) -> InstanceHandle {
    InstanceHandle {
        driver: driver.capabilities().name.clone(),
        id: instance.handle.name.clone(),
        name: instance.handle.name.clone(),
    }
}

fn is_active(phase: &str) -> bool {
    phase == "pending" || phase == "running"
}

impl NodeRuntime {
    /// Load the last saved state from `dir` (empty when there is none).
    pub fn open(
        node_id: i32,
        dir: impl Into<std::path::PathBuf>,
        scope: impl Into<String>,
        driver: Arc<dyn RuntimeDriver>,
        link: Arc<dyn ControlPlaneLink>,
    ) -> Result<Self, NodeRuntimeError> {
        let dir = dir.into();
        let snapshots = SnapshotStore::new(&dir);
        let mut state = snapshots.load()?;
        if state.watermark.is_empty() {
            state.watermark = "0".to_string();
        }
        let synced = state.watermark != "0";
        Ok(Self {
            node_id,
            driver,
            link,
            snapshots,
            secrets: SecretStore::new(&dir),
            scope: scope.into(),
            state: tokio::sync::Mutex::new(state),
            doomed: Mutex::new(BTreeSet::new()),
            synced: std::sync::atomic::AtomicBool::new(synced),
            sent: Mutex::new(BTreeMap::new()),
            reports_changed: Arc::new(tokio::sync::Notify::new()),
        })
    }

    pub fn node_id(&self) -> i32 {
        self.node_id
    }

    pub fn snapshot_path(&self) -> std::path::PathBuf {
        self.snapshots.path()
    }

    /// The cursor of the next read.
    pub async fn watermark(&self) -> String {
        self.state.lock().await.watermark.clone()
    }

    /// The instances the node holds, by id.
    pub async fn instances(&self) -> BTreeMap<i64, InstanceObject> {
        self.state.lock().await.instances.clone()
    }

    /// Apply one stream answer and save the state. Returns the acks to
    /// send.
    pub async fn apply(&self, response: StateResponse) -> Result<StateAck, NodeRuntimeError> {
        if response.node_id != self.node_id {
            return Err(NodeRuntimeError::WrongNode {
                node_id: self.node_id,
                answered: response.node_id,
            });
        }
        let mut state = self.state.lock().await;
        let mut removed_instances: Vec<InstanceObject> = Vec::new();
        if response.full.is_some() {
            let keep: BTreeSet<i64> = response
                .changes
                .iter()
                .filter_map(|change| match change {
                    StateChange::Upsert {
                        object: StateObject::Instance(i),
                    } => Some(i.id),
                    _ => None,
                })
                .collect();
            let gone: Vec<i64> = state
                .instances
                .keys()
                .copied()
                .filter(|id| !keep.contains(id))
                .collect();
            for id in gone {
                if let Some(instance) = state.instances.remove(&id) {
                    removed_instances.push(instance);
                }
            }
            state.workloads.clear();
            state.dns_claims.clear();
        }
        for change in response.changes {
            match change {
                StateChange::Upsert { object } => match object {
                    StateObject::Workload(w) => {
                        state.workloads.insert(w.id, w);
                    }
                    StateObject::Instance(i) => {
                        state.instances.insert(i.id, i);
                    }
                    StateObject::DnsClaim(c) => {
                        state.dns_claims.insert(c.id, c);
                    }
                },
                StateChange::Remove { kind, id } => match kind {
                    ObjectKind::Workload => {
                        state.workloads.remove(&id);
                    }
                    ObjectKind::Instance => {
                        if let Some(instance) = state.instances.remove(&id) {
                            removed_instances.push(instance);
                        }
                    }
                    ObjectKind::DnsClaim => {
                        state.dns_claims.remove(&id);
                    }
                },
            }
        }
        for instance in &removed_instances {
            state.observed.remove(&instance.id);
        }
        {
            let mut doomed = self
                .doomed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            doomed.extend(removed_instances.iter().map(|i| i.handle.name.clone()));
        }
        state.watermark = response.watermark.clone();
        if response.full.is_some() {
            self.synced
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.snapshots.save(&state)?;
        Ok(StateAck {
            acks: ObjectKind::ALL
                .iter()
                .map(|kind| (*kind, response.watermark.clone()))
                .collect(),
        })
    }

    /// One pass over the instances the node holds.
    pub async fn reconcile(&self) -> ReconcileOutcome {
        let mut outcome = ReconcileOutcome::default();
        let doomed: Vec<String> = {
            let mut doomed = self
                .doomed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *doomed).into_iter().collect()
        };
        for name in doomed {
            self.remove_container(&name, &mut outcome).await;
        }

        let (instances, observed) = {
            let state = self.state.lock().await;
            (state.instances.clone(), state.observed.clone())
        };
        // Only instances created from the stream are this runtime's to
        // drive. One the control plane created through `/agent/*` (an
        // older placement, or a socket-granted run) is in the view so it
        // is never swept as an orphan, and is otherwise left alone.
        let driven = instances
            .values()
            .filter(|instance| instance.handle.driver == STREAM_DRIVER);
        // The latest attempt of each workload on this node; earlier ones
        // were replaced.
        let mut latest: BTreeMap<i64, i32> = BTreeMap::new();
        for instance in driven.clone() {
            let entry = latest
                .entry(instance.workload_id)
                .or_insert(instance.attempt);
            *entry = (*entry).max(instance.attempt);
        }
        let mut reports: BTreeMap<i64, InstanceReport> = BTreeMap::new();
        for instance in driven {
            if latest.get(&instance.workload_id) != Some(&instance.attempt) {
                self.remove_container(&instance.handle.name, &mut outcome)
                    .await;
                continue;
            }
            let previous = observed.get(&instance.id);
            if let Some(report) = self.reconcile_one(instance, previous, &mut outcome).await {
                reports.insert(instance.id, report);
            }
        }
        if self.synced.load(std::sync::atomic::Ordering::Relaxed) {
            self.sweep_orphans(&instances, &mut outcome).await;
        }
        let mut state = self.state.lock().await;
        let mut changed = false;
        for (id, report) in reports {
            if !state.instances.contains_key(&id) {
                continue;
            }
            if state.observed.get(&id) != Some(&report) {
                state.observed.insert(id, report);
                changed = true;
            }
        }
        if changed {
            if let Err(error) = self.snapshots.save(&state) {
                tracing::warn!(
                    node_id = self.node_id,
                    "saving the node state failed: {error}"
                );
            }
            self.reports_changed.notify_one();
        }
        outcome
    }

    async fn reconcile_one(
        &self,
        instance: &InstanceObject,
        previous: Option<&InstanceReport>,
        outcome: &mut ReconcileOutcome,
    ) -> Option<InstanceReport> {
        let handle = local_handle(self.driver.as_ref(), instance);
        let observed = match self.driver.inspect(&handle).await {
            Ok(observed) => observed,
            Err(error) => {
                outcome.errors += 1;
                tracing::warn!(
                    node_id = self.node_id,
                    instance = %handle.name,
                    "inspecting the instance failed: {error}"
                );
                return None;
            }
        };
        let base = InstanceReport {
            instance_id: instance.id,
            phase: instance.phase.clone(),
            exit_code: None,
            started_at: observed.started_at,
            finished_at: None,
            conditions: Vec::new(),
        };
        if !is_active(&instance.phase) {
            if observed.state == InstanceState::Running {
                match self.driver.stop(&handle).await {
                    Ok(()) => {
                        outcome.stopped += 1;
                        tracing::info!(
                            node_id = self.node_id,
                            instance = %handle.name,
                            phase = %instance.phase,
                            "stopped an instance the control plane finished"
                        );
                    }
                    Err(error) => {
                        outcome.errors += 1;
                        tracing::warn!(
                            node_id = self.node_id,
                            instance = %handle.name,
                            "stopping a finished instance failed: {error}"
                        );
                    }
                }
            }
            return None;
        }
        match observed.state {
            InstanceState::Running => Some(InstanceReport {
                phase: "running".to_string(),
                ..base
            }),
            InstanceState::Exited => {
                let phase = if observed.exit_code == Some(0) {
                    "succeeded"
                } else {
                    "failed"
                };
                Some(InstanceReport {
                    phase: phase.to_string(),
                    exit_code: observed.exit_code,
                    finished_at: observed.finished_at.or(Some(Utc::now())),
                    ..base
                })
            }
            InstanceState::Created => match self.driver.start(&handle).await {
                Ok(()) => Some(InstanceReport {
                    phase: "running".to_string(),
                    started_at: Some(Utc::now()),
                    ..base
                }),
                Err(error) => Some(self.failed_to(&base, "StartFailed", &handle, error, outcome)),
            },
            InstanceState::Missing => {
                if previous.is_some_and(|p| p.phase == "running") {
                    // It ran here and is gone: the attempt was lost.
                    return Some(InstanceReport {
                        phase: "failed".to_string(),
                        finished_at: Some(Utc::now()),
                        conditions: vec![condition(
                            ConditionKind::Running,
                            "InstanceLost",
                            format!(
                                "instance '{}' disappeared from node {} while running",
                                handle.name, self.node_id
                            ),
                        )],
                        ..base
                    });
                }
                self.create(instance, &handle, base, outcome).await
            }
        }
    }

    fn failed_to(
        &self,
        base: &InstanceReport,
        reason: &str,
        handle: &InstanceHandle,
        error: DriverError,
        outcome: &mut ReconcileOutcome,
    ) -> InstanceReport {
        outcome.errors += 1;
        tracing::warn!(node_id = self.node_id, instance = %handle.name, "{reason}: {error}");
        InstanceReport {
            phase: "failed".to_string(),
            finished_at: Some(Utc::now()),
            conditions: vec![condition(
                ConditionKind::Running,
                reason,
                format!(
                    "instance '{}' on node {}: {error}",
                    handle.name, self.node_id
                ),
            )],
            ..base.clone()
        }
    }

    async fn create(
        &self,
        instance: &InstanceObject,
        handle: &InstanceHandle,
        base: InstanceReport,
        outcome: &mut ReconcileOutcome,
    ) -> Option<InstanceReport> {
        if instance.template.security == SecurityProfile::HostPrivileged {
            // Socket-granted runs go through the `/agent/*` path, which
            // applies this host's own ADR-045 grant; the stream never
            // mounts the socket.
            return Some(InstanceReport {
                conditions: vec![condition(
                    ConditionKind::Running,
                    "SocketNotGranted",
                    format!(
                        "instance '{}' asks for the Docker socket, which node {} does not \
                         provide through its state stream",
                        handle.name, self.node_id
                    ),
                )],
                ..base
            });
        }
        let env = match self.link.instance_env(instance.id).await {
            Ok(env) => {
                if let Err(error) = self.secrets.save(&handle.name, &env.env) {
                    tracing::warn!(
                        node_id = self.node_id,
                        instance = %handle.name,
                        "keeping the instance's secret file failed: {error}"
                    );
                }
                env.env
            }
            Err(link_error) => match self.secrets.load(&handle.name) {
                Ok(Some(env)) => env,
                Ok(None) | Err(_) => {
                    outcome.waiting_for_secrets += 1;
                    let message = format!(
                        "instance '{}' cannot be created on node {}: its secret files are gone \
                         and the control plane is unreachable ({link_error}); retrying when the \
                         stream reconnects",
                        handle.name, self.node_id
                    );
                    tracing::warn!(node_id = self.node_id, instance = %handle.name, "{message}");
                    return Some(InstanceReport {
                        conditions: vec![condition(
                            ConditionKind::Running,
                            "SecretsUnavailable",
                            message,
                        )],
                        ..base
                    });
                }
            },
        };
        let prepared = match self.driver.prepare(&instance.template.artifact).await {
            Ok(prepared) => prepared,
            Err(error) => {
                // Not terminal: the control plane may still be moving the
                // image onto this node. Its deadline bounds the wait.
                outcome.errors += 1;
                return Some(InstanceReport {
                    conditions: vec![condition(
                        ConditionKind::ArtifactReady,
                        "ArtifactPullFailed",
                        format!(
                            "instance '{}' on node {}: {error}",
                            handle.name, self.node_id
                        ),
                    )],
                    ..base
                });
            }
        };
        let attach = instance.template.attachments.clone();
        let spec = instance
            .template
            .clone()
            .into_spec(prepared.local_ref.clone(), env);
        let created = match self.driver.create(&spec, &attach).await {
            Ok(created) => created,
            Err(error) => {
                let _ = self.driver.remove(handle, true).await;
                return Some(self.failed_to(&base, "CreateFailed", handle, error, outcome));
            }
        };
        if let Err(error) = self.driver.start(&created).await {
            let _ = self.driver.remove(&created, true).await;
            return Some(self.failed_to(&base, "StartFailed", handle, error, outcome));
        }
        outcome.created += 1;
        tracing::info!(
            node_id = self.node_id,
            instance = %handle.name,
            instance_id = instance.id,
            "created and started an instance from the state stream"
        );
        Some(InstanceReport {
            phase: "running".to_string(),
            started_at: Some(Utc::now()),
            ..base
        })
    }

    async fn remove_container(&self, name: &str, outcome: &mut ReconcileOutcome) {
        let handle = InstanceHandle {
            driver: self.driver.capabilities().name.clone(),
            id: name.to_string(),
            name: name.to_string(),
        };
        match self.driver.inspect(&handle).await {
            Ok(status) if status.state == InstanceState::Missing => {}
            Ok(_) => match self.driver.remove(&handle, true).await {
                Ok(()) => {
                    outcome.removed += 1;
                    tracing::info!(node_id = self.node_id, instance = %name, "removed an instance");
                }
                Err(DriverError::NotFound { .. }) => {}
                Err(error) => {
                    outcome.errors += 1;
                    tracing::warn!(node_id = self.node_id, instance = %name, "removing failed: {error}");
                    self.doomed
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(name.to_string());
                    return;
                }
            },
            Err(error) => {
                outcome.errors += 1;
                tracing::warn!(node_id = self.node_id, instance = %name, "inspecting before removal failed: {error}");
                self.doomed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(name.to_string());
                return;
            }
        }
        if let Err(error) = self.secrets.remove(name) {
            tracing::warn!(node_id = self.node_id, instance = %name, "removing the secret file failed: {error}");
        }
    }

    async fn sweep_orphans(
        &self,
        instances: &BTreeMap<i64, InstanceObject>,
        outcome: &mut ReconcileOutcome,
    ) {
        let selector = BTreeMap::from([
            ("sh.temps.plane".to_string(), self.scope.clone()),
            (
                "sh.temps.managed_by".to_string(),
                "workload-plane".to_string(),
            ),
        ]);
        let listed = match self.driver.list(&selector).await {
            Ok(listed) => listed,
            Err(DriverError::Unsupported { .. }) => return,
            Err(error) => {
                tracing::debug!(node_id = self.node_id, "orphan cleanup skipped: {error}");
                return;
            }
        };
        let known: BTreeSet<&str> = instances.values().map(|i| i.handle.name.as_str()).collect();
        for found in listed {
            if known.contains(found.handle.name.as_str()) {
                continue;
            }
            match self.driver.remove(&found.handle, true).await {
                Ok(()) => {
                    outcome.removed += 1;
                    tracing::info!(
                        node_id = self.node_id,
                        instance = %found.handle.name,
                        "removed an orphaned instance: the stream names no such instance"
                    );
                    let _ = self.secrets.remove(&found.handle.name);
                }
                Err(error) => tracing::warn!(
                    node_id = self.node_id,
                    instance = %found.handle.name,
                    "removing an orphaned instance failed: {error}"
                ),
            }
        }
    }

    /// Reports that changed since they were last sent.
    pub async fn pending_reports(&self) -> Vec<InstanceReport> {
        let state = self.state.lock().await;
        let sent = self
            .sent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .observed
            .values()
            .filter(|report| sent.get(&report.instance_id) != Some(*report))
            .cloned()
            .collect()
    }

    /// The control plane accepted `reports`.
    pub fn mark_sent(&self, reports: &[InstanceReport]) {
        let mut sent = self
            .sent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for report in reports {
            sent.insert(report.instance_id, report.clone());
        }
    }

    /// One read of the stream, applied and acked. Returns whether the
    /// control plane has more to send right away.
    pub async fn sync_once(&self) -> Result<bool, SyncError> {
        let since = self.watermark().await;
        let response = self.link.fetch_state(&since).await?;
        let more = response.more;
        let ack = self.apply(response).await?;
        if let Err(error) = self.link.ack(&ack).await {
            tracing::debug!(node_id = self.node_id, "acking the stream failed: {error}");
        }
        Ok(more)
    }
}

/// A failed [`NodeRuntime::sync_once`].
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    Runtime(#[from] NodeRuntimeError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use std::collections::HashMap;
    use temps_workload::stream::{InstanceTemplate, WorkloadObject};
    use temps_workload::{
        Artifact, Attachments, DriverCapabilities, EnvSpec, InstanceLimits, InstanceSpec,
        InstanceStats, InstanceStatus, Lifetime, LogQuery, LogStream, PreparedArtifact,
        RestartPolicy, WorkloadKind, WorkloadRef, WorkloadSpec,
    };

    const SEEDED_SECRET: &str = "seeded-secret-value-7f3a91";
    const SEEDED_IMPORTED: &str = "postgres://app:imported-secret-2b8c@db:5432/app";

    /// A fake container: its state and the environment it was created with.
    type FakeContainer = (InstanceState, Vec<(String, String)>);

    #[derive(Default)]
    struct FakeDocker {
        containers: Mutex<HashMap<String, FakeContainer>>,
    }

    fn caps() -> &'static DriverCapabilities {
        static CAPS: std::sync::OnceLock<DriverCapabilities> = std::sync::OnceLock::new();
        CAPS.get_or_init(|| DriverCapabilities {
            name: "docker".into(),
            artifacts: Default::default(),
            isolation: Default::default(),
            network_attachment: temps_workload::AttachmentKind::DockerNetwork,
            volumes: Default::default(),
            exec: false,
            snapshot: false,
            resize_disk: false,
            limits: Default::default(),
        })
    }

    #[async_trait]
    impl RuntimeDriver for FakeDocker {
        fn capabilities(&self) -> &DriverCapabilities {
            caps()
        }
        async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
            Ok(PreparedArtifact {
                artifact: artifact.clone(),
                local_ref: "app:latest".into(),
            })
        }
        async fn create(
            &self,
            spec: &InstanceSpec,
            _: &Attachments,
        ) -> Result<InstanceHandle, DriverError> {
            self.containers.lock().expect("lock").insert(
                spec.name.clone(),
                (InstanceState::Created, spec.env.clone()),
            );
            Ok(InstanceHandle {
                driver: "docker".into(),
                id: spec.name.clone(),
                name: spec.name.clone(),
            })
        }
        async fn start(&self, h: &InstanceHandle) -> Result<(), DriverError> {
            if let Some(c) = self.containers.lock().expect("lock").get_mut(&h.name) {
                c.0 = InstanceState::Running;
            }
            Ok(())
        }
        async fn stop(&self, h: &InstanceHandle) -> Result<(), DriverError> {
            if let Some(c) = self.containers.lock().expect("lock").get_mut(&h.name) {
                c.0 = InstanceState::Exited;
            }
            Ok(())
        }
        async fn remove(&self, h: &InstanceHandle, _: bool) -> Result<(), DriverError> {
            self.containers.lock().expect("lock").remove(&h.name);
            Ok(())
        }
        async fn inspect(&self, h: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
            let state = self
                .containers
                .lock()
                .expect("lock")
                .get(&h.name)
                .map(|c| c.0)
                .unwrap_or(InstanceState::Missing);
            Ok(InstanceStatus {
                state,
                exit_code: (state == InstanceState::Exited).then_some(0),
                started_at: None,
                finished_at: None,
                address: None,
                published_ports: Vec::new(),
            })
        }
        async fn logs(&self, _: &InstanceHandle, _: LogQuery) -> Result<LogStream, DriverError> {
            Ok(Box::pin(stream::empty()))
        }
        async fn stats(&self, _: &InstanceHandle) -> Result<InstanceStats, DriverError> {
            Ok(InstanceStats::default())
        }
        /// Every container carries the plane's labels here.
        async fn list(
            &self,
            _: &std::collections::BTreeMap<String, String>,
        ) -> Result<Vec<temps_workload::ListedInstance>, DriverError> {
            Ok(self
                .containers
                .lock()
                .expect("lock")
                .keys()
                .map(|name| temps_workload::ListedInstance {
                    handle: InstanceHandle {
                        driver: "docker".into(),
                        id: name.clone(),
                        name: name.clone(),
                    },
                    labels: Default::default(),
                })
                .collect())
        }
    }

    struct FakeLink {
        up: std::sync::atomic::AtomicBool,
        responses: Mutex<Vec<StateResponse>>,
    }

    #[async_trait]
    impl ControlPlaneLink for FakeLink {
        async fn fetch_state(&self, _since: &str) -> Result<StateResponse, LinkError> {
            self.responses
                .lock()
                .expect("lock")
                .pop()
                .ok_or(LinkError::Unreachable {
                    reason: "no scripted answer".into(),
                })
        }
        async fn ack(&self, _: &StateAck) -> Result<(), LinkError> {
            Ok(())
        }
        async fn instance_env(&self, instance_id: i64) -> Result<InstanceEnv, LinkError> {
            if !self.up.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(LinkError::Unreachable {
                    reason: "connection refused".into(),
                });
            }
            Ok(InstanceEnv {
                instance_id,
                env: vec![
                    ("DATABASE_URL".into(), SEEDED_IMPORTED.into()),
                    ("MODE".into(), "batch".into()),
                ],
            })
        }
    }

    fn spec() -> WorkloadSpec {
        WorkloadSpec {
            owner: WorkloadRef::new(WorkloadKind::Job, 3),
            artifact: Artifact::oci("app:latest"),
            isolation: Default::default(),
            process: Default::default(),
            env: EnvSpec {
                vars: std::collections::BTreeMap::from([
                    ("API_TOKEN".to_string(), SEEDED_SECRET.to_string()),
                    ("LOG_LEVEL".to_string(), "info".to_string()),
                ]),
                imports: vec!["deployment_container:9".into()],
                ..Default::default()
            },
            resources: Default::default(),
            volumes: vec![],
            ports: vec![],
            network: Default::default(),
            probes: Default::default(),
            placement: Default::default(),
            lifetime: Lifetime::RunToCompletion {
                timeout_secs: 60,
                retry_limit: 0,
                ttl_secs_after_finished: None,
            },
            security: None,
            labels: Default::default(),
        }
    }

    fn instance(id: i64, attempt: i32, phase: &str) -> InstanceObject {
        let name = format!("temps-job-3-a{attempt}");
        InstanceObject {
            id,
            workload_id: 3,
            attempt,
            revision: 1,
            node_id: 5,
            phase: phase.into(),
            handle: InstanceHandle {
                driver: "node-stream".into(),
                id: name.clone(),
                name: name.clone(),
            },
            template: InstanceTemplate {
                owner: WorkloadRef::new(WorkloadKind::Job, 3),
                name,
                artifact: Artifact::oci("app:latest"),
                platform: None,
                command: Some(vec!["true".into()]),
                entrypoint: None,
                workdir: None,
                user: None,
                ports: vec![],
                restart: RestartPolicy::Never,
                limits: InstanceLimits::default(),
                log_rotation: None,
                labels: Default::default(),
                security: SecurityProfile::Trusted,
                attachments: Attachments::default(),
            },
        }
    }

    fn full(watermark: &str, instances: Vec<InstanceObject>) -> StateResponse {
        let mut changes = vec![StateChange::Upsert {
            object: StateObject::Workload(WorkloadObject {
                id: 3,
                name: "cron-3-1".into(),
                generation: 1,
                phase: "running".into(),
                deleted: false,
                spec: spec(),
            }),
        }];
        changes.extend(instances.into_iter().map(|i| StateChange::Upsert {
            object: StateObject::Instance(i),
        }));
        StateResponse {
            node_id: 5,
            watermark: watermark.into(),
            full: Some(temps_workload::stream::FullReason::Initial),
            changes,
            more: false,
        }
    }

    fn runtime(dir: &std::path::Path, up: bool) -> (NodeRuntime, Arc<FakeDocker>, Arc<FakeLink>) {
        let docker = Arc::new(FakeDocker::default());
        let link = Arc::new(FakeLink {
            up: std::sync::atomic::AtomicBool::new(up),
            responses: Mutex::new(Vec::new()),
        });
        let runtime =
            NodeRuntime::open(5, dir, "temps", docker.clone(), link.clone()).expect("opened");
        (runtime, docker, link)
    }

    /// ADR-055 §7: the snapshot file a worker keeps never holds a secret
    /// value. A secret-classified spec variable and the imported
    /// environment (fetched for the create call) are seeded; the file is
    /// read back as bytes and searched for both.
    #[tokio::test]
    async fn the_snapshot_file_contains_no_seeded_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (runtime, docker, _) = runtime(dir.path(), true);
        runtime
            .apply(full("100", vec![instance(11, 1, "pending")]))
            .await
            .expect("applied");
        let outcome = runtime.reconcile().await;
        assert_eq!(outcome.created, 1, "{outcome:?}");
        // The instance got its environment...
        let env = docker.containers.lock().expect("lock")["temps-job-3-a1"]
            .1
            .clone();
        assert!(env.iter().any(|(_, v)| v == SEEDED_IMPORTED), "{env:?}");

        // ...and the snapshot file holds none of it.
        let bytes = std::fs::read(runtime.snapshot_path()).expect("snapshot written");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(SEEDED_SECRET),
            "seeded spec secret in {text}"
        );
        assert!(
            !text.contains(SEEDED_IMPORTED) && !text.contains("imported-secret-2b8c"),
            "seeded imported secret in {text}"
        );
        // Non-secret values and the references stay.
        assert!(text.contains("LOG_LEVEL") && text.contains("deployment_container:9"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(runtime.snapshot_path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test]
    async fn an_instance_without_secret_files_waits_for_the_control_plane() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (runtime, docker, link) = runtime(dir.path(), false);
        runtime
            .apply(full("100", vec![instance(11, 1, "pending")]))
            .await
            .expect("applied");
        let outcome = runtime.reconcile().await;
        assert_eq!(outcome.waiting_for_secrets, 1);
        assert!(docker.containers.lock().expect("lock").is_empty());
        let reports = runtime.pending_reports().await;
        let condition = &reports[0].conditions[0];
        assert_eq!(condition.reason, "SecretsUnavailable");
        assert_eq!(condition.kind, ConditionKind::Running);
        assert_eq!(condition.status, ConditionStatus::False);
        assert!(
            condition.message.contains("temps-job-3-a1"),
            "{}",
            condition.message
        );

        // The stream reconnects: the instance is created.
        link.up.store(true, std::sync::atomic::Ordering::Relaxed);
        let outcome = runtime.reconcile().await;
        assert_eq!(outcome.created, 1, "{outcome:?}");
        let reports = runtime.pending_reports().await;
        assert_eq!(reports[0].phase, "running");
        assert!(reports[0].conditions.is_empty());
    }

    #[tokio::test]
    async fn a_kept_secret_file_recreates_the_instance_without_the_control_plane() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let (runtime, docker, _) = runtime(dir.path(), true);
            runtime
                .apply(full("100", vec![instance(11, 1, "pending")]))
                .await
                .expect("applied");
            runtime.reconcile().await;
            // The container is lost (a pruned daemon), before it ever ran
            // to the point the node recorded it.
            docker.containers.lock().expect("lock").clear();
        }
        // A restarted agent, control plane down, state from disk.
        let (runtime, docker, _) = runtime(dir.path(), false);
        {
            let mut state = runtime.state.lock().await;
            state.observed.clear();
        }
        let outcome = runtime.reconcile().await;
        assert_eq!(outcome.created, 1, "{outcome:?}");
        assert!(docker
            .containers
            .lock()
            .expect("lock")
            .contains_key("temps-job-3-a1"));
    }

    #[tokio::test]
    async fn exits_are_reported_once_and_finished_instances_are_stopped_then_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (runtime, docker, _) = runtime(dir.path(), true);
        runtime
            .apply(full("100", vec![instance(11, 1, "pending")]))
            .await
            .expect("applied");
        runtime.reconcile().await;
        let first = runtime.pending_reports().await;
        runtime.mark_sent(&first);
        assert!(
            runtime.pending_reports().await.is_empty(),
            "unchanged: nothing to send"
        );

        docker
            .containers
            .lock()
            .expect("lock")
            .get_mut("temps-job-3-a1")
            .expect("created")
            .0 = InstanceState::Exited;
        runtime.reconcile().await;
        let reports = runtime.pending_reports().await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].phase, "succeeded");
        assert_eq!(reports[0].exit_code, Some(0));

        // A retry: attempt 2 replaces attempt 1, whose container goes.
        runtime
            .apply(full(
                "200",
                vec![instance(11, 1, "failed"), instance(12, 2, "pending")],
            ))
            .await
            .expect("applied");
        runtime.reconcile().await;
        let names: Vec<String> = docker
            .containers
            .lock()
            .expect("lock")
            .keys()
            .cloned()
            .collect();
        assert_eq!(names, vec!["temps-job-3-a2".to_string()]);

        // The control plane fails the run (its deadline): the node stops it.
        runtime
            .apply(StateResponse {
                node_id: 5,
                watermark: "300".into(),
                full: None,
                changes: vec![StateChange::Upsert {
                    object: StateObject::Instance(instance(12, 2, "failed")),
                }],
                more: false,
            })
            .await
            .expect("applied");
        let outcome = runtime.reconcile().await;
        assert_eq!(outcome.stopped, 1, "{outcome:?}");

        // The instance is removed: so is its container and secret file.
        runtime
            .apply(StateResponse {
                node_id: 5,
                watermark: "400".into(),
                full: None,
                changes: vec![StateChange::Remove {
                    kind: ObjectKind::Instance,
                    id: 12,
                }],
                more: false,
            })
            .await
            .expect("applied");
        runtime.reconcile().await;
        assert!(docker.containers.lock().expect("lock").is_empty());
        assert_eq!(runtime.secrets.load("temps-job-3-a2").expect("read"), None);
    }

    #[tokio::test]
    async fn an_instance_created_through_the_agent_api_is_left_to_the_control_plane() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (runtime, docker, _) = runtime(dir.path(), true);
        let mut agent_api = instance(11, 1, "pending");
        agent_api.handle.driver = "agent-api".into();
        // The control plane's `/agent/*` call created its container.
        docker.containers.lock().expect("lock").insert(
            agent_api.handle.name.clone(),
            (InstanceState::Running, Vec::new()),
        );
        runtime
            .apply(full("100", vec![agent_api]))
            .await
            .expect("applied");
        let outcome = runtime.reconcile().await;
        assert_eq!(
            outcome,
            ReconcileOutcome::default(),
            "nothing is done to it"
        );
        assert!(runtime.pending_reports().await.is_empty(), "nor reported");
        assert_eq!(
            docker.containers.lock().expect("lock")["temps-job-3-a1"].0,
            InstanceState::Running,
            "and it is not swept as an orphan"
        );

        // A plane container no instance names is.
        docker.containers.lock().expect("lock").insert(
            "temps-job-99-a1".into(),
            (InstanceState::Running, Vec::new()),
        );
        let outcome = runtime.reconcile().await;
        assert_eq!(outcome.removed, 1, "{outcome:?}");
        assert!(docker
            .containers
            .lock()
            .expect("lock")
            .contains_key("temps-job-3-a1"));
    }

    #[tokio::test]
    async fn an_answer_for_another_node_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (runtime, _, _) = runtime(dir.path(), true);
        let mut answer = full("1", vec![]);
        answer.node_id = 6;
        assert!(matches!(
            runtime.apply(answer).await,
            Err(NodeRuntimeError::WrongNode {
                node_id: 5,
                answered: 6
            })
        ));
    }
}
