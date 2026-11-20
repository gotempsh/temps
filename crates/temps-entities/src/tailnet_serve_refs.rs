// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A run's reference to a `tailnet_serve_entries` row (ADR 065 D4): the run
//! made it (`created`) or found it already there (`found`). Undo removes its
//! own reference; the entry goes only with its last one.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

pub const KIND_CREATED: &str = "created";
pub const KIND_FOUND: &str = "found";

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "tailnet_serve_refs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub entry_id: i32,
    pub run_id: i32,
    /// `created` or `found`.
    pub kind: String,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::tailnet_serve_entries::Entity",
        from = "Column::EntryId",
        to = "super::tailnet_serve_entries::Column::Id",
        on_delete = "Cascade"
    )]
    Entry,
}

impl Related<super::tailnet_serve_entries::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Entry.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
