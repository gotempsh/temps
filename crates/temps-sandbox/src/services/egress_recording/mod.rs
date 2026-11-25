// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording a sandbox's outbound connections (opt-in, Linux nodes).
//!
//! Telemetry an agent sends about itself is optional and can be omitted or
//! falsified by a prompt-injected agent. The per-node egress proxy sees
//! every outbound connection without the sandbox's cooperation, so when a
//! sandbox's owner turns recording on, the proxy records each connection
//! and this service stores the records as OpenTelemetry log records of the
//! sandbox's project, where the logs explorer and every OTel consumer find
//! them.
//!
//! - **Opt-in** ([`EgressRecordingService::set_enabled`]): a column on the
//!   sandbox's own row (`sandboxes.egress_recording`), off by default. It
//!   is refused, with a typed error naming the sandbox and the node, when
//!   the sandbox has no project to record into or its node cannot record
//!   (each node reports this about itself, see
//!   `temps_network_policy::recording`).
//! - **Ingest** ([`ingest`]): batches from a node, attributed by the proxy
//!   from the source address. The control plane trusts the node only for
//!   sandboxes placed on it, and re-checks every sandbox's opt-in, so a
//!   sandbox can neither forge nor suppress its records and a worker cannot
//!   write records for a sandbox it does not host.
//! - **Records** ([`records`]): one log record per connection, with OTel
//!   semantic-convention attributes, marked as platform-observed in a
//!   namespace a workload cannot write.

pub mod activity;
pub mod ingest;
pub mod local;
pub mod policy;
pub mod records;
pub mod sink;

#[cfg(test)]
pub(crate) mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde::Serialize;
use temps_entities::{nodes, projects, sandboxes};
use temps_network_policy::NodeRecordingStatus;
use temps_otel::types::{LogQuery, LogRecord};
use temps_otel::OtelError;
use utoipa::ToSchema;

use crate::services::public_id;

/// The node id the control plane uses for itself.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;

/// The attribute every record carries with the sandbox's public id, which
/// the console's logs link filters on.
pub const SANDBOX_ID_ATTRIBUTE: &str = "temps.sandbox.id";

/// Where the console fixes a node that cannot record.
pub const NODES_SETUP_PATH: &str = "/settings/nodes";

#[derive(Debug, thiserror::Error)]
pub enum EgressRecordingError {
    #[error("Sandbox {sandbox_id} not found")]
    SandboxNotFound { sandbox_id: String },

    #[error(
        "Sandbox {sandbox_id} belongs to no project, and recorded connections are stored as \
         OpenTelemetry logs of the sandbox's project. Create the sandbox from a project to \
         record its outbound connections."
    )]
    NoProject { sandbox_id: String },

    #[error(
        "Recording outbound connections is unavailable for sandbox {sandbox_id} on node \
         {node_id} ('{node_name}'): {reason}"
    )]
    Unavailable {
        sandbox_id: String,
        node_id: i32,
        node_name: String,
        code: &'static str,
        reason: String,
    },

    #[error(
        "Recording outbound connections is unavailable for sandbox {sandbox_id}: OpenTelemetry \
         storage is not enabled on this control plane"
    )]
    OtelUnavailable { sandbox_id: String },

    #[error(
        "node {node_id} sent {records} egress records in one batch; at most {max} are accepted"
    )]
    BatchTooLarge {
        node_id: i32,
        records: usize,
        max: usize,
    },

    #[error("node {node_id} sent an invalid egress record: {reason}")]
    InvalidRecord { node_id: i32, reason: String },

    #[error("node {node_id} sent egress records for sandbox {sandbox}, which is not placed on it")]
    NotPlacedOnNode { node_id: i32, sandbox: i64 },

    #[error("node {node_id} sent egress records for sandbox {sandbox}, which does not exist")]
    UnknownSandbox { node_id: i32, sandbox: i64 },

    #[error(
        "OpenTelemetry storage is not enabled on this control plane; {records} egress records \
         from node {node_id} cannot be stored"
    )]
    IngestUnavailable { node_id: i32, records: usize },

    #[error("storing {records} egress records from node {node_id} failed: {source}")]
    Store {
        node_id: i32,
        records: usize,
        #[source]
        source: OtelError,
    },

    #[error("reading the recorded connections of sandbox {sandbox_id} failed: {source}")]
    Query {
        sandbox_id: String,
        #[source]
        source: OtelError,
    },

    #[error("database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl EgressRecordingError {
    fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| Self::Database { operation, source }
    }
}

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

/// Whether a sandbox records its outbound connections, and whether it can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressRecordingStatus {
    /// The sandbox's public id.
    pub sandbox_id: String,
    /// The owner turned recording on.
    pub enabled: bool,
    /// Recording can work for this sandbox now: it has a project, OTel
    /// storage is enabled and its node reports that it can record. When
    /// `enabled` is true and this is false, nothing is being recorded.
    pub configured: bool,
    /// Why recording cannot work, when `configured` is false: one of
    /// `no_project`, `otel_unavailable`, `unsupported_platform`,
    /// `proxy_not_serving`, `node_not_reporting`, `agent_predates_recording`.
    pub reason_code: Option<String>,
    /// The same, in a sentence that names what is missing.
    pub reason: Option<String>,
    /// Console path that fixes it, when one does.
    pub setup_path: Option<String>,
    /// The sandbox's node; `None` is the control plane.
    pub node_id: Option<i32>,
    pub node_name: String,
    pub project_id: Option<i32>,
    /// Console path of the project's logs, filtered to this sandbox's
    /// records. `None` without a project.
    pub logs_path: Option<String>,
}

/// Why recording cannot work for a sandbox.
struct Blocker {
    code: &'static str,
    reason: String,
    setup_path: Option<String>,
}

/// What happens on this host when a sandbox's recording is turned on or
/// off ([`local::LocalHooks`] on a control plane that records).
#[async_trait]
pub trait EgressRecordingHooks: Send + Sync {
    async fn changed(&self, row: &sandboxes::Model);
}

pub struct EgressRecordingService {
    db: Arc<DatabaseConnection>,
    capabilities: Arc<dyn NodeRecordingCapabilities>,
    /// `None` when this control plane runs without OTel storage: recording
    /// then reports itself unavailable, saying so.
    store: Option<Arc<dyn EgressLogStore>>,
    /// Set once the local recorder runs; absent, a change is only stored.
    hooks: std::sync::OnceLock<Arc<dyn EgressRecordingHooks>>,
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
            hooks: std::sync::OnceLock::new(),
        }
    }

    /// Wires what a change does on this host. Set once.
    pub fn set_hooks(&self, hooks: Arc<dyn EgressRecordingHooks>) {
        let _ = self.hooks.set(hooks);
    }

    /// The caller's live sandbox `public_id_value`, or not found.
    async fn owned_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, EgressRecordingError> {
        let not_found = || EgressRecordingError::SandboxNotFound {
            sandbox_id: public_id_value.to_string(),
        };
        if !public_id::is_valid(public_id_value) {
            return Err(not_found());
        }
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id_value))
            .one(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db("looking up a sandbox"))?
            .ok_or_else(not_found)?;
        // Don't leak existence to non-owners; a destroyed sandbox is gone.
        if row.user_id != Some(user_id) || row.status == "destroyed" {
            return Err(not_found());
        }
        Ok(row)
    }

    async fn node_name(&self, node_id: i32) -> Result<String, EgressRecordingError> {
        if node_id == CONTROL_PLANE_NODE_ID {
            return Ok("control-plane".to_string());
        }
        Ok(nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db("looking up a sandbox's node"))?
            .map(|node| node.name)
            .unwrap_or_else(|| format!("node-{node_id}")))
    }

    fn blocker(&self, row: &sandboxes::Model) -> Option<Blocker> {
        if row.project_id.is_none() {
            return Some(Blocker {
                code: "no_project",
                reason: "the sandbox belongs to no project, and recorded connections are \
                         stored as OpenTelemetry logs of the sandbox's project; create the \
                         sandbox from a project to record its connections"
                    .to_string(),
                setup_path: None,
            });
        }
        if self.store.is_none() {
            return Some(Blocker {
                code: "otel_unavailable",
                reason: "OpenTelemetry storage is not enabled on this control plane".to_string(),
                setup_path: None,
            });
        }
        let status = self
            .capabilities
            .status(row.node_id.unwrap_or(CONTROL_PLANE_NODE_ID));
        let reason = status.describe()?;
        // The node's page fixes only an agent that is not reporting or is
        // too old; nothing in the console fixes a platform or a proxy that
        // is not serving, so those get no link.
        let fixable_on_the_node = matches!(
            status,
            NodeRecordingStatus::NotReporting { .. } | NodeRecordingStatus::AgentPredatesRecording
        );
        Some(Blocker {
            code: status.code(),
            reason,
            setup_path: fixable_on_the_node.then(|| match row.node_id {
                Some(node_id) => format!("{NODES_SETUP_PATH}/{node_id}"),
                None => NODES_SETUP_PATH.to_string(),
            }),
        })
    }

    async fn status_of(
        &self,
        row: &sandboxes::Model,
    ) -> Result<EgressRecordingStatus, EgressRecordingError> {
        let node_name = self
            .node_name(row.node_id.unwrap_or(CONTROL_PLANE_NODE_ID))
            .await?;
        let logs_path = match row.project_id {
            Some(project_id) => projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await
                .map_err(EgressRecordingError::db("looking up a sandbox's project"))?
                .filter(|project| project.deleted_at.is_none())
                .map(|project| logs_path(&project.slug, &row.public_id)),
            None => None,
        };
        let blocker = self.blocker(row);
        Ok(EgressRecordingStatus {
            sandbox_id: row.public_id.clone(),
            enabled: row.egress_recording,
            configured: blocker.is_none(),
            reason_code: blocker.as_ref().map(|b| b.code.to_string()),
            reason: blocker.as_ref().map(|b| b.reason.clone()),
            setup_path: blocker.and_then(|b| b.setup_path),
            node_id: row.node_id,
            node_name,
            project_id: row.project_id,
            logs_path,
        })
    }

    /// Whether the caller's sandbox records its connections, and whether it
    /// can.
    pub async fn status(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<EgressRecordingStatus, EgressRecordingError> {
        let row = self.owned_sandbox(public_id_value, user_id).await?;
        self.status_of(&row).await
    }

    /// Turns recording on or off for the caller's sandbox. Turning it off
    /// always succeeds; turning it on is refused when it could not work.
    /// Returns the status before and after, for the audit log.
    pub async fn set_enabled(
        &self,
        public_id_value: &str,
        user_id: i32,
        enabled: bool,
    ) -> Result<(bool, EgressRecordingStatus), EgressRecordingError> {
        let row = self.owned_sandbox(public_id_value, user_id).await?;
        let previous = row.egress_recording;
        if enabled {
            if row.project_id.is_none() {
                return Err(EgressRecordingError::NoProject {
                    sandbox_id: row.public_id.clone(),
                });
            }
            if self.store.is_none() {
                return Err(EgressRecordingError::OtelUnavailable {
                    sandbox_id: row.public_id.clone(),
                });
            }
            if let Some(blocker) = self.blocker(&row) {
                let node_id = row.node_id.unwrap_or(CONTROL_PLANE_NODE_ID);
                return Err(EgressRecordingError::Unavailable {
                    sandbox_id: row.public_id.clone(),
                    node_id,
                    node_name: self.node_name(node_id).await?,
                    code: blocker.code,
                    reason: blocker.reason,
                });
            }
        }
        let row = if previous == enabled {
            row
        } else {
            let changes = sandboxes::ActiveModel {
                id: Set(row.id),
                egress_recording: Set(enabled),
                ..Default::default()
            };
            changes
                .update(self.db.as_ref())
                .await
                .map_err(EgressRecordingError::db(
                    "updating a sandbox's egress recording",
                ))?
        };
        if previous != enabled {
            if let Some(hooks) = self.hooks.get() {
                hooks.changed(&row).await;
            }
        }
        tracing::info!(
            sandbox_id = %row.public_id,
            internal_id = row.id,
            node_id = row.node_id.unwrap_or(CONTROL_PLANE_NODE_ID),
            from = previous,
            to = enabled,
            "sandbox egress recording updated"
        );
        Ok((previous, self.status_of(&row).await?))
    }
}

/// The console path of a project's logs filtered to one sandbox's records.
pub fn logs_path(project_slug: &str, sandbox_public_id: &str) -> String {
    format!(
        "/projects/{project_slug}/telemetry-logs?attribute_key={SANDBOX_ID_ATTRIBUTE}&attribute_value={sandbox_public_id}"
    )
}
