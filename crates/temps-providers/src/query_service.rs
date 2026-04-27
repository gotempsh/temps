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
use tracing::{debug, error, info, warn};

use crate::externalsvc::mongodb::MongodbInputConfig;
use crate::externalsvc::postgres::PostgresInputConfig;
use crate::externalsvc::redis::RedisInputConfig;
use crate::externalsvc::rustfs::RustfsConfig;
use crate::externalsvc::s3::S3InputConfig;
use crate::ExternalServiceManager;

/// Cache of active connections by (service_id, database_name)
type ConnectionCache = HashMap<(i32, String), Arc<dyn DataSource>>;

/// Service for managing query connections to external services
pub struct QueryService {
    external_service_manager: Arc<ExternalServiceManager>,
    connections: Arc<RwLock<ConnectionCache>>,
}

impl QueryService {
    pub fn new(external_service_manager: Arc<ExternalServiceManager>) -> Self {
        Self {
            external_service_manager,
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// The read-only user name used for explorer/query connections.
    const EXPLORER_USER: &'static str = "temps_explorer";

    /// Ensure a read-only `temps_explorer` user exists on the PostgreSQL instance
    /// and has SELECT-only access to the target database schemas.
    /// Connects as the admin user, creates the role if missing, and grants permissions.
    /// Returns the password for the explorer user.
    async fn ensure_readonly_user(
        host: &str,
        port: u16,
        admin_user: &str,
        admin_password: &str,
        database: &str,
    ) -> std::result::Result<String, DataError> {
        // Deterministic password derived from admin password so it's stable across calls
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        format!("temps_explorer_{}_{}", host, admin_password).hash(&mut hasher);
        let explorer_password = format!("te_{:x}", hasher.finish());

        // Connect as admin to the target database
        let pg_source = PostgresSource::connect(host, port, admin_user, admin_password, database)
            .await
            .map_err(|e| {
                DataError::ConnectionFailed(format!(
                    "Failed to connect as admin to provision read-only user: {}",
                    e
                ))
            })?;

        // Create the role if it doesn't exist (role is cluster-wide)
        let create_role_sql = format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{}') THEN \
                 CREATE ROLE {} LOGIN PASSWORD '{}'; \
               ELSE \
                 ALTER ROLE {} PASSWORD '{}'; \
               END IF; \
             END $$",
            Self::EXPLORER_USER,
            Self::EXPLORER_USER,
            explorer_password.replace('\'', "''"),
            Self::EXPLORER_USER,
            explorer_password.replace('\'', "''"),
        );

        pg_source.execute_raw(&create_role_sql).await.map_err(|e| {
            DataError::QueryFailed(format!("Failed to create read-only role: {}", e))
        })?;

        // Grant CONNECT on the database
        let grant_connect = format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO {}",
            database.replace('"', "\"\""),
            Self::EXPLORER_USER,
        );
        pg_source
            .execute_raw(&grant_connect)
            .await
            .map_err(|e| DataError::QueryFailed(format!("Failed to grant CONNECT: {}", e)))?;

        // Grant USAGE on all schemas and SELECT on all tables
        let grant_sql = format!(
            "DO $$ DECLARE s record; BEGIN \
               FOR s IN SELECT schema_name FROM information_schema.schemata \
                 WHERE schema_name NOT IN ('information_schema') LOOP \
                 EXECUTE format('GRANT USAGE ON SCHEMA %I TO {}', s.schema_name); \
                 EXECUTE format('GRANT SELECT ON ALL TABLES IN SCHEMA %I TO {}', s.schema_name); \
                 EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT SELECT ON TABLES TO {}', s.schema_name); \
               END LOOP; \
             END $$",
            Self::EXPLORER_USER,
            Self::EXPLORER_USER,
            Self::EXPLORER_USER,
        );
        pg_source.execute_raw(&grant_sql).await.map_err(|e| {
            DataError::QueryFailed(format!("Failed to grant SELECT privileges: {}", e))
        })?;

        info!(
            "Read-only explorer user '{}' provisioned for database '{}'",
            Self::EXPLORER_USER,
            database
        );

        Ok(explorer_password)
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

                // For cluster services, route directly via the primary's
                // underlay address + host-mapped port — NOT via the FQDN
                // VIP. Reasoning:
                //   - The control plane is not on the multi-host overlay
                //     (`temps0`), so it can't reach members by their
                //     overlay IP (`172.20.x.x`).
                //   - The control plane doesn't run a per-node Hickory
                //     resolver (the resolver is per-worker), so
                //     `<svc>.temps.local` doesn't resolve here either.
                //   - The underlay address (worker's `private_address`)
                //     plus the container's host-mapped port IS reachable
                //     from the control plane and goes through the same
                //     pg_hba `md5 0.0.0.0/0` rule we open for the app
                //     user during cluster init.
                //
                // Apps deployed *into* the cluster keep using
                // `<svc>.temps.local` because they run on the overlay and
                // resolve via the local Hickory listener — but that's the
                // app's path, not ours.
                let (host, port) = match self
                    .external_service_manager
                    .get_cluster_primary_address(service_id)
                    .await
                {
                    Ok(Some((primary_host, primary_port))) => {
                        debug!(
                            "Using cluster primary {}:{} for service {} explorer",
                            primary_host, primary_port, service_id
                        );
                        (primary_host, primary_port)
                    }
                    Ok(None) => {
                        // Standalone service — use config host/port as before
                        let port = config
                            .port
                            .unwrap_or_else(|| "5432".to_string())
                            .parse::<u16>()
                            .map_err(|e| {
                                DataError::InvalidConfiguration(format!(
                                    "Invalid port number: {}",
                                    e
                                ))
                            })?;
                        (config.host.clone(), port)
                    }
                    Err(e) => {
                        return Err(DataError::ConnectionFailed(format!(
                            "Failed to resolve cluster primary for service {}: {}",
                            service_id, e
                        )));
                    }
                };

                let admin_password = config.password.unwrap_or_default();

                // Ensure a read-only explorer user exists, then connect with it.
                // Falls back to admin user if provisioning fails (e.g. managed DB
                // without CREATE ROLE permission).
                let (connect_user, connect_password) = match Self::ensure_readonly_user(
                    &host,
                    port,
                    &config.username,
                    &admin_password,
                    database,
                )
                .await
                {
                    Ok(explorer_password) => (Self::EXPLORER_USER.to_string(), explorer_password),
                    Err(e) => {
                        warn!(
                            "Could not provision read-only user for service {}: {}. Falling back to admin user.",
                            service_id, e
                        );
                        (config.username.clone(), admin_password)
                    }
                };

                // Connect to the specified database with the explorer (or fallback admin) user
                let pg_source = PostgresSource::connect(
                    &host,
                    port,
                    &connect_user,
                    &connect_password,
                    database,
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
            // S3 now uses RustFS by default - same connection pattern as Blob/Rustfs
            crate::externalsvc::ServiceType::S3 => {
                let config: RustfsConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                        DataError::InvalidConfiguration(format!(
                            "Failed to parse S3 (RustFS) configuration: {}",
                            e
                        ))
                    })?;

                let endpoint = format!("http://{}:{}", config.host, config.port);

                let s3_source = S3Source::new(
                    &config.region,
                    Some(&endpoint),
                    &config.access_key,
                    &config.secret_key,
                )
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
                let password = config.password.unwrap_or_default();

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
                let password = config.password.unwrap_or_default();

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
            // Temps KV uses Redis backend - treat the same as Redis for query purposes
            crate::externalsvc::ServiceType::Kv => {
                let config: RedisInputConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                    DataError::InvalidConfiguration(format!(
                        "Failed to parse KV (Redis) configuration: {}",
                        e
                    ))
                })?;

                let port = config.port.unwrap_or_else(|| "6379".to_string());
                let password = config.password.unwrap_or_default();

                let connection_string = if password.is_empty() {
                    format!("redis://{}:{}", config.host, port)
                } else {
                    let encoded_password = urlencoding::encode(&password);
                    format!("redis://:{}@{}:{}", encoded_password, config.host, port)
                };

                let redis_source = RedisSource::new(&connection_string).await.map_err(|e| {
                    error!("Failed to connect to KV service {}: {}", service_id, e);
                    e
                })?;

                Arc::new(redis_source)
            }
            // Temps Blob uses RustfsService (RustFS) for S3-compatible storage
            crate::externalsvc::ServiceType::Blob => {
                // Blob services are backed by RustFS, so use RustfsConfig
                let config: RustfsConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                        DataError::InvalidConfiguration(format!(
                            "Failed to parse Blob (RustFS) configuration: {}",
                            e
                        ))
                    })?;

                let endpoint = format!("http://{}:{}", config.host, config.port);

                let s3_source = S3Source::new(
                    &config.region,
                    Some(&endpoint),
                    &config.access_key,
                    &config.secret_key,
                )
                .await
                .map_err(|e| {
                    error!("Failed to connect to Blob service {}: {}", service_id, e);
                    e
                })?;

                Arc::new(s3_source)
            }
            // RustFS standalone: S3-compatible storage with same connection pattern as Blob
            crate::externalsvc::ServiceType::Rustfs => {
                let config: RustfsConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                        DataError::InvalidConfiguration(format!(
                            "Failed to parse RustFS (S3) configuration: {}",
                            e
                        ))
                    })?;

                let endpoint = format!("http://{}:{}", config.host, config.port);

                let s3_source = S3Source::new(
                    &config.region,
                    Some(&endpoint),
                    &config.access_key,
                    &config.secret_key,
                )
                .await
                .map_err(|e| {
                    error!("Failed to connect to RustFS service {}: {}", service_id, e);
                    e
                })?;

                Arc::new(s3_source)
            }
            // MinIO (deprecated) - uses legacy S3InputConfig
            #[allow(deprecated)]
            crate::externalsvc::ServiceType::Minio => {
                let config: S3InputConfig = serde_json::from_value(service.parameters.clone())
                    .map_err(|e| {
                        DataError::InvalidConfiguration(format!(
                            "Failed to parse MinIO configuration: {}",
                            e
                        ))
                    })?;

                let endpoint = format!(
                    "http://{}:{}",
                    config.host,
                    config.port.unwrap_or_else(|| "9000".to_string())
                );

                let access_key = config.access_key.ok_or_else(|| {
                    DataError::InvalidConfiguration("MinIO access_key is required".to_string())
                })?;
                let secret_key = config.secret_key.ok_or_else(|| {
                    DataError::InvalidConfiguration("MinIO secret_key is required".to_string())
                })?;

                let s3_source =
                    S3Source::new(&config.region, Some(&endpoint), &access_key, &secret_key)
                        .await
                        .map_err(|e| {
                            error!("Failed to connect to MinIO service {}: {}", service_id, e);
                            e
                        })?;

                Arc::new(s3_source)
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
            // Temps KV uses Redis backend
            crate::externalsvc::ServiceType::Kv => "_kv_root".to_string(),
            // Temps Blob uses S3-compatible storage
            crate::externalsvc::ServiceType::Blob => "_blob_root".to_string(),
            // RustFS standalone S3-compatible storage
            crate::externalsvc::ServiceType::Rustfs => "_rustfs_root".to_string(),
            // MinIO (deprecated) S3-compatible storage
            #[allow(deprecated)]
            crate::externalsvc::ServiceType::Minio => "_minio_root".to_string(),
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
                // Check if this is S3-compatible (S3, Blob, or RustFS) and use pagination
                #[allow(deprecated)]
                if matches!(
                    service_type,
                    crate::externalsvc::ServiceType::S3
                        | crate::externalsvc::ServiceType::Blob
                        | crate::externalsvc::ServiceType::Rustfs
                        | crate::externalsvc::ServiceType::Minio
                ) {
                    if let Some(s3_source) = conn.downcast_ref::<S3Source>() {
                        return s3_source
                            .list_entities_paginated(&path_clone, limit, continuation_token)
                            .await;
                    }
                }

                // Check if this is Redis and use pagination (Redis can have millions of keys)
                if service_type == crate::externalsvc::ServiceType::Redis
                    || service_type == crate::externalsvc::ServiceType::Kv
                {
                    if let Some(redis_source) = conn.downcast_ref::<RedisSource>() {
                        return redis_source
                            .list_entities_paginated(&path_clone, limit, continuation_token)
                            .await;
                    }
                }

                // For other backends (PostgreSQL, MongoDB), just return all entities with no pagination
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

                if let Some(queryable) = conn.downcast_ref::<RedisSource>() {
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
