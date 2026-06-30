use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_provider_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Provider identifier: "openai", "anthropic", "xai", "gemini", "custom"
    pub provider: String,
    /// Human-readable display name, e.g. "OpenAI Production"
    pub display_name: String,
    /// AES-256-GCM encrypted API key via EncryptionService
    pub api_key_encrypted: String,
    /// Optional base URL override for custom/self-hosted providers
    pub base_url: Option<String>,
    /// Optional model id this provider should serve (e.g. "gpt-4o-mini" or a
    /// local Ollama tag). NULL → fall back to the per-provider default in
    /// `resolve_model`. Set/edited from the AI Providers settings UI.
    pub default_model: Option<String>,
    /// Whether this provider key is active and available for routing
    pub is_active: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
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
