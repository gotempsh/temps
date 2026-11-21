// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable receipt for a signal from a verified plugin actor.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "plugin_watch_events")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub actor_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: String,
    pub source: String,
    pub subject: String,
    pub attributes: Json,
    pub origin_chat_id: Option<String>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
