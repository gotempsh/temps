// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Domain types returned by [`GitProviderManager`] to handlers and other crates.

use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectPresetDomain {
    pub path: String,
    pub preset: String,
    pub preset_label: String,
    pub exposed_port: Option<u16>,
    pub icon_url: Option<String>,
    pub project_type: String,
    /// Compose file paths found in the repository (only for docker-compose preset)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_files: Option<Vec<String>>,
    /// Repository-root-relative path to the Dockerfile, when it does not
    /// live directly under `{path}/Dockerfile`. See
    /// [`temps_presets::DetectedPreset::dockerfile_path`] for the full
    /// explanation.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dockerfile_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RepositoryPresetDomain {
    pub repository_id: i32,
    pub owner: String,
    pub name: String,
    pub presets: Vec<ProjectPresetDomain>,
    pub calculated_at: UtcDateTime,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnvExampleVariableDomain {
    pub key: String,
    pub default_value: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RepositoryEnvExampleDomain {
    pub repository_id: i32,
    /// Path of the detected env-example file, `None` if the repository has none.
    pub path: Option<String>,
    pub variables: Vec<EnvExampleVariableDomain>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComposeServicePreviewDomain {
    pub name: String,
    pub image: Option<String>,
    pub depends_on: Vec<String>,
    pub environment_variables: Vec<String>,
    pub looks_like_database: bool,
    pub detected_service_type: Option<temps_entities::preset::ComposeServiceFamily>,
    pub ports: Vec<temps_entities::preset::ComposePortMapping>,
}

#[derive(Debug, Clone)]
pub struct RepositoryComposeServicesDomain {
    pub repository_id: i32,
    pub path: String,
    pub services: Vec<ComposeServicePreviewDomain>,
}

/// A Compose file read from a connected repository, with what is needed to
/// read its neighbours (the repository `.env`) at the same revision.
#[derive(Debug, Clone)]
pub struct RepositoryComposeSourceDomain {
    pub repository_id: i32,
    pub connection_id: i32,
    pub owner: String,
    pub name: String,
    /// Branch name or commit SHA the file was read at.
    pub git_ref: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct RepositoryComposePreviewDomain {
    pub repository_id: i32,
    pub path: String,
    pub preview: temps_presets::EffectiveComposePreview,
}

#[derive(Debug, Clone)]
pub struct ProjectUsageInfo {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub connection_id: i32,
    pub connection_name: String,
}

#[derive(Debug, Clone)]
pub struct ProviderDeletionCheck {
    pub can_delete: bool,
    pub projects_in_use: Vec<ProjectUsageInfo>,
    pub message: String,
}
