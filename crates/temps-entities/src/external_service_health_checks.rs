// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// One row per health probe performed against an external service
/// (Postgres, Redis, MongoDB, RustFS, ...). Drives the health badge
/// and sparkline in the service detail UI.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "external_service_health_checks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub service_id: i32,
    pub checked_at: DBDateTime,
    /// "operational" | "degraded" | "down"
    pub status: String,
    pub response_time_ms: Option<i32>,
    pub error_message: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id",
        on_delete = "Cascade"
    )]
    ExternalService,
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalService.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert && self.checked_at.is_not_set() {
            self.checked_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
