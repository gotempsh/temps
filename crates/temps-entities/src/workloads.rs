// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A workload object (ADR-055 §2): what a product asked the plane to run,
//! and what the plane reports about it.
//!
//! `spec` and `status` hold `temps_workload::WorkloadSpec` and
//! `WorkloadStatus`; only `temps-workload-plane` reads or writes them, and it
//! converts them to the typed structs. `change_seq` is bumped by a trigger on
//! every update.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "workloads")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The product that owns the workload: `cron`, later `environment`,
    /// `sandbox`, `service`.
    pub owner_kind: String,
    pub owner_id: i64,
    /// Unique per owner among live workloads.
    pub name: String,
    pub spec: Json,
    /// Bumped on every spec change.
    pub generation: i64,
    /// The last generation the plane fully applied.
    pub observed_generation: i64,
    /// Resource version: the next value of `workload_change_seq`.
    pub change_seq: i64,
    pub status: Json,
    /// Mirrors `status.phase` for the reconciler's indexes.
    pub phase: String,
    pub owner_workload_id: Option<i64>,
    pub finalizers: Vec<String>,
    /// When a finished run's time-to-live runs out.
    pub expires_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub deleted_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::workload_instances::Entity")]
    Instances,
    #[sea_orm(has_many = "super::workload_events::Entity")]
    Events,
}

impl Related<super::workload_instances::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Instances.def()
    }
}

impl Related<super::workload_events::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Events.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
