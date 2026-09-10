// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "notification_routes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub enabled: bool,
    /// Lowest notification severity matched by this route.
    /// Stored as a lowercase `NotificationSeverity` name.
    pub min_severity: String,
    /// Highest notification severity matched by this route.
    /// Stored as a lowercase `NotificationSeverity` name.
    pub max_severity: String,
    /// Set only for the auto-generated catch-all route created alongside a
    /// provider (see `NotificationRoutingService::create_catch_all_route_for_provider`).
    /// `None` for routes an operator created explicitly. The database FK
    /// cascades on provider deletion, and the service layer uses this
    /// column (not name parsing) to keep the route's display name in sync
    /// when the provider is renamed.
    pub catch_all_provider_id: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::notification_route_providers::Entity")]
    NotificationRouteProviders,
    #[sea_orm(
        belongs_to = "super::notification_providers::Entity",
        from = "Column::CatchAllProviderId",
        to = "super::notification_providers::Column::Id",
        on_delete = "Cascade"
    )]
    CatchAllProvider,
}

impl Related<super::notification_route_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::NotificationRouteProviders.def()
    }
}

impl Related<super::notification_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CatchAllProvider.def()
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
