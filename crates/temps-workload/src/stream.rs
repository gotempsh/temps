// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-node desired-state stream's wire types (ADR-055 §7).
//!
//! `GET /internal/nodes/{id}/state?since=W` answers with a
//! [`StateResponse`]: level-triggered `upsert`/`remove` changes for every
//! object marked for the node since its cursor, or the node's whole view
//! (`full`). Nothing here carries a secret value: a workload spec holds
//! references and imports, and an [`InstanceTemplate`] is an
//! [`InstanceSpec`] without its environment. A node fetches the
//! environment for one instance, at create time, from a separate call.
//!
//! Status flows back on the heartbeat as [`InstanceReport`]s.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    Attachments, Condition, InstanceHandle, InstanceLimits, InstanceSpec, LogRotation, PortBinding,
    RestartPolicy, SecurityProfile, WorkloadRef, WorkloadSpec,
};

/// The heartbeat capability a worker advertises when it consumes the
/// stream. A worker that does not advertise it is driven through the
/// `/agent/*` endpoints, as before.
pub const STATE_STREAM_CAPABILITY: &str = "state_stream";

/// The driver name of instances a worker's node runtime creates from its
/// stream. Any other instance in a node's view (one created through the
/// `/agent/*` API) is the control plane's to drive; the node only keeps
/// it from being swept as an orphan.
pub const STREAM_DRIVER: &str = "node-stream";

/// The kinds a marker can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Workload,
    Instance,
    DnsClaim,
}

impl ObjectKind {
    pub const ALL: [ObjectKind; 3] = [
        ObjectKind::Workload,
        ObjectKind::Instance,
        ObjectKind::DnsClaim,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Workload => "workload",
            ObjectKind::Instance => "instance",
            ObjectKind::DnsClaim => "dns_claim",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "workload" => Some(ObjectKind::Workload),
            "instance" => Some(ObjectKind::Instance),
            "dns_claim" => Some(ObjectKind::DnsClaim),
            _ => None,
        }
    }
}

/// Why the server answered with the node's whole view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullReason {
    /// `since = 0`: a first connection or a wiped node.
    Initial,
    /// `since` is below the retention horizon: the markers it needs are
    /// gone.
    BelowHorizon,
    /// `since` is above the database's `xmax`: the database was restored
    /// and transaction ids restarted.
    AheadOfDatabase,
    /// One transaction marked more objects than a delta carries.
    Oversized,
}

/// One answer of the stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateResponse {
    pub node_id: i32,
    /// The visibility watermark `W` (an `xid8`, in decimal). The node's
    /// next `since`.
    pub watermark: String,
    /// `Some`: `changes` is the node's whole view; everything the node
    /// holds that is not in it is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full: Option<FullReason>,
    pub changes: Vec<StateChange>,
    /// More markers are visible than one answer carries: ask again now.
    #[serde(default)]
    pub more: bool,
}

/// What to do with one object.
///
/// Upserts are the common case and removes the rare one, so boxing the
/// object would add an allocation per change to save space only on the
/// rare variant; one answer holds a bounded page of changes.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StateChange {
    Upsert { object: StateObject },
    Remove { kind: ObjectKind, id: i64 },
}

impl StateChange {
    pub fn kind(&self) -> ObjectKind {
        match self {
            StateChange::Upsert { object } => object.kind(),
            StateChange::Remove { kind, .. } => *kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateObject {
    Workload(WorkloadObject),
    Instance(InstanceObject),
    DnsClaim(DnsClaimObject),
}

impl StateObject {
    pub fn kind(&self) -> ObjectKind {
        match self {
            StateObject::Workload(_) => ObjectKind::Workload,
            StateObject::Instance(_) => ObjectKind::Instance,
            StateObject::DnsClaim(_) => ObjectKind::DnsClaim,
        }
    }

    pub fn id(&self) -> i64 {
        match self {
            StateObject::Workload(w) => w.id,
            StateObject::Instance(i) => i.id,
            StateObject::DnsClaim(c) => c.id,
        }
    }
}

/// A workload as a node sees it. The spec holds references only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadObject {
    pub id: i64,
    pub name: String,
    pub generation: i64,
    /// `pending | running | succeeded | failed | terminating`.
    pub phase: String,
    pub deleted: bool,
    pub spec: WorkloadSpec,
}

/// One instance assigned to the node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceObject {
    pub id: i64,
    pub workload_id: i64,
    pub attempt: i32,
    pub revision: i64,
    pub node_id: i32,
    /// The phase the control plane recorded. `pending`/`running`: the node
    /// keeps it running; `succeeded`/`failed`: the node stops it and keeps
    /// it (for its logs) until the instance is removed.
    pub phase: String,
    pub handle: InstanceHandle,
    pub template: InstanceTemplate,
}

/// A DNS name claim. Every node gets every claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsClaimObject {
    /// `hashtextextended(fqdn, 0)`: the marker's object id.
    pub id: i64,
    pub fqdn: String,
    pub owner_kind: String,
    pub owner_id: i64,
    pub target_workload_id: Option<i64>,
}

/// An [`InstanceSpec`] without its environment: what can be stored and
/// streamed. [`InstanceTemplate::into_spec`] adds the environment a node
/// fetched for the create call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceTemplate {
    pub owner: WorkloadRef,
    pub name: String,
    /// The artifact to prepare, then run.
    pub artifact: crate::Artifact,
    pub platform: Option<String>,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub workdir: Option<String>,
    pub user: Option<String>,
    pub ports: Vec<PortBinding>,
    pub restart: RestartPolicy,
    pub limits: InstanceLimits,
    pub log_rotation: Option<LogRotation>,
    pub labels: BTreeMap<String, String>,
    pub security: SecurityProfile,
    pub attachments: Attachments,
    /// A long-running instance (a deployment replica): a stop is reported
    /// as `Running = False / Stopped`, not as a finished attempt, and a
    /// container removed after it ran is never recreated. Absent from
    /// older control planes, which only sent jobs.
    #[serde(default)]
    pub long_running: bool,
}

impl InstanceTemplate {
    /// The create call's spec: this template with `image` (the prepared
    /// local reference) and `env`.
    pub fn into_spec(self, image: String, env: Vec<(String, String)>) -> InstanceSpec {
        InstanceSpec {
            owner: self.owner,
            name: self.name,
            image,
            platform: self.platform,
            env,
            secret_files: Vec::new(),
            command: self.command,
            entrypoint: self.entrypoint,
            workdir: self.workdir,
            user: self.user,
            ports: self.ports,
            restart: self.restart,
            limits: self.limits,
            log_rotation: self.log_rotation,
            labels: self.labels,
            security: self.security,
        }
    }
}

/// `POST /internal/nodes/{id}/state/ack`: the cursor the node applied,
/// per kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateAck {
    pub acks: BTreeMap<ObjectKind, String>,
}

/// `GET /internal/nodes/{id}/instances/{instance_id}/env`. Sent only over
/// the authenticated node connection, never stored in the snapshot and
/// never logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceEnv {
    pub instance_id: i64,
    pub env: Vec<(String, String)>,
    /// Delivered at `/run/secrets/<name>`. Absent from older control
    /// planes, which never sent any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_files: Vec<crate::SecretFile>,
}

impl std::fmt::Debug for InstanceEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceEnv")
            .field("instance_id", &self.instance_id)
            .field("variables", &self.env.len())
            .field("secret_files", &self.secret_files.len())
            .finish()
    }
}

/// What a node observed of one instance. Rides on the heartbeat, only
/// when it changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceReport {
    pub instance_id: i64,
    /// `pending | running | succeeded | failed`.
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// The node's conditions: `Running = False / SecretsUnavailable`,
    /// `ArtifactReady = False / ArtifactPullFailed`.
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// The host ports the instance actually got. Absent from older nodes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub published_ports: Vec<crate::PortBinding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_env_debug_output_never_shows_values() {
        let env = InstanceEnv {
            instance_id: 7,
            env: vec![("API_TOKEN".into(), "tok-very-secret".into())],
            secret_files: vec![crate::SecretFile {
                name: "DB_PASSWORD".into(),
                value: "file-very-secret".into(),
            }],
        };
        let shown = format!("{env:?}");
        assert!(!shown.contains("tok-very-secret"), "{shown}");
        assert!(!shown.contains("file-very-secret"), "{shown}");
        assert!(shown.contains("variables: 1"), "{shown}");
    }

    #[test]
    fn changes_round_trip_with_their_kind_tag() {
        let change = StateChange::Remove {
            kind: ObjectKind::Instance,
            id: 4,
        };
        let json = serde_json::to_string(&change).expect("encoded");
        assert_eq!(json, r#"{"op":"remove","kind":"instance","id":4}"#);
        let back: StateChange = serde_json::from_str(&json).expect("decoded");
        assert_eq!(back, change);
        assert_eq!(ObjectKind::parse("dns_claim"), Some(ObjectKind::DnsClaim));
    }
}
