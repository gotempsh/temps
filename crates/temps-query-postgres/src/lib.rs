//! PostgreSQL driver for temps-query
//!
//! Implements DataSource, Introspect, Queryable, and SqlFeature traits for PostgreSQL.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use temps_query::{
    Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType, DataError,
    DataRow, DataSource, DatasetSchema, EntityInfo, FieldDef, FieldType, Introspect, QueryOptions,
    QueryResult, QueryStats, Queryable, Result, SqlFeature,
};
use tokio::sync::RwLock;
use tokio_postgres::{Client, NoTls, Row};
use tracing::{debug, error};

/// PostgreSQL data source implementation
pub struct PostgresSource {
    client: Arc<RwLock<Client>>,
    database_name: String,
}

impl PostgresSource {
    /// Create a new PostgreSQL data source
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
    ) -> Result<Self> {
        let config = format!(
            "host={} port={} user={} password={} dbname={}",
            host, port, username, password, database
        );

        debug!(
            "Connecting to PostgreSQL: {}@{}:{}/{}",
            username, host, port, database
        );

        let (client, connection) = tokio_postgres::connect(&config, NoTls).await.map_err(|e| {
            DataError::ConnectionFailed(format!("PostgreSQL connection failed: {}", e))
        })?;

        // Spawn connection handler
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("PostgreSQL connection error: {}", e);
            }
        });

        debug!(
            "Successfully connected to PostgreSQL database: {}",
            database
        );

        Ok(Self {
            client: Arc::new(RwLock::new(client)),
            database_name: database.to_string(),
        })
    }

    /// Map PostgreSQL type to FieldType
    fn map_pg_type(pg_type: &str) -> FieldType {
        match pg_type {
            "boolean" | "bool" => FieldType::Boolean,
            "smallint" | "int2" => FieldType::Int32,
            "integer" | "int" | "int4" => FieldType::Int32,
            "bigint" | "int8" => FieldType::Int64,
            "real" | "float4" => FieldType::Float32,
            "double precision" | "float8" => FieldType::Float64,
            "numeric" | "decimal" => FieldType::Float64,
            "character varying" | "varchar" | "character" | "char" | "text" => FieldType::String,
            "bytea" => FieldType::Bytes,
            "date" => FieldType::Date,
            "timestamp"
            | "timestamp without time zone"
            | "timestamp with time zone"
            | "timestamptz" => FieldType::Timestamp,
            "json" | "jsonb" => FieldType::Json,
            "uuid" => FieldType::Uuid,
            _ => FieldType::String, // Default fallback
        }
    }

    /// Convert PostgreSQL row to DataRow
    fn row_to_datarow(row: &Row) -> Result<DataRow> {
        let mut data_row = HashMap::new();

        for (idx, column) in row.columns().iter().enumerate() {
            let name = column.name().to_string();
            let value = Self::extract_value(row, idx)?;
            data_row.insert(name, value);
        }

        Ok(data_row)
    }

    /// Extract value from PostgreSQL row
    fn extract_value(row: &Row, idx: usize) -> Result<serde_json::Value> {
        let column = &row.columns()[idx];
        let type_name = column.type_().name();

        let value = match type_name {
            "bool" => row
                .try_get::<_, Option<bool>>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::Bool(v))
                .unwrap_or(serde_json::Value::Null),

            "int2" | "int4" => row
                .try_get::<_, Option<i32>>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::Number(v.into()))
                .unwrap_or(serde_json::Value::Null),

            "int8" => row
                .try_get::<_, Option<i64>>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::Number(v.into()))
                .unwrap_or(serde_json::Value::Null),

            "float4" => row
                .try_get::<_, Option<f32>>(idx)
                .ok()
                .flatten()
                .and_then(|v| serde_json::Number::from_f64(v as f64))
                .map(|n| serde_json::Value::Number(n))
                .unwrap_or(serde_json::Value::Null),

            "float8" => row
                .try_get::<_, Option<f64>>(idx)
                .ok()
                .flatten()
                .and_then(|v| serde_json::Number::from_f64(v))
                .map(|n| serde_json::Value::Number(n))
                .unwrap_or(serde_json::Value::Null),

            "varchar" | "text" | "char" | "bpchar" => row
                .try_get::<_, Option<String>>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::String(v))
                .unwrap_or(serde_json::Value::Null),

            "timestamp" | "timestamptz" => row
                .try_get::<_, Option<chrono::NaiveDateTime>>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::String(v.to_string()))
                .unwrap_or(serde_json::Value::Null),

            "json" | "jsonb" => row
                .try_get::<_, Option<serde_json::Value>>(idx)
                .ok()
                .flatten()
                .unwrap_or(serde_json::Value::Null),

            "uuid" => row
                .try_get::<_, Option<uuid::Uuid>>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::String(v.to_string()))
                .unwrap_or(serde_json::Value::Null),

            _ => {
                // Try to get as string for unknown types
                row.try_get::<_, Option<String>>(idx)
                    .ok()
                    .flatten()
                    .map(|v| serde_json::Value::String(v))
                    .unwrap_or(serde_json::Value::Null)
            }
        };

        Ok(value)
    }
}

#[async_trait]
impl DataSource for PostgresSource {
    fn source_type(&self) -> &'static str {
        "postgres"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Sql, Capability::TextSearch]
    }

    async fn list_containers(&self, path: &ContainerPath) -> Result<Vec<ContainerInfo>> {
        let client = self.client.read().await;

        match path.depth() {
            // Depth 0: List databases
            0 => {
                debug!("Listing PostgreSQL databases");

                let query = r#"
                    SELECT
                        datname,
                        pg_database_size(datname) as size_bytes,
                        pg_get_userbyid(datdba) as owner,
                        pg_encoding_to_char(encoding) as encoding
                    FROM pg_database
                    WHERE datistemplate = false
                    ORDER BY datname
                "#;

                let rows = client.query(query, &[]).await.map_err(|e| {
                    DataError::QueryFailed(format!("Failed to list databases: {}", e))
                })?;

                let databases: Vec<ContainerInfo> = rows
                    .iter()
                    .map(|row| {
                        let name: String = row.get(0);
                        let size_bytes: Option<i64> = row.try_get(1).ok();
                        let owner: Option<String> = row.try_get(2).ok();
                        let encoding: Option<String> = row.try_get(3).ok();

                        let mut metadata = HashMap::new();
                        if let Some(size) = size_bytes {
                            metadata.insert("size_bytes".to_string(), serde_json::json!(size));
                        }
                        if let Some(own) = owner {
                            metadata.insert("owner".to_string(), serde_json::json!(own));
                        }
                        if let Some(enc) = encoding {
                            metadata.insert("encoding".to_string(), serde_json::json!(enc));
                        }

                        ContainerInfo {
                            name,
                            container_type: ContainerType::Database,
                            capabilities: ContainerCapabilities {
                                can_contain_containers: true,
                                can_contain_entities: false,
                                child_container_type: Some(ContainerType::Schema),
                                entity_type_label: None,
                            },
                            metadata,
                        }
                    })
                    .collect();

                debug!("Found {} databases", databases.len());
                Ok(databases)
            }

            // Depth 1: List schemas in a database
            1 => {
                let database_name = &path.segments[0];

                // Check if we're connected to the right database
                if database_name != &self.database_name {
                    return Err(DataError::OperationNotSupported(format!(
                        "Cannot list schemas from database '{}' while connected to '{}'. Create a connection to that database.",
                        database_name, self.database_name
                    )));
                }

                debug!("Listing PostgreSQL schemas in database: {}", database_name);

                let query = r#"
                    SELECT
                        schema_name,
                        COUNT(table_name) as table_count
                    FROM information_schema.schemata
                    LEFT JOIN information_schema.tables
                        ON information_schema.tables.table_schema = information_schema.schemata.schema_name
                        AND table_type = 'BASE TABLE'
                    WHERE schema_name NOT IN ('information_schema')
                    GROUP BY schema_name
                    ORDER BY schema_name
                "#;

                let rows = client.query(query, &[]).await.map_err(|e| {
                    DataError::QueryFailed(format!("Failed to list schemas: {}", e))
                })?;

                let schemas: Vec<ContainerInfo> = rows
                    .iter()
                    .map(|row| {
                        let name: String = row.get(0);
                        let entity_count: i64 = row.try_get(1).unwrap_or(0);

                        let mut metadata = HashMap::new();
                        metadata
                            .insert("entity_count".to_string(), serde_json::json!(entity_count));

                        ContainerInfo {
                            name,
                            container_type: ContainerType::Schema,
                            capabilities: ContainerCapabilities {
                                can_contain_containers: false,
                                can_contain_entities: true,
                                child_container_type: None,
                                entity_type_label: Some("table".to_string()),
                            },
                            metadata,
                        }
                    })
                    .collect();

                debug!(
                    "Found {} schemas in database '{}'",
                    schemas.len(),
                    database_name
                );
                Ok(schemas)
            }

            // Depth >= 2: Not supported
            _ => Err(DataError::InvalidQuery(format!(
                "PostgreSQL hierarchy only supports 2 levels (database/schema). Path depth: {}",
                path.depth()
            ))),
        }
    }

    async fn get_container_info(&self, path: &ContainerPath) -> Result<ContainerInfo> {
        let client = self.client.read().await;

        match path.depth() {
            // Depth 1: Get database info
            1 => {
                let database_name = &path.segments[0];

                let query = r#"
                    SELECT
                        datname,
                        pg_database_size(datname) as size_bytes,
                        pg_get_userbyid(datdba) as owner,
                        pg_encoding_to_char(encoding) as encoding
                    FROM pg_database
                    WHERE datname = $1
                "#;

                let row = client
                    .query_one(query, &[database_name])
                    .await
                    .map_err(|e| {
                        DataError::NotFound(format!(
                            "Database '{}' not found: {}",
                            database_name, e
                        ))
                    })?;

                let name: String = row.get(0);
                let size_bytes: Option<i64> = row.try_get(1).ok();
                let owner: Option<String> = row.try_get(2).ok();
                let encoding: Option<String> = row.try_get(3).ok();

                let mut metadata = HashMap::new();
                if let Some(size) = size_bytes {
                    metadata.insert("size_bytes".to_string(), serde_json::json!(size));
                }
                if let Some(own) = owner {
                    metadata.insert("owner".to_string(), serde_json::json!(own));
                }
                if let Some(enc) = encoding {
                    metadata.insert("encoding".to_string(), serde_json::json!(enc));
                }

                Ok(ContainerInfo {
                    name,
                    container_type: ContainerType::Database,
                    capabilities: ContainerCapabilities {
                        can_contain_containers: true,
                        can_contain_entities: false,
                        child_container_type: Some(ContainerType::Schema),
                        entity_type_label: None,
                    },
                    metadata,
                })
            }

            // Depth 2: Get schema info
            2 => {
                let database_name = &path.segments[0];
                let schema_name = &path.segments[1];

                if database_name != &self.database_name {
                    return Err(DataError::OperationNotSupported(format!(
                        "Cannot get schema info from database '{}' while connected to '{}'",
                        database_name, self.database_name
                    )));
                }

                let query = r#"
                    SELECT COUNT(*)
                    FROM information_schema.tables
                    WHERE table_schema = $1 AND table_type = 'BASE TABLE'
                "#;

                let row = client.query_one(query, &[schema_name]).await.map_err(|e| {
                    DataError::NotFound(format!("Schema '{}' not found: {}", schema_name, e))
                })?;

                let entity_count: i64 = row.get(0);

                let mut metadata = HashMap::new();
                metadata.insert("entity_count".to_string(), serde_json::json!(entity_count));

                Ok(ContainerInfo {
                    name: schema_name.clone(),
                    container_type: ContainerType::Schema,
                    capabilities: ContainerCapabilities {
                        can_contain_containers: false,
                        can_contain_entities: true,
                        child_container_type: None,
                        entity_type_label: Some("table".to_string()),
                    },
                    metadata,
                })
            }

            _ => Err(DataError::InvalidQuery(format!(
                "Invalid path depth for get_container_info: {}",
                path.depth()
            ))),
        }
    }

    async fn list_entities(&self, container_path: &ContainerPath) -> Result<Vec<EntityInfo>> {
        // Must be at depth 2 (database/schema)
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(format!(
                "list_entities requires path depth 2 (database/schema), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        let schema_name = &container_path.segments[1];

        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot list entities from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let client = self.client.read().await;

        debug!("Listing tables in schema: {}", schema_name);

        let query = r#"
            SELECT
                table_schema,
                table_name,
                table_type
            FROM information_schema.tables
            WHERE table_schema = $1 AND table_type = 'BASE TABLE'
            ORDER BY table_name
        "#;

        let rows = client.query(query, &[schema_name]).await.map_err(|e| {
            DataError::QueryFailed(format!(
                "Failed to list tables in schema '{}': {}",
                schema_name, e
            ))
        })?;

        let entities: Vec<EntityInfo> = rows
            .iter()
            .map(|row| {
                let schema: String = row.get(0);
                let table_name: String = row.get(1);
                let table_type: String = row.get(2);

                EntityInfo {
                    namespace: schema,
                    name: table_name,
                    entity_type: table_type,
                    row_count: None,
                    size_bytes: None,
                    schema: None,
                }
            })
            .collect();

        debug!(
            "Found {} tables in schema '{}'",
            entities.len(),
            schema_name
        );

        Ok(entities)
    }

    async fn get_entity_info(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(format!(
                "get_entity_info requires path depth 2 (database/schema), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        let schema_name = &container_path.segments[1];

        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot get entity info from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let client = self.client.read().await;

        let query = r#"
            SELECT table_type
            FROM information_schema.tables
            WHERE table_schema = $1 AND table_name = $2
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name])
            .await
            .map_err(|e| {
                DataError::NotFound(format!(
                    "Table '{}.{}' not found: {}",
                    schema_name, entity_name, e
                ))
            })?;

        let table_type: String = row.get(0);

        // Get row count
        let count_query = format!(
            "SELECT COUNT(*) FROM \"{}\".\"{}\"",
            schema_name, entity_name
        );

        let row_count = client
            .query_one(&count_query, &[])
            .await
            .ok()
            .and_then(|row| row.try_get::<_, i64>(0).ok())
            .map(|c| c as usize);

        Ok(EntityInfo {
            namespace: schema_name.clone(),
            name: entity_name.to_string(),
            entity_type: table_type,
            row_count,
            size_bytes: None,
            schema: Some(self.get_schema(container_path, entity_name).await?),
        })
    }

    async fn get_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<DatasetSchema> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(format!(
                "get_schema requires path depth 2 (database/schema), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        let schema_name = &container_path.segments[1];

        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot get schema from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let client = self.client.read().await;

        debug!("Getting schema for table: {}.{}", schema_name, entity_name);

        let query = r#"
            SELECT
                column_name,
                data_type,
                is_nullable,
                column_default
            FROM information_schema.columns
            WHERE table_schema = $1 AND table_name = $2
            ORDER BY ordinal_position
        "#;

        let rows = client
            .query(query, &[schema_name, &entity_name])
            .await
            .map_err(|e| {
                DataError::SchemaError(format!(
                    "Failed to get schema for table '{}.{}': {}",
                    schema_name, entity_name, e
                ))
            })?;

        let fields: Vec<FieldDef> = rows
            .iter()
            .map(|row| {
                let name: String = row.get(0);
                let data_type: String = row.get(1);
                let is_nullable: String = row.get(2);
                let _column_default: Option<String> = row.get(3);

                FieldDef {
                    name,
                    field_type: Self::map_pg_type(&data_type),
                    nullable: is_nullable == "YES",
                    description: None,
                }
            })
            .collect();

        debug!(
            "Found {} columns for table '{}.{}'",
            fields.len(),
            schema_name,
            entity_name
        );

        Ok(DatasetSchema {
            fields,
            partitions: None,
            primary_key: None,
        })
    }

    async fn close(&self) -> Result<()> {
        debug!("Closing PostgreSQL connection");
        // Connection cleanup handled by Drop
        Ok(())
    }
}

#[async_trait]
impl Introspect for PostgresSource {
    async fn inspect_fields(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<Vec<FieldDef>> {
        let schema = self.get_schema(container_path, entity_name).await?;
        Ok(schema.fields)
    }

    async fn field_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<bool> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery("Invalid path depth".to_string()));
        }

        let schema_name = &container_path.segments[1];
        let client = self.client.read().await;

        let query = r#"
            SELECT COUNT(*)
            FROM information_schema.columns
            WHERE table_schema = $1 AND table_name = $2 AND column_name = $3
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name, &field])
            .await
            .map_err(|e| {
                DataError::QueryFailed(format!("Failed to check field existence: {}", e))
            })?;

        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    async fn get_field_type(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<FieldType> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery("Invalid path depth".to_string()));
        }

        let schema_name = &container_path.segments[1];
        let client = self.client.read().await;

        let query = r#"
            SELECT data_type
            FROM information_schema.columns
            WHERE table_schema = $1 AND table_name = $2 AND column_name = $3
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name, &field])
            .await
            .map_err(|e| {
                DataError::NotFound(format!(
                    "Field '{}' not found in table '{}.{}': {}",
                    field, schema_name, entity_name, e
                ))
            })?;

        let data_type: String = row.get(0);
        Ok(Self::map_pg_type(&data_type))
    }
}

#[async_trait]
impl Queryable for PostgresSource {
    async fn query(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
        options: QueryOptions,
    ) -> Result<QueryResult> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(
                "Invalid path depth for query".to_string(),
            ));
        }

        let schema_name = &container_path.segments[1];
        let client = self.client.read().await;

        let start = std::time::Instant::now();

        // Build SQL query
        let mut sql = format!("SELECT * FROM \"{}\".\"{}\"", schema_name, entity_name);

        // Add WHERE clause if filters provided
        if let Some(filter_json) = filters {
            if let Some(where_clause) = filter_json.get("where").and_then(|v| v.as_str()) {
                sql.push_str(" WHERE ");
                sql.push_str(where_clause);
            }
        }

        // Add ORDER BY
        if let Some(sort_by) = &options.sort_by {
            let sort_order = options.sort_order.as_deref().unwrap_or("asc");
            sql.push_str(&format!(" ORDER BY {} {}", sort_by, sort_order));
        }

        // Add LIMIT and OFFSET
        let limit = options.limit.unwrap_or(100);
        let offset = options.offset.unwrap_or(0);
        sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

        debug!("Executing query: {}", sql);

        let rows = client.query(&sql, &[]).await.map_err(|e| {
            error!("PostgreSQL query failed: {}", e);
            error!("Failed SQL: {}", sql);

            // Extract detailed error message from PostgreSQL error
            let error_msg = if let Some(db_error) = e.as_db_error() {
                // Build detailed error message from PostgreSQL error fields
                let mut msg = format!("{}", db_error.message());

                if let Some(detail) = db_error.detail() {
                    msg.push_str(&format!("\nDetail: {}", detail));
                }

                if let Some(hint) = db_error.hint() {
                    msg.push_str(&format!("\nHint: {}", hint));
                }

                if let Some(position) = db_error.position() {
                    msg.push_str(&format!("\nPosition: {:?}", position));
                }

                if let Some(column) = db_error.column() {
                    msg.push_str(&format!("\nColumn: {}", column));
                }

                msg
            } else {
                // Non-database error (connection error, etc.)
                format!("{}", e)
            };

            DataError::QueryFailed(format!("{}\n\nQuery: {}", error_msg, sql))
        })?;

        // Convert rows to DataRow
        let data_rows: Result<Vec<DataRow>> = rows.iter().map(Self::row_to_datarow).collect();
        let data_rows = data_rows?;

        // Get schema from first row or from table schema
        let schema = self.get_schema(container_path, entity_name).await?;

        let execution_ms = start.elapsed().as_millis() as u64;
        let row_count = data_rows.len();

        debug!("Query returned {} rows in {}ms", row_count, execution_ms);

        Ok(QueryResult {
            schema,
            rows: data_rows,
            stats: QueryStats {
                row_count,
                total_rows: None,
                execution_ms,
                has_more: row_count >= limit,
                next_cursor: None,
            },
        })
    }

    async fn count(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
    ) -> Result<u64> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(
                "Invalid path depth for count".to_string(),
            ));
        }

        let schema_name = &container_path.segments[1];
        let client = self.client.read().await;

        let mut sql = format!(
            "SELECT COUNT(*) FROM \"{}\".\"{}\"",
            schema_name, entity_name
        );

        // Add WHERE clause if filters provided
        if let Some(filter_json) = filters {
            if let Some(where_clause) = filter_json.get("where").and_then(|v| v.as_str()) {
                sql.push_str(" WHERE ");
                sql.push_str(where_clause);
            }
        }

        let row = client
            .query_one(&sql, &[])
            .await
            .map_err(|e| DataError::QueryFailed(format!("Count query failed: {}", e)))?;

        let count: i64 = row.get(0);
        Ok(count as u64)
    }

    async fn entity_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<bool> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery("Invalid path depth".to_string()));
        }

        let schema_name = &container_path.segments[1];
        let client = self.client.read().await;

        let query = r#"
            SELECT COUNT(*)
            FROM information_schema.tables
            WHERE table_schema = $1 AND table_name = $2
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name])
            .await
            .map_err(|e| DataError::QueryFailed(format!("Entity existence check failed: {}", e)))?;

        let count: i64 = row.get(0);
        Ok(count > 0)
    }
}

#[async_trait]
impl SqlFeature for PostgresSource {
    async fn execute_sql(
        &self,
        sql: &str,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<QueryResult> {
        let client = self.client.read().await;

        let start = std::time::Instant::now();

        debug!("Executing raw SQL: {}", sql);

        let rows = client.query(sql, &[]).await.map_err(|e| {
            error!("PostgreSQL SQL execution failed: {}", e);
            error!("Failed SQL: {}", sql);

            // Extract detailed error message from PostgreSQL error
            let error_msg = if let Some(db_error) = e.as_db_error() {
                // Build detailed error message from PostgreSQL error fields
                let mut msg = format!("{}", db_error.message());

                if let Some(detail) = db_error.detail() {
                    msg.push_str(&format!("\nDetail: {}", detail));
                }

                if let Some(hint) = db_error.hint() {
                    msg.push_str(&format!("\nHint: {}", hint));
                }

                if let Some(position) = db_error.position() {
                    msg.push_str(&format!("\nPosition: {:?}", position));
                }

                if let Some(column) = db_error.column() {
                    msg.push_str(&format!("\nColumn: {}", column));
                }

                msg
            } else {
                // Non-database error (connection error, etc.)
                format!("{}", e)
            };

            DataError::QueryFailed(format!("{}\n\nQuery: {}", error_msg, sql))
        })?;

        // Convert rows to DataRow
        let data_rows: Result<Vec<DataRow>> = rows.iter().map(Self::row_to_datarow).collect();
        let data_rows = data_rows?;

        // Build schema from result columns
        let fields = if let Some(first_row) = rows.first() {
            first_row
                .columns()
                .iter()
                .map(|col| FieldDef {
                    name: col.name().to_string(),
                    field_type: Self::map_pg_type(col.type_().name()),
                    nullable: true,
                    description: None,
                })
                .collect()
        } else {
            Vec::new()
        };

        let schema = DatasetSchema {
            fields,
            partitions: None,
            primary_key: None,
        };

        let execution_ms = start.elapsed().as_millis() as u64;
        let row_count = data_rows.len();

        debug!("SQL returned {} rows in {}ms", row_count, execution_ms);

        Ok(QueryResult {
            schema,
            rows: data_rows,
            stats: QueryStats {
                row_count,
                total_rows: None,
                execution_ms,
                has_more: false,
                next_cursor: None,
            },
        })
    }

    async fn explain(&self, sql: &str) -> Result<String> {
        let client = self.client.read().await;

        let explain_sql = format!("EXPLAIN {}", sql);

        let rows = client
            .query(&explain_sql, &[])
            .await
            .map_err(|e| DataError::QueryFailed(format!("EXPLAIN failed: {}", e)))?;

        let plan = rows
            .iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(plan)
    }

    fn validate_sql(&self, sql: &str) -> Result<()> {
        // Basic SQL validation
        let sql_lower = sql.trim().to_lowercase();

        // Prevent dangerous operations
        if sql_lower.contains("drop table")
            || sql_lower.contains("drop database")
            || sql_lower.contains("truncate")
        {
            return Err(DataError::InvalidQuery(
                "Dangerous SQL operations are not allowed".to_string(),
            ));
        }

        Ok(())
    }
}

impl temps_query::QuerySchemaProvider for PostgresSource {
    fn get_filter_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "title": "PostgreSQL Query Filters",
            "description": "Filter data using SQL WHERE clause syntax",
            "properties": {
                "where": {
                    "type": "string",
                    "title": "WHERE Clause",
                    "description": "SQL WHERE clause (without 'WHERE' keyword). Example: status = 'active' AND created_at > '2025-01-01'",
                    "examples": [
                        "status = 'active'",
                        "created_at > '2025-01-01'",
                        "age >= 18 AND country = 'US'",
                        "name LIKE '%test%'",
                        "id IN (1, 2, 3)"
                    ],
                    // UI hints embedded as custom properties
                    "x-ui-widget": "textarea",
                    "x-ui-placeholder": "status = 'active' AND created_at > NOW() - INTERVAL '7 days'",
                    "x-ui-rows": 3
                }
            },
            "additionalProperties": false
        })
    }

    fn get_sort_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<serde_json::Value> {
        // Get entity schema to know available fields
        let schema_result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.get_schema(container_path, entity_name).await })
        });

        let schema = schema_result?;

        // Build enum of available fields
        let field_names: Vec<String> = schema.fields.iter().map(|f| f.name.clone()).collect();

        Ok(serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "title": "Sort Options",
            "description": "Specify how to sort query results",
            "properties": {
                "sort_by": {
                    "type": "string",
                    "title": "Sort By",
                    "description": "Field to sort by",
                    "enum": field_names,
                    "x-ui-widget": "select"
                },
                "sort_order": {
                    "type": "string",
                    "title": "Sort Order",
                    "description": "Sort direction",
                    "enum": ["asc", "desc"],
                    "default": "asc",
                    "x-ui-widget": "select"
                }
            }
        }))
    }

    fn get_filter_ui_schema(&self) -> Option<serde_json::Value> {
        // No longer needed - UI hints are embedded in filter_schema
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pg_type_mapping() {
        assert_eq!(PostgresSource::map_pg_type("integer"), FieldType::Int32);
        assert_eq!(PostgresSource::map_pg_type("bigint"), FieldType::Int64);
        assert_eq!(PostgresSource::map_pg_type("text"), FieldType::String);
        assert_eq!(
            PostgresSource::map_pg_type("timestamp"),
            FieldType::Timestamp
        );
        assert_eq!(PostgresSource::map_pg_type("uuid"), FieldType::Uuid);
        assert_eq!(PostgresSource::map_pg_type("jsonb"), FieldType::Json);
    }
}
