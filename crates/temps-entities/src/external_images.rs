//! External images entity
//!
//! Tracks Docker images from external registries that are used for deployments.
//! These images are pre-built externally (e.g., via CI/CD) and deployed to Temps.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "external_images")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Project this image belongs to
    pub project_id: i32,
    /// Full image reference (e.g., "docker.io/myorg/myapp:v1.0.0" or "ghcr.io/owner/repo:sha-abc123")
    pub image_ref: String,
    /// Image digest (sha256:...) if known
    pub digest: Option<String>,
    /// Image size in bytes if known
    pub size_bytes: Option<i64>,
    /// Tag extracted from image_ref (e.g., "v1.0.0", "sha-abc123", "latest")
    pub tag: Option<String>,
    /// Additional metadata (registry info, build info, etc.)
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,
    /// When this image was pushed/registered
    pub pushed_at: DBDateTime,
    /// Record creation timestamp
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
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
            if self.pushed_at.is_not_set() {
                self.pushed_at = Set(now);
            }
        }

        Ok(self)
    }
}

impl Model {
    /// Extract the registry host from the image reference
    pub fn registry(&self) -> Option<&str> {
        // Image refs like "docker.io/library/nginx:latest" or "ghcr.io/owner/repo:tag"
        // The registry is the first part before the first '/'
        // Note: docker.io images might not have a registry prefix
        let parts: Vec<&str> = self.image_ref.split('/').collect();
        if parts.len() > 1 && (parts[0].contains('.') || parts[0].contains(':')) {
            Some(parts[0])
        } else {
            // Default Docker Hub
            Some("docker.io")
        }
    }

    /// Extract the image name without tag from the image reference
    pub fn image_name(&self) -> &str {
        // Remove tag if present
        self.image_ref
            .rsplit_once(':')
            .map(|(name, _)| name)
            .unwrap_or(&self.image_ref)
    }

    /// Extract the tag from the image reference (or return "latest" if not specified)
    pub fn image_tag(&self) -> &str {
        self.tag.as_deref().unwrap_or_else(|| {
            self.image_ref
                .rsplit_once(':')
                .map(|(_, tag)| tag)
                .unwrap_or("latest")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_extraction() {
        let model = Model {
            id: 1,
            project_id: 1,
            image_ref: "ghcr.io/owner/repo:v1.0".to_string(),
            digest: None,
            size_bytes: None,
            tag: Some("v1.0".to_string()),
            metadata: None,
            pushed_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
        };
        assert_eq!(model.registry(), Some("ghcr.io"));
    }

    #[test]
    fn test_docker_hub_default() {
        let model = Model {
            id: 1,
            project_id: 1,
            image_ref: "nginx:latest".to_string(),
            digest: None,
            size_bytes: None,
            tag: Some("latest".to_string()),
            metadata: None,
            pushed_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
        };
        assert_eq!(model.registry(), Some("docker.io"));
    }

    #[test]
    fn test_image_name_extraction() {
        let model = Model {
            id: 1,
            project_id: 1,
            image_ref: "ghcr.io/owner/repo:v1.0".to_string(),
            digest: None,
            size_bytes: None,
            tag: Some("v1.0".to_string()),
            metadata: None,
            pushed_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
        };
        assert_eq!(model.image_name(), "ghcr.io/owner/repo");
        assert_eq!(model.image_tag(), "v1.0");
    }
}
