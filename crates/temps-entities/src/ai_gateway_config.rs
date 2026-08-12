use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_gateway_config")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Scope: "instance", "project:{id}", "environment:{id}"
    pub scope: String,
    /// JSON array of allowed model IDs, NULL means all models allowed
    pub allowed_models: Option<serde_json::Value>,
    /// Max requests per minute for this scope, NULL means unlimited
    pub max_requests_per_minute: Option<i64>,
    /// Max spending per month in microcents, NULL means unlimited
    pub max_cost_per_month_microcents: Option<i64>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    /// Routing preference for this scope: `"gateway"` (BYOK, default) or
    /// `"agent_cli"` (subscription-backed CLI via ADR-037).
    pub provider_type: String,
    /// Catalog id of the agent CLI to use when `provider_type = "agent_cli"`.
    /// `NULL` when `provider_type = "gateway"`.
    pub agent_cli_provider_id: Option<String>,
    /// When `true` (and `provider_type = "agent_cli"`, `agent_cli_provider_id = "claude_cli"`),
    /// `ConversationService` uses the long-lived `--permission-prompt-tool stdio` interactive
    /// path instead of the one-shot `--dangerously-skip-permissions` path (ADR-038 Phase 2).
    /// Opt-in: defaults to `false` so existing deployments are unaffected.
    pub interactive_bridge_enabled: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

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
