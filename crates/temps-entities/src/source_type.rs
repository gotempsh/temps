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
/// - `Manual`: Flexible type that accepts any deployment method
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

    /// Manual/Flexible deployments
    /// Accepts any deployment method: Docker images, static files, or Git-based
    /// Allows switching between deployment methods without recreating the project
    #[sea_orm(string_value = "manual")]
    Manual,
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceType::Git => write!(f, "git"),
            SourceType::DockerImage => write!(f, "docker_image"),
            SourceType::StaticFiles => write!(f, "static_files"),
            SourceType::Manual => write!(f, "manual"),
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
        matches!(
            self,
            SourceType::Git | SourceType::DockerImage | SourceType::Manual
        )
    }

    /// Returns true if this source type supports Docker image deployments
    pub fn is_container_based(&self) -> bool {
        matches!(
            self,
            SourceType::Git | SourceType::DockerImage | SourceType::Manual
        )
    }

    /// Returns true if this source type is for static file serving
    pub fn is_static(&self) -> bool {
        matches!(self, SourceType::StaticFiles)
    }

    /// Returns true if this source type is flexible (accepts any deployment method)
    ///
    /// Manual projects can deploy via Docker images, static files, or Git-based methods.
    pub fn is_flexible(&self) -> bool {
        matches!(self, SourceType::Manual)
    }

    /// Returns true if this project source type allows the given deployment method
    ///
    /// - `Manual` projects accept any deployment method (Docker, static, Git)
    /// - Other project types only accept their specific deployment method
    pub fn allows_deployment_method(&self, method: &SourceType) -> bool {
        match self {
            SourceType::Manual => true, // Accepts everything
            other => other == method,
        }
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
        assert_eq!(SourceType::Manual.to_string(), "manual");
    }

    #[test]
    fn test_requires_git_info() {
        assert!(SourceType::Git.requires_git_info());
        assert!(!SourceType::DockerImage.requires_git_info());
        assert!(!SourceType::StaticFiles.requires_git_info());
        assert!(!SourceType::Manual.requires_git_info());
    }

    #[test]
    fn test_is_container_based() {
        assert!(SourceType::Git.is_container_based());
        assert!(SourceType::DockerImage.is_container_based());
        assert!(!SourceType::StaticFiles.is_container_based());
        assert!(SourceType::Manual.is_container_based());
    }

    #[test]
    fn test_is_static() {
        assert!(!SourceType::Git.is_static());
        assert!(!SourceType::DockerImage.is_static());
        assert!(SourceType::StaticFiles.is_static());
        assert!(!SourceType::Manual.is_static());
    }

    #[test]
    fn test_is_flexible() {
        assert!(!SourceType::Git.is_flexible());
        assert!(!SourceType::DockerImage.is_flexible());
        assert!(!SourceType::StaticFiles.is_flexible());
        assert!(SourceType::Manual.is_flexible());
    }

    #[test]
    fn test_allows_deployment_method() {
        // Manual accepts everything
        assert!(SourceType::Manual.allows_deployment_method(&SourceType::Git));
        assert!(SourceType::Manual.allows_deployment_method(&SourceType::DockerImage));
        assert!(SourceType::Manual.allows_deployment_method(&SourceType::StaticFiles));
        assert!(SourceType::Manual.allows_deployment_method(&SourceType::Manual));

        // Git only accepts Git
        assert!(SourceType::Git.allows_deployment_method(&SourceType::Git));
        assert!(!SourceType::Git.allows_deployment_method(&SourceType::DockerImage));
        assert!(!SourceType::Git.allows_deployment_method(&SourceType::StaticFiles));

        // DockerImage only accepts DockerImage
        assert!(SourceType::DockerImage.allows_deployment_method(&SourceType::DockerImage));
        assert!(!SourceType::DockerImage.allows_deployment_method(&SourceType::Git));
        assert!(!SourceType::DockerImage.allows_deployment_method(&SourceType::StaticFiles));

        // StaticFiles only accepts StaticFiles
        assert!(SourceType::StaticFiles.allows_deployment_method(&SourceType::StaticFiles));
        assert!(!SourceType::StaticFiles.allows_deployment_method(&SourceType::Git));
        assert!(!SourceType::StaticFiles.allows_deployment_method(&SourceType::DockerImage));
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
        assert_eq!(
            serde_json::to_string(&SourceType::Manual).unwrap(),
            "\"manual\""
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
        assert_eq!(
            serde_json::from_str::<SourceType>("\"manual\"").unwrap(),
            SourceType::Manual
        );
    }
}
