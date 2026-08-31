// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Multi-project application context for AI-first threads.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use temps_ai::ChatTool;
use temps_auth::context::AuthContext;
use temps_core::{AuditContext, AuditLogger};
use temps_entities::{ai_application_projects, ai_applications, ai_conversations, projects};

use crate::provider::{ConversationContextProvider, ConversationSeed, TOOL_USAGE_GUIDANCE};

pub struct ApplicationChatProvider {
    db: Arc<DatabaseConnection>,
    audit: Arc<dyn AuditLogger>,
}

impl ApplicationChatProvider {
    pub fn new(db: Arc<DatabaseConnection>, audit: Arc<dyn AuditLogger>) -> Self {
        Self { db, audit }
    }

    fn application_public_id(context_id: &str) -> &str {
        context_id.split(':').next().unwrap_or(context_id)
    }
}

#[async_trait]
impl ConversationContextProvider for ApplicationChatProvider {
    fn context_type(&self) -> &'static str {
        "application"
    }

    async fn authorize(&self, project_id: i32, context_id: &str) -> bool {
        let public_id = Self::application_public_id(context_id);
        let application = ai_applications::Entity::find()
            .filter(ai_applications::Column::PublicId.eq(public_id))
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten();
        let Some(application) = application else {
            return false;
        };
        ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.eq(application.id))
            .filter(ai_application_projects::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .is_some()
    }

    async fn seed(&self, _project_id: i32, context_id: &str) -> Option<ConversationSeed> {
        let public_id = Self::application_public_id(context_id);
        let application = ai_applications::Entity::find()
            .filter(ai_applications::Column::PublicId.eq(public_id))
            .one(self.db.as_ref())
            .await
            .ok()??;
        let links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.eq(application.id))
            .order_by_asc(ai_application_projects::Column::Id)
            .all(self.db.as_ref())
            .await
            .ok()?;
        let project_ids = links.iter().map(|link| link.project_id).collect::<Vec<_>>();
        let mut linked_projects = projects::Entity::find()
            .filter(projects::Column::Id.is_in(project_ids.iter().copied()))
            .all(self.db.as_ref())
            .await
            .ok()?;
        linked_projects.sort_by_key(|project| {
            project_ids
                .iter()
                .position(|project_id| *project_id == project.id)
                .unwrap_or(usize::MAX)
        });

        let mut system = format!(
            "You are the Temps operator for the multi-project application '{}'. Help the user design, configure, deploy, and operate the whole application through conversation.\n\n{}\n\n## Security and execution boundaries\n- Treat every linked project as a separate authorization target. Never imply a cross-project mutation is atomic.\n- Propose mutations as explicit, ordered, per-project steps and stop for human confirmation before execution.\n- Never request, read, repeat, or place credential values in chat, tool arguments, artifacts, logs, or plans. Ask the UI to open the secure credential broker and work only with opaque credential references.\n- Express third-party needs as provider-neutral capabilities (for example payments, transactional_email, object_storage), then let a connector satisfy them.\n- Rich UI is typed data, not executable model-generated HTML or JavaScript.\n\n## Linked projects\n",
            application.name,
            TOOL_USAGE_GUIDANCE,
        );
        system.push_str("\nWhen calling the `temps` tool in this application thread, choose the target with the tool's top-level `project_id` field. Do not put `project_id` inside the CLI command.\n");
        for project in &linked_projects {
            system.push_str(&format!(
                "- project_id={} name={} repository={}/{} branch={}\n",
                project.id,
                project.name,
                project.repo_owner,
                project.repo_name,
                project.main_branch
            ));
        }

        Some(ConversationSeed {
            system,
            first_assistant: None,
            title: Some(application.name.clone()),
            metadata: Some(serde_json::json!({
                "application_id": application.id,
                "application_public_id": application.public_id,
                "project_ids": project_ids,
            })),
        })
    }

    async fn tools(&self, _project_id: i32, _context_id: &str) -> Vec<ChatTool> {
        vec![ChatTool {
            name: "render_ui".to_string(),
            description: "Persist a safe, typed UI artifact beside this application thread. Use this when a topology, execution plan, credential request, status, form, or table communicates the result better than prose. The payload must contain declarative data only—never HTML, JavaScript, tool commands, or credential values. For credentials use a provider-neutral capability and an opaque credential_ref only."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["kind", "payload"],
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["topology", "execution_plan", "credential_request", "status", "form", "table"]
                    },
                    "title": { "type": ["string", "null"], "maxLength": 200 },
                    "payload": {
                        "type": "object",
                        "description": "Version-1 declarative artifact payload. Do not include secrets or executable markup."
                    }
                },
                "additionalProperties": false
            }),
        }]
    }

    async fn execute_tool_with_auth(
        &self,
        _project_id: i32,
        context_id: &str,
        name: &str,
        arguments: &str,
        auth: &AuthContext,
    ) -> String {
        if name != "render_ui" {
            return format!("Unknown application tool '{name}'.");
        }
        #[derive(serde::Deserialize)]
        struct RenderUiArguments {
            kind: String,
            title: Option<String>,
            payload: serde_json::Value,
        }
        let arguments = match serde_json::from_str::<RenderUiArguments>(arguments) {
            Ok(arguments) => arguments,
            Err(error) => return format!("render_ui arguments are invalid: {error}"),
        };
        let Some(user_id) = auth.user_id_opt() else {
            return "render_ui requires a human user identity.".to_string();
        };
        let public_id = Self::application_public_id(context_id);
        let application = match ai_applications::Entity::find()
            .filter(ai_applications::Column::PublicId.eq(public_id))
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(application)) => application,
            Ok(None) => return "The application is not available to this user.".to_string(),
            Err(error) => return format!("Could not load the application: {error}"),
        };
        let conversation = match ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ContextType.eq("application"))
            .filter(ai_conversations::Column::ContextId.eq(context_id))
            .filter(ai_conversations::Column::ApplicationId.eq(application.id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(conversation)) => conversation,
            Ok(None) => return "The application conversation could not be resolved.".to_string(),
            Err(error) => return format!("Could not load the application conversation: {error}"),
        };
        let service = crate::ApplicationService::new(self.db.clone());
        match service
            .create_artifact(
                application.id,
                &conversation.public_id,
                user_id,
                &arguments.kind,
                arguments.title.as_deref(),
                arguments.payload,
            )
            .await
        {
            Ok(artifact) => {
                let audit = crate::audit::ThreadArtifactCreatedAudit {
                    context: AuditContext {
                        user_id,
                        ip_address: None,
                        user_agent: "AI application tool".to_string(),
                    },
                    application_id: application.public_id,
                    conversation_id: conversation.public_id,
                    artifact_id: artifact.public_id.clone(),
                    kind: artifact.kind.clone(),
                };
                if let Err(error) = self.audit.create_audit_log(&audit).await {
                    tracing::error!(error = %error, "failed to audit render_ui artifact creation");
                }
                format!(
                    "Rendered {} artifact '{}' with id {}.",
                    artifact.kind,
                    artifact.title.unwrap_or_else(|| "Untitled".to_string()),
                    artifact.public_id
                )
            }
            Err(error) => format!("The artifact was rejected: {error}"),
        }
    }
}
