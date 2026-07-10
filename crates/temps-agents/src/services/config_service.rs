use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_core::AgentYamlConfig;
use temps_entities::{project_agents, projects, settings};

use crate::error::AgentError;

/// Result of a YAML sync operation.
pub struct SyncResult {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct UpsertAgentRequest {
    pub slug: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub ai_provider: Option<String>,
    /// Preferred model identifier for the CLI. `Some("")` clears the stored value.
    pub ai_model: Option<String>,
    /// Plain-text API key — will be encrypted before storage
    pub api_key: Option<String>,
    pub ai_provider_key_id: Option<i32>,
    pub daily_budget_cents: Option<i32>,
    pub max_turns: Option<i32>,
    pub cooldown_minutes: Option<i32>,
    /// Trigger configuration JSON: { "error": { "new_issue": true, "regression": true }, "manual": true }
    pub trigger_config: Option<serde_json::Value>,
    pub prompt: Option<String>,
    pub timeout_seconds: Option<i32>,
    pub deliverable: Option<String>,
    pub branch_prefix: Option<String>,
    pub sandbox_enabled: Option<bool>,
    /// Private config repo containing .claude/ directory (skills, MCP, plugins).
    pub config_repo_url: Option<String>,
    /// Branch of the config repo to use (default: "main").
    pub config_repo_branch: Option<String>,
    /// MCP servers config (Claude Code settings.json mcpServers format).
    pub mcp_servers_config: Option<serde_json::Value>,
    /// Skills config as JSON array.
    pub skills_config: Option<serde_json::Value>,
    /// Tools config as JSON array.
    pub tools_config: Option<serde_json::Value>,
}

/// Generate a short non-secret webhook ID (8 bytes = 16 hex chars) for the URL path.
fn generate_webhook_id() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Generate a cryptographically random webhook token (32 bytes = 64 hex chars) for header auth.
fn generate_webhook_token() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Check if webhook trigger is enabled in a trigger_config JSON value.
fn is_webhook_enabled(trigger_config: &serde_json::Value) -> bool {
    trigger_config
        .get("webhook")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub struct AgentConfigService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl AgentConfigService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Read the platform's configured default AI provider from agent_sandbox settings.
    /// Falls back to `"claude_cli"` when no settings exist yet.
    async fn platform_default_provider(&self) -> String {
        let record = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten();

        record
            .as_ref()
            .and_then(|r| r.data.get("agent_sandbox"))
            .and_then(|v| v.get("default_provider"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("claude_cli")
            .to_string()
    }

    /// Get the first agent for a project (backward compat — prefer list_agents or get_agent_by_id)
    pub async fn get_config(
        &self,
        project_id: i32,
    ) -> Result<Option<project_agents::Model>, AgentError> {
        let config = project_agents::Entity::find()
            .filter(project_agents::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(config)
    }

    /// Get a specific agent by ID
    pub async fn get_agent_by_id(
        &self,
        agent_id: i32,
    ) -> Result<Option<project_agents::Model>, AgentError> {
        project_agents::Entity::find_by_id(agent_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// List all agents for a project
    pub async fn list_agents(
        &self,
        project_id: i32,
    ) -> Result<Vec<project_agents::Model>, AgentError> {
        project_agents::Entity::find()
            .filter(project_agents::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// List all enabled agents across all projects.
    /// Used by the cron scheduler to check schedules.
    pub async fn list_all_enabled_agents(&self) -> Result<Vec<project_agents::Model>, AgentError> {
        project_agents::Entity::find()
            .filter(project_agents::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// List enabled agents that match a trigger type for a project.
    /// Checks the `trigger_config` JSONB for matching trigger types.
    pub async fn list_agents_for_trigger(
        &self,
        project_id: i32,
        trigger_type: &str,
    ) -> Result<Vec<project_agents::Model>, AgentError> {
        let all_agents = self.list_agents(project_id).await?;
        Ok(all_agents
            .into_iter()
            .filter(|a| {
                if !a.enabled {
                    return false;
                }
                match trigger_type {
                    "new_issue" => a
                        .trigger_config
                        .get("error")
                        .and_then(|e| e.get("new_issue"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    "regression" => a
                        .trigger_config
                        .get("error")
                        .and_then(|e| e.get("regression"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    "monitoring_downtime" => a
                        .trigger_config
                        .get("monitoring")
                        .and_then(|m| m.get("downtime"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    "monitoring_latency_spike" => a
                        .trigger_config
                        .get("monitoring")
                        .and_then(|m| m.get("latency_spike"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    "deploy_production" => a
                        .trigger_config
                        .get("deploy")
                        .and_then(|d| d.get("production"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    "deploy_preview" => a
                        .trigger_config
                        .get("deploy")
                        .and_then(|d| d.get("preview"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    "manual" => a
                        .trigger_config
                        .get("manual")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                    "schedule" => a
                        .trigger_config
                        .get("schedule")
                        .and_then(|s| s.get("cron"))
                        .and_then(|v| v.as_str())
                        .is_some(),
                    "webhook" => a
                        .trigger_config
                        .get("webhook")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    _ => false,
                }
            })
            .collect())
    }

    pub async fn upsert_config(
        &self,
        project_id: i32,
        request: UpsertAgentRequest,
    ) -> Result<project_agents::Model, AgentError> {
        // Validate: project must have a git provider connection with write access.
        // Autopilot pushes branches and creates PRs — it cannot work with public repos
        // or read-only connections.
        if request.enabled.unwrap_or(false) {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await
                .map_err(AgentError::Database)?
                .ok_or(AgentError::ProjectNotFound { project_id })?;

            if project.git_provider_connection_id.is_none() {
                return Err(AgentError::Validation {
                    message: format!(
                        "Autopilot requires a git provider connection with write access (GitHub App or PAT). \
                         Project {} has no git provider connected.",
                        project_id
                    ),
                });
            }
        }

        // Encrypt the API key if provided
        let encrypted_key = if let Some(ref raw_key) = request.api_key {
            if raw_key.is_empty() {
                return Err(AgentError::Validation {
                    message: "API key cannot be empty".to_string(),
                });
            }
            let encrypted = self
                .encryption_service
                .encrypt_string(raw_key)
                .map_err(|e| AgentError::EncryptionError {
                    message: format!(
                        "Failed to encrypt API key for project {}: {}",
                        project_id, e
                    ),
                })?;
            Some(encrypted)
        } else {
            None
        };

        let existing = self.get_config(project_id).await?;

        if let Some(existing_config) = existing {
            // Update existing config
            let mut active: project_agents::ActiveModel = existing_config.into();

            if let Some(enabled) = request.enabled {
                active.enabled = Set(enabled);
            }
            if let Some(provider) = request.ai_provider {
                if provider.is_empty() {
                    return Err(AgentError::Validation {
                        message: "AI provider cannot be empty".to_string(),
                    });
                }
                active.ai_provider = Set(provider);
            }
            if let Some(model) = request.ai_model {
                // Empty string is the convention for "clear the stored model"
                active.ai_model = Set(if model.is_empty() { None } else { Some(model) });
            }
            if let Some(encrypted) = encrypted_key {
                active.api_key_encrypted = Set(Some(encrypted));
            }
            if let Some(key_id) = request.ai_provider_key_id {
                active.ai_provider_key_id = Set(Some(key_id));
            }
            if let Some(budget) = request.daily_budget_cents {
                if budget < 0 {
                    return Err(AgentError::Validation {
                        message: "Daily budget cannot be negative".to_string(),
                    });
                }
                active.daily_budget_cents = Set(budget);
            }
            if let Some(max_turns) = request.max_turns {
                if max_turns < 1 {
                    return Err(AgentError::Validation {
                        message: "max_turns must be at least 1".to_string(),
                    });
                }
                active.max_turns = Set(max_turns);
            }
            if let Some(cooldown) = request.cooldown_minutes {
                if cooldown < 0 {
                    return Err(AgentError::Validation {
                        message: "Cooldown minutes cannot be negative".to_string(),
                    });
                }
                active.cooldown_minutes = Set(cooldown);
            }
            if let Some(trigger_config) = request.trigger_config {
                active.trigger_config = Set(trigger_config);
            }
            if let Some(prompt) = request.prompt {
                active.prompt = Set(Some(prompt));
            }
            if let Some(timeout_seconds) = request.timeout_seconds {
                active.timeout_seconds = Set(timeout_seconds);
            }
            if let Some(deliverable) = request.deliverable {
                active.deliverable = Set(deliverable);
            }
            if let Some(slug) = request.slug {
                active.slug = Set(slug);
            }
            if let Some(name) = request.name {
                active.name = Set(name);
            }
            if let Some(description) = request.description {
                active.description = Set(Some(description));
            }
            if let Some(prefix) = request.branch_prefix {
                active.branch_prefix = Set(prefix);
            }
            if let Some(sandbox_enabled) = request.sandbox_enabled {
                active.sandbox_enabled = Set(Some(sandbox_enabled));
            }
            if let Some(ref config_repo_url) = request.config_repo_url {
                active.config_repo_url = Set(Some(config_repo_url.clone()));
            }
            if let Some(ref config_repo_branch) = request.config_repo_branch {
                active.config_repo_branch = Set(Some(config_repo_branch.clone()));
            }

            let model = active
                .update(self.db.as_ref())
                .await
                .map_err(AgentError::Database)?;

            Ok(model)
        } else {
            // Insert new config with defaults
            let default_trigger_config = serde_json::json!({
                "error": { "new_issue": true, "regression": true },
                "manual": true
            });
            let trigger_config = request.trigger_config.unwrap_or(default_trigger_config);
            let (webhook_id, webhook_token) = if is_webhook_enabled(&trigger_config) {
                (Some(generate_webhook_id()), Some(generate_webhook_token()))
            } else {
                (None, None)
            };
            let default_provider = self.platform_default_provider().await;
            let active = project_agents::ActiveModel {
                project_id: Set(project_id),
                slug: Set(request
                    .slug
                    .unwrap_or_else(|| format!("agent-{}", project_id))),
                name: Set(request.name.unwrap_or_else(|| "Default Agent".to_string())),
                description: Set(request.description),
                source: Set("dashboard".to_string()),
                enabled: Set(request.enabled.unwrap_or(false)),
                trigger_config: Set(trigger_config),
                prompt: Set(request.prompt),
                ai_provider: Set(request.ai_provider.unwrap_or(default_provider)),
                ai_model: Set(request.ai_model.filter(|m| !m.is_empty())),
                api_key_encrypted: Set(encrypted_key),
                ai_provider_key_id: Set(request.ai_provider_key_id),
                max_turns: Set(request.max_turns.unwrap_or(10)),
                timeout_seconds: Set(request.timeout_seconds.unwrap_or(600)),
                daily_budget_cents: Set(request.daily_budget_cents.unwrap_or(500)),
                cooldown_minutes: Set(request.cooldown_minutes.unwrap_or(60)),
                branch_prefix: Set(request.branch_prefix.unwrap_or_default()),
                deliverable: Set(request
                    .deliverable
                    .unwrap_or_else(|| "pull_request".to_string())),
                sandbox_enabled: Set(request.sandbox_enabled),
                config_repo_url: Set(request.config_repo_url),
                config_repo_branch: Set(request.config_repo_branch),
                webhook_id: Set(webhook_id),
                webhook_token: Set(webhook_token),
                ..Default::default()
            };

            let model = active
                .insert(self.db.as_ref())
                .await
                .map_err(AgentError::Database)?;

            Ok(model)
        }
    }

    pub async fn delete_config(&self, project_id: i32) -> Result<(), AgentError> {
        let config = self
            .get_config(project_id)
            .await?
            .ok_or(AgentError::ConfigNotFound { project_id })?;

        let active: project_agents::ActiveModel = config.into();
        active
            .delete(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(())
    }

    /// Find an agent by its webhook ID (non-secret, used in URL path).
    /// Used by the public webhook trigger endpoint.
    pub async fn get_agent_by_webhook_id(
        &self,
        webhook_id: &str,
    ) -> Result<Option<project_agents::Model>, AgentError> {
        project_agents::Entity::find()
            .filter(project_agents::Column::WebhookId.eq(webhook_id))
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// Get a specific agent by slug for a project.
    pub async fn get_agent_by_slug(
        &self,
        project_id: i32,
        slug: &str,
    ) -> Result<Option<project_agents::Model>, AgentError> {
        project_agents::Entity::find()
            .filter(project_agents::Column::ProjectId.eq(project_id))
            .filter(project_agents::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// Create a new agent for a project (dashboard source).
    pub async fn create_agent(
        &self,
        project_id: i32,
        request: UpsertAgentRequest,
    ) -> Result<project_agents::Model, AgentError> {
        let slug = match &request.slug {
            Some(s) if !s.is_empty() => s.clone(),
            _ => {
                return Err(AgentError::Validation {
                    message: "Agent slug is required".to_string(),
                })
            }
        };
        let name = match &request.name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => {
                return Err(AgentError::Validation {
                    message: "Agent name is required".to_string(),
                })
            }
        };

        // Validate git connection if enabled
        if request.enabled.unwrap_or(false) {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await
                .map_err(AgentError::Database)?
                .ok_or(AgentError::ProjectNotFound { project_id })?;

            if project.git_provider_connection_id.is_none() {
                return Err(AgentError::Validation {
                    message: format!(
                        "Autopilot requires a git provider connection with write access (GitHub App or PAT). \
                         Project {} has no git provider connected.",
                        project_id
                    ),
                });
            }
        }

        // Encrypt the API key if provided
        let encrypted_key = if let Some(ref raw_key) = request.api_key {
            if raw_key.is_empty() {
                return Err(AgentError::Validation {
                    message: "API key cannot be empty".to_string(),
                });
            }
            let encrypted = self
                .encryption_service
                .encrypt_string(raw_key)
                .map_err(|e| AgentError::EncryptionError {
                    message: format!(
                        "Failed to encrypt API key for project {}: {}",
                        project_id, e
                    ),
                })?;
            Some(encrypted)
        } else {
            None
        };

        let default_trigger_config = serde_json::json!({
            "error": { "new_issue": true, "regression": true },
            "manual": true
        });

        let trigger_config = request.trigger_config.unwrap_or(default_trigger_config);

        // Auto-generate webhook token if webhook trigger is enabled
        let (webhook_id, webhook_token) = if is_webhook_enabled(&trigger_config) {
            (Some(generate_webhook_id()), Some(generate_webhook_token()))
        } else {
            (None, None)
        };

        let default_provider = self.platform_default_provider().await;
        let active = project_agents::ActiveModel {
            project_id: Set(project_id),
            slug: Set(slug),
            name: Set(name),
            description: Set(request.description),
            source: Set("dashboard".to_string()),
            enabled: Set(request.enabled.unwrap_or(false)),
            trigger_config: Set(trigger_config),
            prompt: Set(request.prompt),
            ai_provider: Set(request.ai_provider.unwrap_or(default_provider)),
            ai_model: Set(request.ai_model.filter(|m| !m.is_empty())),
            api_key_encrypted: Set(encrypted_key),
            ai_provider_key_id: Set(request.ai_provider_key_id),
            max_turns: Set(request.max_turns.unwrap_or(25)),
            timeout_seconds: Set(request.timeout_seconds.unwrap_or(600)),
            daily_budget_cents: Set(request.daily_budget_cents.unwrap_or(500)),
            cooldown_minutes: Set(request.cooldown_minutes.unwrap_or(30)),
            branch_prefix: Set(request
                .branch_prefix
                .unwrap_or_else(|| "agents/".to_string())),
            deliverable: Set(request
                .deliverable
                .unwrap_or_else(|| "pull_request".to_string())),
            sandbox_enabled: Set(request.sandbox_enabled),
            config_repo_url: Set(request.config_repo_url),
            config_repo_branch: Set(request.config_repo_branch),
            mcp_servers_config: Set(request.mcp_servers_config),
            skills_config: Set(request.skills_config),
            tools_config: Set(request.tools_config),
            webhook_id: Set(webhook_id),
            webhook_token: Set(webhook_token),
            ..Default::default()
        };

        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(model)
    }

    /// Update an agent identified by slug for a project.
    pub async fn update_agent(
        &self,
        project_id: i32,
        slug: &str,
        request: UpsertAgentRequest,
    ) -> Result<project_agents::Model, AgentError> {
        let existing = self
            .get_agent_by_slug(project_id, slug)
            .await?
            .ok_or_else(|| AgentError::AgentNotFound {
                project_id,
                slug: slug.to_string(),
            })?;

        let existing_has_webhook = existing.webhook_id.is_some();

        // Validate git connection if being enabled
        if request.enabled.unwrap_or(false) && !existing.enabled {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await
                .map_err(AgentError::Database)?
                .ok_or(AgentError::ProjectNotFound { project_id })?;

            if project.git_provider_connection_id.is_none() {
                return Err(AgentError::Validation {
                    message: format!(
                        "Autopilot requires a git provider connection with write access (GitHub App or PAT). \
                         Project {} has no git provider connected.",
                        project_id
                    ),
                });
            }
        }

        // Encrypt the API key if provided
        let encrypted_key = if let Some(ref raw_key) = request.api_key {
            if raw_key.is_empty() {
                return Err(AgentError::Validation {
                    message: "API key cannot be empty".to_string(),
                });
            }
            let encrypted = self
                .encryption_service
                .encrypt_string(raw_key)
                .map_err(|e| AgentError::EncryptionError {
                    message: format!(
                        "Failed to encrypt API key for project {}: {}",
                        project_id, e
                    ),
                })?;
            Some(encrypted)
        } else {
            None
        };

        let mut active: project_agents::ActiveModel = existing.into();

        if let Some(enabled) = request.enabled {
            active.enabled = Set(enabled);
        }
        if let Some(provider) = request.ai_provider {
            if provider.is_empty() {
                return Err(AgentError::Validation {
                    message: "AI provider cannot be empty".to_string(),
                });
            }
            active.ai_provider = Set(provider);
        }
        if let Some(model) = request.ai_model {
            active.ai_model = Set(if model.is_empty() { None } else { Some(model) });
        }
        if let Some(encrypted) = encrypted_key {
            active.api_key_encrypted = Set(Some(encrypted));
        }
        if let Some(key_id) = request.ai_provider_key_id {
            active.ai_provider_key_id = Set(Some(key_id));
        }
        if let Some(budget) = request.daily_budget_cents {
            if budget < 0 {
                return Err(AgentError::Validation {
                    message: "Daily budget cannot be negative".to_string(),
                });
            }
            active.daily_budget_cents = Set(budget);
        }
        if let Some(max_turns) = request.max_turns {
            if max_turns < 1 {
                return Err(AgentError::Validation {
                    message: "max_turns must be at least 1".to_string(),
                });
            }
            active.max_turns = Set(max_turns);
        }
        if let Some(cooldown) = request.cooldown_minutes {
            if cooldown < 0 {
                return Err(AgentError::Validation {
                    message: "Cooldown minutes cannot be negative".to_string(),
                });
            }
            active.cooldown_minutes = Set(cooldown);
        }
        if let Some(trigger_config) = request.trigger_config {
            // Auto-manage webhook ID + token when trigger_config changes
            if is_webhook_enabled(&trigger_config) {
                if !existing_has_webhook {
                    active.webhook_id = Set(Some(generate_webhook_id()));
                    active.webhook_token = Set(Some(generate_webhook_token()));
                }
            } else {
                active.webhook_id = Set(None);
                active.webhook_token = Set(None);
            }
            active.trigger_config = Set(trigger_config);
        }
        if let Some(prompt) = request.prompt {
            active.prompt = Set(Some(prompt));
        }
        if let Some(timeout_seconds) = request.timeout_seconds {
            active.timeout_seconds = Set(timeout_seconds);
        }
        if let Some(deliverable) = request.deliverable {
            active.deliverable = Set(deliverable);
        }
        if let Some(new_slug) = request.slug {
            active.slug = Set(new_slug);
        }
        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(description) = request.description {
            active.description = Set(Some(description));
        }
        if let Some(prefix) = request.branch_prefix {
            active.branch_prefix = Set(prefix);
        }
        if let Some(sandbox_enabled) = request.sandbox_enabled {
            active.sandbox_enabled = Set(Some(sandbox_enabled));
        }
        if let Some(ref config_repo_url) = request.config_repo_url {
            active.config_repo_url = Set(Some(config_repo_url.clone()));
        }
        if let Some(ref config_repo_branch) = request.config_repo_branch {
            active.config_repo_branch = Set(Some(config_repo_branch.clone()));
        }
        if let Some(mcp) = request.mcp_servers_config {
            active.mcp_servers_config = Set(Some(mcp));
        }
        if let Some(skills) = request.skills_config {
            active.skills_config = Set(Some(skills));
        }
        if let Some(tools) = request.tools_config {
            active.tools_config = Set(Some(tools));
        }

        let model = active
            .update(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(model)
    }

    /// Delete an agent identified by slug for a project.
    pub async fn delete_agent(&self, project_id: i32, slug: &str) -> Result<(), AgentError> {
        let agent = self
            .get_agent_by_slug(project_id, slug)
            .await?
            .ok_or_else(|| AgentError::AgentNotFound {
                project_id,
                slug: slug.to_string(),
            })?;

        let active: project_agents::ActiveModel = agent.into();
        active
            .delete(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(())
    }

    /// Sync agents from parsed YAML configuration.
    /// YAML is the source of truth:
    /// - Upserts all agents present in `yaml_agents` (creates or updates, regardless of source)
    /// - Deletes ALL agents not present in the YAML (including dashboard-created ones)
    pub async fn sync_agents_from_yaml(
        &self,
        project_id: i32,
        yaml_agents: Vec<AgentYamlConfig>,
    ) -> Result<SyncResult, AgentError> {
        // Sandbox is mandatory. Reject any YAML that tries to opt out so the
        // author gets a clear error instead of a silently-ignored toggle.
        for a in &yaml_agents {
            if a.sandbox == Some(false) {
                return Err(AgentError::Validation {
                    message: format!(
                        "Agent '{}' sets `sandbox: false`. Sandbox execution is mandatory — remove the field or set `sandbox: true`.",
                        a.slug()
                    ),
                });
            }
        }

        // 1. Load ALL existing agents for this project (not just yaml-sourced)
        let existing = project_agents::Entity::find()
            .filter(project_agents::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        let mut created = 0usize;
        let mut updated = 0usize;
        let mut deleted = 0usize;

        // Read platform default once — used when YAML doesn't specify a provider
        let platform_provider = self.platform_default_provider().await;

        let yaml_slugs: Vec<String> = yaml_agents.iter().map(|a| a.slug()).collect();

        // 2. Upsert each YAML agent
        for yaml_agent in &yaml_agents {
            let slug = yaml_agent.slug();
            if let Some(existing_agent) = existing.iter().find(|e| e.slug == slug) {
                // Update (also converts dashboard agents to yaml-sourced)
                let mut active: project_agents::ActiveModel = existing_agent.clone().into();
                active.source = Set("yaml".to_string());
                active.name = Set(yaml_agent.name.clone());
                active.description = Set(yaml_agent.description.clone());
                active.trigger_config = Set(yaml_agent.trigger_config_json());
                active.prompt = Set(yaml_agent.resolved_prompt().map(|s| s.to_string()));
                active.ai_provider = Set(yaml_agent
                    .resolved_provider()
                    .unwrap_or(&platform_provider)
                    .to_string());
                active.ai_model = Set(yaml_agent.resolved_model().map(|s| s.to_string()));
                active.max_turns = Set(yaml_agent.max_turns);
                active.timeout_seconds = Set(yaml_agent.timeout_seconds);
                active.daily_budget_cents = Set(yaml_agent.daily_budget_cents);
                active.cooldown_minutes = Set(yaml_agent.cooldown_minutes);
                active.branch_prefix = Set(yaml_agent.branch_prefix.clone());
                active.deliverable = Set(yaml_agent.deliverable.clone());
                active.sandbox_enabled = Set(yaml_agent.sandbox);
                active.mcp_servers_config = Set(yaml_agent.mcp_servers_json());
                active.skills_config = Set(yaml_agent.skills_config_json());
                active.tools_config = Set(yaml_agent.tools_config_json());
                active.config_repo_url = Set(yaml_agent.config_repo.clone());
                active.config_repo_branch = Set(yaml_agent.config_repo_branch.clone());
                active.enabled = Set(yaml_agent.enabled);
                // Auto-manage webhook ID + token
                if yaml_agent.on.webhook {
                    if existing_agent.webhook_id.is_none() {
                        active.webhook_id = Set(Some(generate_webhook_id()));
                        active.webhook_token = Set(Some(generate_webhook_token()));
                    }
                } else {
                    active.webhook_id = Set(None);
                    active.webhook_token = Set(None);
                }
                active
                    .update(self.db.as_ref())
                    .await
                    .map_err(AgentError::Database)?;
                updated += 1;
            } else {
                // Insert
                let (webhook_id, webhook_token) = if yaml_agent.on.webhook {
                    (Some(generate_webhook_id()), Some(generate_webhook_token()))
                } else {
                    (None, None)
                };
                let active = project_agents::ActiveModel {
                    project_id: Set(project_id),
                    slug: Set(slug),
                    name: Set(yaml_agent.name.clone()),
                    description: Set(yaml_agent.description.clone()),
                    source: Set("yaml".to_string()),
                    enabled: Set(yaml_agent.enabled),
                    trigger_config: Set(yaml_agent.trigger_config_json()),
                    prompt: Set(yaml_agent.resolved_prompt().map(|s| s.to_string())),
                    ai_provider: Set(yaml_agent
                        .resolved_provider()
                        .unwrap_or(&platform_provider)
                        .to_string()),
                    ai_model: Set(yaml_agent.resolved_model().map(|s| s.to_string())),
                    max_turns: Set(yaml_agent.max_turns),
                    timeout_seconds: Set(yaml_agent.timeout_seconds),
                    daily_budget_cents: Set(yaml_agent.daily_budget_cents),
                    cooldown_minutes: Set(yaml_agent.cooldown_minutes),
                    branch_prefix: Set(yaml_agent.branch_prefix.clone()),
                    deliverable: Set(yaml_agent.deliverable.clone()),
                    sandbox_enabled: Set(yaml_agent.sandbox),
                    mcp_servers_config: Set(yaml_agent.mcp_servers_json()),
                    skills_config: Set(yaml_agent.skills_config_json()),
                    tools_config: Set(yaml_agent.tools_config_json()),
                    config_repo_url: Set(yaml_agent.config_repo.clone()),
                    config_repo_branch: Set(yaml_agent.config_repo_branch.clone()),
                    webhook_id: Set(webhook_id),
                    webhook_token: Set(webhook_token),
                    ..Default::default()
                };
                active
                    .insert(self.db.as_ref())
                    .await
                    .map_err(AgentError::Database)?;
                created += 1;
            }
        }

        // 3. Delete agents not present in the YAML (YAML is source of truth)
        for existing_agent in &existing {
            if !yaml_slugs.contains(&existing_agent.slug) {
                let active: project_agents::ActiveModel = existing_agent.clone().into();
                active
                    .delete(self.db.as_ref())
                    .await
                    .map_err(AgentError::Database)?;
                deleted += 1;
            }
        }

        Ok(SyncResult {
            created,
            updated,
            deleted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_config(project_id: i32) -> project_agents::Model {
        project_agents::Model {
            id: 1,
            project_id,
            slug: "default-agent".to_string(),
            name: "Default Agent".to_string(),
            description: None,
            source: "dashboard".to_string(),
            enabled: true,
            trigger_config: serde_json::json!({
                "error": { "new_issue": true, "regression": true },
                "manual": true
            }),
            prompt: None,
            ai_provider: "claude_cli".to_string(),
            ai_model: None,
            api_key_encrypted: None,
            ai_provider_key_id: None,
            max_turns: 10,
            timeout_seconds: 600,
            daily_budget_cents: 500,
            cooldown_minutes: 60,
            branch_prefix: String::new(),
            deliverable: "pull_request".to_string(),
            sandbox_enabled: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
            webhook_id: None,
            webhook_token: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_encryption_service() -> Arc<temps_core::EncryptionService> {
        Arc::new(
            temps_core::EncryptionService::new(
                "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
            )
            .expect("valid test key"),
        )
    }

    #[tokio::test]
    async fn test_get_config_returns_some_when_exists() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_config(42)]])
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let result = svc.get_config(42).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_get_config_returns_none_when_missing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_agents::Model>::new()])
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let result = svc.get_config(99).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_delete_config_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_agents::Model>::new()])
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let result = svc.delete_config(99).await;
        assert!(matches!(
            result.unwrap_err(),
            AgentError::ConfigNotFound { project_id: 99 }
        ));
    }

    fn make_project(id: i32, has_git: bool) -> projects::Model {
        projects::Model {
            id,
            name: "test".into(),
            repo_name: "repo".into(),
            repo_owner: "owner".into(),
            directory: ".".into(),
            main_branch: "main".into(),
            preset: temps_entities::preset::Preset::NextJs,
            preset_config: None,
            deployment_config: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: "test".into(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: if has_git { Some(1) } else { None },
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            cross_project_trace_sharing: true,
        }
    }

    #[tokio::test]
    async fn test_upsert_validation_empty_api_key() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // project lookup (enabled=true triggers git check)
            .append_query_results(vec![vec![make_project(1, true)]])
            // existing config lookup
            .append_query_results(vec![Vec::<project_agents::Model>::new()])
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let request = UpsertAgentRequest {
            enabled: Some(true),
            ai_provider: None,
            ai_model: None,
            api_key: Some(String::new()),
            ai_provider_key_id: None,
            daily_budget_cents: None,
            max_turns: None,
            cooldown_minutes: None,
            trigger_config: None,
            prompt: None,
            timeout_seconds: None,
            deliverable: None,
            slug: None,
            name: None,
            description: None,
            branch_prefix: None,
            sandbox_enabled: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
        };

        let result = svc.upsert_config(1, request).await;
        assert!(matches!(result.unwrap_err(), AgentError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_upsert_rejects_project_without_git_connection() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // project lookup — no git connection
            .append_query_results(vec![vec![make_project(1, false)]])
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let request = UpsertAgentRequest {
            enabled: Some(true),
            ai_provider: None,
            ai_model: None,
            api_key: None,
            ai_provider_key_id: None,
            daily_budget_cents: None,
            max_turns: None,
            cooldown_minutes: None,
            trigger_config: None,
            prompt: None,
            timeout_seconds: None,
            deliverable: None,
            slug: None,
            name: None,
            description: None,
            branch_prefix: None,
            sandbox_enabled: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
        };

        let result = svc.upsert_config(1, request).await;
        let err = result.unwrap_err();
        assert!(matches!(err, AgentError::Validation { .. }));
        assert!(
            err.to_string().contains("git provider connection"),
            "error should mention git provider: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_upsert_allows_disabled_config_without_git() {
        // When enabled=false, the git check is skipped
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // platform_default_provider() → settings lookup
            .append_query_results(vec![Vec::<settings::Model>::new()])
            // No existing config found
            .append_query_results(vec![Vec::<project_agents::Model>::new()])
            // insert returns model
            .append_query_results(vec![vec![make_config(1)]])
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let request = UpsertAgentRequest {
            enabled: Some(false),
            ai_provider: None,
            ai_model: None,
            api_key: None,
            ai_provider_key_id: None,
            daily_budget_cents: None,
            max_turns: None,
            cooldown_minutes: None,
            trigger_config: None,
            prompt: None,
            timeout_seconds: None,
            deliverable: None,
            slug: None,
            name: None,
            description: None,
            branch_prefix: None,
            sandbox_enabled: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
        };

        let result = svc.upsert_config(1, request).await;
        assert!(result.is_ok());
    }

    // ---------------------------------------------------------------------------
    // Feature: sandbox_enabled as Option<bool> in config service
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_upsert_sandbox_enabled_none_stored_correctly() {
        // sandbox_enabled: None means "use global default" — must be stored as NULL
        let mut expected = make_config(1);
        expected.sandbox_enabled = None; // model returned by mock DB reflects what was stored

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<settings::Model>::new()]) // platform_default_provider
            .append_query_results(vec![Vec::<project_agents::Model>::new()]) // no existing config
            .append_query_results(vec![vec![expected.clone()]]) // insert returns model
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let request = UpsertAgentRequest {
            enabled: Some(false),
            sandbox_enabled: None, // explicit None
            ai_provider: None,
            ai_model: None,
            api_key: None,
            ai_provider_key_id: None,
            daily_budget_cents: None,
            max_turns: None,
            cooldown_minutes: None,
            trigger_config: None,
            prompt: None,
            timeout_seconds: None,
            deliverable: None,
            slug: None,
            name: None,
            description: None,
            branch_prefix: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
        };

        let result = svc.upsert_config(1, request).await;
        assert!(
            result.is_ok(),
            "upsert with sandbox_enabled=None should succeed"
        );
        let model = result.unwrap();
        assert!(
            model.sandbox_enabled.is_none(),
            "sandbox_enabled should be None in the returned model"
        );
    }

    #[tokio::test]
    async fn test_upsert_sandbox_enabled_some_true_stored_correctly() {
        let mut expected = make_config(1);
        expected.sandbox_enabled = Some(true);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<settings::Model>::new()]) // platform_default_provider
            .append_query_results(vec![Vec::<project_agents::Model>::new()]) // no existing config
            .append_query_results(vec![vec![expected.clone()]]) // insert returns model
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let request = UpsertAgentRequest {
            enabled: Some(false),
            sandbox_enabled: Some(true),
            ai_provider: None,
            ai_model: None,
            api_key: None,
            ai_provider_key_id: None,
            daily_budget_cents: None,
            max_turns: None,
            cooldown_minutes: None,
            trigger_config: None,
            prompt: None,
            timeout_seconds: None,
            deliverable: None,
            slug: None,
            name: None,
            description: None,
            branch_prefix: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
        };

        let result = svc.upsert_config(1, request).await;
        assert!(
            result.is_ok(),
            "upsert with sandbox_enabled=Some(true) should succeed"
        );
        let model = result.unwrap();
        assert_eq!(
            model.sandbox_enabled,
            Some(true),
            "sandbox_enabled should be Some(true) in the returned model"
        );
    }

    #[tokio::test]
    async fn test_upsert_sandbox_enabled_some_false_stored_correctly() {
        let mut expected = make_config(1);
        expected.sandbox_enabled = Some(false);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<settings::Model>::new()]) // platform_default_provider
            .append_query_results(vec![Vec::<project_agents::Model>::new()]) // no existing config
            .append_query_results(vec![vec![expected.clone()]]) // insert returns model
            .into_connection();
        let svc = AgentConfigService::new(Arc::new(db), make_encryption_service());

        let request = UpsertAgentRequest {
            enabled: Some(false),
            sandbox_enabled: Some(false),
            ai_provider: None,
            ai_model: None,
            api_key: None,
            ai_provider_key_id: None,
            daily_budget_cents: None,
            max_turns: None,
            cooldown_minutes: None,
            trigger_config: None,
            prompt: None,
            timeout_seconds: None,
            deliverable: None,
            slug: None,
            name: None,
            description: None,
            branch_prefix: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
        };

        let result = svc.upsert_config(1, request).await;
        assert!(
            result.is_ok(),
            "upsert with sandbox_enabled=Some(false) should succeed"
        );
        let model = result.unwrap();
        assert_eq!(
            model.sandbox_enabled,
            Some(false),
            "sandbox_enabled should be Some(false) in the returned model"
        );
    }

    /// Verify the sandbox override logic in isolation:
    /// `config.sandbox_enabled.unwrap_or(global_sandbox.enabled)`
    #[test]
    fn test_sandbox_enabled_option_logic_all_combinations() {
        fn resolve(agent: Option<bool>, global: bool) -> bool {
            agent.unwrap_or(global)
        }
        // None + global=false → false
        assert!(!resolve(None, false));
        // None + global=true → true
        assert!(resolve(None, true));
        // Some(true) + global=false → true (per-agent overrides global)
        assert!(resolve(Some(true), false));
        // Some(true) + global=true → true
        assert!(resolve(Some(true), true));
        // Some(false) + global=false → false
        assert!(!resolve(Some(false), false));
        // Some(false) + global=true → false (per-agent overrides global)
        assert!(!resolve(Some(false), true));
    }
}
