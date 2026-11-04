// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workspaces that hold several assistant chats (ADR-053).
//!
//! A workspace is an `ai_applications` row; an assistant chat belongs to one
//! through `ai_conversations.application_id` and keeps its `assistant`
//! context, so it keeps the gateway models and the API tools. The chats of a
//! workspace share its sandbox (see `sandbox_link::metadata_scope`), its
//! projects and its architecture blueprint.
//!
//! This module also holds the two tools the model uses on the blueprint:
//! `temps_propose_blueprint` and `temps_bind_blueprint`.

use std::sync::Arc;

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Statement,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use temps_ai::ChatTool;
use temps_auth::context::AuthContext;
use temps_auth::permissions::Permission;
use temps_core::{AuditContext, AuditLogger};
use temps_entities::{ai_applications, ai_conversations, external_services, projects};
use temps_sandbox::SandboxService;
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

use crate::application_link::LinkProjectError;
use crate::applications::{ApplicationError, ApplicationService};
use crate::blueprint::{
    BlueprintDocument, BlueprintError, BlueprintService, ComponentBinding, ComponentKind,
};
use crate::providers::assistant::ASSISTANT_CONTEXT_TYPE;
use crate::sandbox_link::{
    metadata_scope, SandboxLinkService, SANDBOX_LINK_KEY, SANDBOX_PREVIEW_KEY,
};

/// Tool the model uses to propose a version of the workspace's architecture.
pub const PROPOSE_BLUEPRINT_TOOL: &str = "temps_propose_blueprint";
/// Tool the model uses to record which project or service realises a part.
pub const BIND_BLUEPRINT_TOOL: &str = "temps_bind_blueprint";

/// Most sibling chats a workspace summary lists.
const MAX_LISTED_CHATS: u64 = 20;
const MAX_WORKSPACE_NAME_CHARS: usize = 200;

pub fn is_blueprint_tool(name: &str) -> bool {
    matches!(name, PROPOSE_BLUEPRINT_TOOL | BIND_BLUEPRINT_TOOL)
}

/// Whether a `temps_propose_blueprint` result stored a proposal (rather than
/// reporting problems to fix).
pub fn is_proposal_receipt(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .is_some_and(|status| status == "proposed")
}

#[derive(Error, Debug)]
pub enum WorkspaceChatError {
    #[error(
        "Chat {conversation_id} is a {context_type} chat; only assistant chats that are not tied to a project can be in a workspace"
    )]
    NotAssistantChat {
        conversation_id: String,
        context_type: String,
    },

    #[error(
        "Workspace {application_id} was not found among the active workspaces of user {user_id}"
    )]
    WorkspaceNotFound {
        application_id: String,
        user_id: i32,
    },

    #[error("Chat {conversation_id} is not in a workspace")]
    NoWorkspace { conversation_id: String },

    #[error("Invalid workspace request for chat {conversation_id}: {message}")]
    Validation {
        conversation_id: String,
        message: String,
    },

    #[error("Could not create a workspace for chat {conversation_id}: {source}")]
    Application {
        conversation_id: String,
        #[source]
        source: ApplicationError,
    },

    #[error(transparent)]
    Blueprint(#[from] BlueprintError),

    #[error("Database error while {operation} for chat {conversation_id}: {source}")]
    Database {
        conversation_id: String,
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

fn db_error(
    conversation: &ai_conversations::Model,
    operation: &'static str,
) -> impl FnOnce(DbErr) -> WorkspaceChatError {
    let conversation_id = conversation.public_id.clone();
    move |source| WorkspaceChatError::Database {
        conversation_id,
        operation,
        source,
    }
}

/// Which sandbox the chats keep when a chat with a sandbox of its own joins a
/// workspace that already has one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeepSandbox {
    /// The workspace's sandbox; the joining chat switches to it.
    #[default]
    Workspace,
    /// The joining chat's sandbox; every chat of the workspace switches to it.
    Chat,
}

/// Where a chat goes.
#[derive(Debug, Clone)]
pub enum JoinTarget {
    /// An existing workspace, by public id.
    Existing(String),
    /// A new workspace, created for the chat.
    Create {
        name: String,
        description: Option<String>,
    },
}

/// One chat of a workspace, for the switcher.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkspaceChatSummary {
    pub public_id: String,
    pub title: Option<String>,
    /// ISO 8601.
    pub last_activity_at: String,
    pub turn_status: String,
}

/// A chat's workspace and the other chats in it.
#[derive(Debug, Clone)]
pub struct WorkspaceOfChat {
    pub application: ai_applications::Model,
    /// The workspace's chats, most recently active first, including this one.
    pub chats: Vec<WorkspaceChatSummary>,
}

/// Puts chats in workspaces and reads them back.
#[derive(Clone)]
pub struct WorkspaceChatService {
    db: Arc<DatabaseConnection>,
    applications: Arc<ApplicationService>,
    blueprints: BlueprintService,
    /// Disconnects a sandbox no chat uses any more from its services. Absent
    /// on a server without a sandbox backend, where there is nothing to free.
    sandboxes: Option<Arc<SandboxService>>,
}

impl WorkspaceChatService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        applications: Arc<ApplicationService>,
        blueprints: BlueprintService,
    ) -> Self {
        Self {
            db,
            applications,
            blueprints,
            sandboxes: None,
        }
    }

    /// Free the services of a sandbox the chats stop using when a chat joins
    /// or leaves a workspace.
    pub fn with_sandboxes(mut self, sandboxes: Option<Arc<SandboxService>>) -> Self {
        self.sandboxes = sandboxes;
        self
    }

    pub fn blueprints(&self) -> &BlueprintService {
        &self.blueprints
    }

    /// Whether a chat can be put in a workspace: assistant chats that are not
    /// tied to one project.
    pub fn is_joinable(conversation: &ai_conversations::Model) -> bool {
        Self::ensure_joinable(conversation).is_ok()
    }

    fn ensure_joinable(conversation: &ai_conversations::Model) -> Result<(), WorkspaceChatError> {
        if conversation.context_type != ASSISTANT_CONTEXT_TYPE || conversation.project_id.is_some()
        {
            return Err(WorkspaceChatError::NotAssistantChat {
                conversation_id: conversation.public_id.clone(),
                context_type: conversation.context_type.clone(),
            });
        }
        Ok(())
    }

    /// The active workspace a chat is in, if any.
    pub async fn workspace_of(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<Option<ai_applications::Model>, WorkspaceChatError> {
        let Some(application_id) = conversation.application_id else {
            return Ok(None);
        };
        if conversation.context_type != ASSISTANT_CONTEXT_TYPE {
            return Ok(None);
        }
        ai_applications::Entity::find_by_id(application_id)
            .filter(ai_applications::Column::CreatedBy.eq(conversation.created_by))
            .filter(ai_applications::Column::Status.eq("active"))
            .one(self.db.as_ref())
            .await
            .map_err(db_error(conversation, "reading the workspace"))
    }

    /// The chat's workspace with its chats, or `None` outside a workspace.
    pub async fn summary(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<Option<WorkspaceOfChat>, WorkspaceChatError> {
        let Some(application) = self.workspace_of(conversation).await? else {
            return Ok(None);
        };
        let rows = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ApplicationId.eq(application.id))
            .filter(ai_conversations::Column::CreatedBy.eq(conversation.created_by))
            .filter(ai_conversations::Column::ContextType.eq(ASSISTANT_CONTEXT_TYPE))
            .filter(ai_conversations::Column::Status.eq("active"))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .limit(MAX_LISTED_CHATS)
            .all(self.db.as_ref())
            .await
            .map_err(db_error(conversation, "listing the workspace's chats"))?;
        let chats = rows
            .into_iter()
            .map(|row| WorkspaceChatSummary {
                public_id: row.public_id,
                title: row.title,
                last_activity_at: row
                    .last_activity_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                turn_status: row.turn_status,
            })
            .collect();
        Ok(Some(WorkspaceOfChat { application, chats }))
    }

    /// Put the chat in a workspace, creating it first when asked. Returns the
    /// workspace and whether it was created.
    pub async fn join(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        target: JoinTarget,
        keep: KeepSandbox,
    ) -> Result<(ai_applications::Model, bool), WorkspaceChatError> {
        Self::ensure_joinable(conversation)?;
        let (application, created) = match target {
            JoinTarget::Existing(public_id) => {
                let found =
                    self.applications.get(user_id, &public_id).await.map_err(
                        |error| match error {
                            ApplicationError::NotFound { .. } => {
                                WorkspaceChatError::WorkspaceNotFound {
                                    application_id: public_id.clone(),
                                    user_id,
                                }
                            }
                            source => WorkspaceChatError::Application {
                                conversation_id: conversation.public_id.clone(),
                                source,
                            },
                        },
                    )?;
                (found.application, false)
            }
            JoinTarget::Create { name, description } => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > MAX_WORKSPACE_NAME_CHARS {
                    return Err(WorkspaceChatError::Validation {
                        conversation_id: conversation.public_id.clone(),
                        message: format!(
                            "a workspace name must be 1 to {MAX_WORKSPACE_NAME_CHARS} characters"
                        ),
                    });
                }
                let created = self
                    .applications
                    .create(user_id, name, description.as_deref(), &[])
                    .await
                    .map_err(|source| WorkspaceChatError::Application {
                        conversation_id: conversation.public_id.clone(),
                        source,
                    })?;
                (created.application, true)
            }
        };
        if conversation.application_id == Some(application.id) {
            return Ok((application, created));
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE ai_conversations SET application_id = $1 WHERE id = $2",
                [application.id.into(), conversation.id.into()],
            ))
            .await
            .map_err(db_error(conversation, "joining the workspace"))?;
        self.settle_sandbox(conversation, keep).await?;
        Ok((application, created))
    }

    /// Make every chat of the workspace use one sandbox after a chat joined.
    async fn settle_sandbox(
        &self,
        conversation: &ai_conversations::Model,
        keep: KeepSandbox,
    ) -> Result<(), WorkspaceChatError> {
        let chat_has_link = conversation
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(SANDBOX_LINK_KEY))
            .is_some_and(|link| !link.is_null());
        // The most recently active other chat of the workspace with a sandbox.
        let sibling = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT sibling.id FROM ai_conversations sibling \
                     JOIN ai_conversations chat ON chat.id = $1 \
                     WHERE sibling.id <> chat.id \
                       AND sibling.application_id = chat.application_id \
                       AND sibling.created_by = chat.created_by \
                       AND sibling.context_type = '{ASSISTANT_CONTEXT_TYPE}' \
                       AND jsonb_typeof(sibling.metadata) = 'object' \
                       AND sibling.metadata ? '{SANDBOX_LINK_KEY}' \
                       AND sibling.metadata -> '{}' IS NULL \
                     ORDER BY sibling.last_activity_at DESC LIMIT 1",
                    crate::autofix::INCIDENT_METADATA_KEY
                ),
                [conversation.id.into()],
            ))
            .await
            .map_err(db_error(conversation, "finding the workspace's sandbox"))?
            .and_then(|row| row.try_get::<i64>("", "id").ok());
        let source = match (keep, sibling, chat_has_link) {
            (KeepSandbox::Chat, _, true) => Some(conversation.id),
            (_, Some(sibling), _) => Some(sibling),
            (_, None, true) => Some(conversation.id),
            (_, None, false) => None,
        };
        let Some(source) = source else {
            return Ok(());
        };
        let before = self.linked_sandboxes(conversation).await?;
        // Copy the source chat's link and preview port to every chat of the
        // workspace, the joining chat included, in one statement.
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE ai_conversations \
                     SET metadata = ((CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata ELSE '{{}}'::jsonb END) \
                         - '{SANDBOX_LINK_KEY}' - '{SANDBOX_PREVIEW_KEY}') \
                         || COALESCE((SELECT jsonb_strip_nulls(jsonb_build_object( \
                                '{SANDBOX_LINK_KEY}', source.metadata -> '{SANDBOX_LINK_KEY}', \
                                '{SANDBOX_PREVIEW_KEY}', source.metadata -> '{SANDBOX_PREVIEW_KEY}')) \
                             FROM ai_conversations source WHERE source.id = $1), '{{}}'::jsonb) \
                     WHERE {}",
                    metadata_scope("$2", true)
                ),
                [source.into(), conversation.id.into()],
            ))
            .await
            .map_err(db_error(conversation, "sharing the workspace's sandbox"))?;
        self.release_unused(conversation, before).await;
        Ok(())
    }

    /// Sandbox ids the chat and the other chats of its workspace link to.
    async fn linked_sandboxes(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<Vec<String>, WorkspaceChatError> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT DISTINCT metadata -> '{SANDBOX_LINK_KEY}' ->> 'sandbox_id' AS sandbox_id \
                     FROM ai_conversations \
                     WHERE {} \
                       AND jsonb_typeof(metadata) = 'object' \
                       AND metadata -> '{SANDBOX_LINK_KEY}' ->> 'sandbox_id' IS NOT NULL",
                    metadata_scope("$1", true)
                ),
                [conversation.id.into()],
            ))
            .await
            .map_err(db_error(conversation, "reading the workspace's sandboxes"))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.try_get::<String>("", "sandbox_id").ok())
            .collect())
    }

    /// Of `candidates`, the sandboxes no chat links to any more.
    async fn unused_sandboxes(
        &self,
        conversation: &ai_conversations::Model,
        candidates: Vec<String>,
    ) -> Result<Vec<String>, WorkspaceChatError> {
        if candidates.is_empty() {
            return Ok(candidates);
        }
        let placeholders = (1..=candidates.len())
            .map(|n| format!("${n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT DISTINCT metadata -> '{SANDBOX_LINK_KEY}' ->> 'sandbox_id' AS sandbox_id \
                     FROM ai_conversations \
                     WHERE jsonb_typeof(metadata) = 'object' \
                       AND metadata -> '{SANDBOX_LINK_KEY}' ->> 'sandbox_id' IN ({placeholders})"
                ),
                candidates.iter().map(|id| id.clone().into()),
            ))
            .await
            .map_err(db_error(conversation, "checking which sandboxes are still in use"))?;
        let used: std::collections::HashSet<String> = rows
            .into_iter()
            .filter_map(|row| row.try_get::<String>("", "sandbox_id").ok())
            .collect();
        Ok(candidates
            .into_iter()
            .filter(|id| !used.contains(id))
            .collect())
    }

    /// Disconnect the sandboxes of `candidates` that no chat uses any more
    /// from their services, so a sandbox nobody works in stops reaching the
    /// databases it was given. Best effort: the chat already moved.
    async fn release_unused(
        &self,
        conversation: &ai_conversations::Model,
        candidates: Vec<String>,
    ) {
        let Some(sandboxes) = &self.sandboxes else {
            return;
        };
        let unused = match self.unused_sandboxes(conversation, candidates).await {
            Ok(unused) => unused,
            Err(error) => {
                warn!(
                    conversation_id = %conversation.public_id,
                    "could not tell which sandboxes the workspace stopped using: {error}"
                );
                return;
            }
        };
        if unused.is_empty() {
            return;
        }
        let links = SandboxLinkService::new(self.db.clone(), sandboxes.clone());
        for sandbox_id in unused {
            links
                .release_sandbox(
                    conversation.created_by,
                    &sandbox_id,
                    &conversation.public_id,
                )
                .await;
        }
    }

    /// Take the chat out of its workspace. It stops using the workspace's
    /// sandbox; the sandbox and the other chats are untouched, unless no chat
    /// uses the sandbox any more, which frees its service connections.
    pub async fn leave(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<ai_applications::Model, WorkspaceChatError> {
        Self::ensure_joinable(conversation)?;
        let application = self.workspace_of(conversation).await?.ok_or_else(|| {
            WorkspaceChatError::NoWorkspace {
                conversation_id: conversation.public_id.clone(),
            }
        })?;
        let link = crate::sandbox_link::link_from_metadata(conversation.metadata.as_ref());
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE ai_conversations \
                     SET application_id = NULL, \
                         metadata = CASE WHEN jsonb_typeof(metadata) = 'object' \
                             THEN metadata - '{SANDBOX_LINK_KEY}' - '{SANDBOX_PREVIEW_KEY}' \
                             ELSE metadata END \
                     WHERE id = $1"
                ),
                [conversation.id.into()],
            ))
            .await
            .map_err(db_error(conversation, "leaving the workspace"))?;
        // The last chat on a sandbox leaving lets it go.
        if let Some(link) = link {
            self.release_unused(conversation, vec![link.sandbox_id])
                .await;
        }
        Ok(application)
    }

    /// The per-turn prompt section of a workspace chat.
    pub async fn prompt_section(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<Option<String>, WorkspaceChatError> {
        let Some(application) = self.workspace_of(conversation).await? else {
            return Ok(None);
        };
        let state = self.blueprints.state(application.id).await?;
        Ok(Some(crate::blueprint::prompt_section(
            &application.name,
            &state,
        )))
    }
}

/// Names of the workspaces each chat is in, in one query, for chat lists.
/// An archived workspace is left out, as [`WorkspaceChatService::workspace_of`]
/// leaves it out: its chats are listed on their own until it is restored.
pub async fn workspace_names(
    db: &DatabaseConnection,
    application_ids: &[i64],
) -> Result<std::collections::HashMap<i64, (String, String)>, DbErr> {
    if application_ids.is_empty() {
        return Ok(Default::default());
    }
    let rows = ai_applications::Entity::find()
        .filter(ai_applications::Column::Id.is_in(application_ids.iter().copied()))
        .filter(ai_applications::Column::Status.eq("active"))
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.id, (row.public_id, row.name)))
        .collect())
}

#[derive(Deserialize)]
struct ProposeArgs {
    document: BlueprintDocument,
    change_summary: String,
    #[serde(default)]
    workspace_name: Option<String>,
}

#[derive(Deserialize)]
struct BindArgs {
    component_id: String,
    #[serde(default)]
    project_id: Option<i32>,
    #[serde(default)]
    service_id: Option<i32>,
}

/// The blueprint tools of an assistant chat.
#[derive(Clone)]
pub struct BlueprintToolSupport {
    pub workspaces: WorkspaceChatService,
    /// Adds a bound project to the workspace with the same checks and setup
    /// as linking it from the console.
    pub linker: crate::application_link::ApplicationProjectLinker,
    pub audit: Option<Arc<dyn AuditLogger>>,
    pub project_access: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

fn assistant_audit_context(user_id: i32) -> AuditContext {
    AuditContext {
        user_id,
        ip_address: None,
        user_agent: "temps-ai-assistant".to_string(),
    }
}

impl BlueprintToolSupport {
    /// The two tool definitions.
    pub fn tools() -> Vec<ChatTool> {
        vec![
            ChatTool {
                name: PROPOSE_BLUEPRINT_TOOL.to_string(),
                description: "Propose the architecture of the application the user wants: its \
                    parts (clients, apps Temps builds and runs, data Temps operates, external \
                    services) and the flows of data between them. The user sees it as a diagram \
                    and accepts or rejects it; nothing is built or created by this call. Use it \
                    before building anything new, and again (with the whole new document) when \
                    the work needs a part or a flow the accepted architecture does not have. \
                    The chat is put in a workspace first if it is not in one. After calling it, \
                    end your turn with one or two sentences on the design choices and wait for \
                    the user's decision. Do not build in the same turn.\n\n\
                    Part kinds: `client` (needs `surface`: browser, mobile, cli, third_party); \
                    `app` (needs `role`: web, api, worker, cron, static; `stack`, free text such \
                    as \"Next.js\" or \"Bun + Hono\"; `runtime`: preset, dockerfile or image, \
                    with `image` such as \"nats:2.10\" when it is image); `data` (needs \
                    `engine`: postgres, redis, mongodb, mariadb, s3; `holds`: the tables, key \
                    prefixes, buckets or streams it keeps); `external` (needs `provider`). Flow \
                    kinds: http, websocket, query (to postgres/mariadb/mongodb), cache (to \
                    redis), blob (to s3), publish and subscribe (to redis or an app running a \
                    broker), webhook. Draw a flow from the part that starts it; data never \
                    starts a flow. Every problem in the document is reported at once."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "summary": { "type": "string", "description": "At most three sentences: what the application does and for whom. No visual design, stack or build notes: those go in the components and in your reply." },
                                "components": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "id": { "type": "string", "description": "Lowercase slug, unique: web, api, orders-worker." },
                                            "name": { "type": "string" },
                                            "purpose": { "type": "string", "description": "Why it exists, one sentence." },
                                            "kind": { "type": "string", "enum": ["client", "app", "data", "external"] },
                                            "surface": { "type": "string", "enum": ["browser", "mobile", "cli", "third_party"] },
                                            "role": { "type": "string", "enum": ["web", "api", "worker", "cron", "static"] },
                                            "stack": { "type": "string" },
                                            "runtime": { "type": "string", "enum": ["preset", "dockerfile", "image"] },
                                            "image": { "type": "string" },
                                            "engine": { "type": "string", "enum": ["postgres", "redis", "mongodb", "mariadb", "s3"] },
                                            "holds": { "type": "array", "items": { "type": "string" } },
                                            "provider": { "type": "string" }
                                        },
                                        "required": ["id", "name", "purpose", "kind"]
                                    }
                                },
                                "flows": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "from": { "type": "string" },
                                            "to": { "type": "string" },
                                            "kind": { "type": "string", "enum": ["http", "websocket", "query", "publish", "subscribe", "cache", "blob", "webhook"] },
                                            "label": { "type": "string", "description": "What travels: \"POST /api/orders\", \"stream order.created\", \"tables orders, users\"." }
                                        },
                                        "required": ["from", "to", "kind", "label"]
                                    }
                                }
                            },
                            "required": ["summary", "components", "flows"]
                        },
                        "change_summary": { "type": "string", "description": "One line: what this version is or changes, e.g. \"First design\" or \"Add a Redis cache for sessions\"." },
                        "workspace_name": { "type": "string", "description": "Name for the workspace when the chat is not in one yet, e.g. the application's name." }
                    },
                    "required": ["document", "change_summary"]
                }),
            },
            ChatTool {
                name: BIND_BLUEPRINT_TOOL.to_string(),
                description: "Record which Temps project builds an `app` part of the accepted \
                    architecture, or which service runs a `data` part, so the architecture \
                    diagram shows it live. Call it right after you create the project or \
                    service. Pass `component_id` and exactly one of `project_id` (for an app) or \
                    `service_id` (for data). The project is also added to the workspace."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "component_id": { "type": "string", "description": "The part's id in the accepted architecture." },
                        "project_id": { "type": "integer" },
                        "service_id": { "type": "integer" }
                    },
                    "required": ["component_id"]
                }),
            },
        ]
    }

    async fn load_chat(
        &self,
        conversation_id: i64,
        user_id: i32,
    ) -> Result<ai_conversations::Model, String> {
        match ai_conversations::Entity::find_by_id(conversation_id)
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.workspaces.db.as_ref())
            .await
        {
            Ok(Some(conversation)) => Ok(conversation),
            Ok(None) => Err("This chat could not be found.".to_string()),
            Err(error) => Err(format!("Could not read this chat: {error}")),
        }
    }

    /// Run one blueprint tool call and return what the model reads back.
    pub async fn dispatch(
        &self,
        name: &str,
        arguments: &str,
        conversation_id: i64,
        auth: &AuthContext,
    ) -> String {
        let Some(user_id) = auth.user_id_opt() else {
            return format!("`{name}` needs a signed-in user.");
        };
        if !auth.has_permission(&Permission::ProjectsWrite) {
            return format!(
                "`{name}` is not available: it requires the {} permission.",
                Permission::ProjectsWrite
            );
        }
        let conversation = match self.load_chat(conversation_id, user_id).await {
            Ok(conversation) => conversation,
            Err(message) => return message,
        };
        match name {
            PROPOSE_BLUEPRINT_TOOL => self.propose(arguments, &conversation, user_id, auth).await,
            BIND_BLUEPRINT_TOOL => self.bind(arguments, &conversation, user_id, auth).await,
            other => format!("Unknown blueprint tool '{other}'."),
        }
    }

    async fn propose(
        &self,
        arguments: &str,
        conversation: &ai_conversations::Model,
        user_id: i32,
        auth: &AuthContext,
    ) -> String {
        let args: ProposeArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => {
                return format!(
                    "`{PROPOSE_BLUEPRINT_TOOL}` arguments do not match the document shape: {error}. Fix them and call it again."
                )
            }
        };
        // Validate before creating a workspace, so a bad document leaves nothing behind.
        if let Err(error) = crate::blueprint::validate(0, &args.document) {
            return match error {
                BlueprintError::Invalid { problems, .. } => format!(
                    "The architecture was not proposed. Fix every problem and call `{PROPOSE_BLUEPRINT_TOOL}` again:\n- {}",
                    problems.join("\n- ")
                ),
                other => format!("The architecture was not proposed: {other}"),
            };
        }
        let (application, created) = match self.workspaces.workspace_of(conversation).await {
            Ok(Some(application)) => (application, false),
            Ok(None) => {
                if !auth.has_permission(&Permission::SandboxesWrite) {
                    return format!(
                        "This chat is not in a workspace, and creating one requires the {} permission. Ask the user to put the chat in a workspace from the chat's header.",
                        Permission::SandboxesWrite
                    );
                }
                let name = args
                    .workspace_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .or_else(|| conversation.title.clone())
                    .unwrap_or_else(|| "New application".to_string());
                match self
                    .workspaces
                    .join(
                        conversation,
                        user_id,
                        JoinTarget::Create {
                            name,
                            description: Some(args.document.summary.clone()),
                        },
                        KeepSandbox::Chat,
                    )
                    .await
                {
                    Ok(joined) => joined,
                    Err(error) => {
                        return format!(
                            "The architecture was not proposed: the chat could not be put in a workspace: {error}"
                        )
                    }
                }
            }
            Err(error) => return format!("The architecture was not proposed: {error}"),
        };
        if created {
            self.audit_joined(user_id, conversation, &application).await;
        }
        let version = match self
            .workspaces
            .blueprints
            .propose(
                application.id,
                user_id,
                Some(conversation.id),
                args.document,
                &args.change_summary,
            )
            .await
        {
            Ok(version) => version,
            Err(BlueprintError::Invalid { problems, .. }) => {
                return format!(
                    "The architecture was not proposed. Fix every problem and call `{PROPOSE_BLUEPRINT_TOOL}` again:\n- {}",
                    problems.join("\n- ")
                )
            }
            Err(error) => return format!("The architecture was not proposed: {error}"),
        };
        if let Some(audit) = &self.audit {
            let entry = crate::audit::AiBlueprintVersionCreatedAudit {
                context: assistant_audit_context(user_id),
                application_id: application.public_id.clone(),
                version: version.version,
                status: "proposed".to_string(),
                conversation_id: Some(conversation.public_id.clone()),
                change_summary: version.change_summary.clone(),
            };
            if let Err(error) = audit.create_audit_log(&entry).await {
                warn!(application_id = %application.public_id, version = version.version, "failed to audit a blueprint proposal: {error}");
            }
        }
        json!({
            "status": "proposed",
            "application_id": application.public_id,
            "workspace_name": application.name,
            "workspace_created": created,
            "version": version.version,
            "change_summary": version.change_summary,
            "document": version.document,
            "next": "The user is reviewing this architecture now. End your turn: say in one or two sentences why it is shaped this way, then wait. Do not build until they accept it; if they ask for changes, propose a new version."
        })
        .to_string()
    }

    async fn audit_joined(
        &self,
        user_id: i32,
        conversation: &ai_conversations::Model,
        application: &ai_applications::Model,
    ) {
        let Some(audit) = &self.audit else {
            return;
        };
        let entry = crate::audit::AiChatWorkspaceJoinedAudit {
            context: assistant_audit_context(user_id),
            conversation_id: conversation.public_id.clone(),
            application_id: application.public_id.clone(),
            created: true,
        };
        if let Err(error) = audit.create_audit_log(&entry).await {
            warn!(conversation_id = %conversation.public_id, "failed to audit the workspace the assistant created: {error}");
        }
    }

    async fn project_allowed(&self, auth: &AuthContext, project_id: i32) -> bool {
        if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
            return true;
        }
        match (&self.project_access, auth.user_id_opt()) {
            (Some(checker), Some(user_id)) => checker
                .user_can_access_project(user_id, project_id)
                .await
                .unwrap_or(false),
            (None, Some(_)) => true,
            (_, None) => false,
        }
    }

    /// Link `project_id` into the workspace, or, when it is already linked,
    /// check that the user may still change it. Returns the message for the
    /// model when the project may not or could not be added.
    async fn add_project(
        &self,
        auth: &AuthContext,
        application_id: &str,
        project_id: i32,
    ) -> Result<(), String> {
        let previous = self
            .workspaces
            .applications
            .get(auth.user_id(), application_id)
            .await
            .map_err(|error| format!("Could not read the workspace: {error}"))?;
        let checker = self.project_access.as_ref();
        if previous
            .projects
            .iter()
            .any(|project| project.id == project_id)
        {
            return self
                .linker
                .authorize(auth, checker, &previous, project_id)
                .await
                .map_err(|error| format!("Project {project_id} cannot be bound: {error}."));
        }
        match self
            .linker
            .link(auth, checker, &previous, project_id)
            .await
        {
            Ok(_) => Ok(()),
            Err(LinkProjectError::Permission(error)) => Err(format!(
                "Project {project_id} cannot be added to this workspace: {error}. Nothing was bound."
            )),
            Err(error) => {
                warn!(
                    application_id,
                    project_id,
                    "could not add a bound project to the workspace: {error}"
                );
                Err(format!(
                    "Project {project_id} could not be added to this workspace, so nothing was bound: {error}"
                ))
            }
        }
    }

    async fn bind(
        &self,
        arguments: &str,
        conversation: &ai_conversations::Model,
        user_id: i32,
        auth: &AuthContext,
    ) -> String {
        let args: BindArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => {
                return format!("`{BIND_BLUEPRINT_TOOL}` arguments are not valid: {error}")
            }
        };
        let application = match self.workspaces.workspace_of(conversation).await {
            Ok(Some(application)) => application,
            Ok(None) => {
                return format!(
                    "This chat is not in a workspace with an accepted architecture. Propose one with `{PROPOSE_BLUEPRINT_TOOL}` first."
                )
            }
            Err(error) => return format!("Could not read the workspace: {error}"),
        };
        let db = self.workspaces.db.as_ref();
        match (args.project_id, args.service_id) {
            (Some(project_id), None) => {
                let exists = projects::Entity::find_by_id(project_id)
                    .filter(projects::Column::IsDeleted.eq(false))
                    .one(db)
                    .await;
                match exists {
                    Ok(Some(_)) if self.project_allowed(auth, project_id).await => {}
                    Ok(_) => return format!("Project {project_id} was not found."),
                    Err(error) => return format!("Could not read project {project_id}: {error}"),
                }
            }
            (None, Some(service_id)) => {
                if !auth.has_permission(&Permission::ExternalServicesRead) {
                    return format!(
                        "Binding a service requires the {} permission.",
                        Permission::ExternalServicesRead
                    );
                }
                match external_services::Entity::find_by_id(service_id).one(db).await {
                    Ok(Some(_)) => {}
                    Ok(None) => return format!("Service {service_id} was not found."),
                    Err(error) => return format!("Could not read service {service_id}: {error}"),
                }
            }
            _ => {
                return "Pass exactly one of `project_id` (for an app part) or `service_id` (for a data part).".to_string()
            }
        }
        // Add the project to the workspace before recording the binding, so a
        // refused or failed link leaves the architecture unchanged.
        if let Some(project_id) = args.project_id {
            if let Err(problem) = self
                .add_project(auth, &application.public_id, project_id)
                .await
            {
                return problem;
            }
        }
        let binding = ComponentBinding {
            project_id: args.project_id,
            service_id: args.service_id,
        };
        if let Err(error) = self
            .workspaces
            .blueprints
            .bind(application.id, &args.component_id, Some(binding))
            .await
        {
            return match error {
                BlueprintError::Invalid { problems, .. } => problems.join("; "),
                other => other.to_string(),
            };
        }
        if let Some(audit) = &self.audit {
            let entry = crate::audit::AiBlueprintBindingChangedAudit {
                context: assistant_audit_context(user_id),
                application_id: application.public_id.clone(),
                component_id: args.component_id.clone(),
                project_id: args.project_id,
                service_id: args.service_id,
                actor: "assistant".to_string(),
            };
            if let Err(error) = audit.create_audit_log(&entry).await {
                warn!(application_id = %application.public_id, "failed to audit a blueprint binding: {error}");
            }
        }
        let what = match (args.project_id, args.service_id) {
            (Some(project_id), _) => format!("project {project_id}"),
            (_, Some(service_id)) => format!("service {service_id}"),
            _ => String::new(),
        };
        json!({
            "status": "bound",
            "application_id": application.public_id,
            "component_id": args.component_id,
            "project_id": args.project_id,
            "service_id": args.service_id,
            "message": format!("Part '{}' is now shown as {what} on the architecture.", args.component_id),
        })
        .to_string()
    }
}

/// What kinds of part bind to what, for messages.
pub fn binding_target(kind: ComponentKind) -> Option<&'static str> {
    match kind {
        ComponentKind::App => Some("project"),
        ComponentKind::Data => Some("service"),
        ComponentKind::Client | ComponentKind::External => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_model(context_type: &str, project_id: Option<i32>) -> ai_conversations::Model {
        let now = chrono::Utc::now();
        ai_conversations::Model {
            id: 1,
            public_id: "conv_join".to_string(),
            project_id,
            application_id: None,
            context_type: context_type.to_string(),
            context_id: "ctx_1".to_string(),
            title: None,
            status: "active".to_string(),
            kind: "ai".to_string(),
            created_by: 1,
            metadata: None,
            created_at: now,
            last_activity_at: now,
            ai_provider: "gateway_key:1".to_string(),
            ai_model: "model".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "auto".to_string(),
            cli_session_id: None,
            cli_session_fingerprint: None,
            turn_status: "idle".to_string(),
            active_turn_id: None,
            last_turn_id: None,
            turn_started_at: None,
            next_step_suggestions: None,
        }
    }

    #[test]
    fn only_assistant_chats_without_a_project_can_join_a_workspace() {
        assert!(WorkspaceChatService::is_joinable(&chat_model(
            ASSISTANT_CONTEXT_TYPE,
            None
        )));
        assert!(!WorkspaceChatService::is_joinable(&chat_model(
            ASSISTANT_CONTEXT_TYPE,
            Some(4)
        )));
        assert!(!WorkspaceChatService::is_joinable(&chat_model(
            "deployment",
            None
        )));
        let refused = WorkspaceChatService::ensure_joinable(&chat_model("deployment", Some(4)));
        assert!(matches!(
            refused,
            Err(WorkspaceChatError::NotAssistantChat { ref conversation_id, .. })
                if conversation_id == "conv_join"
        ));
    }

    #[test]
    fn the_shared_scope_only_reaches_assistant_siblings_of_the_same_owner() {
        let scope = metadata_scope("$2", true);
        assert!(scope.starts_with("(id = $2 OR id IN ("));
        assert!(scope.contains("sibling.application_id = chat.application_id"));
        assert!(scope.contains("sibling.created_by = chat.created_by"));
        assert!(scope.contains("chat.context_type = 'assistant'"));
        assert!(scope.contains("sibling.context_type = 'assistant'"));
        assert_eq!(metadata_scope("$3", false), "id = $3");
    }

    #[test]
    fn an_autofix_incident_chat_neither_shares_nor_receives_the_workspace_sandbox() {
        let scope = metadata_scope("$2", true);
        assert!(scope.contains("chat.metadata -> 'autofix_incident' IS NULL"));
        assert!(scope.contains("sibling.metadata -> 'autofix_incident' IS NULL"));
    }

    #[test]
    fn the_tools_are_named_and_described() {
        let tools = BlueprintToolSupport::tools();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec![PROPOSE_BLUEPRINT_TOOL, BIND_BLUEPRINT_TOOL]);
        assert!(is_blueprint_tool(PROPOSE_BLUEPRINT_TOOL));
        assert!(!is_blueprint_tool("temps_write"));
        let propose = &tools[0];
        assert_eq!(
            propose.parameters["required"],
            json!(["document", "change_summary"])
        );
        assert!(propose.description.contains("wait for the user's decision"));
    }

    #[test]
    fn only_a_stored_proposal_is_a_receipt() {
        assert!(is_proposal_receipt(r#"{"status":"proposed","version":1}"#));
        assert!(!is_proposal_receipt(
            "The architecture was not proposed. Fix every problem"
        ));
        assert!(!is_proposal_receipt(r#"{"status":"bound"}"#));
    }

    #[test]
    fn only_runnable_parts_bind() {
        assert_eq!(binding_target(ComponentKind::App), Some("project"));
        assert_eq!(binding_target(ComponentKind::Data), Some("service"));
        assert_eq!(binding_target(ComponentKind::Client), None);
    }

    use crate::blueprint::{Decision, VersionStatus};
    use sea_orm::{ActiveModelTrait, Set};
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    async fn database(test: &str) -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                println!("Skipping {test}: {error}");
                None
            }
            Err(error) => panic!("{test}: database setup failed: {error}"),
        }
    }

    async fn user(db: &DatabaseConnection) -> i32 {
        db.query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "INSERT INTO users (name, email, email_verified, must_change_password, mfa_enabled, \
             created_at, updated_at) VALUES ('Builder', 'builder@example.test', true, false, \
             false, NOW(), NOW()) RETURNING id",
        ))
        .await
        .expect("insert user")
        .expect("a row")
        .try_get::<i32>("", "id")
        .expect("id")
    }

    async fn chat(
        db: &DatabaseConnection,
        user_id: i32,
        public_id: &str,
        sandbox: Option<&str>,
    ) -> ai_conversations::Model {
        let now = chrono::Utc::now();
        let metadata = match sandbox {
            Some(id) => json!({
                "sandbox_link": { "sandbox_id": id, "name": id, "linked_at": now },
                "sandbox_preview_port": 3000,
                "focus_project_id": 4
            }),
            None => json!({}),
        };
        ai_conversations::ActiveModel {
            public_id: Set(public_id.to_string()),
            project_id: Set(None),
            application_id: Set(None),
            context_type: Set(ASSISTANT_CONTEXT_TYPE.to_string()),
            context_id: Set(format!("assistant_{public_id}")),
            title: Set(Some(public_id.to_string())),
            status: Set("active".to_string()),
            created_by: Set(user_id),
            metadata: Set(Some(metadata)),
            created_at: Set(now),
            last_activity_at: Set(now),
            ai_provider: Set("gateway".to_string()),
            ai_model: Set("example/model".to_string()),
            ai_thinking_level: Set(None),
            ai_permission_mode: Set("confirm".to_string()),
            cli_session_id: Set(None),
            cli_session_fingerprint: Set(None),
            turn_status: Set("idle".to_string()),
            active_turn_id: Set(None),
            last_turn_id: Set(None),
            turn_started_at: Set(None),
            next_step_suggestions: Set(None),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert chat")
    }

    async fn reload(
        db: &DatabaseConnection,
        chat: &ai_conversations::Model,
    ) -> ai_conversations::Model {
        ai_conversations::Entity::find_by_id(chat.id)
            .one(db)
            .await
            .expect("read chat")
            .expect("chat exists")
    }

    fn linked_sandbox(chat: &ai_conversations::Model) -> Option<String> {
        chat.metadata
            .as_ref()
            .and_then(|metadata| metadata.get(SANDBOX_LINK_KEY))
            .and_then(|link| link.get("sandbox_id"))
            .and_then(|id| id.as_str())
            .map(str::to_string)
    }

    fn design(extra_part: bool) -> BlueprintDocument {
        let mut components = vec![
            json!({ "id": "browser", "name": "Browser", "purpose": "Where people use it.", "kind": "client", "surface": "browser" }),
            json!({ "id": "web", "name": "Web", "purpose": "Pages and API.", "kind": "app", "role": "web", "stack": "Next.js", "runtime": "preset" }),
            json!({ "id": "db", "name": "Database", "purpose": "Keeps orders.", "kind": "data", "engine": "postgres", "holds": ["orders"] }),
        ];
        let mut flows = vec![
            json!({ "from": "browser", "to": "web", "kind": "http", "label": "pages" }),
            json!({ "from": "web", "to": "db", "kind": "query", "label": "tables orders" }),
        ];
        if extra_part {
            components.push(json!({ "id": "cache", "name": "Cache", "purpose": "Keeps sessions.", "kind": "data", "engine": "redis", "holds": ["sessions:*"] }));
            flows.push(
                json!({ "from": "web", "to": "cache", "kind": "cache", "label": "sessions" }),
            );
        }
        serde_json::from_value(json!({
            "summary": "A small shop.",
            "components": components,
            "flows": flows
        }))
        .expect("a document")
    }

    /// The chats of a workspace share one sandbox: joining adopts the
    /// workspace's, `keep_sandbox: chat` moves everyone to the joining chat's,
    /// and leaving drops only the leaver's link.
    #[tokio::test]
    async fn chats_in_a_workspace_share_one_sandbox() {
        let Some(test_db) = database("chats_in_a_workspace_share_one_sandbox").await else {
            return;
        };
        let db = test_db.connection_arc();
        let owner = user(&db).await;
        let applications = Arc::new(ApplicationService::new(db.clone()));
        let service =
            WorkspaceChatService::new(db.clone(), applications, BlueprintService::new(db.clone()));

        let first = chat(&db, owner, "conv_first", Some("sbx_one")).await;
        let (workspace, created) = service
            .join(
                &first,
                owner,
                JoinTarget::Create {
                    name: "Shop".into(),
                    description: None,
                },
                KeepSandbox::Workspace,
            )
            .await
            .expect("create and join");
        assert!(created);

        // A chat with no sandbox adopts the workspace's, preview port included.
        let second = chat(&db, owner, "conv_second", None).await;
        service
            .join(
                &second,
                owner,
                JoinTarget::Existing(workspace.public_id.clone()),
                KeepSandbox::Workspace,
            )
            .await
            .expect("join");
        let second = reload(&db, &second).await;
        assert_eq!(linked_sandbox(&second).as_deref(), Some("sbx_one"));
        assert_eq!(
            second.metadata.as_ref().unwrap()[SANDBOX_PREVIEW_KEY],
            json!(3000)
        );

        // A chat that keeps its own sandbox moves the whole workspace to it,
        // and other metadata keys are left alone.
        let third = chat(&db, owner, "conv_third", Some("sbx_three")).await;
        service
            .join(
                &third,
                owner,
                JoinTarget::Existing(workspace.public_id.clone()),
                KeepSandbox::Chat,
            )
            .await
            .expect("join keeping the chat's sandbox");
        for chat in [&first, &second, &third] {
            let chat = reload(&db, chat).await;
            assert_eq!(
                linked_sandbox(&chat).as_deref(),
                Some("sbx_three"),
                "{}",
                chat.public_id
            );
        }
        assert_eq!(
            reload(&db, &first).await.metadata.unwrap()["focus_project_id"],
            json!(4)
        );

        // Linking from any chat is a shared write.
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE ai_conversations SET metadata = metadata || jsonb_build_object($1::text, $2::jsonb) WHERE {}",
                metadata_scope("$3", true)
            ),
            [SANDBOX_LINK_KEY.into(), json!({ "sandbox_id": "sbx_four" }).into(), second.id.into()],
        ))
        .await
        .expect("shared write");
        assert_eq!(
            linked_sandbox(&reload(&db, &first).await).as_deref(),
            Some("sbx_four")
        );

        // Leaving drops only the leaver's link.
        let third = reload(&db, &third).await;
        service.leave(&third).await.expect("leave");
        let third = reload(&db, &third).await;
        assert_eq!(third.application_id, None);
        assert_eq!(linked_sandbox(&third), None);
        assert_eq!(
            linked_sandbox(&reload(&db, &first).await).as_deref(),
            Some("sbx_four")
        );

        let summary = service
            .summary(&reload(&db, &first).await)
            .await
            .expect("summary")
            .expect("in a workspace");
        assert_eq!(summary.application.name, "Shop");
        assert_eq!(summary.chats.len(), 2);

        // A chat that is not in a workspace cannot leave one, naming the chat.
        let error = service.leave(&third).await.expect_err("not in a workspace");
        assert!(error.to_string().contains("conv_third"), "{error}");
    }

    /// Chat lists name a chat's workspace only while the workspace is active.
    #[tokio::test]
    async fn chat_lists_leave_out_an_archived_workspace() {
        let Some(test_db) = database("chat_lists_leave_out_an_archived_workspace").await else {
            return;
        };
        let db = test_db.connection_arc();
        let owner = user(&db).await;
        let applications = Arc::new(ApplicationService::new(db.clone()));
        let service =
            WorkspaceChatService::new(db.clone(), applications, BlueprintService::new(db.clone()));

        let first = chat(&db, owner, "conv_first", None).await;
        service
            .join(
                &first,
                owner,
                JoinTarget::Create {
                    name: "Shop".into(),
                    description: None,
                },
                KeepSandbox::Workspace,
            )
            .await
            .expect("create and join");
        let application_id = reload(&db, &first)
            .await
            .application_id
            .expect("in a workspace");

        let names = workspace_names(&db, &[application_id])
            .await
            .expect("names");
        assert_eq!(
            names.get(&application_id).map(|(_, name)| name.as_str()),
            Some("Shop")
        );

        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE ai_applications SET status = 'archived' WHERE id = $1",
            [application_id.into()],
        ))
        .await
        .expect("archive");
        let names = workspace_names(&db, &[application_id])
            .await
            .expect("names");
        assert!(names.is_empty(), "{names:?}");
    }

    /// A sandbox the workspace stops using is only let go when no chat at all
    /// links to it any more: a chat outside the workspace keeps it in use.
    #[tokio::test]
    async fn a_sandbox_is_unused_once_no_chat_links_to_it() {
        let Some(test_db) = database("a_sandbox_is_unused_once_no_chat_links_to_it").await else {
            return;
        };
        let db = test_db.connection_arc();
        let owner = user(&db).await;
        let applications = Arc::new(ApplicationService::new(db.clone()));
        let service =
            WorkspaceChatService::new(db.clone(), applications, BlueprintService::new(db.clone()));

        let first = chat(&db, owner, "conv_first", Some("sbx_one")).await;
        let (workspace, _) = service
            .join(
                &first,
                owner,
                JoinTarget::Create {
                    name: "Shop".into(),
                    description: None,
                },
                KeepSandbox::Workspace,
            )
            .await
            .expect("create and join");
        let _outside = chat(&db, owner, "conv_outside", Some("sbx_shared")).await;

        // Joining with a sandbox another chat still uses frees nothing.
        let shared = chat(&db, owner, "conv_shared", Some("sbx_shared")).await;
        service
            .join(
                &shared,
                owner,
                JoinTarget::Existing(workspace.public_id.clone()),
                KeepSandbox::Workspace,
            )
            .await
            .expect("join");
        let shared = reload(&db, &shared).await;
        assert_eq!(linked_sandbox(&shared).as_deref(), Some("sbx_one"));
        let unused = service
            .unused_sandboxes(&shared, vec!["sbx_one".into(), "sbx_shared".into()])
            .await
            .expect("unused");
        assert!(unused.is_empty(), "{unused:?}");

        // Joining with a sandbox nobody else has leaves it unused.
        let alone = chat(&db, owner, "conv_alone", Some("sbx_alone")).await;
        service
            .join(
                &alone,
                owner,
                JoinTarget::Existing(workspace.public_id.clone()),
                KeepSandbox::Workspace,
            )
            .await
            .expect("join");
        let alone = reload(&db, &alone).await;
        let mut linked = service.linked_sandboxes(&alone).await.expect("linked");
        linked.sort();
        assert_eq!(linked, vec!["sbx_one".to_string()]);
        let unused = service
            .unused_sandboxes(&alone, vec!["sbx_one".into(), "sbx_alone".into()])
            .await
            .expect("unused");
        assert_eq!(unused, vec!["sbx_alone".to_string()]);
    }

    /// Proposals supersede each other, a decision is final, a user version is
    /// accepted as it is saved, and bindings name parts of the accepted design.
    #[tokio::test]
    async fn blueprint_versions_are_proposed_decided_and_bound() {
        let Some(test_db) = database("blueprint_versions_are_proposed_decided_and_bound").await
        else {
            return;
        };
        let db = test_db.connection_arc();
        let owner = user(&db).await;
        let applications = Arc::new(ApplicationService::new(db.clone()));
        let workspace = applications
            .create(owner, "Shop", None, &[])
            .await
            .expect("workspace")
            .application;
        let blueprints = BlueprintService::new(db.clone());
        let chat = chat(&db, owner, "conv_design", None).await;

        let first = blueprints
            .propose(
                workspace.id,
                owner,
                Some(chat.id),
                design(false),
                "First design",
            )
            .await
            .expect("propose v1");
        assert_eq!((first.version, first.status), (1, VersionStatus::Proposed));
        let second = blueprints
            .propose(
                workspace.id,
                owner,
                Some(chat.id),
                design(true),
                "Add a cache",
            )
            .await
            .expect("propose v2");
        assert_eq!(second.version, 2);

        // Only the newest proposal is open.
        let error = blueprints
            .decide(workspace.id, 1, owner, Decision::Accept)
            .await
            .expect_err("v1 was superseded");
        assert!(
            matches!(error, BlueprintError::NotOpen { version: 1, ref status, .. } if status == "superseded")
        );

        let accepted = blueprints
            .decide(workspace.id, 2, owner, Decision::Accept)
            .await
            .expect("accept v2");
        assert_eq!(accepted.status, VersionStatus::Accepted);
        assert!(matches!(
            blueprints
                .decide(workspace.id, 2, owner, Decision::Reject)
                .await,
            Err(BlueprintError::NotOpen { version: 2, .. })
        ));
        assert!(matches!(
            blueprints
                .decide(workspace.id, 9, owner, Decision::Accept)
                .await,
            Err(BlueprintError::VersionNotFound { version: 9, .. })
        ));

        // Bindings name parts of the accepted design, of the right kind.
        assert!(matches!(
            blueprints.bind(workspace.id, "queue", Some(ComponentBinding { project_id: None, service_id: Some(1) })).await,
            Err(BlueprintError::UnknownComponent { ref component_id, .. }) if component_id == "queue"
        ));
        assert!(matches!(
            blueprints
                .bind(
                    workspace.id,
                    "db",
                    Some(ComponentBinding {
                        project_id: Some(1),
                        service_id: None
                    })
                )
                .await,
            Err(BlueprintError::Invalid { .. })
        ));
        let bindings = blueprints
            .bind(
                workspace.id,
                "cache",
                Some(ComponentBinding {
                    project_id: None,
                    service_id: Some(424_242),
                }),
            )
            .await
            .expect("bind the cache");
        let live = blueprints
            .live(workspace.id, &bindings)
            .await
            .expect("live");
        assert_eq!(live["cache"].status, crate::blueprint::LiveStatus::Missing);

        // A version the user writes is accepted as it is saved.
        let edited = blueprints
            .save_user_version(workspace.id, owner, design(false), "Drop the cache")
            .await
            .expect("save");
        assert_eq!(
            (edited.version, edited.status),
            (3, VersionStatus::Accepted)
        );
        let state = blueprints.state(workspace.id).await.expect("state");
        assert_eq!(state.accepted.map(|v| v.version), Some(3));
        assert!(state.proposal.is_none());
        let (versions, total) = blueprints
            .versions(workspace.id, 1, 20)
            .await
            .expect("versions");
        assert_eq!(total, 3);
        assert_eq!(
            versions
                .iter()
                .map(|v| (v.version, v.status))
                .collect::<Vec<_>>(),
            vec![
                (3, VersionStatus::Accepted),
                (2, VersionStatus::Superseded),
                (1, VersionStatus::Superseded)
            ]
        );

        // An invalid document is refused before anything is stored.
        let mut broken = design(false);
        broken.flows[1].kind = crate::blueprint::FlowKind::Cache;
        assert!(matches!(
            blueprints
                .propose(workspace.id, owner, None, broken, "Broken")
                .await,
            Err(BlueprintError::Invalid { .. })
        ));
        assert_eq!(blueprints.versions(workspace.id, 1, 20).await.unwrap().1, 3);
    }

    #[test]
    fn keep_sandbox_defaults_to_the_workspace() {
        assert_eq!(KeepSandbox::default(), KeepSandbox::Workspace);
        let parsed: KeepSandbox = serde_json::from_value(json!("chat")).unwrap();
        assert_eq!(parsed, KeepSandbox::Chat);
    }
}
