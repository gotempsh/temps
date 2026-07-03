use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

use super::deployment_config::DeploymentConfig;
use super::preset::{Preset, PresetConfig};
use super::source_type::SourceType;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "projects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    /// Repository name (required)
    pub repo_name: String,
    /// Repository owner/namespace (required)
    pub repo_owner: String,
    pub directory: String,
    pub main_branch: String,
    /// Preset/framework type (required - every project must have a preset)
    pub preset: Preset,
    /// Preset-specific configuration (e.g., NextJsConfig with custom build commands)
    /// This is typed based on the preset enum variant
    pub preset_config: Option<PresetConfig>,
    /// Deployment configuration (CPU, memory, port, analytics, auto-deploy settings, security)
    /// These serve as defaults for all environments unless overridden
    /// Security settings are in deployment_config.security
    pub deployment_config: Option<DeploymentConfig>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub slug: String,
    pub is_deleted: bool,
    pub deleted_at: Option<DBDateTime>,
    pub last_deployment: Option<DBDateTime>,
    pub is_public_repo: bool,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    /// Attack mode - when enabled, requires CAPTCHA verification for all visitors
    pub attack_mode: bool,
    /// ADR-021 Tier 2: opt-in AI summarization of metric alert notifications for
    /// this project. NULL/false = off (the deterministic Tier-1 text is used);
    /// true = enrich with the configured AI provider when one is available.
    pub ai_alert_summaries_enabled: Option<bool>,
    /// ADR-023: opt-in AI debugging chat (e.g. on deployment failures) for this
    /// project. NULL/false = off; true = offer the chat when AI is configured.
    pub ai_debug_chat_enabled: Option<bool>,
    /// Opt-in for the AI propose-then-confirm write-action feature. When false
    /// (the default), the AI may only read data; write-action proposals are
    /// suppressed. Operators enable this per-project via the UI.
    pub ai_write_actions_enabled: bool,
    /// Enable automatic preview environment creation for each branch
    pub enable_preview_environments: bool,
    /// When true, preview environments auto-created for branches are
    /// configured in on-demand mode (containers stopped after
    /// `preview_envs_idle_timeout_seconds` of inactivity).
    /// Only applies to newly-created previews.
    #[sea_orm(default_value = "false")]
    pub preview_envs_on_demand: bool,
    /// Idle timeout (seconds) applied to on-demand preview environments.
    /// Only used when `preview_envs_on_demand` is true.
    #[sea_orm(default_value = "300")]
    pub preview_envs_idle_timeout_seconds: i32,
    /// Wake timeout (seconds) applied to on-demand preview environments.
    /// Only used when `preview_envs_on_demand` is true.
    #[sea_orm(default_value = "30")]
    pub preview_envs_wake_timeout_seconds: i32,
    /// Source type - determines how deployments are triggered and executed
    /// Defaults to 'git' for backward compatibility
    #[sea_orm(default_value = "git")]
    pub source_type: SourceType,
    /// GitLab webhook ID returned by POST /projects/:id/hooks when we auto-install
    /// the webhook on repo connect. NULL when not connected to a GitLab repository.
    pub gitlab_webhook_id: Option<i32>,
    /// Encrypted signing token we generated and sent to GitLab as `signing_token`
    /// when creating the webhook. Used to validate incoming webhook payloads.
    /// Never serialized in API responses.
    #[serde(skip_serializing)]
    pub gitlab_webhook_signing_token: Option<String>,
    /// Encrypted HMAC-SHA256 signing token for Gitea webhook signature verification.
    /// Sent as the `secret` field when creating the Gitea repo hook.
    /// Never serialized in API responses (MUST-FIX 5).
    #[serde(skip_serializing)]
    pub gitea_webhook_signing_token: Option<String>,
    /// Encrypted secret-in-path token for Bitbucket webhook delivery URL.
    /// Embedded in the webhook callback URL path instead of HMAC (Bitbucket
    /// Cloud does not provide HMAC body signing).
    /// Never serialized in API responses (MUST-FIX 5).
    #[serde(skip_serializing)]
    pub bitbucket_webhook_token: Option<String>,
    /// The hook UUID returned by Bitbucket when we auto-registered the webhook
    /// via `POST /2.0/repositories/{workspace}/{slug}/hooks`. Stored so we can
    /// `DELETE` it on disconnect. Bitbucket UUIDs include braces: `{uuid-v4}`.
    /// NULL when no auto-registered hook exists for this project.
    pub bitbucket_webhook_hook_id: Option<String>,
    /// Encrypted secret-in-path token for Generic/Manual git provider webhook URL.
    /// Embedded in the webhook callback URL path.
    /// Never serialized in API responses (MUST-FIX 5).
    #[serde(skip_serializing)]
    pub generic_webhook_token: Option<String>,
    /// ADR-027 Phase 3: whether this project's traces appear in cross-project
    /// discovery results. TRUE (the default) is consistent with the OSS
    /// global-observability model where any OtelRead holder can query any
    /// project's telemetry. Operators who want private-by-default can set this
    /// to FALSE; cross-project links to this project will then be suppressed.
    #[sea_orm(default_value = "true")]
    pub cross_project_trace_sharing: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::env_vars::Entity")]
    EnvVars,
    #[sea_orm(has_many = "super::environments::Entity")]
    Environments,
    #[sea_orm(
        belongs_to = "super::git_provider_connections::Entity",
        from = "Column::GitProviderConnectionId",
        to = "super::git_provider_connections::Column::Id"
    )]
    GitProviderConnection,
}

impl Related<super::env_vars::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EnvVars.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environments.def()
    }
}

impl Related<super::git_provider_connections::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::GitProviderConnection.def()
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
