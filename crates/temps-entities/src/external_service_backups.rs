use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "external_service_backups")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub service_id: i32,
    pub backup_id: i32,
    pub backup_type: String,
    pub state: String,
    pub started_at: DBDateTime,
    pub finished_at: Option<DBDateTime>,
    pub size_bytes: Option<i32>,
    pub s3_location: String,
    pub error_message: Option<String>,
    pub metadata: Json,
    pub checksum: Option<String>,
    pub compression_type: String,
    pub created_by: i32,
    pub expires_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id"
    )]
    ExternalService,
    #[sea_orm(
        belongs_to = "super::backups::Entity",
        from = "Column::BackupId",
        to = "super::backups::Column::Id"
    )]
    Backup,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatedBy",
        to = "super::users::Column::Id"
    )]
    User,
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalService.def()
    }
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backup.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}