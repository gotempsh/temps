//! Sea-ORM entity for the cross-project trace discovery index (ADR-027 Phase 0).
//!
//! Each row records that `project_id` has ingested at least one span belonging
//! to `trace_id`. The table is append-only after the initial insert; rows are
//! removed when the owning project is deleted (ON DELETE CASCADE) or pruned by
//! the daily `Job::PruneStaleTraceHints` cleanup job after 90 days.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cross_project_trace_refs")]
pub struct Model {
    /// W3C trace-context `trace_id` — 32-character lowercase hexadecimal string.
    #[sea_orm(primary_key, auto_increment = false)]
    pub trace_id: String,
    /// The project that holds spans for this trace_id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,
    /// Wall-clock time of the first ingest that wrote this row.
    pub first_seen: DBDateTime,
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
