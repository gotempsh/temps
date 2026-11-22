// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversion goals (ADR 109 D2). Called "Conversions" in the console.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "analytics_goals")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    /// `event` or `page`.
    pub kind: String,
    /// Event to match, by `COALESCE(event_name, event_type)`. Set for `event`.
    pub event_name: Option<String>,
    /// JSON array of funnel `SmartFilter`s that must all hold.
    #[sea_orm(column_type = "JsonBinary")]
    pub event_filter: serde_json::Value,
    /// Page path pattern, `*` matching any run of characters. Set for `page`.
    pub page_path: Option<String>,
    /// Event property holding the revenue value of one conversion.
    pub revenue_property: Option<String>,
    /// ISO 4217 code of `revenue_property`, set whenever it is.
    pub revenue_currency: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
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

impl ActiveModelBehavior for ActiveModel {}
