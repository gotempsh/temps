//! Email domains entity

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "email_domains")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub provider_id: i32,
    pub domain: String,
    pub status: String,
    pub spf_record_name: Option<String>,
    pub spf_record_value: Option<String>,
    pub dkim_selector: Option<String>,
    pub dkim_record_name: Option<String>,
    pub dkim_record_value: Option<String>,
    pub mx_record_name: Option<String>,
    pub mx_record_value: Option<String>,
    pub mx_record_priority: Option<i16>,
    pub provider_identity_id: Option<String>,
    pub last_verified_at: Option<DBDateTime>,
    pub verification_error: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::email_providers::Entity",
        from = "Column::ProviderId",
        to = "super::email_providers::Column::Id"
    )]
    EmailProvider,
    #[sea_orm(has_many = "super::emails::Entity")]
    Emails,
}

impl Related<super::email_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EmailProvider.def()
    }
}

impl Related<super::emails::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Emails.def()
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
