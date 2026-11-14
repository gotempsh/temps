// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use temps_core::DBDateTime;

#[derive(Clone, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "workflow_project_keys")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: i32,
    pub wrapped_key: Option<String>,
    pub created_at: DBDateTime,
    pub shredded_at: Option<DBDateTime>,
}
impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowProjectKey")
            .field("id", &self.id)
            .field("project_id", &self.project_id)
            .field("shredded_at", &self.shredded_at)
            .finish_non_exhaustive()
    }
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
