//! Blob Service for LocalTemps
//!
//! Provides blob storage operations on top of RustFS (S3-compatible) with project isolation.

use anyhow::Result;
use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use std::sync::Arc;
use tracing::debug;
use uuid::Uuid;

use super::RustfsService;

/// Options for PUT operations
#[derive(Debug, Clone, Default)]
pub struct PutOptions {
    /// Content type of the blob
    pub content_type: Option<String>,
    /// Whether to add a random suffix to the pathname
    pub add_random_suffix: bool,
}

/// Options for LIST operations
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// Maximum number of items to return
    pub limit: Option<i32>,
    /// Prefix filter
    pub prefix: Option<String>,
    /// Cursor for pagination
    pub cursor: Option<String>,
}

/// Information about an uploaded blob
#[derive(Debug, Clone, serde::Serialize)]
pub struct BlobUploadInfo {
    pub url: String,
    pub pathname: String,
    pub content_type: String,
    pub size: i64,
    pub uploaded_at: DateTime<Utc>,
}

/// Information about a blob in a list
#[derive(Debug, Clone, serde::Serialize)]
pub struct BlobListItem {
    pub url: String,
    pub pathname: String,
    pub content_type: String,
    pub size: i64,
    pub uploaded_at: DateTime<Utc>,
}

/// Result of a list operation
#[derive(Debug, Clone, serde::Serialize)]
pub struct BlobListResult {
    pub blobs: Vec<BlobListItem>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// Blob Service - provides blob storage operations with project isolation
pub struct BlobService {
    rustfs: Arc<RustfsService>,
}

impl BlobService {
    pub fn new(rustfs: Arc<RustfsService>) -> Self {
        Self { rustfs }
    }

    /// Get project-scoped bucket name
    fn bucket_name(&self, project_id: i32) -> String {
        format!("project-{}", project_id)
    }

    /// Build URL for a blob
    fn build_url(&self, project_id: i32, pathname: &str) -> String {
        format!("http://localhost:4000/api/blob/{}/{}", project_id, pathname)
    }

    /// Ensure the project bucket exists
    async fn ensure_bucket(&self, project_id: i32) -> Result<()> {
        let client = self.rustfs.get_client().await?;
        let bucket = self.bucket_name(project_id);

        // Check if bucket exists
        match client.head_bucket().bucket(&bucket).send().await {
            Ok(_) => {
                debug!("Bucket {} already exists", bucket);
                Ok(())
            }
            Err(_) => {
                // Create bucket
                client.create_bucket().bucket(&bucket).send().await?;
                debug!("Created bucket {}", bucket);
                Ok(())
            }
        }
    }

    /// Upload a blob
    pub async fn put(
        &self,
        project_id: i32,
        pathname: &str,
        data: Bytes,
        options: PutOptions,
    ) -> Result<BlobUploadInfo> {
        self.ensure_bucket(project_id).await?;

        let client = self.rustfs.get_client().await?;
        let bucket = self.bucket_name(project_id);

        // Generate final pathname
        let final_pathname = if options.add_random_suffix {
            let suffix = Uuid::new_v4().to_string()[..8].to_string();
            let (name, ext) = pathname.rsplit_once('.').unwrap_or((pathname, ""));
            if ext.is_empty() {
                format!("{}-{}", name, suffix)
            } else {
                format!("{}-{}.{}", name, suffix, ext)
            }
        } else {
            pathname.to_string()
        };

        let size = data.len() as i64;
        let content_type = options
            .content_type
            .unwrap_or_else(|| "application/octet-stream".to_string());

        client
            .put_object()
            .bucket(&bucket)
            .key(&final_pathname)
            .body(ByteStream::from(data))
            .content_type(&content_type)
            .send()
            .await?;

        let url = self.build_url(project_id, &final_pathname);

        debug!(
            "BLOB PUT {} in bucket {} (project {})",
            final_pathname, bucket, project_id
        );

        Ok(BlobUploadInfo {
            url,
            pathname: final_pathname,
            content_type,
            size,
            uploaded_at: Utc::now(),
        })
    }

    /// List blobs
    pub async fn list(&self, project_id: i32, options: ListOptions) -> Result<BlobListResult> {
        self.ensure_bucket(project_id).await?;

        let client = self.rustfs.get_client().await?;
        let bucket = self.bucket_name(project_id);

        let mut request = client.list_objects_v2().bucket(&bucket);

        if let Some(ref prefix) = options.prefix {
            if !prefix.is_empty() {
                request = request.prefix(prefix);
            }
        }

        if let Some(limit) = options.limit {
            request = request.max_keys(limit);
        }

        if let Some(ref cursor) = options.cursor {
            request = request.continuation_token(cursor);
        }

        let response = request.send().await?;

        let blobs = response
            .contents()
            .iter()
            .map(|obj| {
                let pathname = obj.key().unwrap_or_default().to_string();
                BlobListItem {
                    url: self.build_url(project_id, &pathname),
                    pathname,
                    content_type: "application/octet-stream".to_string(), // S3 doesn't return content-type in list
                    size: obj.size().unwrap_or(0),
                    uploaded_at: obj
                        .last_modified()
                        .map(|dt| {
                            DateTime::parse_from_rfc3339(&dt.to_string())
                                .map(|d| d.with_timezone(&Utc))
                                .unwrap_or_else(|_| Utc::now())
                        })
                        .unwrap_or_else(Utc::now),
                }
            })
            .collect();

        let has_more = response.is_truncated().unwrap_or(false);
        let cursor = response.next_continuation_token().map(|s| s.to_string());

        debug!(
            "BLOB LIST prefix={:?} (project {})",
            options.prefix, project_id
        );

        Ok(BlobListResult {
            blobs,
            cursor,
            has_more,
        })
    }

    /// Delete multiple blobs and return count of deleted items
    pub async fn del(&self, project_id: i32, pathnames: Vec<String>) -> Result<i64> {
        let client = self.rustfs.get_client().await?;
        let bucket = self.bucket_name(project_id);
        let count = pathnames.len() as i64;

        for pathname in pathnames {
            let _ = client
                .delete_object()
                .bucket(&bucket)
                .key(&pathname)
                .send()
                .await;
            debug!(
                "BLOB DELETE {} from bucket {} (project {})",
                pathname, bucket, project_id
            );
        }

        Ok(count)
    }

    /// Get blob metadata (HEAD)
    pub async fn head(&self, project_id: i32, pathname: &str) -> Result<BlobUploadInfo> {
        let client = self.rustfs.get_client().await?;
        let bucket = self.bucket_name(project_id);

        let response = client
            .head_object()
            .bucket(&bucket)
            .key(pathname)
            .send()
            .await?;

        let content_type = response
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let size = response.content_length().unwrap_or(0);

        let uploaded_at = response
            .last_modified()
            .map(|dt| {
                DateTime::parse_from_rfc3339(&dt.to_string())
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now())
            })
            .unwrap_or_else(Utc::now);

        Ok(BlobUploadInfo {
            url: self.build_url(project_id, pathname),
            pathname: pathname.to_string(),
            content_type,
            size,
            uploaded_at,
        })
    }

    /// Download blob (returns stream, content_type, size)
    pub async fn download(
        &self,
        project_id: i32,
        pathname: &str,
    ) -> Result<(
        BoxStream<'static, Result<Bytes, anyhow::Error>>,
        String,
        i64,
    )> {
        let client = self.rustfs.get_client().await?;
        let bucket = self.bucket_name(project_id);

        let response = client
            .get_object()
            .bucket(&bucket)
            .key(pathname)
            .send()
            .await?;

        let content_type = response
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let size = response.content_length().unwrap_or(0);

        // Convert ByteStream to a stream of Bytes
        let byte_stream = response.body;
        let stream = futures::stream::unfold(byte_stream, |mut bs| async move {
            match bs.next().await {
                Some(Ok(chunk)) => Some((Ok(chunk), bs)),
                Some(Err(e)) => Some((Err(anyhow::anyhow!("Stream error: {}", e)), bs)),
                None => None,
            }
        });

        Ok((Box::pin(stream), content_type, size))
    }

    /// Copy a blob to a new pathname
    pub async fn copy(
        &self,
        project_id: i32,
        from_pathname: &str,
        to_pathname: &str,
    ) -> Result<BlobUploadInfo> {
        let client = self.rustfs.get_client().await?;
        let bucket = self.bucket_name(project_id);

        // Get source metadata
        let head = client
            .head_object()
            .bucket(&bucket)
            .key(from_pathname)
            .send()
            .await?;

        let content_type = head
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let size = head.content_length().unwrap_or(0);

        // Copy object
        let copy_source = format!("{}/{}", bucket, from_pathname);
        client
            .copy_object()
            .bucket(&bucket)
            .key(to_pathname)
            .copy_source(&copy_source)
            .send()
            .await?;

        debug!(
            "BLOB COPY {} -> {} in bucket {} (project {})",
            from_pathname, to_pathname, bucket, project_id
        );

        Ok(BlobUploadInfo {
            url: self.build_url(project_id, to_pathname),
            pathname: to_pathname.to_string(),
            content_type,
            size,
            uploaded_at: Utc::now(),
        })
    }
}
