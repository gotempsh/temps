// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One instance of a workload (ADR-055 §2): for a run-to-completion
//! workload, one attempt.
//!
//! The row is written before the driver creates anything, with the
//! deterministic name the instance will get, so a crash between the two
//! leaves a row the next reconcile pass can look up instead of an untracked
//! container.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workload_instances")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub workload_id: i64,
    /// The workload generation the instance was created from.
    pub revision: i64,
    /// 1 for the first attempt, then one more per retry.
    pub attempt: i32,
    /// NULL is the control plane (node 0).
    pub node_id: Option<i32>,
    pub driver: String,
    /// `temps_workload::InstanceHandle`.
    pub handle: Json,
    pub address: Option<String>,
    /// `pending | running | succeeded | failed`.
    pub phase: String,
    pub ready: bool,
    pub conditions: Json,
    pub exit_code: Option<i64>,
    /// `[temps_workload::PortBinding]` a worker's node runtime published,
    /// as its heartbeat reports them. `[]` on the control plane, whose
    /// driver answers directly.
    pub published_ports: Json,
    pub started_at: Option<DBDateTime>,
    pub finished_at: Option<DBDateTime>,
    pub change_seq: i64,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
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
