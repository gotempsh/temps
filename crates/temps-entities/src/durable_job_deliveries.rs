// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "durable_job_deliveries")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub job_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub consumer: String,
    pub claimed_at: Option<DBDateTime>,
    pub completed_at: Option<DBDateTime>,
    pub attempts: i32,
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::durable_jobs::Entity",
        from = "Column::JobId",
        to = "super::durable_jobs::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Job,
}

impl Related<super::durable_jobs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Job.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
