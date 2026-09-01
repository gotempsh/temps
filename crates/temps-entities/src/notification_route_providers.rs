// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "notification_route_providers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub route_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub provider_id: i32,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::notification_routes::Entity",
        from = "Column::RouteId",
        to = "super::notification_routes::Column::Id",
        on_delete = "Cascade"
    )]
    NotificationRoute,
    #[sea_orm(
        belongs_to = "super::notification_providers::Entity",
        from = "Column::ProviderId",
        to = "super::notification_providers::Column::Id",
        on_delete = "Cascade"
    )]
    NotificationProvider,
}

impl Related<super::notification_routes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::NotificationRoute.def()
    }
}

impl Related<super::notification_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::NotificationProvider.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
