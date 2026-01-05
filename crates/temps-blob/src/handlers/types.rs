//! Request and response types for Blob HTTP handlers

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_core::AuditLogger;
use temps_providers::externalsvc::RustfsService;
use temps_providers::ExternalServiceManager;
use utoipa::ToSchema;

use crate::services::{BlobInfo, BlobService, ListResult};

/// Application state for blob handlers
pub struct BlobAppState {
    pub blob_service: Arc<BlobService>,
    pub rustfs_service: Arc<RustfsService>,
    pub external_service_manager: Arc<ExternalServiceManager>,
    pub audit_service: Arc<dyn AuditLogger>,
}

/// Options for uploading a blob
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PutBlobRequest {
    /// Path where the blob will be stored
    #[schema(example = "images/avatar.png")]
    pub pathname: String,
    /// Content type of the blob (optional, will be guessed from extension)
    #[schema(example = "image/png")]
    pub content_type: Option<String>,
    /// Add random suffix to pathname to prevent collisions
    #[schema(example = true)]
    #[serde(default)]
    pub add_random_suffix: bool,
}

/// Query parameters for blob upload
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PutBlobQuery {
    /// Path where the blob will be stored
    #[schema(example = "images/avatar.png")]
    pub pathname: Option<String>,
    /// Content type of the blob (optional, will be guessed from extension)
    #[schema(example = "image/png")]
    pub content_type: Option<String>,
    /// Add random suffix to pathname to prevent collisions
    #[schema(example = true)]
    #[serde(default)]
    pub add_random_suffix: bool,
    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Response after uploading a blob
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobResponse {
    /// URL path to access the blob
    #[schema(example = "/api/blob/123/images/avatar-abc123.png")]
    pub url: String,
    /// Original pathname
    #[schema(example = "images/avatar-abc123.png")]
    pub pathname: String,
    /// Content type of the blob
    #[schema(example = "image/png")]
    pub content_type: String,
    /// Size in bytes
    #[schema(example = 12345)]
    pub size: i64,
    /// Upload timestamp
    #[schema(example = "2025-01-03T12:00:00Z")]
    pub uploaded_at: DateTime<Utc>,
}

impl From<BlobInfo> for BlobResponse {
    fn from(info: BlobInfo) -> Self {
        Self {
            url: info.url,
            pathname: info.pathname,
            content_type: info.content_type,
            size: info.size,
            uploaded_at: info.uploaded_at,
        }
    }
}

/// Request to delete blobs
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteBlobRequest {
    /// Pathnames to delete (relative to project)
    #[schema(example = json!(["images/avatar.png", "documents/file.pdf"]))]
    pub pathnames: Vec<String>,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Response after deleting blobs
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteBlobResponse {
    /// Number of blobs deleted
    #[schema(example = 2)]
    pub deleted: i64,
}

/// Request to copy a blob
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CopyBlobRequest {
    /// Source blob URL or pathname
    #[schema(example = "/api/blob/10/images/avatar.png")]
    pub from_url: String,
    /// Destination pathname
    #[schema(example = "images/avatar-copy.png")]
    pub to_pathname: String,
    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Query parameters for listing blobs
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ListBlobsQuery {
    /// Maximum number of items to return
    #[schema(example = 100)]
    pub limit: Option<i32>,
    /// Prefix to filter by
    #[schema(example = "images/")]
    pub prefix: Option<String>,
    /// Continuation token for pagination
    pub cursor: Option<String>,
    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Response for listing blobs
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListBlobsResponse {
    /// List of blobs
    pub blobs: Vec<BlobResponse>,
    /// Continuation token for next page
    pub cursor: Option<String>,
    /// Whether there are more results
    #[schema(example = false)]
    pub has_more: bool,
}

impl From<ListResult> for ListBlobsResponse {
    fn from(result: ListResult) -> Self {
        Self {
            blobs: result.blobs.into_iter().map(BlobResponse::from).collect(),
            cursor: result.cursor,
            has_more: result.has_more,
        }
    }
}

// =============================================================================
// Management Types
// =============================================================================

/// Response for Blob service status
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BlobStatusResponse {
    /// Whether the Blob service is enabled
    #[schema(example = true)]
    pub enabled: bool,
    /// Whether the service is healthy
    #[schema(example = true)]
    pub healthy: bool,
    /// Current version (if running)
    #[schema(example = "0.5.0")]
    pub version: Option<String>,
    /// Docker image being used
    #[schema(example = "ghcr.io/rustfs/rustfs:0.5.0")]
    pub docker_image: Option<String>,
}

/// Request to enable Blob service
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EnableBlobRequest {
    /// Docker image to use (optional, defaults to RustFS)
    #[schema(example = "ghcr.io/rustfs/rustfs:0.5.0")]
    pub docker_image: Option<String>,
    /// Root user for S3 access
    pub root_user: Option<String>,
    /// Root password for S3 access
    pub root_password: Option<String>,
}

/// Response after enabling Blob service
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnableBlobResponse {
    /// Whether the operation succeeded
    #[schema(example = true)]
    pub success: bool,
    /// Human-readable message
    #[schema(example = "Blob service enabled successfully")]
    pub message: String,
    /// Current status
    pub status: BlobStatusResponse,
}

/// Response after disabling Blob service
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DisableBlobResponse {
    /// Whether the operation succeeded
    #[schema(example = true)]
    pub success: bool,
    /// Human-readable message
    #[schema(example = "Blob service disabled successfully")]
    pub message: String,
}

/// Request to update Blob service configuration
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateBlobRequest {
    /// Docker image to use (e.g., "minio/minio:RELEASE.2025-09-07T16-13-09Z")
    #[schema(example = "minio/minio:RELEASE.2025-09-07T16-13-09Z")]
    pub docker_image: Option<String>,
}

/// Response after updating Blob service
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpdateBlobResponse {
    /// Whether the operation succeeded
    #[schema(example = true)]
    pub success: bool,
    /// Human-readable message
    #[schema(example = "Blob service updated successfully")]
    pub message: String,
    /// Current status
    pub status: BlobStatusResponse,
}
