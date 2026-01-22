//! Remote Deployment Service
//!
//! Handles deployments from external sources:
//! - Pre-built Docker images from external registries (DockerHub, GHCR, ECR, etc.)
//! - Static file bundles (tar.gz or zip) uploaded via API
//!
//! This service enables Git-less deployments where builds happen externally
//! (CI/CD pipelines, local machines, etc.) and only the final artifacts are deployed.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{external_images, static_bundles};
use thiserror::Error;
use tracing::{debug, info};
use utoipa::ToSchema;

/// Error types for remote deployment operations
#[derive(Error, Debug)]
pub enum RemoteDeploymentError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("External image not found: {0}")]
    ImageNotFound(String),

    #[error("Static bundle not found: {0}")]
    BundleNotFound(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(i32),

    #[error("Environment not found: {0}")]
    EnvironmentNotFound(i32),

    #[error("Invalid image reference: {0}")]
    InvalidImageRef(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Deployment error: {0}")]
    DeploymentError(String),
}

/// Request to register an external Docker image
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct RegisterExternalImageRequest {
    /// Full image reference (e.g., "docker.io/myapp:v1.0", "ghcr.io/org/app:sha-abc123")
    pub image_ref: String,
    /// Optional digest for verification (e.g., "sha256:abc123...")
    pub digest: Option<String>,
    /// Optional tag for this image
    pub tag: Option<String>,
    /// Optional metadata (build info, commit hash, etc.)
    pub metadata: Option<serde_json::Value>,
}

/// Request to deploy from an external image
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct DeployFromExternalImageRequest {
    /// External image ID (from register_external_image) or direct image reference
    pub image_ref: String,
    /// Environment ID to deploy to
    pub environment_id: i32,
    /// Number of replicas (defaults to 1)
    pub replicas: Option<i32>,
    /// Additional environment variables for this deployment
    pub environment_variables: Option<std::collections::HashMap<String, String>>,
}

/// Request to upload a static bundle
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct UploadStaticBundleRequest {
    /// Original filename from upload (e.g., "dist.tar.gz")
    pub original_filename: Option<String>,
    /// Content type (e.g., "application/gzip", "application/zip")
    /// If not provided, will be auto-detected from filename
    pub content_type: Option<String>,
    /// Optional metadata (build info, commit hash, etc.)
    pub metadata: Option<serde_json::Value>,
}

// Re-export BundleFormat from entity for convenience
pub use temps_entities::static_bundles::BundleFormat;

/// Request to deploy a static bundle
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct DeployStaticBundleRequest {
    /// Static bundle ID
    pub bundle_id: i32,
    /// Environment ID to deploy to
    pub environment_id: i32,
}

/// Response for external image operations
#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalImageInfo {
    pub id: i32,
    pub project_id: i32,
    pub image_ref: String,
    pub digest: Option<String>,
    pub size_bytes: Option<i64>,
    pub tag: Option<String>,
    pub metadata: Option<serde_json::Value>,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub pushed_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: UtcDateTime,
}

impl From<external_images::Model> for ExternalImageInfo {
    fn from(model: external_images::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            image_ref: model.image_ref,
            digest: model.digest,
            size_bytes: model.size_bytes,
            tag: model.tag,
            metadata: model.metadata,
            pushed_at: model.pushed_at,
            created_at: model.created_at,
        }
    }
}

/// Response for static bundle operations
#[derive(Debug, Serialize, ToSchema)]
pub struct StaticBundleInfo {
    pub id: i32,
    pub project_id: i32,
    /// Path to the bundle in blob storage
    pub blob_path: String,
    /// Original filename from upload
    pub original_filename: Option<String>,
    /// Content type (e.g., "application/gzip", "application/zip")
    pub content_type: String,
    /// Bundle format (derived from content_type)
    pub format: Option<String>,
    pub size_bytes: i64,
    pub checksum: Option<String>,
    pub metadata: Option<serde_json::Value>,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub uploaded_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: UtcDateTime,
}

impl From<static_bundles::Model> for StaticBundleInfo {
    fn from(model: static_bundles::Model) -> Self {
        let format = model.format().map(|f| f.extension().to_string());
        Self {
            id: model.id,
            project_id: model.project_id,
            blob_path: model.blob_path,
            original_filename: model.original_filename,
            content_type: model.content_type,
            format,
            size_bytes: model.size_bytes,
            checksum: model.checksum,
            metadata: model.metadata,
            uploaded_at: model.uploaded_at,
            created_at: model.created_at,
        }
    }
}

/// Service for managing remote deployments
#[derive(Clone)]
pub struct RemoteDeploymentService {
    db: Arc<DatabaseConnection>,
}

impl RemoteDeploymentService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    // =========================================================================
    // External Image Operations
    // =========================================================================

    /// Register an external Docker image for a project
    ///
    /// This records the image reference in the database without actually pulling the image.
    /// The image will be pulled at deployment time using the host's Docker credentials.
    pub async fn register_external_image(
        &self,
        project_id: i32,
        request: RegisterExternalImageRequest,
    ) -> Result<ExternalImageInfo, RemoteDeploymentError> {
        // Validate image reference format
        if request.image_ref.is_empty() {
            return Err(RemoteDeploymentError::InvalidImageRef(
                "Image reference cannot be empty".to_string(),
            ));
        }

        debug!(
            "Registering external image for project {}: {}",
            project_id, request.image_ref
        );

        let now = chrono::Utc::now();

        let active_model = external_images::ActiveModel {
            project_id: Set(project_id),
            image_ref: Set(request.image_ref.clone()),
            digest: Set(request.digest),
            size_bytes: Set(None), // Will be populated when image is pulled
            tag: Set(request.tag),
            metadata: Set(request.metadata),
            pushed_at: Set(now),
            created_at: Set(now),
            ..Default::default()
        };

        let model = active_model.insert(self.db.as_ref()).await?;

        info!(
            "Registered external image {} (id={}) for project {}",
            model.image_ref, model.id, project_id
        );

        Ok(model.into())
    }

    /// Get an external image by ID
    pub async fn get_external_image(
        &self,
        image_id: i32,
    ) -> Result<ExternalImageInfo, RemoteDeploymentError> {
        let model = external_images::Entity::find_by_id(image_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| RemoteDeploymentError::ImageNotFound(image_id.to_string()))?;

        Ok(model.into())
    }

    /// List external images for a project
    pub async fn list_external_images(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<ExternalImageInfo>, u64), RemoteDeploymentError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let query = external_images::Entity::find()
            .filter(external_images::Column::ProjectId.eq(project_id))
            .order_by_desc(external_images::Column::PushedAt);

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let models = paginator.fetch_page(page - 1).await?;

        Ok((models.into_iter().map(Into::into).collect(), total))
    }

    /// Get the latest external image for a project
    pub async fn get_latest_external_image(
        &self,
        project_id: i32,
    ) -> Result<Option<ExternalImageInfo>, RemoteDeploymentError> {
        let model = external_images::Entity::find()
            .filter(external_images::Column::ProjectId.eq(project_id))
            .order_by_desc(external_images::Column::PushedAt)
            .one(self.db.as_ref())
            .await?;

        Ok(model.map(Into::into))
    }

    /// Delete an external image registration
    pub async fn delete_external_image(&self, image_id: i32) -> Result<(), RemoteDeploymentError> {
        let result = external_images::Entity::delete_by_id(image_id)
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(RemoteDeploymentError::ImageNotFound(image_id.to_string()));
        }

        info!("Deleted external image {}", image_id);
        Ok(())
    }

    // =========================================================================
    // Static Bundle Operations
    // =========================================================================

    /// Register a static bundle upload
    ///
    /// This records the bundle metadata in the database. The actual file upload
    /// should be handled separately (e.g., multipart upload to blob storage).
    ///
    /// # Arguments
    /// * `project_id` - The project this bundle belongs to
    /// * `blob_path` - Path to the bundle in blob storage (e.g., "static-bundles/project-123/bundle-abc.tar.gz")
    /// * `size_bytes` - Size of the bundle in bytes
    /// * `request` - Upload request with filename, content type, and metadata
    /// * `checksum` - Optional SHA256 checksum of the bundle
    pub async fn register_static_bundle(
        &self,
        project_id: i32,
        blob_path: String,
        size_bytes: i64,
        request: UploadStaticBundleRequest,
        checksum: Option<String>,
    ) -> Result<StaticBundleInfo, RemoteDeploymentError> {
        debug!(
            "Registering static bundle for project {}: blob_path={}",
            project_id, blob_path
        );

        // Auto-detect content type from filename if not provided
        let content_type = request.content_type.unwrap_or_else(|| {
            if let Some(ref filename) = request.original_filename {
                if let Some(format) = BundleFormat::from_filename(filename) {
                    format.content_type().to_string()
                } else {
                    "application/octet-stream".to_string()
                }
            } else {
                "application/octet-stream".to_string()
            }
        });

        let now = chrono::Utc::now();

        let active_model = static_bundles::ActiveModel {
            project_id: Set(project_id),
            blob_path: Set(blob_path.clone()),
            original_filename: Set(request.original_filename.clone()),
            content_type: Set(content_type),
            size_bytes: Set(size_bytes),
            checksum: Set(checksum),
            metadata: Set(request.metadata),
            uploaded_at: Set(now),
            created_at: Set(now),
            ..Default::default()
        };

        let model = active_model.insert(self.db.as_ref()).await?;

        info!(
            "Registered static bundle (id={}) for project {}: {}",
            model.id,
            project_id,
            request.original_filename.as_deref().unwrap_or(&blob_path)
        );

        Ok(model.into())
    }

    /// Get a static bundle by ID
    pub async fn get_static_bundle(
        &self,
        bundle_id: i32,
    ) -> Result<StaticBundleInfo, RemoteDeploymentError> {
        let model = static_bundles::Entity::find_by_id(bundle_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| RemoteDeploymentError::BundleNotFound(bundle_id.to_string()))?;

        Ok(model.into())
    }

    /// List static bundles for a project
    pub async fn list_static_bundles(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<StaticBundleInfo>, u64), RemoteDeploymentError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let query = static_bundles::Entity::find()
            .filter(static_bundles::Column::ProjectId.eq(project_id))
            .order_by_desc(static_bundles::Column::UploadedAt);

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let models = paginator.fetch_page(page - 1).await?;

        Ok((models.into_iter().map(Into::into).collect(), total))
    }

    /// Get the latest static bundle for a project
    pub async fn get_latest_static_bundle(
        &self,
        project_id: i32,
    ) -> Result<Option<StaticBundleInfo>, RemoteDeploymentError> {
        let model = static_bundles::Entity::find()
            .filter(static_bundles::Column::ProjectId.eq(project_id))
            .order_by_desc(static_bundles::Column::UploadedAt)
            .one(self.db.as_ref())
            .await?;

        Ok(model.map(Into::into))
    }

    /// Delete a static bundle
    pub async fn delete_static_bundle(&self, bundle_id: i32) -> Result<(), RemoteDeploymentError> {
        let result = static_bundles::Entity::delete_by_id(bundle_id)
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(RemoteDeploymentError::BundleNotFound(bundle_id.to_string()));
        }

        info!("Deleted static bundle {}", bundle_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::preset::Preset;

    async fn create_test_project(db: &DatabaseConnection) -> i32 {
        use sea_orm::Set;
        use temps_entities::projects;

        let project = projects::ActiveModel {
            slug: Set("test-project".to_string()),
            name: Set("Test Project".to_string()),
            repo_name: Set(String::new()),
            repo_owner: Set(String::new()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::NodeJs),
            source_type: Set(temps_entities::source_type::SourceType::DockerImage),
            ..Default::default()
        };

        project.insert(db).await.unwrap().id
    }

    #[tokio::test]
    async fn test_register_external_image() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db_arc = test_db.connection_arc();
        let service = RemoteDeploymentService::new(db_arc.clone());

        let project_id = create_test_project(db_arc.as_ref()).await;

        let request = RegisterExternalImageRequest {
            image_ref: "ghcr.io/myorg/myapp:v1.0".to_string(),
            digest: Some("sha256:abc123".to_string()),
            tag: Some("v1.0".to_string()),
            metadata: Some(serde_json::json!({"commit": "abc123"})),
        };

        let result = service
            .register_external_image(project_id, request)
            .await
            .unwrap();

        assert_eq!(result.project_id, project_id);
        assert_eq!(result.image_ref, "ghcr.io/myorg/myapp:v1.0");
        assert_eq!(result.tag, Some("v1.0".to_string()));
    }

    #[tokio::test]
    async fn test_list_external_images() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db_arc = test_db.connection_arc();
        let service = RemoteDeploymentService::new(db_arc.clone());

        let project_id = create_test_project(db_arc.as_ref()).await;

        // Register multiple images
        for i in 1..=3 {
            let request = RegisterExternalImageRequest {
                image_ref: format!("myapp:v{}.0", i),
                digest: None,
                tag: Some(format!("v{}.0", i)),
                metadata: None,
            };
            service
                .register_external_image(project_id, request)
                .await
                .unwrap();
        }

        let (images, total) = service
            .list_external_images(project_id, None, None)
            .await
            .unwrap();

        assert_eq!(total, 3);
        assert_eq!(images.len(), 3);
        // Should be ordered by pushed_at DESC
        assert_eq!(images[0].tag, Some("v3.0".to_string()));
    }

    #[tokio::test]
    async fn test_register_static_bundle() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db_arc = test_db.connection_arc();
        let service = RemoteDeploymentService::new(db_arc.clone());

        let project_id = create_test_project(db_arc.as_ref()).await;

        let request = UploadStaticBundleRequest {
            original_filename: Some("dist.tar.gz".to_string()),
            content_type: Some("application/gzip".to_string()),
            metadata: Some(serde_json::json!({"build": "123"})),
        };

        let result = service
            .register_static_bundle(
                project_id,
                "static-bundles/project-123/dist.tar.gz".to_string(),
                1024 * 1024,
                request,
                Some("sha256:xyz789".to_string()),
            )
            .await
            .unwrap();

        assert_eq!(result.project_id, project_id);
        assert_eq!(result.original_filename, Some("dist.tar.gz".to_string()));
        assert_eq!(result.content_type, "application/gzip");
        assert_eq!(result.format, Some(".tar.gz".to_string()));
        assert_eq!(result.size_bytes, 1024 * 1024);
    }

    #[tokio::test]
    async fn test_auto_detect_content_type() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db_arc = test_db.connection_arc();
        let service = RemoteDeploymentService::new(db_arc.clone());

        let project_id = create_test_project(db_arc.as_ref()).await;

        // Test .tar.gz detection
        let request = UploadStaticBundleRequest {
            original_filename: Some("dist.tar.gz".to_string()),
            content_type: None, // Should auto-detect
            metadata: None,
        };

        let result = service
            .register_static_bundle(
                project_id,
                "static-bundles/project-123/bundle-1.tar.gz".to_string(),
                1024,
                request,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.content_type, "application/gzip");
        assert_eq!(result.format, Some(".tar.gz".to_string()));

        // Test .zip detection
        let request = UploadStaticBundleRequest {
            original_filename: Some("dist.zip".to_string()),
            content_type: None, // Should auto-detect
            metadata: None,
        };

        let result = service
            .register_static_bundle(
                project_id,
                "static-bundles/project-123/bundle-2.zip".to_string(),
                1024,
                request,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.content_type, "application/zip");
        assert_eq!(result.format, Some(".zip".to_string()));
    }
}
