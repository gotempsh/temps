use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "git_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub provider_type: String, // github, gitlab, bitbucket, gitea, generic
    pub base_url: Option<String>, // For self-hosted instances (web UI URL)
    pub api_url: Option<String>, // API endpoint URL (different from base_url for GitHub Apps)
    pub auth_method: String,   // app, oauth, pat, basic, ssh
    pub auth_config: Json,     // JSON with provider-specific auth config
    pub webhook_secret: Option<String>,
    pub is_active: bool,
    pub is_default: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::git_provider_connections::Entity")]
    Connections,
}

impl Related<super::git_provider_connections::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Connections.def()
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
