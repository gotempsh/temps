// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "workflow_invocation_admissions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub run_id: Uuid,
    pub lease: i64,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::workflow_runs::Entity",
        from = "Column::RunId",
        to = "super::workflow_runs::Column::Id",
        on_delete = "Cascade"
    )]
    Run,
}
impl Related<super::workflow_runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Run.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
