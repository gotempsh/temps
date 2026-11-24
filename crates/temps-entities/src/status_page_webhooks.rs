// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A webhook endpoint of a status page (ADR 108 D2).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_webhooks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub status_page_id: i32,
    /// HTTPS endpoint the page's events are posted to.
    pub url: String,
    /// Signing secret, encrypted with `EncryptionService`. Never returned
    /// after creation.
    #[serde(skip_serializing)]
    pub secret_encrypted: String,
    pub enabled: bool,
    pub last_attempt_at: Option<DBDateTime>,
    pub last_success_at: Option<DBDateTime>,
    /// Why the last failed delivery failed; cleared by a success.
    pub last_failure_reason: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_pages::Entity",
        from = "Column::StatusPageId",
        to = "super::status_pages::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    StatusPage,
}

impl Related<super::status_pages::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusPage.def()
    }
}

#[async_trait::async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert {
            self.created_at = sea_orm::ActiveValue::Set(now);
        }
        self.updated_at = sea_orm::ActiveValue::Set(now);
        Ok(self)
    }
}
