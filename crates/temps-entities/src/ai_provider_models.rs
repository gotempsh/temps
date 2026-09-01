// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_provider_models")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub provider_key_id: i32,
    pub model_id: String,
    pub display_name: String,
    /// `discovered`, `manual`, or `bootstrap`.
    pub source: String,
    pub is_available: bool,
    pub is_enabled: bool,
    pub owned_by: Option<String>,
    pub metadata: Option<Json>,
    pub last_seen_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::ai_provider_keys::Entity",
        from = "Column::ProviderKeyId",
        to = "super::ai_provider_keys::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    ProviderKey,
}

impl Related<super::ai_provider_keys::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ProviderKey.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        self.updated_at = Set(now);
        Ok(self)
    }
}
