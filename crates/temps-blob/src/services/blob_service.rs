//! Blob Service implementation with RustFS/S3 backend

use std::sync::Arc;

use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;
use temps_providers::externalsvc::RustfsService;
use tracing::debug;
use uuid::Uuid;

use crate::error::BlobError;

/// Default bucket name for blobs
pub const DEFAULT_BUCKET: &str = "temps-blobs";

/// Options for PUT operations
#[derive(Debug, Clone, Default)]
pub struct PutOptions {
    /// Content type of the blob
    pub content_type: Option<String>,
    /// Add random suffix to pathname
    pub add_random_suffix: bool,
}

/// Information about a stored blob
#[derive(Debug, Clone)]
pub struct BlobInfo {
    /// URL path to access the blob
    pub url: String,
    /// Original pathname
    pub pathname: String,
    /// Content type
    pub content_type: String,
    /// Size in bytes
    pub size: i64,
    /// Upload timestamp
    pub uploaded_at: DateTime<Utc>,
}

/// Options for LIST operations
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// Maximum number of items to return
    pub limit: Option<i32>,
    /// Prefix to filter by
    pub prefix: Option<String>,
    /// Continuation token for pagination
    pub cursor: Option<String>,
}

/// Result of a LIST operation
#[derive(Debug, Clone)]
pub struct ListResult {
    /// List of blobs
    pub blobs: Vec<BlobInfo>,
    /// Continuation token for next page
    pub cursor: Option<String>,
    /// Whether there are more results
    pub has_more: bool,
}

/// Blob Service for file storage operations with project isolation
pub struct BlobService {
    rustfs_service: Arc<RustfsService>,
}

impl BlobService {
    /// Create a new Blob service
    pub fn new(rustfs_service: Arc<RustfsService>) -> Self {
        Self { rustfs_service }
    }

    /// Build the object key with project namespace
    fn object_key(&self, project_id: i32, pathname: &str) -> String {
        // Normalize pathname - remove leading slash if present
        let normalized = pathname.trim_start_matches('/');
        format!("p{}/{}", project_id, normalized)
    }

    /// Build the URL path for a blob
    fn blob_url(&self, project_id: i32, pathname: &str) -> String {
        format!(
            "/api/blob/{}/{}",
            project_id,
            pathname.trim_start_matches('/')
        )
    }

    /// Extract pathname from object key
    fn extract_pathname(&self, project_id: i32, key: &str) -> String {
        let prefix = format!("p{}/", project_id);
        key.strip_prefix(&prefix).unwrap_or(key).to_string()
    }

    /// Upload a blob
    pub async fn put(
        &self,
        project_id: i32,
        pathname: &str,
        body: Bytes,
        options: PutOptions,
    ) -> Result<BlobInfo, BlobError> {
        let client = self
            .rustfs_service
            .get_connection()
            .await
            .map_err(|e| BlobError::ConnectionFailed(e.to_string()))?;

        // Generate final pathname with optional random suffix
        let final_pathname = if options.add_random_suffix {
            add_random_suffix(pathname)
        } else {
            pathname.to_string()
        };

        let key = self.object_key(project_id, &final_pathname);
        let content_type = options
            .content_type
            .unwrap_or_else(|| guess_content_type(&final_pathname));

        let size = body.len() as i64;

        debug!("PUT {} ({} bytes, {})", key, size, content_type);

        client
            .put_object()
            .bucket(DEFAULT_BUCKET)
            .key(&key)
            .body(ByteStream::from(body))
            .content_type(&content_type)
            .send()
            .await
            .map_err(|e| BlobError::UploadFailed(e.to_string()))?;

        Ok(BlobInfo {
            url: self.blob_url(project_id, &final_pathname),
            pathname: final_pathname,
            content_type,
            size,
            uploaded_at: Utc::now(),
        })
    }

    /// Delete one or more blobs
    pub async fn del(&self, project_id: i32, pathnames: Vec<String>) -> Result<i64, BlobError> {
        if pathnames.is_empty() {
            return Ok(0);
        }

        let client = self
            .rustfs_service
            .get_connection()
            .await
            .map_err(|e| BlobError::ConnectionFailed(e.to_string()))?;
        let mut deleted = 0i64;

        for pathname in pathnames {
            let key = self.object_key(project_id, &pathname);
            debug!("DELETE {}", key);

            match client
                .delete_object()
                .bucket(DEFAULT_BUCKET)
                .key(&key)
                .send()
                .await
            {
                Ok(_) => deleted += 1,
                Err(e) => {
                    debug!("Failed to delete {}: {}", key, e);
                    // Continue with other deletions
                }
            }
        }

        Ok(deleted)
    }

    /// Get blob metadata
    pub async fn head(&self, project_id: i32, pathname: &str) -> Result<BlobInfo, BlobError> {
        let client = self
            .rustfs_service
            .get_connection()
            .await
            .map_err(|e| BlobError::ConnectionFailed(e.to_string()))?;
        let key = self.object_key(project_id, pathname);

        debug!("HEAD {}", key);

        let response = client
            .head_object()
            .bucket(DEFAULT_BUCKET)
            .key(&key)
            .send()
            .await
            .map_err(|e| {
                if e.to_string().contains("NotFound") || e.to_string().contains("404") {
                    BlobError::NotFound(pathname.to_string())
                } else {
                    BlobError::S3(e.to_string())
                }
            })?;

        let content_type = response
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let size = response.content_length().unwrap_or(0);

        let uploaded_at = response
            .last_modified()
            .and_then(|dt| {
                DateTime::parse_from_rfc3339(&dt.to_string())
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            })
            .unwrap_or_else(Utc::now);

        Ok(BlobInfo {
            url: self.blob_url(project_id, pathname),
            pathname: pathname.to_string(),
            content_type,
            size,
            uploaded_at,
        })
    }

    /// List blobs with pagination
    pub async fn list(
        &self,
        project_id: i32,
        options: ListOptions,
    ) -> Result<ListResult, BlobError> {
        let client = self
            .rustfs_service
            .get_connection()
            .await
            .map_err(|e| BlobError::ConnectionFailed(e.to_string()))?;

        let prefix = match options.prefix {
            Some(ref p) => self.object_key(project_id, p),
            None => format!("p{}/", project_id),
        };

        debug!("LIST prefix={}", prefix);

        let mut request = client
            .list_objects_v2()
            .bucket(DEFAULT_BUCKET)
            .prefix(&prefix);

        if let Some(limit) = options.limit {
            request = request.max_keys(limit);
        }

        if let Some(cursor) = options.cursor {
            request = request.continuation_token(cursor);
        }

        let response = request
            .send()
            .await
            .map_err(|e| BlobError::S3(e.to_string()))?;

        let blobs: Vec<BlobInfo> = response
            .contents()
            .iter()
            .filter_map(|obj| {
                let key = obj.key()?;
                let pathname = self.extract_pathname(project_id, key);

                Some(BlobInfo {
                    url: self.blob_url(project_id, &pathname),
                    pathname,
                    content_type: "application/octet-stream".to_string(), // Would need head call for actual type
                    size: obj.size().unwrap_or(0),
                    uploaded_at: obj
                        .last_modified()
                        .and_then(|dt| {
                            DateTime::parse_from_rfc3339(&dt.to_string())
                                .ok()
                                .map(|d| d.with_timezone(&Utc))
                        })
                        .unwrap_or_else(Utc::now),
                })
            })
            .collect();

        let cursor = response.next_continuation_token().map(|s| s.to_string());
        let has_more = response.is_truncated().unwrap_or(false);

        Ok(ListResult {
            blobs,
            cursor,
            has_more,
        })
    }

    /// Download blob content as a stream
    pub async fn download(
        &self,
        project_id: i32,
        pathname: &str,
    ) -> Result<
        (
            impl Stream<Item = Result<Bytes, std::io::Error>>,
            String,
            i64,
        ),
        BlobError,
    > {
        let client = self
            .rustfs_service
            .get_connection()
            .await
            .map_err(|e| BlobError::ConnectionFailed(e.to_string()))?;
        let key = self.object_key(project_id, pathname);

        debug!("GET {}", key);

        let response = client
            .get_object()
            .bucket(DEFAULT_BUCKET)
            .key(&key)
            .send()
            .await
            .map_err(|e| {
                if e.to_string().contains("NoSuchKey") || e.to_string().contains("404") {
                    BlobError::NotFound(pathname.to_string())
                } else {
                    BlobError::S3(e.to_string())
                }
            })?;

        let content_type = response
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let size = response.content_length().unwrap_or(0);

        // Convert ByteStream to a Stream of Bytes
        let stream = response.body.into_async_read();
        let reader_stream = tokio_util::io::ReaderStream::new(stream);

        Ok((reader_stream, content_type, size))
    }
}

/// Add a random suffix to a pathname before the extension
fn add_random_suffix(pathname: &str) -> String {
    let suffix = Uuid::new_v4().to_string()[..8].to_string();

    if let Some(dot_pos) = pathname.rfind('.') {
        format!(
            "{}-{}{}",
            &pathname[..dot_pos],
            suffix,
            &pathname[dot_pos..]
        )
    } else {
        format!("{}-{}", pathname, suffix)
    }
}

/// Guess content type from pathname extension
fn guess_content_type(pathname: &str) -> String {
    let extension = pathname.rsplit('.').next().unwrap_or("").to_lowercase();

    match extension.as_str() {
        // Images
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        // Documents
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        // Text
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        // Archives
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" | "gzip" => "application/gzip",
        // Media
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        // Default
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::EncryptionService;

    /// Create a test BlobService with a RustfsService
    fn create_test_blob_service() -> BlobService {
        let docker = bollard::Docker::connect_with_local_defaults().unwrap();
        // Generate a valid 32-byte hex key for testing
        let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let encryption_service = Arc::new(EncryptionService::new(test_key).unwrap());
        let rustfs_service = Arc::new(RustfsService::new(
            "test-blob".to_string(),
            Arc::new(docker),
            encryption_service,
        ));
        BlobService::new(rustfs_service)
    }

    #[test]
    fn test_add_random_suffix() {
        let result = add_random_suffix("image.png");
        assert!(result.starts_with("image-"));
        assert!(result.ends_with(".png"));
        assert!(result.len() > "image.png".len());
    }

    #[test]
    fn test_add_random_suffix_no_extension() {
        let result = add_random_suffix("README");
        assert!(result.starts_with("README-"));
    }

    #[test]
    fn test_guess_content_type() {
        assert_eq!(guess_content_type("image.png"), "image/png");
        assert_eq!(guess_content_type("document.pdf"), "application/pdf");
        assert_eq!(guess_content_type("data.json"), "application/json");
        assert_eq!(guess_content_type("unknown"), "application/octet-stream");
    }

    #[test]
    fn test_object_key() {
        let service = create_test_blob_service();

        assert_eq!(
            service.object_key(123, "images/avatar.png"),
            "p123/images/avatar.png"
        );
        assert_eq!(
            service.object_key(123, "/images/avatar.png"),
            "p123/images/avatar.png"
        );
    }

    #[test]
    fn test_blob_url() {
        let service = create_test_blob_service();

        assert_eq!(
            service.blob_url(123, "images/avatar.png"),
            "/api/blob/123/images/avatar.png"
        );
    }
}
