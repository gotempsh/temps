use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "backups")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub backup_id: String,
    pub schedule_id: Option<i32>,
    pub backup_type: String,
    pub state: String,
    pub started_at: DBDateTime,
    pub finished_at: Option<DBDateTime>,
    pub size_bytes: Option<i32>,
    pub file_count: Option<i32>,
    pub s3_source_id: i32,
    pub s3_location: String,
    pub error_message: Option<String>,
    pub metadata: String,
    pub checksum: Option<String>,
    pub compression_type: String,
    pub created_by: i32,
    pub expires_at: Option<DBDateTime>,
    pub tags: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::s3_sources::Entity",
        from = "Column::S3SourceId",
        to = "super::s3_sources::Column::Id"
    )]
    S3Source,
    #[sea_orm(has_many = "super::external_service_backups::Entity")]
    ExternalServiceBackups,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatedBy",
        to = "super::users::Column::Id"
    )]
    User,
}

impl Related<super::s3_sources::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::S3Source.def()
    }
}

impl Related<super::external_service_backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalServiceBackups.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}