//! The general project-assistant context provider.
//!
//! Unlike the deployment/alert providers (each anchored to one failed entity),
//! this seeds a free-form chat scoped to a whole project — "ask the AI anything
//! about this project." `context_id` is an opaque per-thread id (a client-
//! generated uuid), so a project can have MANY independent chat threads.
//!
//! It exposes no provider-specific tools, but the shared, project-scoped trace
//! tools (`list_traces` / `get_trace`) are merged into every chat by
//! `ConversationService`, so a project chat can inspect distributed traces out
//! of the box.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};

use temps_entities::projects;

use crate::provider::{ConversationContextProvider, ConversationSeed};

const SYSTEM_PREAMBLE: &str = "You are an expert SRE/DevOps assistant embedded in the Temps platform, helping a developer with \
their project. Answer questions and help debug issues across the project's deployments, build/runtime logs, distributed \
traces, metrics, and errors. For example, inspect recent traces to find failing or slow requests, then drill into a \
specific trace.";

/// Seeds general, project-scoped assistant chats.
pub struct ProjectChatProvider {
    db: Arc<DatabaseConnection>,
}

impl ProjectChatProvider {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ConversationContextProvider for ProjectChatProvider {
    fn context_type(&self) -> &'static str {
        "project"
    }

    async fn seed(&self, project_id: i32, _context_id: &str) -> Option<ConversationSeed> {
        // The context_id is an opaque thread id; the seed depends only on the
        // project. Look it up both to scope the chat and to frame the prompt.
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .ok()??;

        let mut ctx = String::new();
        ctx.push_str(SYSTEM_PREAMBLE);
        ctx.push_str("\n\n");
        ctx.push_str(crate::provider::TOOL_USAGE_GUIDANCE);
        ctx.push_str("\n\n--- Project ---\n");
        ctx.push_str(&format!("Name: {}\n", project.name));
        ctx.push_str(&format!(
            "Repository: {}/{}\n",
            project.repo_owner, project.repo_name
        ));
        ctx.push_str(&format!("Default branch: {}\n", project.main_branch));

        Some(ConversationSeed {
            system: ctx,
            first_assistant: None,
            title: Some("Project chat".to_string()),
            metadata: Some(serde_json::json!({ "project_id": project_id })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn project_model(id: i32, name: &str) -> projects::Model {
        let now = chrono::Utc::now();
        projects::Model {
            id,
            name: name.to_string(),
            repo_name: "repo".to_string(),
            repo_owner: "owner".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Static,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "slug".to_string(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: Some(true),
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
        }
    }

    #[tokio::test]
    async fn test_seed_present_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![project_model(7, "Acme")]])
            .into_connection();
        let provider = ProjectChatProvider::new(Arc::new(db));

        let seed = provider
            .seed(7, "any-uuid")
            .await
            .expect("a known project should seed");
        assert_eq!(seed.title.as_deref(), Some("Project chat"));
        assert!(seed.system.contains("Acme"));
        assert!(seed.system.contains("owner/repo"));
        // No first assistant turn — the user drives a general chat.
        assert!(seed.first_assistant.is_none());
    }

    #[tokio::test]
    async fn test_seed_absent_project_is_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<projects::Model>::new()])
            .into_connection();
        let provider = ProjectChatProvider::new(Arc::new(db));
        assert!(provider.seed(999, "any-uuid").await.is_none());
    }
}
