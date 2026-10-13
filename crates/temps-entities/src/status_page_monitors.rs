// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A monitor a project admin opted into a status page, with the public name
//! shown in place of the monitor's internal name.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_page_monitors")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub status_page_id: i32,
    pub monitor_id: i32,
    pub display_name: String,
    pub position: i32,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_pages::Entity",
        from = "Column::StatusPageId",
        to = "super::status_pages::Column::Id"
    )]
    StatusPage,
    #[sea_orm(
        belongs_to = "super::status_monitors::Entity",
        from = "Column::MonitorId",
        to = "super::status_monitors::Column::Id"
    )]
    StatusMonitor,
}

impl Related<super::status_pages::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusPage.def()
    }
}

impl Related<super::status_monitors::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusMonitor.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
