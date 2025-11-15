use std::collections::HashMap;
use std::sync::Arc;
use temps_query::{
    ContainerInfo, ContainerPath, DataError, DataSource, EntityInfo, QueryOptions, QueryResult,
    Result,
};
use temps_query_mongodb::MongoDBSource;
use temps_query_postgres::PostgresSource;
use temps_query_redis::RedisSource;
use temps_query_s3::S3Source;
use tokio::sync::RwLock;
use tracing::{debug, error};

use crate::externalsvc::mongodb::MongodbInputConfig;
use crate::externalsvc::postgres::PostgresInputConfig;
use crate::externalsvc::redis::RedisInputConfig;
use crate::externalsvc::s3::S3InputConfig;
use crate::ExternalServiceManager;

/// Service for managing query connections to external services
pub struct QueryService {
    external_service_manager: Arc<ExternalServiceManager>,
    /// Cache of active connections by (service_id, database_name)
    connections: Arc<RwLock<HashMap<(i32, String), Arc<dyn DataSource>>>>,
}

impl QueryService {
    pub fn new(external_service_manager: Arc<ExternalServiceManager>) -> Self {
        Self {
            external_service_manager,
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create a connection to a specific database
    /// If force_new is true, bypass cache and create a new connection
    async fn get_connection_for_database_internal(
        &self,
        service_id: i32,
        database: &str,
        force_new: bool,
    ) -> Result<Arc<dyn DataSource>> {
        let cache_key = (service_id, database.to_string());

        // Check if we already have a connection (unless force_new)
        if !force_new {
            let connections = self.connections.read().await;
            if let Some(conn) = connections.get(&cache_key) {
                debug!(
                    "Reusing existing connection for service {} database {}",
                    service_id, database
                );
                return Ok(conn.clone());
            }
        }

        debug!(
            "Creating new connection for service {} database {}",
            service_id, database
        );

        // Get service configuration
        let service = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| DataError::ConnectionFailed(format!("Service not found: {}", e)))?;

        // Create connection based on service type
        let connection: Arc<dyn DataSource> = match service.service_type {
            crate::externalsvc::ServiceType::Postgres => {
                // Deserialize parameters into typed PostgresInputConfig
                let config: PostgresInputConfig =
                    serde_json::from_value(service.parameters.clone()).map_err(|e| {
                        DataError::InvalidConfiguration(format!(
                            "Failed to parse PostgreSQL configuration: {}",
                            e
                        ))
                    })?;

                // Parse port from string
                let port = config
                    .port
                    .unwrap_or_else(|| "5432".to_string())
                    .parse::<u16>()
                    .map_err(|e| {
                        DataError::InvalidConfiguration(format!("Invalid port number: {}", e))
                    })?;

                // Connect to the specified database (not the configured one)
                let pg_source = PostgresSource::connect(
                    &config.host,
                    port,
                    &config.username,
                    &config.password.unwrap_or_else(|| "".to_string()),
                    database, // Use the requested database, not config.database
                )
                .await
                .map_err(|e| {
                    error!(
                        "Failed to connect to PostgreSQL service {} database {}: {}",
                        service_id, database, e
                    );
                    e
                })?;

                Arc::new(pg_source)
            }
            crate::externalsvc::ServiceType::S3 => {
                // Deserialize parameters into typed S3InputConfig
                let config: S3InputConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                        DataError::InvalidConfiguration(format!(
                            "Failed to parse S3 configuration: {}",
                            e
                        ))
                    })?;

                // Build endpoint URL
                let endpoint = format!(
                    "http://{}:{}",
                    config.host,
                    config.port.unwrap_or_else(|| "9000".to_string())
                );

                // Get credentials
                let access_key = config.access_key.ok_or_else(|| {
                    DataError::InvalidConfiguration("S3 access_key is required".to_string())
                })?;
                let secret_key = config.secret_key.ok_or_else(|| {
                    DataError::InvalidConfiguration("S3 secret_key is required".to_string())
                })?;

                // Create S3 source
                let s3_source =
                    S3Source::new(&config.region, Some(&endpoint), &access_key, &secret_key)
                        .await
                        .map_err(|e| {
                            error!("Failed to connect to S3 service {}: {}", service_id, e);
                            e
                        })?;

                Arc::new(s3_source)
            }
            crate::externalsvc::ServiceType::Mongodb => {
                // Deserialize parameters into typed MongodbInputConfig
                let config: MongodbInputConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                        DataError::InvalidConfiguration(format!(
                            "Failed to parse MongoDB configuration: {}",
                            e
                        ))
                    })?;

                // Build connection string with URL-encoded credentials
                let port = config.port.unwrap_or_else(|| "27017".to_string());
                let password = config.password.unwrap_or_else(|| "".to_string());

                // URL-encode username and password to handle special characters
                let encoded_username = urlencoding::encode(&config.username);

                let connection_string = if password.is_empty() {
                    format!("mongodb://{}@{}:{}", encoded_username, config.host, port)
                } else {
                    let encoded_password = urlencoding::encode(&password);
                    format!(
                        "mongodb://{}:{}@{}:{}",
                        encoded_username, encoded_password, config.host, port
                    )
                };

                // Create MongoDB source
                let mongodb_source = MongoDBSource::new(&connection_string).await.map_err(|e| {
                    error!("Failed to connect to MongoDB service {}: {}", service_id, e);
                    e
                })?;

                Arc::new(mongodb_source)
            }
            crate::externalsvc::ServiceType::Redis => {
                // Deserialize parameters into typed RedisInputConfig
                let config: RedisInputConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                    DataError::InvalidConfiguration(format!(
                        "Failed to parse Redis configuration: {}",
                        e
                    ))
                })?;

                // Build connection string with URL-encoded password
                let port = config.port.unwrap_or_else(|| "6379".to_string());
                let password = config.password.unwrap_or_else(|| "".to_string());

                let connection_string = if password.is_empty() {
                    format!("redis://{}:{}", config.host, port)
                } else {
                    // URL-encode password to handle special characters
                    let encoded_password = urlencoding::encode(&password);
                    format!("redis://:{}@{}:{}", encoded_password, config.host, port)
                };

                // Create Redis source
                let redis_source = RedisSource::new(&connection_string).await.map_err(|e| {
                    error!("Failed to connect to Redis service {}: {}", service_id, e);
                    e
                })?;

                Arc::new(redis_source)
            }
        };

        // Cache the connection (remove old one if force_new)
        let mut connections = self.connections.write().await;
        if force_new {
            connections.remove(&cache_key);
        }
        connections.insert(cache_key, connection.clone());

        Ok(connection)
    }

    /// Get or create a connection to a specific database with automatic retry on connection errors
    async fn get_connection_for_database(
        &self,
        service_id: i32,
        database: &str,
    ) -> Result<Arc<dyn DataSource>> {
        self.get_connection_for_database_internal(service_id, database, false)
            .await
    }

    /// Check if an error is a connection-related error that should trigger a retry
    fn is_connection_error(error: &DataError) -> bool {
        match error {
            DataError::ConnectionFailed(msg) => {
                // Check for common connection error patterns
                msg.contains("connection closed")
                    || msg.contains("connection lost")
                    || msg.contains("connection reset")
                    || msg.contains("broken pipe")
                    || msg.contains("EOF")
                    || msg.contains("timeout")
                    || msg.contains("timed out")
                    || msg.contains("Connection refused")
                    || msg.contains("network unreachable")
            }
            DataError::QueryFailed(msg) => {
                // Database-specific connection errors
                msg.contains("connection closed")
                    || msg.contains("connection lost")
                    || msg.contains("no connection")
                    || msg.contains("server closed the connection")
                    || msg.contains("lost connection")
            }
            _ => false,
        }
    }

    /// Execute an operation with automatic connection retry on failure
    async fn with_connection_retry<F, T, Fut>(
        &self,
        service_id: i32,
        database: &str,
        operation: F,
    ) -> Result<T>
    where
        F: Fn(Arc<dyn DataSource>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        // First attempt with cached connection
        let conn = self
            .get_connection_for_database(service_id, database)
            .await?;

        match operation(conn).await {
            Ok(result) => Ok(result),
            Err(e) if Self::is_connection_error(&e) => {
                // Connection error detected - log and retry with new connection
                tracing::warn!(
                    "Connection error for service {} database {}: {}. Retrying with new connection...",
                    service_id,
                    database,
                    e
                );

                // Remove stale connection from cache and create new one
                let cache_key = (service_id, database.to_string());
                {
                    let mut connections = self.connections.write().await;
                    connections.remove(&cache_key);
                }

                // Create new connection and retry
                let new_conn = self
                    .get_connection_for_database_internal(service_id, database, true)
                    .await?;

                operation(new_conn).await
            }
            Err(e) => Err(e),
        }
    }

    /// List containers at a specific path in the hierarchy
    /// Empty path (root) lists top-level containers (databases, keyspaces, etc.)
    /// Example: path=[] → lists databases
    /// Example: path=["mydb"] → lists schemas in database "mydb"
    pub async fn list_containers(
        &self,
        service_id: i32,
        path: &ContainerPath,
    ) -> Result<Vec<ContainerInfo>> {
        // Get service to determine type
        let service = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| DataError::ConnectionFailed(format!("Service not found: {}", e)))?;

        // Determine which database/identifier to connect to based on service type and path depth
        let database = match service.service_type {
            crate::externalsvc::ServiceType::Postgres => {
                if path.depth() == 0 {
                    // Root level - use configured database for connection
                    let config: PostgresInputConfig =
                        serde_json::from_value(service.parameters.clone()).map_err(|e| {
                            DataError::InvalidConfiguration(format!(
                                "Failed to parse PostgreSQL configuration: {}",
                                e
                            ))
                        })?;
                    config.database.clone()
                } else {
                    // Use first segment as database name
                    path.segments[0].clone()
                }
            }
            crate::externalsvc::ServiceType::S3 => {
                // For S3, use a dummy database identifier
                // The actual bucket listing happens in S3Source::list_containers
                "_s3_root".to_string()
            }
            crate::externalsvc::ServiceType::Mongodb => {
                // For MongoDB, use a dummy identifier
                // The actual database listing happens in MongoDBSource::list_containers
                "_mongodb_root".to_string()
            }
            crate::externalsvc::ServiceType::Redis => {
                // For Redis, use a dummy identifier
                // The actual database listing happens in RedisSource::list_containers
                "_redis_root".to_string()
            }
        };

        // Use retry mechanism for connection errors
        let path_clone = path.clone();
        self.with_connection_retry(service_id, &database, move |conn| {
            let path_clone = path_clone.clone();
            async move { conn.list_containers(&path_clone).await }
        })
        .await
    }

    /// Get information about a specific container
    pub async fn get_container_info(
        &self,
        service_id: i32,
        path: &ContainerPath,
    ) -> Result<ContainerInfo> {
        if path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot get info for root path - use list_containers instead".to_string(),
            ));
        }

        let database = path.segments[0].clone();
        let path_clone = path.clone();
        self.with_connection_retry(service_id, &database, move |conn| {
            let path_clone = path_clone.clone();
            async move { conn.get_container_info(&path_clone).await }
        })
        .await
    }

    /// List entities (tables, collections, objects) at a specific container path
    /// The path should point to a container that can hold entities
    /// Example: path=["mydb", "public"] → lists tables in the public schema
    pub async fn list_entities(
        &self,
        service_id: i32,
        container_path: &ContainerPath,
    ) -> Result<Vec<EntityInfo>> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot list entities at root level - specify a container path".to_string(),
            ));
        }

        let database = container_path.segments[0].clone();
        let path_clone = container_path.clone();
        self.with_connection_retry(service_id, &database, move |conn| {
            let path_clone = path_clone.clone();
            async move { conn.list_entities(&path_clone).await }
        })
        .await
    }

    /// List entities with pagination support
    /// Returns (entities, next_continuation_token)
    pub async fn list_entities_paginated(
        &self,
        service_id: i32,
        container_path: &ContainerPath,
        limit: usize,
        continuation_token: Option<String>,
    ) -> Result<(Vec<EntityInfo>, Option<String>)> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot list entities at root level - specify a container path".to_string(),
            ));
        }

        // Get service type to determine which implementation to use
        let service = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| DataError::ConnectionFailed(format!("Service not found: {}", e)))?;

        let database = container_path.segments[0].clone();
        let service_type = service.service_type;
        let path_clone = container_path.clone();

        self.with_connection_retry(service_id, &database, move |conn| {
            let path_clone = path_clone.clone();
            let continuation_token = continuation_token.clone();
            async move {
                // Check if this is S3 and use pagination
                if service_type == crate::externalsvc::ServiceType::S3 {
                    if let Some(s3_source) = conn.downcast_ref::<S3Source>() {
                        return s3_source
                            .list_entities_paginated(&path_clone, limit, continuation_token)
                            .await;
                    }
                }

                // For other backends (PostgreSQL, MongoDB, Redis), just return all entities with no pagination
                // These typically don't have thousands of entities
                let entities = conn.list_entities(&path_clone).await?;
                Ok((entities, None))
            }
        })
        .await
    }

    /// Get detailed information about an entity
    /// The container_path points to the parent container, entity_name is the entity within it
    pub async fn get_entity_info(
        &self,
        service_id: i32,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot get entity at root level - specify a container path".to_string(),
            ));
        }

        let database = container_path.segments[0].clone();
        let path_clone = container_path.clone();
        let entity_name = entity_name.to_string();
        self.with_connection_retry(service_id, &database, move |conn| {
            let path_clone = path_clone.clone();
            let entity_name = entity_name.clone();
            async move { conn.get_entity_info(&path_clone, &entity_name).await }
        })
        .await
    }

    /// Query data from an entity
    pub async fn query_data(
        &self,
        service_id: i32,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
        options: QueryOptions,
    ) -> Result<QueryResult> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot query entity at root level - specify a container path".to_string(),
            ));
        }

        let database = container_path.segments[0].clone();
        let path_clone = container_path.clone();
        let entity_name = entity_name.to_string();
        self.with_connection_retry(service_id, &database, move |conn| {
            let path_clone = path_clone.clone();
            let entity_name = entity_name.clone();
            let filters = filters.clone();
            let options = options.clone();
            async move {
                // Check if source supports querying
                use temps_query::Queryable;

                if let Some(queryable) = conn.downcast_ref::<PostgresSource>() {
                    return queryable
                        .query(&path_clone, &entity_name, filters, options)
                        .await;
                }

                if let Some(queryable) = conn.downcast_ref::<MongoDBSource>() {
                    return queryable
                        .query(&path_clone, &entity_name, filters, options)
                        .await;
                }

                Err(DataError::OperationNotSupported(
                    "Service does not support querying".to_string(),
                ))
            }
        })
        .await
    }

    /// Get filter schema for a service (if it supports QuerySchemaProvider)
    pub async fn get_filter_schema(&self, service_id: i32) -> Result<serde_json::Value> {
        // Get service config to determine database
        let service = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| DataError::ConnectionFailed(format!("Service not found: {}", e)))?;

        let config: PostgresInputConfig = serde_json::from_value(service.parameters.clone())
            .map_err(|e| {
                DataError::InvalidConfiguration(format!(
                    "Failed to parse PostgreSQL configuration: {}",
                    e
                ))
            })?;

        let database = config.database.clone();
        let conn = self
            .get_connection_for_database(service_id, &database)
            .await?;

        // Check if source supports schema provider
        if let Some(provider) = conn.downcast_ref::<PostgresSource>() {
            use temps_query::QuerySchemaProvider;
            Ok(provider.get_filter_schema())
        } else {
            Err(DataError::OperationNotSupported(
                "Service does not support query schemas".to_string(),
            ))
        }
    }

    /// Get sort schema for an entity
    pub async fn get_sort_schema(
        &self,
        service_id: i32,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<serde_json::Value> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot get sort schema at root level".to_string(),
            ));
        }

        let database = &container_path.segments[0];
        let conn = self
            .get_connection_for_database(service_id, database)
            .await?;

        // Check if source supports schema provider
        if let Some(provider) = conn.downcast_ref::<PostgresSource>() {
            use temps_query::QuerySchemaProvider;
            provider.get_sort_schema(container_path, entity_name)
        } else {
            Err(DataError::OperationNotSupported(
                "Service does not support query schemas".to_string(),
            ))
        }
    }

    /// Close and remove a cached connection
    pub async fn close_connection(&self, service_id: i32, database: &str) -> Result<()> {
        let cache_key = (service_id, database.to_string());
        let mut connections = self.connections.write().await;
        if let Some(conn) = connections.remove(&cache_key) {
            conn.close().await?;
        }
        Ok(())
    }

    /// Close all cached connections
    pub async fn close_all_connections(&self) -> Result<()> {
        let mut connections = self.connections.write().await;
        for (_, conn) in connections.drain() {
            let _ = conn.close().await;
        }
        Ok(())
    }

    /// Download an entity as a stream (for sources that support Downloadable trait)
    pub async fn download(
        &self,
        service_id: i32,
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
                "Cannot download from root level - specify a container path".to_string(),
            ));
        }

        let database = container_path.segments[0].clone();
        let path_clone = container_path.clone();
        let entity_name = entity_name.to_string();
        self.with_connection_retry(service_id, &database, move |conn| {
            let path_clone = path_clone.clone();
            let entity_name = entity_name.clone();
            async move {
                // Check if source implements Downloadable trait
                if let Some(downloadable) = conn.downcast_ref::<S3Source>() {
                    use temps_query::Downloadable;
                    downloadable.download(&path_clone, &entity_name).await
                } else {
                    Err(DataError::OperationNotSupported(
                        "This data source does not support downloads".to_string(),
                    ))
                }
            }
        })
        .await
    }
}
