// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "project_agents")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    /// "yaml" (synced from .temps/agents/) or "dashboard" (created in UI)
    pub source: String,
    pub enabled: bool,
    /// Trigger configuration as JSON: { "error": { "new_issue": true }, "schedule": { "cron": "..." }, "manual": true }
    #[sea_orm(column_type = "JsonBinary")]
    pub trigger_config: serde_json::Value,
    /// Custom prompt with template variables: {{error_type}}, {{error_message}}, {{stack_trace}}, etc.
    /// NULL means use the built-in prompt for the trigger type.
    pub prompt: Option<String>,
    pub ai_provider: String,
    /// Preferred model for this agent (e.g. "sonnet", "gpt-5-codex").
    /// NULL means: let the CLI pick its default.
    pub ai_model: Option<String>,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub ai_provider_key_id: Option<i32>,
    pub max_turns: i32,
    pub timeout_seconds: i32,
    pub daily_budget_cents: i32,
    pub cooldown_minutes: i32,
    pub branch_prefix: String,
    /// "pull_request", "commit", or "none"
    pub deliverable: String,
    /// Whether to run this agent inside an isolated sandbox container.
    /// None = use global default, Some(true) = force on, Some(false) = force off.
    pub sandbox_enabled: Option<bool>,
    /// MCP servers config as JSON — Claude Code settings.json mcpServers format.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub mcp_servers_config: Option<serde_json::Value>,
    /// Skills config as JSON array.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub skills_config: Option<serde_json::Value>,
    /// Tools config as JSON array.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub tools_config: Option<serde_json::Value>,
    /// Private config repo containing .claude/ directory (skills, MCP, plugins).
    /// Format: "owner/repo" (e.g. "myorg/claude-config").
    pub config_repo_url: Option<String>,
    /// Branch of the config repo to use (default: "main").
    pub config_repo_branch: Option<String>,
    /// Short non-secret ID used in the webhook URL path.
    /// Safe to log. Auto-generated when `on: { webhook: true }` is set.
    pub webhook_id: Option<String>,
    /// Secret token validated via `X-Webhook-Token` header.
    /// Never exposed in URLs. Auto-generated alongside `webhook_id`.
    pub webhook_token: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::ai_provider_keys::Entity",
        from = "Column::AiProviderKeyId",
        to = "super::ai_provider_keys::Column::Id",
        on_delete = "SetNull"
    )]
    AiProviderKey,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::ai_provider_keys::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AiProviderKey.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }
        Ok(self)
    }
}
