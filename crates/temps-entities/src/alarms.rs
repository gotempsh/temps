use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "alarms")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub container_id: Option<i32>,
    /// External service that triggered this alarm, if it is service-scoped
    /// (e.g. a database metric threshold). `None` for container/outage/
    /// deployment alarms which have no associated service.
    pub service_id: Option<i32>,

    pub alarm_type: String,
    pub severity: String,
    pub status: String,

    pub title: String,
    pub message: Option<String>,
    pub metadata: Option<Json>,

    pub fired_at: DBDateTime,
    pub acknowledged_at: Option<DBDateTime>,
    pub acknowledged_by: Option<i32>,
    pub resolved_at: Option<DBDateTime>,

    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id"
    )]
    Environment,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id"
    )]
    Deployment,
    #[sea_orm(
        belongs_to = "super::deployment_containers::Entity",
        from = "Column::ContainerId",
        to = "super::deployment_containers::Column::Id"
    )]
    Container,
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id"
    )]
    Service,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::AcknowledgedBy",
        to = "super::users::Column::Id"
    )]
    AcknowledgedByUser,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployment.def()
    }
}

impl Related<super::deployment_containers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Container.def()
    }
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Service.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AcknowledgedByUser.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}
