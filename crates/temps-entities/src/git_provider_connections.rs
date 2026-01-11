use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "git_provider_connections")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub provider_id: i32,
    pub user_id: Option<i32>,
    pub account_name: String,
    pub account_type: String, // User, Organization
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<DBDateTime>,
    pub refresh_token_expires_at: Option<DBDateTime>,
    pub installation_id: Option<String>, // Provider-specific installation ID
    pub metadata: Option<Json>,          // Provider-specific metadata
    pub is_active: bool,
    pub is_expired: bool, // True if PAT token has been validated and found to be expired
    pub syncing: bool,
    pub last_synced_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::git_providers::Entity",
        from = "Column::ProviderId",
        to = "super::git_providers::Column::Id"
    )]
    Provider,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id"
    )]
    User,
    #[sea_orm(has_many = "super::repositories::Entity")]
    Repositories,
}

impl Related<super::git_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Provider.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl Related<super::repositories::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repositories.def()
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
