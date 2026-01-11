//! Emails entity

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "emails")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub domain_id: Option<i32>,
    pub project_id: Option<i32>,
    pub from_address: String,
    pub from_name: Option<String>,
    #[sea_orm(column_type = "JsonBinary")]
    pub to_addresses: Json,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub cc_addresses: Option<Json>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub bcc_addresses: Option<Json>,
    pub reply_to: Option<String>,
    pub subject: String,
    pub html_body: Option<String>,
    pub text_body: Option<String>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub headers: Option<Json>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub tags: Option<Json>,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub error_message: Option<String>,
    pub sent_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::email_domains::Entity",
        from = "Column::DomainId",
        to = "super::email_domains::Column::Id"
    )]
    EmailDomain,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
}

impl Related<super::email_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EmailDomain.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
