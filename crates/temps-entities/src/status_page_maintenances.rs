// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A scheduled maintenance window on a status page (ADR 108 D2).
//!
//! The title and description are written for the public page. While the
//! window is in progress its monitors' checks are left out of uptime and
//! raise no outage alert. A cancelled window (`cancelled_at` set) counts for
//! nothing and is not shown.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_maintenances")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub status_page_id: i32,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: DBDateTime,
    pub ends_at: DBDateTime,
    pub cancelled_at: Option<DBDateTime>,
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
