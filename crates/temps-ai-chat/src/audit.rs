// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit events for AI chat write operations.
//!
//! Per the project audit convention, every mutating handler records an audit
//! entry. These cover creating a conversation, sending a message (which runs an
//! AI turn and incurs token cost), and archiving a conversation — so there's a
//! trail of who started/drove/closed an AI chat. Emission is best-effort: a
//! failed audit log never fails the underlying request.

use serde::Serialize;

use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct ConversationCreatedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    /// Public id of the created conversation.
    pub conversation_id: String,
    pub context_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessageSentAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationArchivedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationRestoredAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationRenamedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    /// New human-facing title after the rename.
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationPermissionModeChangedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub permission_mode: String,
    pub applied_to_active_turn: bool,
}

/// A watched goal failed and the assistant started a fix turn on the owner's
/// behalf. Recorded because it is the one case where the assistant acts while
/// the user is not looking.
#[derive(Debug, Clone, Serialize)]
pub struct AiGoalHealStartedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub goal_id: String,
    pub attempt: i32,
}

/// A deployment goal was reached and the watcher started one turn in the chat
/// to check the live app, on the owner's behalf, like a fix turn.
#[derive(Debug, Clone, Serialize)]
pub struct AiGoalLiveCheckStartedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub goal_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiGoalCancelledAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub goal_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiGoalRetriedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub goal_id: String,
}

/// The user gave the assistant a credential through a sealed form. Records who
/// and for which operation and field; the value is never part of an audit row.
#[derive(Debug, Clone, Serialize)]
pub struct AiSecretProvidedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub secret_id: String,
    pub operation_id: String,
    pub field: String,
}

/// The user discarded a credential they had provided (or declined to).
#[derive(Debug, Clone, Serialize)]
pub struct AiSecretDiscardedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub secret_id: String,
    pub operation_id: String,
    pub field: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiSandboxLinkedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub sandbox_id: String,
    /// True when the sandbox was created for the chat rather than picked.
    pub created: bool,
}

/// A chat's sleeping sandbox was started again, by the console or the assistant.
#[derive(Debug, Clone, Serialize)]
pub struct AiSandboxWokenAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub sandbox_id: String,
}

/// A folder of a chat's sandbox was put under Git from the Changes tab, so its
/// changes can be shown: a first commit of what it held.
#[derive(Debug, Clone, Serialize)]
pub struct AiSandboxChangesTrackedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub sandbox_id: String,
    pub path: String,
}

/// A chat's older messages were folded into its running summary. Records how
/// many, never what they said.
#[derive(Debug, Clone, Serialize)]
pub struct AiConversationCompactedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub folded_messages: usize,
    pub kept_messages: usize,
    pub summary_model: String,
}

/// The user's default for next-step suggestions changed.
#[derive(Debug, Clone, Serialize)]
pub struct AiSuggestionDefaultUpdatedAudit {
    pub context: AuditContext,
    pub mode: String,
}

/// A chat's own next-step suggestion mode was set, or cleared to follow the
/// user's default (`mode: None`).
#[derive(Debug, Clone, Serialize)]
pub struct AiSuggestionChatModeUpdatedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub mode: Option<String>,
}

/// Next steps were suggested after an answer: a new model request was made
/// (its cost counts toward the chat's spend) and the set saved on the answer.
/// Records how many, never their text. Returning an already saved set writes
/// nothing, so it is not audited.
#[derive(Debug, Clone, Serialize)]
pub struct AiNextStepsSuggestedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    /// The answer the suggestions were saved on.
    pub message_id: Option<i64>,
    pub suggestion_count: usize,
    /// The model that wrote them.
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiSandboxUnlinkedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub sandbox_id: Option<String>,
}

/// A chat's sandbox was connected to a project's services (ADR-051). Names the
/// variables the sandbox's commands run with, never their values.
#[derive(Debug, Clone, Serialize)]
pub struct AiSandboxServicesConnectedAudit {
    pub context: AuditContext,
    /// The project the services were connected through, if any.
    pub project_id: Option<i32>,
    pub conversation_id: String,
    pub sandbox_id: String,
    pub connected_service_ids: Vec<i32>,
    pub variable_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiSandboxServicesDisconnectedAudit {
    pub context: AuditContext,
    /// The project whose services were disconnected, when the whole project was.
    pub project_id: Option<i32>,
    /// The single service disconnected, when it was disconnected on its own.
    pub service_id: Option<i32>,
    pub conversation_id: String,
    pub sandbox_id: String,
}

/// The files of a chat's sandbox were deployed as a project's source, with no
/// Git repository. Records who approved it and how much was uploaded, never
/// file names or contents.
#[derive(Debug, Clone, Serialize)]
pub struct AiSandboxSourceDeployedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    /// The project the chat belongs to, when it is a project chat.
    pub chat_project_id: Option<i32>,
    pub conversation_id: String,
    pub sandbox_id: String,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub files: usize,
    pub source_bytes: u64,
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
            fn serialize(&self) -> temps_core::AuditResult<String> {
                temps_core::serialize_audit_operation(self)
            }
        }
    };
}

impl_audit_operation!(ConversationCreatedAudit, "AI_CHAT_CONVERSATION_CREATED");
impl_audit_operation!(ChatMessageSentAudit, "AI_CHAT_MESSAGE_SENT");
impl_audit_operation!(ConversationArchivedAudit, "AI_CHAT_CONVERSATION_ARCHIVED");
impl_audit_operation!(ConversationRestoredAudit, "AI_CHAT_CONVERSATION_RESTORED");
impl_audit_operation!(ConversationRenamedAudit, "AI_CHAT_CONVERSATION_RENAMED");
impl_audit_operation!(
    ConversationPermissionModeChangedAudit,
    "AI_CHAT_PERMISSION_MODE_CHANGED"
);

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationCreatedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub project_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationArchivedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub project_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationRenamedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub previous_name: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationRestoredAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub project_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadArtifactCreatedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub conversation_id: String,
    pub artifact_id: String,
    pub kind: String,
}

impl_audit_operation!(ApplicationCreatedAudit, "ai.application.created");
impl_audit_operation!(ApplicationArchivedAudit, "ai.application.archived");
impl_audit_operation!(ApplicationRenamedAudit, "ai.application.renamed");
impl_audit_operation!(ApplicationRestoredAudit, "ai.application.restored");
impl_audit_operation!(ThreadArtifactCreatedAudit, "ai.thread_artifact.created");

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationTopologyChangedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub action: String,
    pub project_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationWorkspaceChangedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub action: String,
    pub sandbox_id: Option<String>,
    pub runtime: Option<String>,
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<i64>,
    pub pids_limit: Option<i64>,
    pub disk_limit_mb: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationWorkspaceDeployedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
}

/// An interactive terminal was attached to a persistent AI workspace sandbox.
///
/// A terminal is arbitrary command execution inside the workspace, so the
/// attach itself is the auditable event; what is typed is never recorded.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceTerminalAttachedAudit {
    pub context: AuditContext,
    /// `application` for an application workspace, `global` for the user's
    /// default workspace.
    pub scope: String,
    /// Application public id, or the global workspace context id.
    pub workspace_id: String,
    pub sandbox_id: String,
    /// Public id of the terminal thread that owns the shell.
    pub thread_id: String,
    /// PTY tab the thread owns.
    pub tab: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationPreviewLinkCreatedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub sandbox_id: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalWorkspacePreviewLinkCreatedAudit {
    pub context: AuditContext,
    pub workspace_id: String,
    pub sandbox_id: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationWorkspaceSourceImportedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub project_id: i32,
    pub used_git_connection: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationWorkspaceFilesWrittenAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub project_id: Option<i32>,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalWorkspaceFilesWrittenAudit {
    pub context: AuditContext,
    pub workspace_id: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationAttachmentUploadedAudit {
    pub context: AuditContext,
    pub conversation_id: String,
    pub attachment_id: String,
    pub size_bytes: u64,
}

impl_audit_operation!(
    ApplicationTopologyChangedAudit,
    "ai.application.topology_changed"
);
impl_audit_operation!(
    ApplicationWorkspaceChangedAudit,
    "ai.application.workspace_changed"
);
impl_audit_operation!(
    ApplicationWorkspaceDeployedAudit,
    "ai.application.workspace_deployed"
);
impl_audit_operation!(
    WorkspaceTerminalAttachedAudit,
    "ai.workspace.terminal_attached"
);
impl_audit_operation!(
    ApplicationPreviewLinkCreatedAudit,
    "ai.application.preview_link.created"
);
impl_audit_operation!(
    GlobalWorkspacePreviewLinkCreatedAudit,
    "ai.workspace.preview_link.created"
);
impl_audit_operation!(
    ApplicationWorkspaceSourceImportedAudit,
    "ai.application.workspace_source.imported"
);
impl_audit_operation!(
    ApplicationWorkspaceFilesWrittenAudit,
    "ai.application.workspace_files.written"
);
impl_audit_operation!(
    GlobalWorkspaceFilesWrittenAudit,
    "ai.workspace.files.written"
);
impl_audit_operation!(
    ConversationAttachmentUploadedAudit,
    "ai.conversation.attachment.uploaded"
);

// --- Pending-action audit events -------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AiActionConfirmedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    /// Public id of the confirmed action.
    pub action_id: String,
    pub operation_id: String,
    /// Final status (`"executed"` or `"failed"`).
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiActionRejectedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub action_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiActionTransitionFailedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub action_id: String,
    pub operation_id: String,
    pub attempted_transition: String,
    /// Sanitized diagnostic only; raw upstream bodies and credentials are
    /// never written to the audit stream.
    pub error: String,
}

impl_audit_operation!(AiGoalHealStartedAudit, "ai.goal.heal_started");
impl_audit_operation!(AiGoalLiveCheckStartedAudit, "ai.goal.live_check_started");
impl_audit_operation!(AiGoalCancelledAudit, "ai.goal.cancelled");
impl_audit_operation!(AiGoalRetriedAudit, "ai.goal.retried");
impl_audit_operation!(AiConversationCompactedAudit, "ai.conversation.compacted");
impl_audit_operation!(
    AiSuggestionDefaultUpdatedAudit,
    "ai.suggestions.default_updated"
);
impl_audit_operation!(
    AiSuggestionChatModeUpdatedAudit,
    "ai.suggestions.chat_mode_updated"
);
impl_audit_operation!(AiNextStepsSuggestedAudit, "ai.suggestions.generated");
impl_audit_operation!(AiSecretProvidedAudit, "ai.secret.provided");
impl_audit_operation!(AiSecretDiscardedAudit, "ai.secret.discarded");
impl_audit_operation!(AiSandboxLinkedAudit, "ai.sandbox.linked");
impl_audit_operation!(AiSandboxUnlinkedAudit, "ai.sandbox.unlinked");
impl_audit_operation!(AiSandboxWokenAudit, "ai.sandbox.woken");
impl_audit_operation!(AiSandboxChangesTrackedAudit, "ai.sandbox.changes_tracked");
impl_audit_operation!(
    AiSandboxServicesConnectedAudit,
    "ai.sandbox.services_connected"
);
impl_audit_operation!(
    AiSandboxServicesDisconnectedAudit,
    "ai.sandbox.services_disconnected"
);
impl_audit_operation!(AiSandboxSourceDeployedAudit, "ai.sandbox.source_deployed");
impl_audit_operation!(AiActionConfirmedAudit, "ai.pending_action.confirmed");
impl_audit_operation!(AiActionRejectedAudit, "ai.pending_action.rejected");
impl_audit_operation!(
    AiActionTransitionFailedAudit,
    "ai.pending_action.transition_failed"
);

// --- Interactive permission-bridge audit events ----------------------------

/// Audit entry for resolving an interactive permission request (ADR-038 Phase 2).
///
/// Only the permission kind and id are recorded — never the raw tool input or
/// answer content, which may carry sensitive data (credentials, PII, code).
#[derive(Debug, Clone, Serialize)]
pub struct PermissionResolvedAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub conversation_id: String,
    /// The CLI's `request_id` (same value used as the registry key and in the
    /// resolve endpoint URL as `{permission_id}`).
    pub permission_id: String,
    /// Sanitized provider tool name. Inputs are never logged.
    pub tool_name: Option<String>,
    /// Decision kind: `"allow_tool"`, `"deny_tool"`, `"answer_question"`,
    /// `"approve_plan"`, or `"reject_plan"`.
    pub decision_kind: String,
}

impl_audit_operation!(PermissionResolvedAudit, "ai.permission.resolved");

// --- Workspace chats and the architecture blueprint (ADR-053) ---------------

/// A chat joined a workspace, which may have been created for it.
#[derive(Debug, Clone, Serialize)]
pub struct AiChatWorkspaceJoinedAudit {
    pub context: AuditContext,
    pub conversation_id: String,
    /// Public id of the workspace (`ai_applications.public_id`).
    pub application_id: String,
    /// True when the workspace was created by this request.
    pub created: bool,
}

/// A chat left its workspace.
#[derive(Debug, Clone, Serialize)]
pub struct AiChatWorkspaceLeftAudit {
    pub context: AuditContext,
    pub conversation_id: String,
    pub application_id: String,
}

/// A blueprint version was written: proposed by the assistant, or saved by
/// the user (and accepted as it was saved).
#[derive(Debug, Clone, Serialize)]
pub struct AiBlueprintVersionCreatedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub version: i32,
    /// `proposed` or `accepted`.
    pub status: String,
    /// The chat that proposed it, when the assistant did.
    pub conversation_id: Option<String>,
    pub change_summary: String,
}

/// The user accepted or rejected a proposed blueprint version.
#[derive(Debug, Clone, Serialize)]
pub struct AiBlueprintVersionDecidedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub version: i32,
    /// `accepted` or `rejected`.
    pub decision: String,
}

/// A part of the accepted blueprint was bound to (or unbound from) a project
/// or service.
#[derive(Debug, Clone, Serialize)]
pub struct AiBlueprintBindingChangedAudit {
    pub context: AuditContext,
    pub application_id: String,
    pub component_id: String,
    /// `None` when the binding was removed.
    pub project_id: Option<i32>,
    pub service_id: Option<i32>,
    /// Who made the change: `user` or `assistant`.
    pub actor: String,
}

impl_audit_operation!(AiChatWorkspaceJoinedAudit, "ai.chat.workspace_joined");
impl_audit_operation!(AiChatWorkspaceLeftAudit, "ai.chat.workspace_left");
impl_audit_operation!(
    AiBlueprintVersionCreatedAudit,
    "ai.blueprint.version_created"
);
impl_audit_operation!(
    AiBlueprintVersionDecidedAudit,
    "ai.blueprint.version_decided"
);
impl_audit_operation!(
    AiBlueprintBindingChangedAudit,
    "ai.blueprint.binding_changed"
);

/// An autofix incident was opened for an error group (ADR-054), by a user
/// ("Fix this crash") or automatically for the project's autofix owner.
#[derive(Debug, Clone, Serialize)]
pub struct AutofixIncidentOpenedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub error_group_id: i32,
    pub incident_id: String,
    pub trigger: String,
}

/// A user stopped a running or waiting autofix incident.
#[derive(Debug, Clone, Serialize)]
pub struct AutofixIncidentStoppedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub incident_id: String,
    pub previous_status: String,
}

/// A user recorded the outcome of an incident: merged, applied or rejected.
#[derive(Debug, Clone, Serialize)]
pub struct AutofixIncidentClosedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub incident_id: String,
    pub decision: String,
    pub pr_url: Option<String>,
}

/// A project's autofix policy changed.
#[derive(Debug, Clone, Serialize)]
pub struct AutofixSettingsUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub mode: String,
    pub min_confidence: i16,
    pub protected_paths: Vec<String>,
    pub has_test_command: bool,
}

/// All autofix work of a project was paused or resumed.
#[derive(Debug, Clone, Serialize)]
pub struct AutofixPausedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub paused: bool,
}

impl_audit_operation!(AutofixIncidentOpenedAudit, "AUTOFIX_INCIDENT_OPENED");
impl_audit_operation!(AutofixIncidentStoppedAudit, "AUTOFIX_INCIDENT_STOPPED");
impl_audit_operation!(AutofixIncidentClosedAudit, "AUTOFIX_INCIDENT_CLOSED");
impl_audit_operation!(AutofixSettingsUpdatedAudit, "AUTOFIX_SETTINGS_UPDATED");
impl_audit_operation!(AutofixPausedAudit, "AUTOFIX_PAUSED");
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_attach_audit_names_the_workspace_but_never_the_input() {
        let audit = WorkspaceTerminalAttachedAudit {
            context: AuditContext {
                user_id: 7,
                ip_address: Some("127.0.0.1".to_string()),
                user_agent: "test".to_string(),
            },
            scope: "application".to_string(),
            workspace_id: "app_123".to_string(),
            sandbox_id: "sbx_0123456789abcdef".to_string(),
            thread_id: "0123456789abcdef0123456789abcdef".to_string(),
            tab: "t-0123456789abcdef0123456789abcdef".to_string(),
        };
        assert_eq!(audit.operation_type(), "ai.workspace.terminal_attached");
        assert_eq!(audit.user_id(), Some(7));
        let body = AuditOperation::serialize(&audit).expect("serialize audit");
        assert!(body.contains("sbx_0123456789abcdef"), "{body}");
        assert!(body.contains("app_123"), "{body}");
        // There is no field that could carry terminal input or output.
        assert!(!body.contains("command"), "{body}");
        assert!(!body.contains("input"), "{body}");
    }
}
