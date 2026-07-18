//! Email providers entity

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "email_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub provider_type: String,
    pub region: String,
    /// Encrypted JSON with provider credentials
    pub credentials: String,
    /// Exact AWS SNS topic authorized to deliver this SES provider's events.
    pub sns_topic_arn: Option<String>,
    /// When the SNS HTTPS subscription for `sns_topic_arn` was last
    /// confirmed. NULL = never confirmed for the current topic; cleared on
    /// every topic change.
    pub sns_subscription_confirmed_at: Option<DBDateTime>,
    pub is_active: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::email_domains::Entity")]
    EmailDomains,
}

impl Related<super::email_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EmailDomains.def()
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
