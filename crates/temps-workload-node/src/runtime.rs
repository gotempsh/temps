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

use crate::store::{KeptEnv, NodeState, SecretStore, SnapshotStore, StoreError};

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
    /// The host address published ports bind to (this node's private
    /// address), read at each create; `None` keeps the template's.
    publish_host_ip: Option<PublishHostIp>,
    /// This node's cluster DNS resolver (ADR-024), read at each create;
    /// `None` keeps the template's (the runtime's default).
    cluster_dns: Option<temps_workload::ResolverSource>,
}

/// Answers this node's current private address.
pub type PublishHostIp = Arc<dyn Fn() -> String + Send + Sync>;

/// A condition with a caller-chosen transition time, so a state reported on
/// every pass compares equal to the last report (and is not re-sent, nor
/// re-written by the control plane, on every heartbeat).
fn condition_at(
    kind: ConditionKind,
    status: ConditionStatus,
    reason: &str,
    message: String,
    at: chrono::DateTime<Utc>,
) -> Condition {
    Condition {
        kind,
        status,
        reason: reason.to_string(),
        message,
        last_transition_at: at,
    }
}

/// When `previous` first reported `reason`, else now.
fn reported_since(previous: Option<&InstanceReport>, reason: &str) -> chrono::DateTime<Utc> {
    previous
        .and_then(|p| p.conditions.iter().find(|c| c.reason == reason))
        .map(|c| c.last_transition_at)
        .unwrap_or_else(Utc::now)
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
            publish_host_ip: None,
            cluster_dns: None,
        })
    }

    /// Bind published ports to the address `host_ip` answers at create
    /// time, never the control plane's (ADR-045's private-address rule).
    pub fn with_publish_host_ip(mut self, host_ip: PublishHostIp) -> Self {
        self.publish_host_ip = Some(host_ip);
        self
    }

    /// Point new instances at this node's resolver, as the legacy agent
    /// does for the containers it starts, so `*.temps.local` resolves.
    pub fn with_cluster_dns(mut self, resolvers: temps_workload::ResolverSource) -> Self {
        self.cluster_dns = Some(resolvers);
        self
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
            published_ports: observed.published_ports.clone(),
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
        let long_running = instance.template.long_running;
        match observed.state {
            InstanceState::Running if long_running => Some(InstanceReport {
                phase: "running".to_string(),
                conditions: vec![condition_at(
                    ConditionKind::Running,
                    ConditionStatus::True,
                    "Started",
                    format!(
                        "instance '{}' is running on node {}",
                        handle.name, self.node_id
                    ),
                    observed
                        .started_at
                        .unwrap_or_else(|| reported_since(previous, "Started")),
                )],
                ..base
            }),
            InstanceState::Running => Some(InstanceReport {
                phase: "running".to_string(),
                ..base
            }),
            // A stopped replica is its owner's to start again: still the
            // same attempt, reported stopped.
            InstanceState::Exited if long_running => Some(InstanceReport {
                phase: "running".to_string(),
                exit_code: observed.exit_code,
                conditions: vec![condition_at(
                    ConditionKind::Running,
                    ConditionStatus::False,
                    "Stopped",
                    format!(
                        "instance '{}' on node {} is stopped (exit code {:?})",
                        handle.name, self.node_id, observed.exit_code
                    ),
                    observed
                        .finished_at
                        .unwrap_or_else(|| reported_since(previous, "Stopped")),
                )],
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
            InstanceState::Missing if long_running && instance.phase == "running" => {
                // Removed after it ran (the owner retired it, or an
                // operator): never recreated, so a retired revision cannot
                // come back. The control plane concludes the workload.
                Some(InstanceReport {
                    phase: "running".to_string(),
                    conditions: vec![condition_at(
                        ConditionKind::Running,
                        ConditionStatus::False,
                        "InstanceRemoved",
                        format!(
                            "instance '{}' was removed from node {} after it ran; it is not \
                             recreated",
                            handle.name, self.node_id
                        ),
                        reported_since(previous, "InstanceRemoved"),
                    )],
                    ..base
                })
            }
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
        let kept = match self.link.instance_env(instance.id).await {
            Ok(env) => {
                let kept = KeptEnv {
                    env: env.env,
                    secret_files: env.secret_files,
                };
                if let Err(error) = self.secrets.save(&handle.name, &kept) {
                    tracing::warn!(
                        node_id = self.node_id,
                        instance = %handle.name,
                        "keeping the instance's secret file failed: {error}"
                    );
                }
                kept
            }
            Err(link_error) => match self.secrets.load(&handle.name) {
                Ok(Some(kept)) => kept,
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
        let mut attach = instance.template.attachments.clone();
        if let Some(dns) = &self.cluster_dns {
            dns.apply(&mut attach.network);
        }
        let mut spec = instance
            .template
            .clone()
            .into_spec(prepared.local_ref.clone(), kept.env);
        spec.secret_files = kept.secret_files;
        if let Some(host_ip) = &self.publish_host_ip {
            let host_ip = host_ip();
            for port in &mut spec.ports {
                port.host_ip = host_ip.clone();
            }
        }
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
        // The host ports Docker picked, in the same report: the control
        // plane routes to them as soon as it sees the instance running.
        let published_ports = match self.driver.inspect(&created).await {
            Ok(status) => status.published_ports,
            Err(error) => {
                tracing::warn!(
                    node_id = self.node_id,
                    instance = %handle.name,
                    "reading the started instance's ports failed; the next pass reports them: \
                     {error}"
                );
                Vec::new()
            }
        };
        Some(InstanceReport {
            phase: "running".to_string(),
            started_at: Some(Utc::now()),
            published_ports,
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
        /// What each container published: Docker picks port 40000.
        ports: Mutex<HashMap<String, Vec<temps_workload::PortBinding>>>,
        /// The resolvers each container was created with.
        dns: Mutex<HashMap<String, Option<Vec<String>>>>,
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
            attach: &Attachments,
        ) -> Result<InstanceHandle, DriverError> {
            self.dns
                .lock()
                .expect("lock")
                .insert(spec.name.clone(), attach.network.dns.clone());
            self.containers.lock().expect("lock").insert(
                spec.name.clone(),
                (InstanceState::Created, spec.env.clone()),
            );
            self.ports.lock().expect("lock").insert(
                spec.name.clone(),
                spec.ports
                    .iter()
                    .map(|port| temps_workload::PortBinding {
                        host_port: Some(40000),
                        ..port.clone()
                    })
                    .collect(),
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
                restart_at: None,
                state,
                exit_code: (state == InstanceState::Exited).then_some(0),
                started_at: None,
                finished_at: None,
                address: None,
                published_ports: if state == InstanceState::Missing {
                    Vec::new()
                } else {
                    self.ports
                        .lock()
                        .expect("lock")
                        .get(&h.name)
                        .cloned()
                        .unwrap_or_default()
                },
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
                secret_files: Vec::new(),
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
                long_running: false,
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

    /// A worker's instances get its cluster resolver, read at each create:
    /// the runtime's default before the resolver is up, its address after.
    #[tokio::test]
    async fn instances_get_the_node_resolver_once_it_is_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (runtime, docker, _) = runtime(dir.path(), true);
        let slot = Arc::new(std::sync::RwLock::new(None::<String>));
        let read = slot.clone();
        let runtime = runtime.with_cluster_dns(temps_workload::ResolverSource::new(move || {
            read.read()
                .ok()
                .and_then(|ip| ip.clone())
                .map(|ip| vec![ip])
        }));
        // Distinct attempts, so distinct container names.
        let replica = |id: i64, attempt: i32| {
            let mut replica = instance(id, attempt, "pending");
            replica.template.long_running = true;
            replica
        };
        runtime
            .apply(full("100", vec![replica(31, 1)]))
            .await
            .expect("applied");
        runtime.reconcile().await;
        *slot.write().expect("slot") = Some("10.0.0.53".to_string());
        runtime
            .apply(full("101", vec![replica(31, 1), replica(32, 2)]))
            .await
            .expect("applied");
        runtime.reconcile().await;

        let mut dns: Vec<_> = docker
            .dns
            .lock()
            .expect("lock")
            .iter()
            .map(|(name, dns)| (name.clone(), dns.clone()))
            .collect();
        dns.sort();
        assert_eq!(dns.len(), 2, "{dns:?}");
        assert_eq!(dns[0].1, None, "created before the resolver was up");
        assert_eq!(dns[1].1, Some(vec!["10.0.0.53".to_string()]));
    }

    /// A deployment replica on a worker: its ports bind to the node's
    /// private address and are reported; a stop is reported as `Stopped`
    /// on the same attempt; once removed after it ran it is never
    /// recreated.
    #[tokio::test]
    async fn a_long_running_replica_is_reported_stopped_and_never_recreated_once_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (runtime, docker, _) = runtime(dir.path(), true);
        let runtime = runtime.with_publish_host_ip(Arc::new(|| "10.0.0.5".to_string()));
        let replica = |phase: &str| {
            let mut replica = instance(21, 1, phase);
            replica.template.long_running = true;
            replica.template.ports = vec![temps_workload::PortBinding {
                container_port: 3000,
                protocol: temps_workload::PortProtocol::Tcp,
                host_ip: "127.0.0.1".into(),
                host_port: None,
            }];
            replica
        };
        runtime
            .apply(full("100", vec![replica("pending")]))
            .await
            .expect("applied");
        runtime.reconcile().await;
        let reports = runtime.pending_reports().await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].phase, "running");
        assert_eq!(
            reports[0].published_ports,
            vec![temps_workload::PortBinding {
                container_port: 3000,
                protocol: temps_workload::PortProtocol::Tcp,
                host_ip: "10.0.0.5".into(),
                host_port: Some(40000),
            }],
            "bound to the private address, reported in the same report"
        );
        runtime.mark_sent(&reports);

        // The control plane keeps the row pending until it ingests that
        // report: a delta delivering it pending again must not create a
        // second container, nor lose the running one.
        runtime
            .apply(StateResponse {
                node_id: 5,
                watermark: "150".into(),
                full: None,
                changes: vec![StateChange::Upsert {
                    object: StateObject::Instance(replica("pending")),
                }],
                more: false,
            })
            .await
            .expect("applied");
        runtime.reconcile().await;
        {
            let containers = docker.containers.lock().expect("lock");
            assert_eq!(containers.len(), 1, "created once");
            assert_eq!(
                containers.get("temps-job-3-a1").map(|c| c.0),
                Some(InstanceState::Running)
            );
        }
        assert!(
            runtime
                .pending_reports()
                .await
                .iter()
                .all(|r| r.phase == "running"),
            "still reported running"
        );

        // Its owner stops it: the same attempt, reported stopped, once.
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
        assert_eq!(reports[0].phase, "running", "not a finished attempt");
        assert_eq!(reports[0].conditions[0].reason, "Stopped");
        assert_eq!(reports[0].conditions[0].status, ConditionStatus::False);
        runtime.mark_sent(&reports);
        runtime.reconcile().await;
        assert!(
            runtime.pending_reports().await.is_empty(),
            "a stopped replica is not re-reported on every pass"
        );

        // Removed after it ran (retired): reported, never recreated.
        runtime
            .apply(StateResponse {
                node_id: 5,
                watermark: "200".into(),
                full: None,
                changes: vec![StateChange::Upsert {
                    object: StateObject::Instance(replica("running")),
                }],
                more: false,
            })
            .await
            .expect("applied");
        docker.containers.lock().expect("lock").clear();
        for _ in 0..2 {
            runtime.reconcile().await;
            assert!(
                docker.containers.lock().expect("lock").is_empty(),
                "a retired replica must not come back"
            );
        }
        let reports = runtime.pending_reports().await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].phase, "running");
        assert_eq!(reports[0].conditions[0].reason, "InstanceRemoved");
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
