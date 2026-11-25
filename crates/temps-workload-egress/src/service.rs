// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The opt-in flow, for every workload kind: whether a workload records,
//! whether it can, and turning it on or off.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::Serialize;
use temps_entities::{nodes, projects};
use temps_network_policy::NodeRecordingStatus;
use temps_otel::types::{LogQuery, LogRecord};
use temps_otel::OtelError;
use temps_workload::WorkloadKind;
use utoipa::ToSchema;

use crate::adapter::{RecordedWorkload, WorkloadEgressAdapter};
use crate::error::EgressRecordingError;

/// The node id the control plane uses for itself.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;

/// Where the console fixes a node that cannot record.
pub const NODES_SETUP_PATH: &str = "/settings/nodes";

/// Whether each node can record, as it reported about itself.
pub trait NodeRecordingCapabilities: Send + Sync {
    fn status(&self, node_id: i32) -> NodeRecordingStatus;
}

impl NodeRecordingCapabilities for temps_network_policy::NetworkPolicyService {
    fn status(&self, node_id: i32) -> NodeRecordingStatus {
        self.egress_recording_status(node_id)
    }
}

/// Where records are stored and read. [`temps_otel::OtelService`] in
/// production.
#[async_trait]
pub trait EgressLogStore: Send + Sync {
    async fn store(&self, records: Vec<LogRecord>) -> Result<u64, OtelError>;
    async fn query(&self, query: LogQuery) -> Result<Vec<LogRecord>, OtelError>;
    async fn count(&self, query: LogQuery) -> Result<u64, OtelError>;
}

#[async_trait]
impl EgressLogStore for temps_otel::OtelService {
    async fn store(&self, records: Vec<LogRecord>) -> Result<u64, OtelError> {
        self.ingest_platform_logs(records).await
    }

    async fn query(&self, query: LogQuery) -> Result<Vec<LogRecord>, OtelError> {
        self.query_logs(query).await
    }

    async fn count(&self, query: LogQuery) -> Result<u64, OtelError> {
        self.count_logs(query).await
    }
}

/// Whether one node the workload runs on can record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressRecordingNode {
    /// `0` is the control plane.
    pub node_id: i32,
    pub node_name: String,
    /// The node reports that it can record.
    pub recording: bool,
    /// Why it cannot, when `recording` is false: one of
    /// `unsupported_platform`, `proxy_not_serving`, `node_not_reporting`,
    /// `agent_predates_recording`.
    pub reason_code: Option<String>,
    /// The same, in a sentence that names what is missing.
    pub reason: Option<String>,
    /// Console path that fixes it, when one does.
    pub setup_path: Option<String>,
}

/// Whether a workload records its outbound connections, and whether it can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressRecordingStatus {
    /// What the workload is: `sandbox`, or `deployment` for an
    /// environment's deployments.
    pub workload_kind: String,
    /// The workload's id in its own API (`sbx_...`, an environment id).
    pub workload_id: String,
    /// The owner turned recording on.
    pub enabled: bool,
    /// Recording can work for this workload now: OTel storage is enabled
    /// and a node it runs on reports that it can record. When `enabled` is
    /// true and this is false, nothing is being recorded.
    pub configured: bool,
    /// Why recording cannot work, when `configured` is false:
    /// `otel_unavailable`, or the `reason_code` of the node that cannot
    /// record.
    pub reason_code: Option<String>,
    /// The same, in a sentence that names what is missing.
    pub reason: Option<String>,
    /// Console path that fixes it, when one does.
    pub setup_path: Option<String>,
    /// The nodes the workload runs on, and whether each can record. Empty
    /// when nothing of it is running.
    pub nodes: Vec<EgressRecordingNode>,
    /// The project that owns the workload, when one does.
    pub project_id: Option<i32>,
    /// Console path of the project's logs, filtered to this workload's
    /// records. `None` without a project: the records are then read only
    /// from the workload itself.
    pub logs_path: Option<String>,
}

/// Why recording cannot work for a workload.
struct Blocker {
    code: &'static str,
    reason: String,
    setup_path: Option<String>,
    /// The node that cannot record, when a node is what is missing.
    node: Option<(i32, String)>,
}

pub struct EgressRecordingService {
    pub(crate) db: Arc<DatabaseConnection>,
    capabilities: Arc<dyn NodeRecordingCapabilities>,
    /// `None` when this control plane runs without OTel storage: recording
    /// then reports itself unavailable, saying so.
    pub(crate) store: Option<Arc<dyn EgressLogStore>>,
    /// One adapter per workload kind that records, registered at startup.
    adapters: RwLock<HashMap<WorkloadKind, Arc<dyn WorkloadEgressAdapter>>>,
}

impl EgressRecordingService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        capabilities: Arc<dyn NodeRecordingCapabilities>,
        store: Option<Arc<dyn EgressLogStore>>,
    ) -> Self {
        Self {
            db,
            capabilities,
            store,
            adapters: RwLock::new(HashMap::new()),
        }
    }

    /// Makes a workload kind recordable. Called once per kind at startup;
    /// a second adapter for the same kind replaces the first.
    pub fn register(&self, adapter: Arc<dyn WorkloadEgressAdapter>) {
        let kind = adapter.kind();
        let replaced = self
            .adapters
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(kind, adapter)
            .is_some();
        tracing::debug!(%kind, replaced, "egress recording adapter registered");
    }

    /// The adapter of `kind`, or that this control plane records none.
    pub(crate) fn adapter(
        &self,
        kind: WorkloadKind,
    ) -> Result<Arc<dyn WorkloadEgressAdapter>, EgressRecordingError> {
        self.adapters
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&kind)
            .cloned()
            .ok_or(EgressRecordingError::KindNotRecorded { kind })
    }

    /// The names of `node_ids`, in one query. A node that is gone reads as
    /// `node-<id>`.
    pub(crate) async fn node_names(
        &self,
        node_ids: &[i32],
    ) -> Result<HashMap<i32, String>, EgressRecordingError> {
        let mut names = HashMap::with_capacity(node_ids.len());
        let workers: Vec<i32> = node_ids
            .iter()
            .copied()
            .filter(|id| *id != CONTROL_PLANE_NODE_ID)
            .collect();
        if !workers.is_empty() {
            let rows = nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(workers))
                .all(self.db.as_ref())
                .await
                .map_err(EgressRecordingError::db("looking up a workload's nodes"))?;
            names.extend(rows.into_iter().map(|node| (node.id, node.name)));
        }
        for id in node_ids {
            names.entry(*id).or_insert_with(|| {
                if *id == CONTROL_PLANE_NODE_ID {
                    "control-plane".to_string()
                } else {
                    format!("node-{id}")
                }
            });
        }
        Ok(names)
    }

    /// Each node the workload runs on, and whether it can record.
    async fn nodes_of(
        &self,
        workload: &RecordedWorkload,
    ) -> Result<Vec<EgressRecordingNode>, EgressRecordingError> {
        let names = self.node_names(&workload.nodes).await?;
        Ok(workload
            .nodes
            .iter()
            .map(|node_id| {
                let status = self.capabilities.status(*node_id);
                // The node's page fixes only an agent that is not reporting
                // or is too old; nothing in the console fixes a platform or
                // a proxy that is not serving, so those get no link.
                let fixable_on_the_node = matches!(
                    status,
                    NodeRecordingStatus::NotReporting { .. }
                        | NodeRecordingStatus::AgentPredatesRecording
                );
                let reason = status.describe();
                EgressRecordingNode {
                    node_id: *node_id,
                    node_name: names.get(node_id).cloned().unwrap_or_default(),
                    recording: reason.is_none(),
                    reason_code: reason.is_some().then(|| status.code().to_string()),
                    setup_path: (reason.is_some() && fixable_on_the_node).then(|| {
                        if *node_id == CONTROL_PLANE_NODE_ID {
                            NODES_SETUP_PATH.to_string()
                        } else {
                            format!("{NODES_SETUP_PATH}/{node_id}")
                        }
                    }),
                    reason,
                }
            })
            .collect())
    }

    /// Why recording cannot work now. A workload that runs nowhere yet is
    /// not blocked: it records wherever it starts, if that node can. One
    /// that runs on several nodes is blocked only when none of them can.
    fn blocker(&self, nodes: &[EgressRecordingNode]) -> Option<Blocker> {
        if self.store.is_none() {
            return Some(Blocker {
                code: "otel_unavailable",
                reason: "OpenTelemetry storage is not enabled on this control plane".to_string(),
                setup_path: None,
                node: None,
            });
        }
        if nodes.is_empty() || nodes.iter().any(|node| node.recording) {
            return None;
        }
        let node = nodes.first()?;
        let status = self.capabilities.status(node.node_id);
        Some(Blocker {
            code: status.code(),
            reason: node.reason.clone().unwrap_or_default(),
            setup_path: node.setup_path.clone(),
            node: Some((node.node_id, node.node_name.clone())),
        })
    }

    async fn status_of(
        &self,
        adapter: &dyn WorkloadEgressAdapter,
        workload: &RecordedWorkload,
    ) -> Result<EgressRecordingStatus, EgressRecordingError> {
        let logs_path = match workload.project_id {
            Some(project_id) => projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await
                .map_err(EgressRecordingError::db("looking up a workload's project"))?
                .filter(|project| project.deleted_at.is_none())
                .map(|project| {
                    logs_path(&project.slug, adapter.id_attribute(), &workload.public_id)
                }),
            None => None,
        };
        let nodes = self.nodes_of(workload).await?;
        let blocker = self.blocker(&nodes);
        Ok(EgressRecordingStatus {
            workload_kind: workload.workload.kind.as_str().to_string(),
            workload_id: workload.public_id.clone(),
            enabled: workload.enabled,
            configured: blocker.is_none(),
            reason_code: blocker.as_ref().map(|b| b.code.to_string()),
            reason: blocker.as_ref().map(|b| b.reason.clone()),
            setup_path: blocker.and_then(|b| b.setup_path),
            nodes,
            project_id: workload.project_id,
            logs_path,
        })
    }

    /// Whether `workload` records its connections, and whether it can. The
    /// caller has resolved it and checked the user may see it.
    pub async fn status(
        &self,
        workload: &RecordedWorkload,
    ) -> Result<EgressRecordingStatus, EgressRecordingError> {
        let adapter = self.adapter(workload.workload.kind)?;
        self.status_of(adapter.as_ref(), workload).await
    }

    /// Turns recording on or off for `workload`, which the caller has
    /// resolved and may change. Turning it off always succeeds; turning it
    /// on is refused when it could not work. Returns the value before and
    /// the status after, for the audit log.
    pub async fn set_enabled(
        &self,
        workload: &RecordedWorkload,
        enabled: bool,
    ) -> Result<(bool, EgressRecordingStatus), EgressRecordingError> {
        let adapter = self.adapter(workload.workload.kind)?;
        let previous = workload.enabled;
        if enabled {
            let nodes = self.nodes_of(workload).await?;
            if let Some(blocker) = self.blocker(&nodes) {
                return Err(match blocker.node {
                    Some((node_id, node_name)) => EgressRecordingError::Unavailable {
                        noun: adapter.noun(),
                        id: workload.public_id.clone(),
                        node_id,
                        node_name,
                        code: blocker.code,
                        reason: blocker.reason,
                    },
                    None => EgressRecordingError::OtelUnavailable {
                        noun: adapter.noun(),
                        id: workload.public_id.clone(),
                    },
                });
            }
        }
        let now = RecordedWorkload {
            enabled,
            ..workload.clone()
        };
        if previous != enabled {
            adapter.set_enabled(workload.workload, enabled).await?;
            adapter.changed(&now).await;
        }
        tracing::info!(
            workload = %workload.workload,
            id = %workload.public_id,
            nodes = ?workload.nodes,
            from = previous,
            to = enabled,
            "egress recording updated"
        );
        Ok((previous, self.status_of(adapter.as_ref(), &now).await?))
    }
}

/// The console path of a project's logs filtered to one workload's records.
pub fn logs_path(project_slug: &str, id_attribute: &str, public_id: &str) -> String {
    format!(
        "/projects/{project_slug}/telemetry-logs?attribute_key={id_attribute}&attribute_value={public_id}"
    )
}
