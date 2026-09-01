// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// Compose file paths found in the repository (only for docker-compose preset)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub compose_files: Option<Vec<String>>,
    /// Repository-root-relative path to the Dockerfile, when it does not
    /// live directly under `{path}/Dockerfile` (e.g. `docker/Dockerfile`
    /// rolled up to a `path` of `"./"`). `None` for a Dockerfile located
    /// directly at `{path}/Dockerfile` and for every non-Dockerfile preset.
    ///
    /// `default` keeps deserialization of preset caches written before this
    /// field existed (`repositories.preset` JSON column) backward
    /// compatible.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dockerfile_path: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A rolled-up bare Dockerfile (e.g. `docker/Dockerfile`) must survive a
    /// round trip through the JSON representation cached in
    /// `repositories.preset`.
    #[test]
    fn preset_info_dockerfile_path_round_trips_through_json() {
        let preset = PresetInfo {
            path: "./".to_string(),
            preset: "dockerfile".to_string(),
            preset_label: "Dockerfile".to_string(),
            exposed_port: None,
            icon_url: None,
            project_type: "backend".to_string(),
            compose_files: None,
            dockerfile_path: Some("docker/Dockerfile".to_string()),
        };

        let json = serde_json::to_value(&preset).unwrap();
        assert_eq!(json["dockerfilePath"], "docker/Dockerfile");

        let round_tripped: PresetInfo = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, preset);
    }

    /// A genuine monorepo service directory (its own Dockerfile plus its own
    /// manifest) keeps `dockerfile_path: None`, and that `None` must be
    /// omitted from the serialized JSON entirely (matching the existing
    /// `compose_files` convention) rather than written as `null`.
    #[test]
    fn preset_info_without_dockerfile_path_omits_the_field() {
        let preset = PresetInfo {
            path: "apps/api".to_string(),
            preset: "dockerfile".to_string(),
            preset_label: "Dockerfile".to_string(),
            exposed_port: None,
            icon_url: None,
            project_type: "backend".to_string(),
            compose_files: None,
            dockerfile_path: None,
        };

        let json = serde_json::to_value(&preset).unwrap();
        assert!(json.get("dockerfilePath").is_none());
    }

    /// Preset cache rows written by an older server version won't have a
    /// `dockerfilePath` key at all. Deserializing them must still succeed
    /// (via `#[serde(default)]`) rather than erroring, or every cached
    /// branch preset in production would break on the next read.
    #[test]
    fn preset_info_deserializes_pre_existing_cache_rows_missing_the_field() {
        let legacy_json = serde_json::json!({
            "path": "apps/web",
            "preset": "nextjs",
            "presetLabel": "Next.js",
            "exposedPort": 3000,
            "iconUrl": null,
            "projectType": "frontend"
            // no "dockerfilePath" key -- this is what pre-fix cached rows look like
        });

        let preset: PresetInfo = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(preset.dockerfile_path, None);
    }
}
