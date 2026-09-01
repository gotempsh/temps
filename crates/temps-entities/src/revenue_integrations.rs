// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "revenue_integrations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    /// Registered provider name (e.g. "stripe").
    pub provider: String,
    /// 256-bit base64url secret embedded in the public webhook URL.
    pub webhook_path_token: String,
    /// AES-256-GCM encrypted signing secret. Never returned over the API.
    #[serde(skip_serializing)]
    pub webhook_signing_secret_encrypted: String,
    /// "pending" (no events yet) | "active" (received at least one event) | "disabled"
    pub status: String,
    pub last_event_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    /// Typed-per-provider settings (price/product allowlist, metered
    /// billing mode, …). NULL means "no per-provider configuration
    /// applied" which is the accept-all-events default. Schema is
    /// [`temps_revenue::providers::ProviderConfig`].
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub config: Option<serde_json::Value>,
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
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
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
