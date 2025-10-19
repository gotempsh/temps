use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "repositories")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub git_provider_connection_id: Option<i32>, // Foreign key to git provider connections
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub fork: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub pushed_at: DBDateTime,
    pub size: i32,
    pub stargazers_count: i32,
    pub watchers_count: i32,
    pub language: Option<String>,
    pub default_branch: String,
    pub open_issues_count: i32,
    pub topics: String,
    pub repo_object: String,
    pub framework: Option<String>,
    pub framework_version: Option<String>,
    pub framework_last_updated_at: Option<DBDateTime>,
    pub package_manager: Option<String>,
    pub installation_id: Option<i32>,
    pub clone_url: Option<String>, // Added for non-API based cloning
    pub ssh_url: Option<String>, // Added for SSH cloning
    pub preset: Option<String>, // Stores the calculated project preset/type
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::git_provider_connections::Entity",
        from = "Column::GitProviderConnectionId",
        to = "super::git_provider_connections::Column::Id"
    )]
    GitProviderConnection,
}

impl Related<super::git_provider_connections::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::GitProviderConnection.def()
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