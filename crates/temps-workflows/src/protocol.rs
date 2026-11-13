// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The executor protocol, version 1 (ADR-103 §3, §12).
//!
//! The normative description is `docs/reference/workflows-executor-protocol.md`;
//! these types are its wire format. Temps sends [`InvokeRequest`] to the SDK's
//! `POST /.well-known/temps/workflows/invoke` and reads [`Manifest`] from its
//! `GET /.well-known/temps/workflows/manifest`, both signed with an
//! invocation token. The SDK calls back with [`StepWriteRequest`],
//! [`HeartbeatRequest`] and [`OutcomeRequest`], authenticated with the same
//! token. Fields are additive within a version: an SDK ignores unknown fields,
//! and a breaking change takes `protocol_version` 2.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::types::{StepKind, StepStatus};

pub const PROTOCOL_VERSION: u32 = 1;
/// Header carrying the protocol version on every request either way.
pub const PROTOCOL_HEADER: &str = "temps-workflows-protocol";
pub const INVOKE_PATH: &str = "/.well-known/temps/workflows/invoke";
pub const MANIFEST_PATH: &str = "/.well-known/temps/workflows/manifest";

/// One recorded journal entry as the executor replays it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowJournalEntry)]
pub struct JournalEntry {
    pub sequence: i32,
    pub name: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub attempts: i32,
    /// The recorded result (`completed`), decrypted for the executor.
    #[schema(value_type = Object)]
    pub result: Option<Value>,
    /// The recorded error message (`failed`, or the last failed attempt).
    pub error: Option<String>,
    /// A `sleep`'s wake time or a `wait`'s timeout.
    pub wake_at: Option<DateTime<Utc>>,
}

/// `POST /.well-known/temps/workflows/invoke` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InvokeRequest {
    pub protocol_version: u32,
    pub run_id: Uuid,
    pub workflow: String,
    /// 1 on the first invocation.
    pub attempt: i32,
    /// The fencing token this invocation holds.
    pub lease: i64,
    /// Temps' clock, so replays never depend on the replica's clock.
    pub now: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    #[schema(value_type = Object)]
    pub input: Option<Value>,
    /// The journal, in sequence order, from 0.
    pub journal: Vec<JournalEntry>,
    /// Set when the journal did not fit: fetch the rest with
    /// `GET /workflow-runs/{run_id}/journal?from=<this>`.
    pub journal_next_sequence: Option<i32>,
}

/// One workflow the executor serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as=WorkflowManifestEntry)]
pub struct ManifestWorkflow {
    pub name: String,
    /// Optional label of the workflow's code version.
    #[serde(default)]
    pub version: Option<String>,
    /// Runs of this workflow allowed to execute at once in the environment.
    #[serde(default)]
    pub concurrency: Option<u32>,
}

/// `GET /.well-known/temps/workflows/manifest` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as=WorkflowManifest)]
pub struct Manifest {
    pub protocol_version: u32,
    #[serde(default)]
    pub sdk: Option<String>,
    pub workflows: Vec<ManifestWorkflow>,
}

/// `POST /workflow-runs/{run_id}/steps` body: record one journal entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowStepWriteRequest)]
pub struct StepWriteRequest {
    pub sequence: i32,
    pub name: String,
    pub kind: StepKind,
    pub status: StepStatus,
    /// Attempts made so far, this one included.
    pub attempts: i32,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub wake_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// OTEL trace id of the attempt's span (metadata only).
    #[serde(default)]
    pub trace_id: Option<String>,
    /// Error-tracking event id of the attempt's exception.
    #[serde(default)]
    pub error_ref: Option<String>,
    /// For `send_event`: the event to deliver, recorded exactly once.
    #[serde(default)]
    pub event: Option<EmittedEvent>,
    /// For `child`: the run to start, started exactly once.
    #[serde(default)]
    pub child: Option<ChildStart>,
}

/// A child run a `child` entry starts. Temps records the entry `pending`
/// with `{"run_id": ...}` and completes it with the child's
/// [`child_result`] when the child finishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowChildStart)]
pub struct ChildStart {
    /// The child's workflow, in the same environment.
    pub workflow: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub input: Option<Value>,
}

/// The event name a run suspended on a child waits for, with the child's run
/// id as the key. Names starting with `temps.` are reserved: no API or
/// workflow can send them.
pub const CHILD_EVENT: &str = "temps.child";
/// The prefix of event names only Temps itself uses.
pub const RESERVED_EVENT_PREFIX: &str = "temps.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowEmittedEvent)]
pub struct EmittedEvent {
    pub name: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: Option<Value>,
}

/// The entry as Temps holds it after the write. When `already_recorded` is
/// true, the executor must continue with this recorded value, not its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowStepWriteResponse)]
pub struct StepWriteResponse {
    pub entry: JournalEntry,
    pub already_recorded: bool,
    pub lease_expires_at: DateTime<Utc>,
}

/// `GET /workflow-runs/{run_id}/journal` response page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowJournalPage)]
pub struct JournalPage {
    pub entries: Vec<JournalEntry>,
    pub next_sequence: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowHeartbeatRequest)]
pub struct HeartbeatRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowHeartbeatResponse)]
pub struct HeartbeatResponse {
    pub lease_expires_at: DateTime<Utc>,
    /// True once an operator cancelled the run: stop at the next step.
    pub cancelled: bool,
}

/// How an invocation ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schema(as = WorkflowOutcome)]
pub enum Outcome {
    Completed {
        #[serde(default)]
        #[schema(value_type = Object)]
        output: Option<Value>,
    },
    Failed {
        error: String,
        /// The journal entry that failed, when a step did.
        #[serde(default)]
        sequence: Option<i32>,
    },
    /// Suspended in `sleep`/`sleepUntil` until `until`.
    Sleep { sequence: i32, until: DateTime<Utc> },
    /// Suspended in `waitForEvent`.
    Wait {
        sequence: i32,
        event: String,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        timeout_at: Option<DateTime<Utc>>,
    },
    /// A step attempt failed and is retried at `at`.
    Retry { sequence: i32, at: DateTime<Utc> },
    /// Replay found a different step than the journal recorded.
    Incompatible {
        sequence: i32,
        expected: String,
        found: String,
    },
}

/// `POST /workflow-runs/{run_id}/outcome` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowOutcomeRequest)]
pub struct OutcomeRequest {
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowOutcomeResponse)]
pub struct OutcomeResponse {
    pub status: crate::types::RunStatus,
}

/// The result shape Temps records for a `wait` entry.
pub fn wait_result(payload: Option<Value>) -> Value {
    match payload {
        Some(payload) => serde_json::json!({ "received": true, "payload": payload }),
        None => serde_json::json!({ "received": true, "payload": null }),
    }
}

/// The result Temps records on a `child` entry when the child finishes:
/// `status` is `completed`, `failed` or `cancelled`; `output` is the
/// child's output when it completed, `error` its error when it did not.
pub fn child_result(
    run_id: Uuid,
    status: &str,
    output: Option<Value>,
    error: Option<String>,
) -> Value {
    serde_json::json!({
        "run_id": run_id,
        "status": status,
        "output": output,
        "error": error,
    })
}

/// The instance's invocation signing key, as SDKs receive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationKey {
    /// `ed25519`.
    pub algorithm: String,
    /// Base64url (no padding) of the 32-byte public key.
    pub public_key: String,
    pub key_id: String,
}

impl InvocationKey {
    pub fn of(signer: &crate::token::InvocationSigner) -> Self {
        Self {
            algorithm: "ed25519".into(),
            public_key: signer.public_key(),
            key_id: signer.key_id(),
        }
    }
}

/// `GET /workflows/v1/invocation-key` response, read with the environment's
/// workflow credential. The SDK accepts only invocation tokens for this
/// project and environment: the key alone is instance-wide, so a token
/// another environment's executor received is still validly signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowInvocationKeyResponse)]
pub struct InvocationKeyResponse {
    /// `ed25519`.
    pub algorithm: String,
    /// Base64url (no padding) of the 32-byte public key.
    pub public_key: String,
    pub key_id: String,
    /// The project the credential belongs to; tokens must carry it as `pid`.
    pub project_id: i32,
    /// The environment the credential belongs to; tokens must carry it as `eid`.
    pub environment_id: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_use_a_type_tag() {
        let wait: Outcome = serde_json::from_str(
            r#"{"type":"wait","sequence":3,"event":"approval","key":"order-1"}"#,
        )
        .unwrap();
        assert_eq!(
            wait,
            Outcome::Wait {
                sequence: 3,
                event: "approval".into(),
                key: Some("order-1".into()),
                timeout_at: None
            }
        );
        let completed = serde_json::to_value(Outcome::Completed {
            output: Some(serde_json::json!(1)),
        })
        .unwrap();
        assert_eq!(completed["type"], "completed");
    }

    #[test]
    fn step_writes_tolerate_omitted_optional_fields() {
        let write: StepWriteRequest = serde_json::from_str(
            r#"{"sequence":0,"name":"charge","kind":"step","status":"completed","attempts":1}"#,
        )
        .unwrap();
        assert!(write.result.is_none() && write.event.is_none());
    }
}
