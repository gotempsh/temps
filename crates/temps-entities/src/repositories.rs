use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use temps_core::DBDateTime;

/// Branch-specific preset data
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchPresetData {
    /// List of detected presets in the repository
    pub presets: Vec<PresetInfo>,
    /// Timestamp when presets were calculated
    pub calculated_at: DBDateTime,
}

/// Information about a detected preset
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetInfo {
    /// Path within the repository (e.g., "./", "apps/web")
    pub path: String,
    /// Preset slug (e.g., "nextjs", "vite")
    pub preset: String,
    /// Human-readable preset label (e.g., "Next.js", "Vite")
    pub preset_label: String,
    /// Exposed port for the preset
    pub exposed_port: Option<u16>,
    /// Icon URL for the preset
    pub icon_url: Option<String>,
    /// Project type category (e.g., "frontend", "backend", "fullstack")
    pub project_type: String,
}

/// Repository preset cache structure
/// Maps branch names to their preset data
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryPresetCache {
    #[serde(flatten)]
    pub branches: HashMap<String, BranchPresetData>,
}
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "repositories")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Git provider connection ID (required - repositories are always linked to a connection)
    pub git_provider_connection_id: i32,
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
    pub installation_id: Option<i32>,
    pub clone_url: Option<String>, // HTTPS clone URL
    pub ssh_url: Option<String>,   // SSH clone URL
    /// Stores preset cache as HashMap<branch, BranchPresetData>
    pub preset: Option<Json>,
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
