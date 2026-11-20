// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Autofix incidents (ADR-054): a crash becomes a chat in a workspace with its
//! own sandbox, built from the deployment that crashed. The agent reproduces
//! and fixes it there; the server re-runs the evidence, scores it, delivers a
//! pull request or a patch, and records every step on a timeline. When the
//! incident closes, the sandbox is deleted and only the transcript, timeline
//! and decision remain.

pub mod evidence;
pub mod service;

use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use temps_entities::{autofix_incident_events, autofix_incidents, project_autofix_settings};
use thiserror::Error;
use utoipa::ToSchema;

pub use evidence::{Assessment, CommandOutcome, FileChange, Signal};
pub use service::AutofixService;

/// `ai_conversations.metadata` key naming the incident a chat belongs to.
pub const INCIDENT_METADATA_KEY: &str = "autofix_incident";

/// Told to the model when it tries a write the incident mode does not allow.
pub const REFUSED_WRITE_REASON: &str = "This is an autofix incident chat with nobody watching it: \
    you can only work inside the linked sandbox (`sandbox_exec`, `sandbox_read_file`, \
    `sandbox_write_file`). Temps commits the fix and opens the pull request itself once you \
    finish, so do not try to deploy, commit, push, ask questions or change platform settings.";

/// Statuses of an incident that is still being worked on or reviewed.
pub const OPEN_STATUSES: [&str; 3] = ["open", "working", "awaiting_review"];
/// Final statuses.
pub const CLOSED_STATUSES: [&str; 5] = ["merged", "applied", "rejected", "failed", "stopped"];

pub fn is_open_status(status: &str) -> bool {
    OPEN_STATUSES.contains(&status)
}

/// The incident a chat belongs to, from its metadata.
pub fn incident_of_conversation(metadata: Option<&serde_json::Value>) -> Option<String> {
    metadata?
        .get(INCIDENT_METADATA_KEY)?
        .as_str()
        .map(str::to_string)
}

/// Tools an incident chat may not run at all. Platform writes through
/// `temps_write` are refused separately, at their approval.
pub fn tool_refused_in_incident(name: &str) -> bool {
    matches!(
        name,
        crate::ask_user::ASK_USER_TOOL
            | crate::sandbox_link::SANDBOX_OFFER_TOOL
            | crate::sandbox_link::SANDBOX_CREATE_TOOL
            | crate::sandbox_deploy::SANDBOX_DEPLOY_TOOL
            | crate::workspace_chats::PROPOSE_BLUEPRINT_TOOL
            | crate::workspace_chats::BIND_BLUEPRINT_TOOL
            | crate::secret_slots::REQUEST_SECRET_TOOL
            | "temps_goal"
    )
}

/// Drop the tools an incident chat may not run from the catalog offered to
/// the model, so it is never tempted to call them. Dispatch still refuses
/// them (see [`tool_refused_in_incident`]) for a model that names one from
/// memory.
pub fn withhold_refused_tools(tools: &mut Vec<temps_ai::ChatTool>) {
    tools.retain(|tool| !tool_refused_in_incident(&tool.name));
}

/// What the model is told when it calls a tool an incident chat refuses.
pub fn refused_tool_message(tool_name: &str) -> String {
    format!(
        "Not run: `{tool_name}` is not available in an autofix incident. {REFUSED_WRITE_REASON}"
    )
}

/// Record on the incident's timeline that the agent tried something the mode
/// blocks. Best-effort: the refusal itself never depends on this write.
pub(crate) async fn record_refused_action<C: ConnectionTrait>(
    db: &C,
    incident_public_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) {
    let incident = match autofix_incidents::Entity::find()
        .filter(autofix_incidents::Column::PublicId.eq(incident_public_id))
        .one(db)
        .await
    {
        Ok(Some(incident)) => incident,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                incident_id = incident_public_id,
                "could not load autofix incident {incident_public_id} to record a refused tool: {error}"
            );
            return;
        }
    };
    let excerpt = evidence::tail(&input.to_string(), 1024);
    let details = EventDetails {
        tool: Some(tool_name.to_string()),
        output_excerpt: Some(excerpt),
        ..Default::default()
    };
    if let Err(error) = insert_event(
        db,
        incident.id,
        "agent",
        "refused_action",
        &format!("Blocked `{tool_name}`: incident chats can only work in their sandbox"),
        details,
    )
    .await
    {
        tracing::warn!(
            incident_id = incident_public_id,
            "could not record refused tool {tool_name} on autofix incident {incident_public_id}: {error}"
        );
    }
}

pub(crate) async fn insert_event<C: ConnectionTrait>(
    db: &C,
    incident_id: i64,
    actor: &str,
    kind: &str,
    summary: &str,
    details: EventDetails,
) -> Result<autofix_incident_events::Model, sea_orm::DbErr> {
    let details = (!details.is_empty())
        .then(|| serde_json::to_value(&details).ok())
        .flatten();
    autofix_incident_events::ActiveModel {
        incident_id: Set(incident_id),
        actor: Set(actor.to_string()),
        kind: Set(kind.to_string()),
        summary: Set(summary.to_string()),
        details: Set(details),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
}

#[derive(Debug, Error)]
pub enum AutofixError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Error group {group_id} not found in project {project_id}")]
    ErrorGroupNotFound { group_id: i32, project_id: i32 },

    #[error("Autofix incident {incident_id} not found in project {project_id}")]
    IncidentNotFound {
        incident_id: String,
        project_id: i32,
    },

    #[error("Invalid autofix request for project {project_id}: {message}")]
    Validation { project_id: i32, message: String },

    #[error("Autofix incident {incident_id} cannot be {action}: it is {status}")]
    InvalidState {
        incident_id: String,
        action: &'static str,
        status: String,
    },

    #[error("Autofix is unavailable for project {project_id}: {reason}")]
    Unavailable { project_id: i32, reason: String },

    #[error("The autofix queue is full ({queued} incidents waiting); error group {group_id} of project {project_id} was not queued")]
    QueueFull {
        project_id: i32,
        group_id: i32,
        queued: u64,
    },

    #[error("Project {project_id} already has {queued} autofix incidents waiting (at most {limit}); error group {group_id} was not queued")]
    ProjectQueueFull {
        project_id: i32,
        group_id: i32,
        queued: u64,
        limit: u64,
    },

    #[error("Autofix for project {project_id} acts as user {owner_user_id}, who cannot act on it any more: {reason}. Someone with access must save the project's autofix settings to take it over")]
    OwnerCannotAct {
        project_id: i32,
        owner_user_id: i32,
        reason: String,
    },

    #[error("Database error while {operation} for autofix: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Autofix incident {incident_id} could not {step}: {reason}")]
    Step {
        incident_id: String,
        step: &'static str,
        reason: String,
    },

    #[error("Autofix incident {incident_id} was stopped")]
    Stopped { incident_id: String },
}

impl AutofixError {
    pub(crate) fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| AutofixError::Database { operation, source }
    }

    pub(crate) fn step(
        incident_id: &str,
        step: &'static str,
        reason: impl std::fmt::Display,
    ) -> Self {
        AutofixError::Step {
            incident_id: incident_id.to_string(),
            step,
            reason: reason.to_string(),
        }
    }
}

/// Structured details of a timeline event. Stored as JSON; every field is
/// optional because each kind uses a few of them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<FileChange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
}

impl EventDetails {
    pub fn is_empty(&self) -> bool {
        self == &EventDetails::default()
    }

    pub fn command(outcome: &CommandOutcome) -> Self {
        EventDetails {
            command: Some(outcome.command.clone()),
            exit_code: Some(outcome.exit_code),
            duration_ms: Some(outcome.duration_ms),
            output_excerpt: Some(outcome.output_excerpt.clone()),
            ..Default::default()
        }
    }
}

// ── API types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutofixSettingsResponse {
    pub project_id: i32,
    /// `off`, `propose` or `auto_merge`.
    pub mode: String,
    /// Confidence level, 0-5, an `auto_merge` fix must reach.
    pub min_confidence: i16,
    pub protected_paths: Vec<String>,
    /// Command the server runs to verify a fix, in addition to the agent's
    /// reproduction.
    pub test_command: Option<String>,
    pub paused: bool,
    /// User automatic incidents act as.
    pub owner_user_id: Option<i32>,
    pub ai_provider: Option<String>,
    pub ai_model: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl AutofixSettingsResponse {
    pub(crate) fn from_row(project_id: i32, row: Option<project_autofix_settings::Model>) -> Self {
        match row {
            Some(row) => Self {
                project_id,
                mode: row.mode,
                min_confidence: row.min_confidence,
                protected_paths: serde_json::from_value(row.protected_paths).unwrap_or_default(),
                test_command: row.test_command,
                paused: row.paused,
                owner_user_id: row.owner_user_id,
                ai_provider: row.ai_provider,
                ai_model: row.ai_model,
                updated_at: Some(row.updated_at),
            },
            None => Self {
                project_id,
                mode: "off".into(),
                min_confidence: 4,
                protected_paths: Vec::new(),
                test_command: None,
                paused: false,
                owner_user_id: None,
                ai_provider: None,
                ai_model: None,
                updated_at: None,
            },
        }
    }
}

/// Whether autofix can run for a project, and what a fix would become.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutofixCapabilityResponse {
    /// An AI provider and sandboxes are available.
    pub configured: bool,
    /// Why autofix cannot run, when `configured` is false.
    pub reason: Option<String>,
    /// Console path that fixes the missing configuration.
    pub setup_path: Option<String>,
    /// How the latest production deployment was built: `git`,
    /// `uploaded_source`, `report_only` or `none`.
    pub source_kind: String,
    /// What a fix becomes: `pull_request`, `patch` or `report`.
    pub delivery: String,
    /// Why the delivery is what it is, e.g. the Git connection is missing.
    pub delivery_reason: Option<String>,
    pub settings: AutofixSettingsResponse,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateAutofixSettingsRequest {
    /// `off`, `propose` or `auto_merge`.
    pub mode: Option<String>,
    /// Confidence level, 0-5, an `auto_merge` fix must reach.
    pub min_confidence: Option<i16>,
    pub protected_paths: Option<Vec<String>>,
    /// Empty string clears it.
    pub test_command: Option<String>,
    /// Empty string clears it (back to the assistant's default).
    pub ai_provider: Option<String>,
    /// Empty string clears it.
    pub ai_model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OpenIncidentRequest {
    /// The error group to fix.
    pub error_group_id: i32,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PauseAutofixRequest {
    pub paused: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CloseIncidentRequest {
    /// `merged` (you merged the pull request), `applied` (you applied the
    /// patch) or `rejected`.
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutofixIncidentResponse {
    /// Public id (`afx_…`).
    pub id: String,
    pub project_id: i32,
    pub error_group_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub title: String,
    /// `manual` or `automatic`.
    pub trigger: String,
    /// `open`, `working`, `awaiting_review`, `merged`, `applied`, `rejected`,
    /// `failed` or `stopped`.
    pub status: String,
    pub is_open: bool,
    /// `queued`, `prepare`, `reproduce`, `verify`, `deliver` or `done`.
    pub phase: String,
    pub source_kind: Option<String>,
    pub delivery: Option<String>,
    pub owner_user_id: i32,
    /// Public id of the incident chat.
    pub conversation_id: Option<String>,
    /// Public id of the workspace the chat is in.
    pub workspace_id: Option<String>,
    /// Public id of the sandbox while it exists.
    pub sandbox_id: Option<String>,
    pub base_revision: Option<String>,
    pub head_revision: Option<String>,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    pub has_patch: bool,
    pub summary: Option<String>,
    /// Confidence level reached, 0-5: see `assessment` for what it proves.
    pub confidence: Option<i16>,
    pub assessment: Option<Assessment>,
    pub failure_reason: Option<String>,
    pub decision: Option<String>,
    pub decided_by_user_id: Option<i32>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub decided_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OpenIncidentResponse {
    pub incident: AutofixIncidentResponse,
    /// False when an open incident for the group already existed.
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutofixIncidentListResponse {
    pub items: Vec<AutofixIncidentResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutofixEventResponse {
    pub id: i64,
    /// `server`, `agent`, `policy` or `user:<id>`.
    pub actor: String,
    pub kind: String,
    pub summary: String,
    pub details: Option<EventDetails>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
}

impl From<autofix_incident_events::Model> for AutofixEventResponse {
    fn from(row: autofix_incident_events::Model) -> Self {
        Self {
            id: row.id,
            actor: row.actor,
            kind: row.kind,
            summary: row.summary,
            details: row
                .details
                .and_then(|value| serde_json::from_value(value).ok()),
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutofixEventListResponse {
    pub items: Vec<AutofixEventResponse>,
    /// The incident's status when the page was read, so a poller knows when
    /// to stop.
    pub incident_status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_names_its_incident_in_metadata() {
        let metadata = serde_json::json!({ INCIDENT_METADATA_KEY: "afx_1" });
        assert_eq!(
            incident_of_conversation(Some(&metadata)).as_deref(),
            Some("afx_1")
        );
        assert_eq!(incident_of_conversation(None), None);
        assert_eq!(
            incident_of_conversation(Some(&serde_json::json!({"other": 1}))),
            None
        );
    }

    #[test]
    fn incident_chats_cannot_ask_deploy_or_create_sandboxes() {
        assert!(tool_refused_in_incident(crate::ask_user::ASK_USER_TOOL));
        assert!(tool_refused_in_incident(
            crate::sandbox_deploy::SANDBOX_DEPLOY_TOOL
        ));
        assert!(tool_refused_in_incident(
            crate::sandbox_link::SANDBOX_CREATE_TOOL
        ));
        assert!(!tool_refused_in_incident(
            crate::sandbox_link::SANDBOX_EXEC_TOOL
        ));
        assert!(!tool_refused_in_incident(
            crate::sandbox_link::SANDBOX_WRITE_TOOL
        ));
    }

    #[test]
    fn every_unattended_interaction_tool_is_refused_in_an_incident() {
        for name in [
            crate::ask_user::ASK_USER_TOOL,
            crate::sandbox_link::SANDBOX_OFFER_TOOL,
            crate::sandbox_link::SANDBOX_CREATE_TOOL,
            crate::sandbox_deploy::SANDBOX_DEPLOY_TOOL,
            crate::workspace_chats::PROPOSE_BLUEPRINT_TOOL,
            crate::workspace_chats::BIND_BLUEPRINT_TOOL,
            crate::secret_slots::REQUEST_SECRET_TOOL,
            "temps_goal",
        ] {
            assert!(tool_refused_in_incident(name), "{name} must be refused");
        }
        for name in [
            "temps",
            "temps_write",
            crate::sandbox_link::SANDBOX_EXEC_TOOL,
            crate::sandbox_link::SANDBOX_READ_TOOL,
            crate::sandbox_link::SANDBOX_WRITE_TOOL,
        ] {
            assert!(!tool_refused_in_incident(name), "{name} must stay usable");
        }
    }

    #[test]
    fn incident_chats_are_not_offered_refused_tools() {
        let tool = |name: &str| temps_ai::ChatTool {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        };
        let mut tools = vec![
            tool("temps"),
            tool(crate::ask_user::ASK_USER_TOOL),
            tool(crate::sandbox_link::SANDBOX_EXEC_TOOL),
            tool(crate::workspace_chats::PROPOSE_BLUEPRINT_TOOL),
            tool(crate::secret_slots::REQUEST_SECRET_TOOL),
            tool("temps_goal"),
            tool(crate::sandbox_link::SANDBOX_OFFER_TOOL),
        ];
        withhold_refused_tools(&mut tools);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["temps", crate::sandbox_link::SANDBOX_EXEC_TOOL]);
    }

    #[test]
    fn the_refusal_names_the_tool_and_the_reason() {
        let message = refused_tool_message("temps_goal");
        assert!(message.starts_with("Not run: `temps_goal`"));
        assert!(message.contains(REFUSED_WRITE_REASON));
    }

    #[test]
    fn open_and_closed_statuses_do_not_overlap() {
        for status in OPEN_STATUSES {
            assert!(is_open_status(status));
            assert!(!CLOSED_STATUSES.contains(&status));
        }
        for status in CLOSED_STATUSES {
            assert!(!is_open_status(status));
        }
    }

    #[test]
    fn empty_event_details_are_not_stored() {
        assert!(EventDetails::default().is_empty());
        assert!(!EventDetails {
            exit_code: Some(0),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn missing_settings_read_as_off() {
        let settings = AutofixSettingsResponse::from_row(4, None);
        assert_eq!(settings.mode, "off");
        assert_eq!(settings.min_confidence, 4);
        assert!(!settings.paused);
    }
}
