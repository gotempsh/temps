use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "deployment_containers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub deployment_id: i32,
    pub container_id: String,
    pub container_name: String,
    pub container_port: i32,
    pub host_port: Option<i32>,
    pub image_name: Option<String>,
    pub status: Option<String>,
    pub created_at: DBDateTime,
    pub deployed_at: DBDateTime,
    pub ready_at: Option<DBDateTime>,
    pub deleted_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id"
    )]
    Deployment,
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployment.def()
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
        } else {
        }
        
        Ok(self)
    }
}