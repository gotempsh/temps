//! Source type definitions for projects
//!
//! Defines the source of code/artifacts for a project deployment.
//! This determines how deployments are triggered and executed.

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Source type for project deployments
///
/// Determines where the deployment artifacts come from:
/// - `Git`: Source code from a Git repository (traditional flow)
/// - `DockerImage`: Pre-built Docker image from external registry
/// - `StaticFiles`: Pre-built static files uploaded as a bundle
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// Traditional Git-based deployments
    /// Source code is pulled from a Git repository, built, and deployed
    #[default]
    #[sea_orm(string_value = "git")]
    Git,

    /// External Docker image deployments
    /// Pre-built image is pulled from an external registry (DockerHub, GHCR, etc.)
    #[sea_orm(string_value = "docker_image")]
    DockerImage,

    /// Static files deployments
    /// Pre-built static files are uploaded as a tar.gz or zip bundle
    #[sea_orm(string_value = "static_files")]
    StaticFiles,
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceType::Git => write!(f, "git"),
            SourceType::DockerImage => write!(f, "docker_image"),
            SourceType::StaticFiles => write!(f, "static_files"),
        }
    }
}

impl SourceType {
    /// Returns true if this source type requires Git repository information
    pub fn requires_git_info(&self) -> bool {
        matches!(self, SourceType::Git)
    }

    /// Returns true if this source type can have cron jobs configured
    pub fn supports_crons(&self) -> bool {
        matches!(self, SourceType::Git | SourceType::DockerImage)
    }

    /// Returns true if this source type supports Docker image deployments
    pub fn is_container_based(&self) -> bool {
        matches!(self, SourceType::Git | SourceType::DockerImage)
    }

    /// Returns true if this source type is for static file serving
    pub fn is_static(&self) -> bool {
        matches!(self, SourceType::StaticFiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_type_default() {
        assert_eq!(SourceType::default(), SourceType::Git);
    }

    #[test]
    fn test_source_type_display() {
        assert_eq!(SourceType::Git.to_string(), "git");
        assert_eq!(SourceType::DockerImage.to_string(), "docker_image");
        assert_eq!(SourceType::StaticFiles.to_string(), "static_files");
    }

    #[test]
    fn test_requires_git_info() {
        assert!(SourceType::Git.requires_git_info());
        assert!(!SourceType::DockerImage.requires_git_info());
        assert!(!SourceType::StaticFiles.requires_git_info());
    }

    #[test]
    fn test_is_container_based() {
        assert!(SourceType::Git.is_container_based());
        assert!(SourceType::DockerImage.is_container_based());
        assert!(!SourceType::StaticFiles.is_container_based());
    }

    #[test]
    fn test_is_static() {
        assert!(!SourceType::Git.is_static());
        assert!(!SourceType::DockerImage.is_static());
        assert!(SourceType::StaticFiles.is_static());
    }

    #[test]
    fn test_serde_serialization() {
        assert_eq!(serde_json::to_string(&SourceType::Git).unwrap(), "\"git\"");
        assert_eq!(
            serde_json::to_string(&SourceType::DockerImage).unwrap(),
            "\"docker_image\""
        );
        assert_eq!(
            serde_json::to_string(&SourceType::StaticFiles).unwrap(),
            "\"static_files\""
        );
    }

    #[test]
    fn test_serde_deserialization() {
        assert_eq!(
            serde_json::from_str::<SourceType>("\"git\"").unwrap(),
            SourceType::Git
        );
        assert_eq!(
            serde_json::from_str::<SourceType>("\"docker_image\"").unwrap(),
            SourceType::DockerImage
        );
        assert_eq!(
            serde_json::from_str::<SourceType>("\"static_files\"").unwrap(),
            SourceType::StaticFiles
        );
    }
}
