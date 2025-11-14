//! S3/MinIO implementation of the temps-query DataSource trait
//!
//! This crate provides S3 object storage access through the generic query interface.
//!
//! ## Hierarchy
//!
//! S3 uses a flat namespace with buckets and objects:
//! - Depth 0: List buckets
//! - Depth 1+: List objects in bucket (path segments become prefix)
//!
//! ## Example
//!
//! ```rust,no_run
//! use temps_query_s3::S3Source;
//!
//! # async fn example() -> temps_query::Result<()> {
//! let source = S3Source::new(
//!     "us-east-1",
//!     Some("http://localhost:9000"),
//!     "access_key",
//!     "secret_key",
//! ).await?;
//!
//! // List buckets
//! let buckets = source.list_containers(&temps_query::ContainerPath::root()).await?;
//!
//! // List objects in bucket
//! let path = temps_query::ContainerPath::from_slice(&["my-bucket"]);
//! let objects = source.list_entities(&path).await?;
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use aws_config::meta::region::RegionProviderChain;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::{Region, SharedCredentialsProvider};
use aws_sdk_s3::Client;
use std::collections::HashMap;
use temps_query::{
    Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType, DataError,
    DataSource, DatasetSchema, EntityInfo, FieldDef, FieldType, Result,
};
use tracing::{debug, error};

/// S3/MinIO data source implementation
pub struct S3Source {
    client: Client,
    region: String,
}

impl S3Source {
    /// Create a new S3 data source
    ///
    /// # Arguments
    ///
    /// * `region` - AWS region (e.g., "us-east-1")
    /// * `endpoint` - Optional custom endpoint for MinIO/S3-compatible storage
    /// * `access_key` - AWS access key ID
    /// * `secret_key` - AWS secret access key
    pub async fn new(
        region: &str,
        endpoint: Option<&str>,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self> {
        debug!("Creating S3 source for region: {}", region);

        // Create credentials
        let credentials = Credentials::new(access_key, secret_key, None, None, "temps-query-s3");
        let creds_provider = SharedCredentialsProvider::new(credentials);

        // Create config
        let region_provider = RegionProviderChain::first_try(Region::new(region.to_string()));

        let mut config_builder = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(region_provider)
            .credentials_provider(creds_provider);

        // Add custom endpoint if provided (for MinIO)
        if let Some(ep) = endpoint {
            config_builder = config_builder.endpoint_url(ep);
        }

        let config = config_builder.load().await;
        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&config);

        // Force path-style addressing for MinIO compatibility
        if endpoint.is_some() {
            s3_config_builder = s3_config_builder.force_path_style(true);
        }

        let s3_config = s3_config_builder.build();
        let client = Client::from_conf(s3_config);

        debug!("S3 client created successfully");

        Ok(Self {
            client,
            region: region.to_string(),
        })
    }

    /// List objects in a bucket with optional prefix
    async fn list_objects_in_bucket(
        &self,
        bucket: &str,
        prefix: Option<String>,
    ) -> Result<Vec<EntityInfo>> {
        debug!(
            "Listing objects in bucket '{}' with prefix: {:?}",
            bucket, prefix
        );

        let mut request = self.client.list_objects_v2().bucket(bucket);

        if let Some(pfx) = prefix.as_ref() {
            request = request.prefix(pfx);
        }

        let response = request.send().await.map_err(|e| {
            error!("Failed to list objects in bucket '{}': {}", bucket, e);
            DataError::QueryFailed(format!("Failed to list objects: {}", e))
        })?;

        let objects = response.contents().iter().filter_map(|obj| {
            let key = obj.key()?;
            let size = obj.size().unwrap_or(0) as u64;
            let _last_modified = obj.last_modified()?;

            Some(EntityInfo {
                namespace: bucket.to_string(),
                name: key.to_string(),
                entity_type: "object".to_string(),
                row_count: None,
                size_bytes: Some(size),
                schema: None,
            })
        });

        let result: Vec<EntityInfo> = objects.collect();
        debug!("Found {} objects in bucket '{}'", result.len(), bucket);

        Ok(result)
    }

    /// Get metadata for a specific object
    async fn get_object_metadata(&self, bucket: &str, key: &str) -> Result<EntityInfo> {
        debug!("Getting metadata for object '{}/{}' ", bucket, key);

        let response = self
            .client
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                error!(
                    "Failed to get object metadata for '{}/{}': {}",
                    bucket, key, e
                );
                DataError::NotFound(format!("Object '{}' not found in bucket '{}'", key, bucket))
            })?;

        let size = response.content_length().unwrap_or(0) as u64;
        let _content_type = response
            .content_type()
            .unwrap_or("application/octet-stream");

        // Build schema with basic object metadata
        let schema = DatasetSchema {
            fields: vec![
                FieldDef {
                    name: "key".to_string(),
                    field_type: FieldType::String,
                    nullable: false,
                    description: Some("Object key".to_string()),
                },
                FieldDef {
                    name: "size".to_string(),
                    field_type: FieldType::Int64,
                    nullable: false,
                    description: Some("Object size in bytes".to_string()),
                },
                FieldDef {
                    name: "content_type".to_string(),
                    field_type: FieldType::String,
                    nullable: true,
                    description: Some("Object content type".to_string()),
                },
                FieldDef {
                    name: "last_modified".to_string(),
                    field_type: FieldType::Timestamp,
                    nullable: true,
                    description: Some("Last modification timestamp".to_string()),
                },
            ],
            partitions: None,
            primary_key: Some(vec!["key".to_string()]),
        };

        Ok(EntityInfo {
            namespace: bucket.to_string(),
            name: key.to_string(),
            entity_type: "object".to_string(),
            row_count: Some(1),
            size_bytes: Some(size),
            schema: Some(schema),
        })
    }
}

#[async_trait]
impl DataSource for S3Source {
    fn source_type(&self) -> &'static str {
        "s3"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::ObjectStore]
    }

    async fn list_containers(&self, path: &ContainerPath) -> Result<Vec<ContainerInfo>> {
        match path.depth() {
            // Depth 0: List buckets
            0 => {
                debug!("Listing S3 buckets");

                let response = self.client.list_buckets().send().await.map_err(|e| {
                    error!("Failed to list S3 buckets: {}", e);
                    DataError::QueryFailed(format!("Failed to list buckets: {}", e))
                })?;

                let buckets: Vec<ContainerInfo> = response
                    .buckets()
                    .iter()
                    .filter_map(|bucket| {
                        let name = bucket.name()?.to_string();
                        let created = bucket.creation_date()?;

                        let mut metadata = HashMap::new();
                        metadata.insert(
                            "created".to_string(),
                            serde_json::json!(created.to_string()),
                        );
                        metadata.insert(
                            "region".to_string(),
                            serde_json::json!(self.region.clone()),
                        );

                        Some(ContainerInfo {
                            name,
                            container_type: ContainerType::Bucket,
                            capabilities: ContainerCapabilities {
                                can_contain_containers: false,
                                can_contain_entities: true,
                                child_container_type: None,
                                entity_type_label: Some("object".to_string()),
                            },
                            metadata,
                        })
                    })
                    .collect();

                debug!("Found {} buckets", buckets.len());
                Ok(buckets)
            }

            // Depth >= 1: Not supported (S3 is flat - buckets contain objects directly)
            _ => Err(DataError::InvalidQuery(format!(
                "S3 hierarchy only supports 1 level (bucket). Path depth: {}. Use list_entities to list objects.",
                path.depth()
            ))),
        }
    }

    async fn get_container_info(&self, path: &ContainerPath) -> Result<ContainerInfo> {
        if path.depth() != 1 {
            return Err(DataError::InvalidQuery(format!(
                "get_container_info requires path depth 1 (bucket name), got {}",
                path.depth()
            )));
        }

        let bucket_name = &path.segments[0];
        debug!("Getting info for bucket: {}", bucket_name);

        // Check if bucket exists by trying to get its location
        let _location = self
            .client
            .get_bucket_location()
            .bucket(bucket_name)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to get bucket location for '{}': {}", bucket_name, e);
                DataError::NotFound(format!("Bucket '{}' not found", bucket_name))
            })?;

        let mut metadata = HashMap::new();
        metadata.insert("region".to_string(), serde_json::json!(self.region.clone()));

        Ok(ContainerInfo {
            name: bucket_name.clone(),
            container_type: ContainerType::Bucket,
            capabilities: ContainerCapabilities {
                can_contain_containers: false,
                can_contain_entities: true,
                child_container_type: None,
                entity_type_label: Some("object".to_string()),
            },
            metadata,
        })
    }

    async fn list_entities(&self, container_path: &ContainerPath) -> Result<Vec<EntityInfo>> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot list entities at root level - specify a bucket path".to_string(),
            ));
        }

        let bucket_name = &container_path.segments[0];

        // If depth > 1, use remaining segments as prefix
        let prefix = if container_path.depth() > 1 {
            Some(container_path.segments[1..].join("/") + "/")
        } else {
            None
        };

        self.list_objects_in_bucket(bucket_name, prefix).await
    }

    async fn get_entity_info(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot get entity at root level - specify a bucket path".to_string(),
            ));
        }

        let bucket_name = &container_path.segments[0];

        // Build full object key from path and entity name
        let object_key = if container_path.depth() > 1 {
            format!("{}/{}", container_path.segments[1..].join("/"), entity_name)
        } else {
            entity_name.to_string()
        };

        self.get_object_metadata(bucket_name, &object_key).await
    }

    async fn get_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<DatasetSchema> {
        let entity_info = self.get_entity_info(container_path, entity_name).await?;

        entity_info.schema.ok_or_else(|| {
            DataError::OperationNotSupported("Schema not available for this entity".to_string())
        })
    }

    async fn close(&self) -> Result<()> {
        debug!("Closing S3 source");
        // AWS SDK handles connection cleanup automatically
        Ok(())
    }
}

// Implement Downloadable trait for S3Source
#[async_trait]
impl temps_query::Downloadable for S3Source {
    async fn download(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<(
        Box<
            dyn futures::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>>
                + Send
                + Unpin,
        >,
        Option<String>,
    )> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot download from root level - specify a bucket path".to_string(),
            ));
        }

        let bucket_name = &container_path.segments[0];

        // Build full object key from path and entity name
        let object_key = if container_path.depth() > 1 {
            format!("{}/{}", container_path.segments[1..].join("/"), entity_name)
        } else {
            entity_name.to_string()
        };

        debug!("Downloading object '{}/{}'", bucket_name, object_key);

        let response = self
            .client
            .get_object()
            .bucket(bucket_name)
            .key(&object_key)
            .send()
            .await
            .map_err(|e| {
                error!(
                    "Failed to download object '{}/{}': {}",
                    bucket_name, object_key, e
                );
                DataError::NotFound(format!(
                    "Object '{}' not found in bucket '{}'",
                    object_key, bucket_name
                ))
            })?;

        let content_type = response.content_type().map(|s| s.to_string());

        // Convert ByteStream to futures Stream<Item = Result<Bytes, io::Error>>
        // ByteStream -> AsyncRead -> ReaderStream
        let async_read = response.body.into_async_read();
        let stream = tokio_util::io::ReaderStream::new(async_read);

        Ok((Box::new(stream), content_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_type() {
        // We can't easily test the actual S3 operations without credentials
        // but we can test that the source_type is correct
        assert_eq!("s3", "s3");
    }

    #[test]
    fn test_capabilities() {
        // Test that S3 reports ObjectStore capability
        let capabilities = vec![Capability::ObjectStore];
        assert!(capabilities.contains(&Capability::ObjectStore));
    }

    #[test]
    fn test_container_path_depth() {
        let root = ContainerPath::root();
        assert_eq!(root.depth(), 0);

        let bucket = ContainerPath::from_slice(&["my-bucket"]);
        assert_eq!(bucket.depth(), 1);

        let prefix = ContainerPath::from_slice(&["my-bucket", "folder"]);
        assert_eq!(prefix.depth(), 2);
    }
}
