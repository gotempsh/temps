// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! S3-compatible storage backend for log chunks
//!
//! Works with AWS S3, MinIO, Tigris, Cloudflare R2, and any S3-compatible API.
//! Uses the same storage key layout as the filesystem backend.

use async_trait::async_trait;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::Config;
use tracing::debug;

use crate::error::LogAggregatorError;
use crate::storage::traits::LogStorage;
use crate::types::StorageConfig;

/// S3-compatible log chunk storage.
///
/// Default storage backend. Works with AWS S3, MinIO, Tigris, Cloudflare R2, etc.
pub struct S3Storage {
    client: S3Client,
    bucket: String,
    prefix: Option<String>,
}

impl S3Storage {
    /// Create a new S3 storage backend from configuration.
    pub fn new(config: &StorageConfig) -> Result<Self, LogAggregatorError> {
        match config {
            StorageConfig::S3 {
                bucket,
                prefix,
                region,
                endpoint,
                access_key_id,
                secret_access_key,
                force_path_style,
            } => {
                let creds = Credentials::new(
                    access_key_id,
                    secret_access_key,
                    None,
                    None,
                    "temps-log-aggregator",
                );

                let mut s3_config = Config::builder()
                    .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                    .region(Region::new(region.clone()))
                    .credentials_provider(creds)
                    .force_path_style(*force_path_style);

                if let Some(endpoint_url) = endpoint {
                    s3_config = s3_config.endpoint_url(endpoint_url);
                }

                let client = S3Client::from_conf(s3_config.build());

                debug!(
                    bucket = bucket,
                    region = region,
                    "S3 log storage initialized"
                );

                Ok(Self {
                    client,
                    bucket: bucket.clone(),
                    prefix: prefix.clone(),
                })
            }
            StorageConfig::Filesystem { .. } => Err(LogAggregatorError::StorageConfiguration {
                message: "S3Storage cannot be created from filesystem config".to_string(),
            }),
        }
    }

    /// Build the full S3 key including optional prefix.
    fn full_key(&self, key: &str) -> String {
        match &self.prefix {
            Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), key),
            None => key.to_string(),
        }
    }
}

#[async_trait]
impl LogStorage for S3Storage {
    async fn write_chunk(&self, key: &str, data: &[u8]) -> Result<u64, LogAggregatorError> {
        let full_key = self.full_key(key);
        let body = ByteStream::from(data.to_vec());
        let data_len = data.len() as u64;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .body(body)
            .content_type("application/zstd")
            .send()
            .await
            .map_err(|e| LogAggregatorError::S3 {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: format!("PutObject failed: {}", e),
            })?;

        debug!(
            bucket = self.bucket,
            key = full_key,
            bytes = data_len,
            "Wrote chunk to S3"
        );
        Ok(data_len)
    }

    async fn read_chunk(&self, key: &str) -> Result<Vec<u8>, LogAggregatorError> {
        let full_key = self.full_key(key);

        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
            .map_err(|e| {
                let err_str = e.to_string();
                if err_str.contains("NoSuchKey") || err_str.contains("404") {
                    LogAggregatorError::ChunkNotFound {
                        chunk_id: uuid::Uuid::nil(),
                        storage_key: key.to_string(),
                    }
                } else {
                    LogAggregatorError::S3 {
                        bucket: self.bucket.clone(),
                        key: full_key.clone(),
                        reason: format!("GetObject failed: {}", e),
                    }
                }
            })?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| LogAggregatorError::S3 {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: format!("Failed to read response body: {}", e),
            })?
            .into_bytes()
            .to_vec();

        Ok(data)
    }

    async fn read_chunk_range(
        &self,
        key: &str,
        start: u64,
        end: Option<u64>,
    ) -> Result<Vec<u8>, LogAggregatorError> {
        let full_key = self.full_key(key);
        let range = match end {
            Some(end) => format!("bytes={}-{}", start, end - 1),
            None => format!("bytes={}-", start),
        };

        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .range(range)
            .send()
            .await
            .map_err(|e| LogAggregatorError::S3 {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: format!("GetObject range request failed: {}", e),
            })?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| LogAggregatorError::S3 {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: format!("Failed to read range response body: {}", e),
            })?
            .into_bytes()
            .to_vec();

        Ok(data)
    }

    async fn list_chunks(&self, prefix: &str) -> Result<Vec<String>, LogAggregatorError> {
        let full_prefix = self.full_key(prefix);
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full_prefix);

            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }

            let response = request.send().await.map_err(|e| LogAggregatorError::S3 {
                bucket: self.bucket.clone(),
                key: full_prefix.clone(),
                reason: format!("ListObjectsV2 failed: {}", e),
            })?;

            if let Some(contents) = response.contents {
                for object in contents {
                    if let Some(obj_key) = object.key {
                        // Strip the optional prefix to return storage-relative keys
                        let relative = match &self.prefix {
                            Some(pfx) => obj_key
                                .strip_prefix(&format!("{}/", pfx.trim_end_matches('/')))
                                .unwrap_or(&obj_key)
                                .to_string(),
                            None => obj_key,
                        };
                        keys.push(relative);
                    }
                }
            }

            if response.is_truncated == Some(true) {
                continuation_token = response.next_continuation_token;
            } else {
                break;
            }
        }

        Ok(keys)
    }

    async fn delete_chunk(&self, key: &str) -> Result<(), LogAggregatorError> {
        let full_key = self.full_key(key);

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
            .map_err(|e| LogAggregatorError::S3 {
                bucket: self.bucket.clone(),
                key: full_key.clone(),
                reason: format!("DeleteObject failed: {}", e),
            })?;

        debug!(
            bucket = self.bucket,
            key = full_key,
            "Deleted chunk from S3"
        );
        Ok(())
    }

    async fn chunk_exists(&self, key: &str) -> Result<bool, LogAggregatorError> {
        let full_key = self.full_key(key);

        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("NotFound") || err_str.contains("404") {
                    Ok(false)
                } else {
                    Err(LogAggregatorError::S3 {
                        bucket: self.bucket.clone(),
                        key: full_key,
                        reason: format!("HeadObject failed: {}", e),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_key_with_prefix() {
        let storage = S3Storage {
            client: {
                // Create a minimal client for testing key construction only
                let config = Config::builder()
                    .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                    .region(Region::new("us-east-1"))
                    .build();
                S3Client::from_conf(config)
            },
            bucket: "test-bucket".to_string(),
            prefix: Some("prod".to_string()),
        };

        assert_eq!(
            storage.full_key("logs/proj/web/2026-01-01/00/cnt-000001.ndjson.zst"),
            "prod/logs/proj/web/2026-01-01/00/cnt-000001.ndjson.zst"
        );
    }

    #[test]
    fn test_full_key_without_prefix() {
        let storage = S3Storage {
            client: {
                let config = Config::builder()
                    .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                    .region(Region::new("us-east-1"))
                    .build();
                S3Client::from_conf(config)
            },
            bucket: "test-bucket".to_string(),
            prefix: None,
        };

        assert_eq!(
            storage.full_key("logs/proj/web/2026-01-01/00/cnt-000001.ndjson.zst"),
            "logs/proj/web/2026-01-01/00/cnt-000001.ndjson.zst"
        );
    }

    #[test]
    fn test_full_key_with_trailing_slash_prefix() {
        let storage = S3Storage {
            client: {
                let config = Config::builder()
                    .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                    .region(Region::new("us-east-1"))
                    .build();
                S3Client::from_conf(config)
            },
            bucket: "test-bucket".to_string(),
            prefix: Some("prod/".to_string()),
        };

        assert_eq!(storage.full_key("logs/key.zst"), "prod/logs/key.zst");
    }
}
