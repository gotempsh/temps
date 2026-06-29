use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A saved metric dashboard for a project.
///
/// Each dashboard owns a typed JSON `layout` (a `DashboardLayout` defined in
/// the `temps-otel` service layer) describing sections and metric tiles. This
/// table holds config/metadata only — it is Postgres-backed, never ClickHouse.
/// The service layer is responsible for converting between the typed
/// `DashboardLayout` struct and the `serde_json::Value` stored here, exactly as
/// `restore_runs` does for its typed PITR/override payloads.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "metric_dashboards")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// FK to `projects(id)`. Indexed; the list view is scoped by project.
    pub project_id: i32,
    /// Human-readable dashboard name.
    pub name: String,
    /// The typed `DashboardLayout` serialized as JSON (JSONB column).
    pub layout: serde_json::Value,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
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

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        // Always bump updated_at — callers that want to preserve it can
        // explicitly Set() before save, though no caller currently does.
        self.updated_at = Set(now);
        Ok(self)
    }
}
