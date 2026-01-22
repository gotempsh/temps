//! Static bundles entity
//!
//! Tracks uploaded static file bundles (tar.gz or zip archives) for static deployments.
//! These bundles contain pre-built static files (e.g., Vite dist output) that are
//! deployed directly without any build step.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "static_bundles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Project this bundle belongs to
    pub project_id: i32,
    /// Path to the bundle in blob storage (e.g., "static-bundles/project-123/bundle-abc.tar.gz")
    pub blob_path: String,
    /// Original filename from upload (e.g., "dist.tar.gz")
    pub original_filename: Option<String>,
    /// Content type (e.g., "application/gzip", "application/zip")
    pub content_type: String,
    /// Bundle size in bytes
    pub size_bytes: i64,
    /// SHA256 checksum of the bundle
    pub checksum: Option<String>,
    /// Additional metadata (file count, entry point, etc.)
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,
    /// When this bundle was uploaded
    pub uploaded_at: DBDateTime,
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
            if self.uploaded_at.is_not_set() {
                self.uploaded_at = Set(now);
            }
        }

        Ok(self)
    }
}

/// Supported bundle formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFormat {
    TarGz,
    Zip,
}

impl BundleFormat {
    /// Detect bundle format from content type
    pub fn from_content_type(content_type: &str) -> Option<Self> {
        match content_type {
            "application/gzip" | "application/x-gzip" | "application/x-tar" => {
                Some(BundleFormat::TarGz)
            }
            "application/zip" | "application/x-zip-compressed" => Some(BundleFormat::Zip),
            _ => None,
        }
    }

    /// Detect bundle format from filename extension
    pub fn from_filename(filename: &str) -> Option<Self> {
        let lower = filename.to_lowercase();
        if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Some(BundleFormat::TarGz)
        } else if lower.ends_with(".zip") {
            Some(BundleFormat::Zip)
        } else {
            None
        }
    }

    /// Get the content type for this format
    pub fn content_type(&self) -> &'static str {
        match self {
            BundleFormat::TarGz => "application/gzip",
            BundleFormat::Zip => "application/zip",
        }
    }

    /// Get the file extension for this format
    pub fn extension(&self) -> &'static str {
        match self {
            BundleFormat::TarGz => ".tar.gz",
            BundleFormat::Zip => ".zip",
        }
    }
}

impl Model {
    /// Get the bundle format based on content type
    pub fn format(&self) -> Option<BundleFormat> {
        BundleFormat::from_content_type(&self.content_type)
    }

    /// Check if this is a tar.gz bundle
    pub fn is_tar_gz(&self) -> bool {
        matches!(self.format(), Some(BundleFormat::TarGz))
    }

    /// Check if this is a zip bundle
    pub fn is_zip(&self) -> bool {
        matches!(self.format(), Some(BundleFormat::Zip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bundle_format_from_content_type() {
        assert_eq!(
            BundleFormat::from_content_type("application/gzip"),
            Some(BundleFormat::TarGz)
        );
        assert_eq!(
            BundleFormat::from_content_type("application/zip"),
            Some(BundleFormat::Zip)
        );
        assert_eq!(BundleFormat::from_content_type("text/plain"), None);
    }

    #[test]
    fn test_bundle_format_from_filename() {
        assert_eq!(
            BundleFormat::from_filename("dist.tar.gz"),
            Some(BundleFormat::TarGz)
        );
        assert_eq!(
            BundleFormat::from_filename("dist.tgz"),
            Some(BundleFormat::TarGz)
        );
        assert_eq!(
            BundleFormat::from_filename("dist.zip"),
            Some(BundleFormat::Zip)
        );
        assert_eq!(BundleFormat::from_filename("dist.txt"), None);
    }

    #[test]
    fn test_bundle_format_content_type() {
        assert_eq!(BundleFormat::TarGz.content_type(), "application/gzip");
        assert_eq!(BundleFormat::Zip.content_type(), "application/zip");
    }
}
