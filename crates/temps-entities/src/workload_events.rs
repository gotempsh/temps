// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A workload's history (ADR-055 §2). Repeats of the same reason and
//! message increment `count`; the plane keeps the last 100 per workload and
//! at most 7 days.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workload_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub workload_id: i64,
    pub instance_id: Option<i64>,
    /// Machine reason, CamelCase.
    pub reason: String,
    pub message: String,
    pub count: i32,
    pub first_seen_at: DBDateTime,
    pub last_seen_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::workloads::Entity",
        from = "Column::WorkloadId",
        to = "super::workloads::Column::Id",
        on_delete = "Cascade"
    )]
    Workload,
}

impl Related<super::workloads::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Workload.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
