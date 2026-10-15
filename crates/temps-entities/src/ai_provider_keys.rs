// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// Provider identifier: "openai", "anthropic", "xai", "gemini", "openrouter",
    /// or "openai_compatible" for a self-hosted OpenAI-compatible server
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
    /// Exact private `ip:port` this key alone may reach, without the address
    /// being in the instance-wide trusted private networks. NULL → no per-key
    /// trust. Only ever set by an administrator, and only to the IP literal in
    /// this key's own `base_url`; a base URL change clears it.
    pub trusted_private_address: Option<String>,
    /// User who last set `trusted_private_address`.
    pub trusted_private_address_set_by: Option<i32>,
    /// When `trusted_private_address` was last set.
    pub trusted_private_address_set_at: Option<DBDateTime>,
    /// Capabilities an administrator declared for a self-hosted
    /// (`openai_compatible`) key, e.g. `{"streaming":true,"tools":true,
    /// "embeddings":false,"responses_api":false}`. NULL for vendor keys,
    /// whose capabilities are defined by their adapter.
    pub capabilities: Option<Json>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::ai_provider_models::Entity")]
    Models,
}

impl Related<super::ai_provider_models::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Models.def()
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
