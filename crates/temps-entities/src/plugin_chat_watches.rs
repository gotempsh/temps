// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A chat owner’s explicit subscription to one plugin revision and resource.
use sea_orm::entity::prelude::*;
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "plugin_chat_watches")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conversation_id: i64,
    pub actor_id: Uuid,
    pub digest: String,
    pub source: String,
    pub label: Option<String>,
    pub subject: String,
    pub predicate: Json,
    pub mode: String,
    pub active_since: DBDateTime,
    pub session_id: i32,
    pub paused: bool,
    pub created_at: DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
