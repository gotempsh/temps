//! Redis implementation of the temps-query DataSource trait
//!
//! This crate provides Redis key-value store access through the generic query interface.
//!
//! ## Hierarchy
//!
//! Redis uses a flat namespace with databases (0-15 by default) and keys:
//! - Depth 0: List databases (0-15)
//! - Depth 1: List keys in database (with pattern matching)
//!
//! ## Example
//!
//! ```rust,no_run
//! use temps_query_redis::RedisSource;
//!
//! # async fn example() -> temps_query::Result<()> {
//! let source = RedisSource::new("redis://localhost:6379").await?;
//!
//! // List databases
//! let databases = source.list_containers(&temps_query::ContainerPath::root()).await?;
//!
//! // List keys in database 0
//! let path = temps_query::ContainerPath::from_slice(&["0"]);
//! let keys = source.list_entities(&path).await?;
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, RedisError};
use std::collections::HashMap;
use temps_query::{
    Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType, DataError,
    DataRow, DataSource, DatasetSchema, EntityInfo, FieldDef, FieldType, Result,
};
use tracing::{debug, error};

/// Redis data source implementation
pub struct RedisSource {
    connection: ConnectionManager,
    url: String,
}

impl RedisSource {
    /// Create a new Redis data source
    ///
    /// # Arguments
    ///
    /// * `url` - Redis connection URL (e.g., "redis://localhost:6379")
    pub async fn new(url: &str) -> Result<Self> {
        debug!("Creating Redis source for URL: {}", url);

        let client = redis::Client::open(url).map_err(|e| {
            error!("Failed to create Redis client: {}", e);
            DataError::ConnectionFailed(format!("Failed to create Redis client: {}", e))
        })?;

        let connection = ConnectionManager::new(client).await.map_err(|e| {
            error!("Failed to connect to Redis: {}", e);
            DataError::ConnectionFailed(format!("Failed to connect to Redis: {}", e))
        })?;

        debug!("Redis client created successfully");

        Ok(Self {
            connection,
            url: url.to_string(),
        })
    }

    /// Get connection to a specific Redis database
    async fn get_db_connection(&self, db: i32) -> Result<ConnectionManager> {
        let mut conn = self.connection.clone();

        // Select the database
        redis::cmd("SELECT")
            .arg(db)
            .query_async(&mut conn)
            .await
            .map_err(|e: RedisError| {
                error!("Failed to select database {}: {}", db, e);
                DataError::QueryFailed(format!("Failed to select database {}: {}", db, e))
            })?;

        Ok(conn)
    }

    /// List keys in a specific database with optional pattern
    async fn list_keys_in_db(&self, db: i32, pattern: Option<&str>) -> Result<Vec<EntityInfo>> {
        let mut conn = self.get_db_connection(db).await?;
        let pattern = pattern.unwrap_or("*");

        debug!("Listing keys in database {} with pattern: {}", db, pattern);

        // Use SCAN for better performance with large datasets
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(pattern)
            .query_async(&mut conn)
            .await
            .map_err(|e: RedisError| {
                error!("Failed to list keys in database {}: {}", db, e);
                DataError::QueryFailed(format!("Failed to list keys: {}", e))
            })?;

        let entities: Vec<EntityInfo> = keys
            .into_iter()
            .map(|key| EntityInfo {
                namespace: db.to_string(),
                name: key,
                entity_type: "key".to_string(),
                row_count: Some(1),
                size_bytes: None,
                schema: None,
                metadata: None,
            })
            .collect();

        debug!("Found {} keys in database {}", entities.len(), db);

        Ok(entities)
    }

    /// Get information about a specific key
    async fn get_key_info(&self, db: i32, key: &str) -> Result<EntityInfo> {
        let mut conn = self.get_db_connection(db).await?;

        debug!("Getting info for key '{}' in database {}", key, db);

        // Check if key exists
        let exists: bool = conn.exists(key).await.map_err(|e: RedisError| {
            error!("Failed to check if key exists: {}", e);
            DataError::QueryFailed(format!("Failed to check key existence: {}", e))
        })?;

        if !exists {
            return Err(DataError::NotFound(format!(
                "Key '{}' not found in database {}",
                key, db
            )));
        }

        // Get key type
        let key_type: String = redis::cmd("TYPE")
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e: RedisError| {
                error!("Failed to get key type: {}", e);
                DataError::QueryFailed(format!("Failed to get key type: {}", e))
            })?;

        // Get TTL (time to live)
        let ttl: i64 = conn.ttl(key).await.map_err(|e: RedisError| {
            error!("Failed to get key TTL: {}", e);
            DataError::QueryFailed(format!("Failed to get key TTL: {}", e))
        })?;

        // Build schema based on key type
        let schema = DatasetSchema {
            fields: vec![
                FieldDef {
                    name: "key".to_string(),
                    field_type: FieldType::String,
                    nullable: false,
                    description: Some("Redis key name".to_string()),
                },
                FieldDef {
                    name: "type".to_string(),
                    field_type: FieldType::String,
                    nullable: false,
                    description: Some("Redis data type".to_string()),
                },
                FieldDef {
                    name: "ttl".to_string(),
                    field_type: FieldType::Int64,
                    nullable: true,
                    description: Some("Time to live in seconds (-1 = no expiry)".to_string()),
                },
                FieldDef {
                    name: "value".to_string(),
                    field_type: FieldType::String,
                    nullable: true,
                    description: Some("Key value (for simple types)".to_string()),
                },
            ],
            partitions: None,
            primary_key: Some(vec!["key".to_string()]),
        };

        Ok(EntityInfo {
            namespace: db.to_string(),
            name: key.to_string(),
            entity_type: key_type,
            row_count: Some(1),
            size_bytes: None,
            schema: Some(schema),
            metadata: None,
        })
    }
}

#[async_trait]
impl DataSource for RedisSource {
    fn source_type(&self) -> &'static str {
        "redis"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::KeyValue]
    }

    async fn list_containers(&self, path: &ContainerPath) -> Result<Vec<ContainerInfo>> {
        match path.depth() {
            // Depth 0: List databases (0-15 by default)
            0 => {
                debug!("Listing Redis databases");

                // Redis typically supports 16 databases (0-15)
                let databases: Vec<ContainerInfo> = (0..16)
                    .map(|db_num| {
                        let mut metadata = HashMap::new();
                        metadata.insert("database_number".to_string(), serde_json::json!(db_num));

                        ContainerInfo {
                            name: db_num.to_string(),
                            container_type: ContainerType::Database,
                            capabilities: ContainerCapabilities {
                                can_contain_containers: false,
                                can_contain_entities: true,
                                child_container_type: None,
                                entity_type_label: Some("key".to_string()),
                            },
                            metadata,
                        }
                    })
                    .collect();

                debug!("Found {} databases", databases.len());
                Ok(databases)
            }

            // Depth >= 1: Not supported (Redis is flat - databases contain keys directly)
            _ => Err(DataError::InvalidQuery(format!(
                "Redis hierarchy only supports 1 level (database). Path depth: {}. Use list_entities to list keys.",
                path.depth()
            ))),
        }
    }

    async fn get_container_info(&self, path: &ContainerPath) -> Result<ContainerInfo> {
        if path.depth() != 1 {
            return Err(DataError::InvalidQuery(format!(
                "get_container_info requires path depth 1 (database number), got {}",
                path.depth()
            )));
        }

        let db_str = &path.segments[0];
        let db_num: i32 = db_str.parse().map_err(|_| {
            DataError::InvalidQuery(format!(
                "Invalid database number '{}'. Must be 0-15",
                db_str
            ))
        })?;

        if !(0..=15).contains(&db_num) {
            return Err(DataError::InvalidQuery(format!(
                "Database number {} out of range. Must be 0-15",
                db_num
            )));
        }

        debug!("Getting info for database: {}", db_num);

        let mut metadata = HashMap::new();
        metadata.insert("database_number".to_string(), serde_json::json!(db_num));

        // Try to get database size (number of keys)
        match self.get_db_connection(db_num).await {
            Ok(mut conn) => {
                if let Ok(size) = redis::cmd("DBSIZE").query_async::<usize>(&mut conn).await {
                    metadata.insert("key_count".to_string(), serde_json::json!(size));
                }
            }
            Err(_) => {
                // Connection failed, but we can still return basic info
            }
        }

        Ok(ContainerInfo {
            name: db_num.to_string(),
            container_type: ContainerType::Database,
            capabilities: ContainerCapabilities {
                can_contain_containers: false,
                can_contain_entities: true,
                child_container_type: None,
                entity_type_label: Some("key".to_string()),
            },
            metadata,
        })
    }

    async fn list_entities(&self, container_path: &ContainerPath) -> Result<Vec<EntityInfo>> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot list entities at root level - specify a database path (0-15)".to_string(),
            ));
        }

        let db_str = &container_path.segments[0];
        let db_num: i32 = db_str.parse().map_err(|_| {
            DataError::InvalidQuery(format!(
                "Invalid database number '{}'. Must be 0-15",
                db_str
            ))
        })?;

        if !(0..=15).contains(&db_num) {
            return Err(DataError::InvalidQuery(format!(
                "Database number {} out of range. Must be 0-15",
                db_num
            )));
        }

        // If depth > 1, use remaining segments as key pattern
        let pattern = if container_path.depth() > 1 {
            Some(container_path.segments[1..].join(":"))
        } else {
            None
        };

        self.list_keys_in_db(db_num, pattern.as_deref()).await
    }

    async fn get_entity_info(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot get entity at root level - specify a database path (0-15)".to_string(),
            ));
        }

        let db_str = &container_path.segments[0];
        let db_num: i32 = db_str.parse().map_err(|_| {
            DataError::InvalidQuery(format!(
                "Invalid database number '{}'. Must be 0-15",
                db_str
            ))
        })?;

        if !(0..=15).contains(&db_num) {
            return Err(DataError::InvalidQuery(format!(
                "Database number {} out of range. Must be 0-15",
                db_num
            )));
        }

        self.get_key_info(db_num, entity_name).await
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
        debug!("Closing Redis source");
        // Connection manager handles cleanup automatically
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_type() {
        assert_eq!("redis", "redis");
    }

    #[test]
    fn test_capabilities() {
        let capabilities = vec![Capability::KeyValue];
        assert!(capabilities.contains(&Capability::KeyValue));
    }

    #[test]
    fn test_database_range() {
        // Redis databases are 0-15
        assert!((0..=15).contains(&0));
        assert!((0..=15).contains(&15));
        assert!(!(0..=15).contains(&16));
    }
}
