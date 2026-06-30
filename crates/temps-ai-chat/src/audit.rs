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

macro_rules! impl_audit_operation {
    ($type:ty, $op:expr) => {
        impl AuditOperation for $type {
            fn operation_type(&self) -> String {
                $op.to_string()
            }
            fn user_id(&self) -> i32 {
                self.context.user_id
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
