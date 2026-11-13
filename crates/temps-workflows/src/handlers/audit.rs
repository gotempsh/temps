// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit events for workflow writes and payload reveals.
//!
//! Records carry ids, names and statuses, never a payload, an event key or
//! a credential token. The actor is a user, an API key without one, or the
//! environment's workflow credential (`credential_id`). Journal writes by
//! the executor are not audited one by one: the journal is their record.

use serde::Serialize;
use temps_core::AuditOperation;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowAuditContext {
    pub user_id: Option<i32>,
    /// Set when the app's workflow credential acted.
    pub credential_id: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

macro_rules! impl_audit_operation {
    ($ty:ty, $op:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $op.to_string()
            }

            fn user_id(&self) -> Option<i32> {
                self.context.user_id
            }

            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }

            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }

            fn serialize(&self) -> temps_core::AuditResult<String> {
                temps_core::serialize_audit_operation(self)
            }
        }
    };
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunStartedAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub run_id: Uuid,
    pub workflow: String,
}

/// A cancel, retry, resume or delete.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunChangedAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub run_id: Uuid,
    pub workflow: String,
    pub previous_status: String,
    pub new_status: String,
    /// For a retry: where the journal was cut.
    pub from_sequence: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunDeletedAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub run_id: Uuid,
    pub workflow: String,
    pub status: String,
}

/// An operator read a decrypted payload.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowPayloadRevealedAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub run_id: Uuid,
    /// `input`, `output`, `error`, `step 3 result`, ...
    pub payload: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowEventSentAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub event_id: i64,
    pub event_name: String,
    pub delivered: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowSettingsUpdatedAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub old: crate::types::WorkflowSettings,
    pub new: crate::types::WorkflowSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowCredentialIssuedAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub credential_id: i32,
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowCredentialRevokedAudit {
    pub context: WorkflowAuditContext,
    pub project_id: i32,
    pub environment_id: i32,
}

impl_audit_operation!(WorkflowRunStartedAudit, "WORKFLOW_RUN_STARTED");
impl_audit_operation!(WorkflowRunChangedAudit, "WORKFLOW_RUN_CHANGED");
impl_audit_operation!(WorkflowRunDeletedAudit, "WORKFLOW_RUN_DELETED");
impl_audit_operation!(WorkflowPayloadRevealedAudit, "WORKFLOW_PAYLOAD_REVEALED");
impl_audit_operation!(WorkflowEventSentAudit, "WORKFLOW_EVENT_SENT");
impl_audit_operation!(WorkflowSettingsUpdatedAudit, "WORKFLOW_SETTINGS_UPDATED");
impl_audit_operation!(WorkflowCredentialIssuedAudit, "WORKFLOW_CREDENTIAL_ISSUED");
impl_audit_operation!(
    WorkflowCredentialRevokedAudit,
    "WORKFLOW_CREDENTIAL_REVOKED"
);
