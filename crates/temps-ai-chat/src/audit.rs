// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit events for AI chat write operations.
//!
//! Per the project audit convention, every mutating handler records an audit
//! entry. These cover creating a conversation, sending a message (which runs an
//! AI turn and incurs token cost), and archiving a conversation — so there's a
//! trail of who started/drove/closed an AI chat. Emission is best-effort: a
//! failed audit log never fails the underlying request.

use anyhow::Result;
use serde::Serialize;

use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct ConversationCreatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    /// Public id of the created conversation.
    pub conversation_id: String,
    pub context_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessageSentAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationArchivedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationRenamedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub conversation_id: String,
    /// New human-facing title after the rename.
    pub title: String,
}

macro_rules! impl_audit_operation {
    ($type:ty, $op:expr) => {
        impl AuditOperation for $type {
            fn operation_type(&self) -> String {
                $op.to_string()
            }
            fn user_id(&self) -> Option<i32> {
                Some(self.context.user_id)
            }
            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }
            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }
            fn serialize(&self) -> Result<String> {
                serde_json::to_string(self)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
            }
        }
    };
}

impl_audit_operation!(ConversationCreatedAudit, "AI_CHAT_CONVERSATION_CREATED");
impl_audit_operation!(ChatMessageSentAudit, "AI_CHAT_MESSAGE_SENT");
impl_audit_operation!(ConversationArchivedAudit, "AI_CHAT_CONVERSATION_ARCHIVED");
impl_audit_operation!(ConversationRenamedAudit, "AI_CHAT_CONVERSATION_RENAMED");

// --- Pending-action audit events -------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AiActionConfirmedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    /// Public id of the confirmed action.
    pub action_id: String,
    pub operation_id: String,
    /// Final status (`"executed"` or `"failed"`).
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiActionRejectedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub action_id: String,
    pub operation_id: String,
}

impl_audit_operation!(AiActionConfirmedAudit, "ai.pending_action.confirmed");
impl_audit_operation!(AiActionRejectedAudit, "ai.pending_action.rejected");

// --- Interactive permission-bridge audit events ----------------------------

/// Audit entry for resolving an interactive permission request (ADR-038 Phase 2).
///
/// Only the permission kind and id are recorded — never the raw tool input or
/// answer content, which may carry sensitive data (credentials, PII, code).
#[derive(Debug, Clone, Serialize)]
pub struct PermissionResolvedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub conversation_id: String,
    /// The CLI's `request_id` (same value used as the registry key and in the
    /// resolve endpoint URL as `{permission_id}`).
    pub permission_id: String,
    /// Decision kind: `"allow_tool"`, `"deny_tool"`, `"answer_question"`,
    /// `"approve_plan"`, or `"reject_plan"`.
    pub decision_kind: String,
}

impl_audit_operation!(PermissionResolvedAudit, "ai.permission.resolved");
