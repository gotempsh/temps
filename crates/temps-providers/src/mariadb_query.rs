use async_trait::async_trait;
use futures::TryStreamExt;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::Row;
use std::collections::HashMap;
use temps_query::{
    BoundedRows, Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType,
    DataError, DataRow, DataSource, DatasetSchema, EntityCountHint, EntityInfo, FieldDef,
    FieldType, Introspect, QueryBudget, QueryOptions, QueryResult, QuerySchemaProvider, QueryStats,
    Queryable, Result,
};
use tracing::{debug, error, warn};

pub struct MariaDbSource {
    pool: MySqlPool,
    database_name: String,
}

impl MariaDbSource {
    /// Above this many estimated rows, report `information_schema.TABLE_ROWS`
    /// rather than running an exact `COUNT(*)`.
    ///
    /// Mirrors the Postgres backend's threshold. On InnoDB `COUNT(*)` is a
    /// full index scan; TABLE_ROWS is a stored estimate that can be off by a
    /// wide margin on small tables, which is why small ones still get counted
    /// exactly.
    const EXACT_COUNT_MAX_ROWS: u64 = 50_000;

    /// Estimated row count and on-disk size (data + indexes) for one table,
    /// read from `information_schema` — a catalog lookup, not a table scan.
    ///
    /// Returns `(None, None)` when the table has no `information_schema` row
    /// (e.g. a view), so callers fall back to an exact count rather than
    /// reporting zero.
    async fn table_stats(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<(Option<u64>, Option<u64>)> {
        let database_name = database_from_path(container_path, &self.database_name)?;
        validate_identifier("table", entity_name)?;

        let row = sqlx::query(
            r#"
            SELECT TABLE_ROWS, DATA_LENGTH, INDEX_LENGTH
            FROM information_schema.TABLES
            WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
            "#,
        )
        .bind(database_name)
        .bind(entity_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            DataError::QueryFailed(format!(
                "Failed to read table statistics for '{}.{}': {}",
                database_name, entity_name, e
            ))
        })?;

        let Some(row) = row else {
            return Ok((None, None));
        };

        let table_rows = row.try_get::<Option<u64>, _>("TABLE_ROWS").ok().flatten();
        let data_length = row
            .try_get::<Option<u64>, _>("DATA_LENGTH")
            .ok()
            .flatten()
            .unwrap_or(0);
        let index_length = row
            .try_get::<Option<u64>, _>("INDEX_LENGTH")
            .ok()
            .flatten()
            .unwrap_or(0);

        let size = data_length.saturating_add(index_length);
        Ok((table_rows, (size > 0).then_some(size)))
    }

    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
    ) -> Result<Self> {
        validate_identifier("database", database)?;

        let url = format!(
            "mysql://{}:{}@{}:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            host,
            port,
            urlencoding::encode(database)
        );

        debug!(
            "Connecting to MariaDB: {}@{}:{}/{}",
            username, host, port, database
        );

        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .map_err(|e| {
                DataError::ConnectionFailed(format!("MariaDB connection failed: {}", e))
            })?;

        Ok(Self {
            pool,
            database_name: database.to_string(),
        })
    }

    fn map_mysql_type(mysql_type: &str) -> FieldType {
        match mysql_type.to_ascii_lowercase().as_str() {
            "bool" | "boolean" => FieldType::Boolean,
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "year" => FieldType::Int32,
            "bigint" => FieldType::Int64,
            "float" => FieldType::Float32,
            "double" | "real" => FieldType::Float64,
            "decimal" | "numeric" => FieldType::String,
            "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => {
                FieldType::Bytes
            }
            "date" => FieldType::Date,
            "datetime" | "timestamp" | "time" => FieldType::Timestamp,
            "json" => FieldType::Json,
            _ => FieldType::String,
        }
    }

    async fn query_columns(
        &self,
        database_name: &str,
        entity_name: &str,
    ) -> Result<Vec<MariaQueryColumn>> {
        let rows = sqlx::query(
            "SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        )
        .bind(database_name)
        .bind(entity_name)
        .fetch_all(&self.pool)
        .await
        .map_err(|_error| {
            DataError::SchemaError(format!(
                "Failed to inspect MariaDB query columns for '{}.{}'",
                database_name, entity_name
            ))
        })?;
        rows.into_iter()
            .map(|row| {
                Ok(MariaQueryColumn {
                    name: row.try_get("COLUMN_NAME").map_err(|_error| {
                        DataError::SchemaError(format!(
                            "Failed to decode a column name for '{}.{}'",
                            database_name, entity_name
                        ))
                    })?,
                    data_type: row.try_get("DATA_TYPE").map_err(|_error| {
                        DataError::SchemaError(format!(
                            "Failed to decode a column type for '{}.{}'",
                            database_name, entity_name
                        ))
                    })?,
                })
            })
            .collect()
    }
}

#[async_trait]
impl DataSource for MariaDbSource {
    fn source_type(&self) -> &'static str {
        "mariadb"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Sql]
    }

    async fn list_containers(&self, path: &ContainerPath) -> Result<Vec<ContainerInfo>> {
        match path.depth() {
            0 => {
                let rows = sqlx::query(
                    r#"
                    SELECT SCHEMA_NAME
                    FROM information_schema.SCHEMATA
                    WHERE SCHEMA_NAME NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys')
                    ORDER BY SCHEMA_NAME
                    "#,
                )
                .fetch_all(&self.pool)
                .await
                .map_err(|e| DataError::QueryFailed(format!("Failed to list databases: {}", e)))?;

                Ok(rows
                    .iter()
                    .filter_map(|row| row.try_get::<String, _>("SCHEMA_NAME").ok())
                    .map(|name| ContainerInfo {
                        name,
                        container_type: ContainerType::Database,
                        capabilities: ContainerCapabilities {
                            can_contain_containers: false,
                            can_contain_entities: true,
                            child_container_type: None,
                            entity_type_label: Some("table".to_string()),
                            entity_count_hint: Some(EntityCountHint::Small),
                        },
                        metadata: HashMap::new(),
                    })
                    .collect())
            }
            _ => Err(DataError::InvalidQuery(format!(
                "MariaDB hierarchy only supports root/database levels. Path depth: {}",
                path.depth()
            ))),
        }
    }

    async fn get_container_info(&self, path: &ContainerPath) -> Result<ContainerInfo> {
        if path.depth() != 1 {
            return Err(DataError::InvalidQuery(format!(
                "get_container_info requires path depth 1 (database), got {}",
                path.depth()
            )));
        }

        let database_name = &path.segments[0];
        validate_identifier("database", database_name)?;

        let row = sqlx::query(
            r#"
            SELECT SCHEMA_NAME, DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME
            FROM information_schema.SCHEMATA
            WHERE SCHEMA_NAME = ?
            "#,
        )
        .bind(database_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            DataError::QueryFailed(format!(
                "Failed to read database '{}': {}",
                database_name, e
            ))
        })?
        .ok_or_else(|| DataError::NotFound(format!("Database '{}' not found", database_name)))?;

        let name: String = row.try_get("SCHEMA_NAME").map_err(|e| {
            DataError::SerializationError(format!("Failed to read database name: {}", e))
        })?;
        let charset: Option<String> = row.try_get("DEFAULT_CHARACTER_SET_NAME").ok();
        let collation: Option<String> = row.try_get("DEFAULT_COLLATION_NAME").ok();

        let mut metadata = HashMap::new();
        if let Some(value) = charset {
            metadata.insert("charset".to_string(), serde_json::json!(value));
        }
        if let Some(value) = collation {
            metadata.insert("collation".to_string(), serde_json::json!(value));
        }

        Ok(ContainerInfo {
            name,
            container_type: ContainerType::Database,
            capabilities: ContainerCapabilities {
                can_contain_containers: false,
                can_contain_entities: true,
                child_container_type: None,
                entity_type_label: Some("table".to_string()),
                entity_count_hint: Some(EntityCountHint::Small),
            },
            metadata,
        })
    }

    async fn list_entities(&self, container_path: &ContainerPath) -> Result<Vec<EntityInfo>> {
        if container_path.depth() != 1 {
            return Err(DataError::InvalidQuery(format!(
                "list_entities requires path depth 1 (database), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        validate_identifier("database", database_name)?;
        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot list tables from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let rows = sqlx::query(
            r#"
            SELECT TABLE_NAME, TABLE_ROWS, DATA_LENGTH, INDEX_LENGTH
            FROM information_schema.TABLES
            WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE'
            ORDER BY TABLE_NAME
            "#,
        )
        .bind(database_name)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            DataError::QueryFailed(format!(
                "Failed to list tables in database '{}': {}",
                database_name, e
            ))
        })?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                let table_name = row.try_get::<String, _>("TABLE_NAME").ok()?;
                let table_rows = row
                    .try_get::<Option<u64>, _>("TABLE_ROWS")
                    .ok()
                    .flatten()
                    .and_then(|v| usize::try_from(v).ok());
                let data_length = row
                    .try_get::<Option<u64>, _>("DATA_LENGTH")
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                let index_length = row
                    .try_get::<Option<u64>, _>("INDEX_LENGTH")
                    .ok()
                    .flatten()
                    .unwrap_or(0);

                Some(EntityInfo {
                    namespace: database_name.clone(),
                    name: table_name,
                    entity_type: "table".to_string(),
                    row_count: table_rows,
                    size_bytes: Some(data_length.saturating_add(index_length)),
                    schema: None,
                    metadata: None,
                })
            })
            .collect())
    }

    async fn get_entity_info(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo> {
        if !self.entity_exists(container_path, entity_name).await? {
            return Err(DataError::NotFound(format!(
                "Table '{}.{}' not found",
                container_path, entity_name
            )));
        }

        // Row count and size from information_schema rather than COUNT(*).
        //
        // `self.count()` issues `SELECT COUNT(*)`, which on InnoDB is a full
        // index scan — so opening a large table blocked on scanning it before
        // any rows could render. `list_entities` (above) already reads
        // TABLE_ROWS/DATA_LENGTH/INDEX_LENGTH for exactly this reason; this
        // path simply never did.
        //
        // TABLE_ROWS is an estimate on InnoDB (exact on MyISAM). Small tables
        // fall back to the exact count below, where it is cheap and the
        // estimate's error is proportionally worst.
        let (estimated_rows, size_bytes) = self
            .table_stats(container_path, entity_name)
            .await
            .unwrap_or((None, None));

        let row_count = match estimated_rows {
            Some(rows) if rows >= Self::EXACT_COUNT_MAX_ROWS => Some(rows),
            // Small, or information_schema had nothing (a view, or stats never
            // gathered) — an exact count is affordable and more useful.
            _ => self.count(container_path, entity_name, None).await.ok(),
        };

        Ok(EntityInfo {
            namespace: container_path.segments[0].clone(),
            name: entity_name.to_string(),
            entity_type: "table".to_string(),
            row_count: row_count.and_then(|v| usize::try_from(v).ok()),
            size_bytes,
            schema: Some(self.get_schema(container_path, entity_name).await?),
            metadata: None,
        })
    }

    async fn get_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<DatasetSchema> {
        if container_path.depth() != 1 {
            return Err(DataError::InvalidQuery(format!(
                "get_schema requires path depth 1 (database), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        validate_identifier("database", database_name)?;
        validate_identifier("table", entity_name)?;
        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot get schema from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let rows = sqlx::query(
            r#"
            SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_KEY
            FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
            ORDER BY ORDINAL_POSITION
            "#,
        )
        .bind(database_name)
        .bind(entity_name)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            DataError::SchemaError(format!(
                "Failed to get schema for table '{}.{}': {}",
                database_name, entity_name, e
            ))
        })?;

        if rows.is_empty() {
            return Err(DataError::NotFound(format!(
                "Table '{}.{}' not found",
                database_name, entity_name
            )));
        }

        let mut primary_key = Vec::new();
        let mut fields = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get("COLUMN_NAME").map_err(|e| {
                DataError::SchemaError(format!("Failed to read column name: {}", e))
            })?;
            let data_type: String = row.try_get("DATA_TYPE").map_err(|e| {
                DataError::SchemaError(format!("Failed to read column type: {}", e))
            })?;
            let is_nullable: String = row.try_get("IS_NULLABLE").unwrap_or_else(|_| "YES".into());
            let column_key: Option<String> = row.try_get("COLUMN_KEY").ok();
            if column_key.as_deref() == Some("PRI") {
                primary_key.push(name.clone());
            }

            fields.push(FieldDef {
                name,
                field_type: Self::map_mysql_type(&data_type),
                nullable: is_nullable == "YES",
                description: None,
            });
        }

        Ok(DatasetSchema {
            fields,
            partitions: None,
            primary_key: if primary_key.is_empty() {
                None
            } else {
                Some(primary_key)
            },
        })
    }

    async fn close(&self) -> Result<()> {
        self.pool.close().await;
        Ok(())
    }
}

#[async_trait]
impl Introspect for MariaDbSource {
    async fn inspect_fields(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<Vec<FieldDef>> {
        Ok(self.get_schema(container_path, entity_name).await?.fields)
    }

    async fn field_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<bool> {
        validate_identifier("field", field)?;
        let schema = self.get_schema(container_path, entity_name).await?;
        Ok(schema.fields.iter().any(|f| f.name == field))
    }

    async fn get_field_type(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<FieldType> {
        validate_identifier("field", field)?;
        let schema = self.get_schema(container_path, entity_name).await?;
        schema
            .fields
            .into_iter()
            .find(|f| f.name == field)
            .map(|f| f.field_type)
            .ok_or_else(|| {
                DataError::NotFound(format!(
                    "Field '{}' not found in table '{}'",
                    field, entity_name
                ))
            })
    }
}

#[async_trait]
impl Queryable for MariaDbSource {
    async fn query(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
        options: QueryOptions,
    ) -> Result<QueryResult> {
        let database_name = database_from_path(container_path, &self.database_name)?;
        validate_identifier("table", entity_name)?;
        let schema = self.get_schema(container_path, entity_name).await?;
        let columns = self.query_columns(database_name, entity_name).await?;

        let start = std::time::Instant::now();
        let mut sql = format!(
            "SELECT * FROM {}.{}",
            quote_identifier(database_name),
            quote_identifier(entity_name)
        );

        if let Some(filter_json) = filters {
            if let Some(where_clause) = filter_json.get("where").and_then(|v| v.as_str()) {
                validate_where_clause(where_clause)?;
                sql.push_str(" WHERE ");
                sql.push_str(where_clause);
            }
        }

        if let Some(sort_by) = &options.sort_by {
            let sort_field = normalize_sort_field(sort_by)?;
            let sort_order = match options.sort_order.as_deref() {
                Some("desc") | Some("DESC") => "DESC",
                _ => "ASC",
            };
            sql.push_str(&format!(
                " ORDER BY {} {}",
                quote_identifier(sort_field),
                sort_order
            ));
        }

        let limit = options.limit.unwrap_or(100);
        let offset = options.offset.unwrap_or(0);
        sql.push_str(" LIMIT ? OFFSET ?");
        let sql = with_wire_row_budget(&sql, &columns, options.budget)?;

        debug!(
            entity = entity_name,
            limit, offset, "executing MariaDB data query"
        );

        // SECURITY / AVAILABILITY: bound the query server-side.
        //
        // The caller's tokio deadline frees the control-plane task, but dropping
        // the future does not stop the server: it keeps executing and keeps
        // holding its share of the operator's database. That matters here
        // because this backend accepts a caller-supplied WHERE clause and a
        // caller-supplied OFFSET, whose cost is O(offset) regardless of LIMIT.
        //
        // See `apply_statement_timeout` for why both spellings are tried and
        // why this must share the query's connection.
        let timeout_ms = options.timeout_ms.unwrap_or(30_000);
        let mut conn = self.pool.acquire().await.map_err(|e| {
            DataError::ConnectionFailed(format!(
                "Failed to acquire a MariaDB connection for query on {}.{}: {}",
                database_name, entity_name, e
            ))
        })?;
        apply_statement_timeout(&mut conn, timeout_ms, database_name).await;

        let mut stream = sqlx::query(&sql)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch(&mut *conn);
        let mut bounded = BoundedRows::new(options.budget);
        while let Some(row) = stream.try_next().await.map_err(|_error| {
            error!(entity = entity_name, limit, "MariaDB row stream failed");
            DataError::BackendQueryFailed {
                backend: "MariaDB",
                entity: entity_name.to_string(),
            }
        })? {
            let observed = row.try_get::<i64, _>("__temps_size").map_err(|error| {
                error!(
                    entity = entity_name,
                    limit,
                    error = %error,
                    "MariaDB bounded row size decode failed"
                );
                DataError::BackendQueryFailed {
                    backend: "MariaDB",
                    entity: entity_name.to_string(),
                }
            })?;
            let observed_cell = row.try_get::<i64, _>("__temps_max_cell").map_err(|error| {
                error!(
                    entity = entity_name,
                    limit,
                    error = %error,
                    "MariaDB bounded cell size decode failed"
                );
                DataError::BackendQueryFailed {
                    backend: "MariaDB",
                    entity: entity_name.to_string(),
                }
            })?;
            let payload = row
                .try_get::<Option<String>, _>("__temps_row")
                .map_err(|_error| {
                    error!(
                        entity = entity_name,
                        limit, "MariaDB bounded row decode failed"
                    );
                    DataError::BackendQueryFailed {
                        backend: "MariaDB",
                        entity: entity_name.to_string(),
                    }
                })?
                .ok_or_else(|| {
                    let observed_cell = usize::try_from(observed_cell).unwrap_or(usize::MAX);
                    let (limit_kind, limit, observed) =
                        if observed_cell > options.budget.max_cell_bytes {
                            (
                                "wire_cell_bytes",
                                options.budget.max_cell_bytes,
                                observed_cell,
                            )
                        } else {
                            (
                                "wire_row_bytes",
                                options.budget.max_bytes,
                                usize::try_from(observed).unwrap_or(usize::MAX),
                            )
                        };
                    DataError::ResultLimitExceeded {
                        entity: entity_name.to_string(),
                        limit_kind,
                        limit,
                        observed,
                    }
                })?;
            let data_row = serde_json::from_str::<DataRow>(&payload).map_err(|_error| {
                error!(
                    entity = entity_name,
                    limit, "MariaDB bounded JSON row decode failed"
                );
                DataError::BackendQueryFailed {
                    backend: "MariaDB",
                    entity: entity_name.to_string(),
                }
            })?;
            if !bounded.push(entity_name, data_row)? {
                break;
            }
        }
        drop(stream);
        drop(conn);

        let (data_rows, truncated) = bounded.into_parts();
        let row_count = data_rows.len();

        Ok(QueryResult {
            schema,
            rows: data_rows,
            stats: QueryStats {
                row_count,
                total_rows: None,
                execution_ms: start.elapsed().as_millis() as u64,
                has_more: row_count >= limit,
                next_cursor: None,
                truncated,
            },
        })
    }

    async fn count(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
    ) -> Result<u64> {
        let database_name = database_from_path(container_path, &self.database_name)?;
        validate_identifier("table", entity_name)?;

        let mut sql = format!(
            "SELECT COUNT(*) AS row_count FROM {}.{}",
            quote_identifier(database_name),
            quote_identifier(entity_name)
        );

        if let Some(filter_json) = filters {
            if let Some(where_clause) = filter_json.get("where").and_then(|v| v.as_str()) {
                validate_where_clause(where_clause)?;
                sql.push_str(" WHERE ");
                sql.push_str(where_clause);
            }
        }

        // SECURITY: bound this the way `query` is bounded.
        //
        // The timeout work originally covered the row-reading path only, which
        // left the more expensive operation unbounded. `COUNT(*)` is a full
        // index scan, it takes a caller-supplied WHERE clause, and
        // `get_entity_info` falls through to it whenever `TABLE_ROWS` is null
        // or below the estimate threshold — true for every view and every table
        // whose stats were never gathered, regardless of real size. Since
        // `get_entity_info` is in the AI read allowlist, "check the row count of
        // every table" would otherwise be a denial of service against the
        // customer's production database.
        let mut conn = self.pool.acquire().await.map_err(|e| {
            DataError::ConnectionFailed(format!(
                "Failed to acquire a MariaDB connection to count {}.{}: {}",
                database_name, entity_name, e
            ))
        })?;
        apply_statement_timeout(&mut conn, COUNT_TIMEOUT_MS, database_name).await;

        let row = sqlx::query(&sql)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| DataError::QueryFailed(format!("Count query failed: {}", e)))?;

        let count = row
            .try_get::<i64, _>("row_count")
            .or_else(|_| row.try_get::<u64, _>("row_count").map(|v| v as i64))
            .map_err(|e| DataError::SerializationError(format!("Invalid count result: {}", e)))?;

        Ok(count.max(0) as u64)
    }

    async fn entity_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<bool> {
        let database_name = database_from_path(container_path, &self.database_name)?;
        validate_identifier("table", entity_name)?;

        let row = sqlx::query(
            r#"
            SELECT COUNT(*) AS table_count
            FROM information_schema.TABLES
            WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND TABLE_TYPE = 'BASE TABLE'
            "#,
        )
        .bind(database_name)
        .bind(entity_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| DataError::QueryFailed(format!("Entity existence check failed: {}", e)))?;

        let count: i64 = row.try_get("table_count").unwrap_or(0);
        Ok(count > 0)
    }
}

impl QuerySchemaProvider for MariaDbSource {
    fn get_filter_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "title": "MariaDB Query Filters",
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
                    "x-ui-widget": "textarea",
                    "x-ui-placeholder": "status = 'active' AND created_at > NOW() - INTERVAL 7 DAY",
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
        let schema_result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.get_schema(container_path, entity_name).await })
        });

        let schema = schema_result?;
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
}

fn database_from_path<'a>(
    container_path: &'a ContainerPath,
    connected_database: &'a str,
) -> Result<&'a str> {
    if container_path.depth() != 1 {
        return Err(DataError::InvalidQuery(format!(
            "MariaDB table operations require path depth 1 (database), got {}",
            container_path.depth()
        )));
    }

    let database_name = container_path.segments[0].as_str();
    validate_identifier("database", database_name)?;
    if database_name != connected_database {
        return Err(DataError::OperationNotSupported(format!(
            "Cannot query database '{}' while connected to '{}'",
            database_name, connected_database
        )));
    }
    Ok(database_name)
}

fn quote_identifier(value: &str) -> String {
    format!("`{}`", value.replace('`', "``"))
}

#[derive(Clone, Debug)]
struct MariaQueryColumn {
    name: String,
    data_type: String,
}

fn mariadb_binary_json_type(data_type: &str) -> bool {
    matches!(
        data_type,
        "binary"
            | "varbinary"
            | "tinyblob"
            | "blob"
            | "mediumblob"
            | "longblob"
            | "geometry"
            | "point"
            | "linestring"
            | "polygon"
            | "multipoint"
            | "multilinestring"
            | "multipolygon"
            | "geometrycollection"
    )
}

fn mariadb_json_projection(columns: &[MariaQueryColumn]) -> String {
    let entries = columns.iter().flat_map(|column| {
        let key_hex = column
            .name
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let key = format!("CONVERT(X'{key_hex}' USING utf8mb4)");
        let column_ref = format!("__temps_source.{}", quote_identifier(&column.name));
        let value = if mariadb_binary_json_type(&column.data_type) {
            format!("TO_BASE64({column_ref})")
        } else if column.data_type == "bit" {
            format!("CAST({column_ref} AS UNSIGNED)")
        } else {
            column_ref
        };
        [key, value]
    });

    format!("JSON_OBJECT({})", entries.collect::<Vec<_>>().join(", "))
}

fn mariadb_column_admission(column: &MariaQueryColumn) -> Result<String> {
    let value = format!("__temps_source.{}", quote_identifier(&column.name));
    let estimate = match column.data_type.as_str() {
        data_type if mariadb_binary_json_type(data_type) => {
            format!("OCTET_LENGTH({value}) * 5 + 8")
        }
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => {
            format!("OCTET_LENGTH({value}) * 6 + 8")
        }
        "json" => format!("JSON_STORAGE_SIZE({value}) * 6 + 8"),
        "bool" | "boolean" | "tinyint" | "smallint" | "mediumint" | "int" | "integer"
        | "bigint" | "float" | "double" | "real" | "decimal" | "numeric" | "date" | "datetime"
        | "timestamp" | "time" | "year" | "bit" => "128".to_string(),
        unsupported => {
            return Err(DataError::OperationNotSupported(format!(
                "MariaDB column '{}' uses unsupported type '{}'",
                column.name, unsupported
            )))
        }
    };
    Ok(format!("COALESCE({estimate}, 4)"))
}

/// Admission expressions execute before the JSON constructor. Rejected rows
/// return only conservative byte metadata, never the original values.
fn with_wire_row_budget(
    sql: &str,
    columns: &[MariaQueryColumn],
    budget: QueryBudget,
) -> Result<String> {
    let estimates = columns
        .iter()
        .map(mariadb_column_admission)
        .collect::<Result<Vec<_>>>()?;
    let max_cell = if estimates.is_empty() {
        "0".to_string()
    } else {
        format!("GREATEST({})", estimates.join(", "))
    };
    let key_overhead = columns.iter().fold(2usize, |total, column| {
        total.saturating_add(column.name.len().saturating_mul(6).saturating_add(4))
    });
    let row_size = if estimates.is_empty() {
        key_overhead.to_string()
    } else {
        format!("{key_overhead} + {}", estimates.join(" + "))
    };
    let projection = mariadb_json_projection(columns);

    Ok(format!(
        "SELECT CASE WHEN {max_cell} <= {} AND {row_size} <= {} \
             THEN {projection} ELSE NULL END AS __temps_row, \
             {row_size} AS __temps_size, {max_cell} AS __temps_max_cell \
         FROM ({sql}) AS __temps_source",
        budget.max_cell_bytes, budget.max_bytes
    ))
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(DataError::InvalidQuery(format!(
            "{} cannot be empty",
            label
        )));
    }
    if value.len() > 63 {
        return Err(DataError::InvalidQuery(format!(
            "{} '{}' exceeds 63 character limit",
            label, value
        )));
    }

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(DataError::InvalidQuery(format!(
            "{} cannot be empty",
            label
        )));
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(DataError::InvalidQuery(format!(
            "{} '{}' must start with a letter or underscore",
            label, value
        )));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(DataError::InvalidQuery(format!(
            "{} '{}' contains invalid characters. Only ASCII letters, digits, and underscores are allowed",
            label, value
        )));
    }

    Ok(())
}

fn normalize_sort_field(sort_by: &str) -> Result<&str> {
    let trimmed = sort_by.trim().trim_start_matches('/');
    validate_identifier("sort field", trimmed)?;
    Ok(trimmed)
}

/// Server-side ceiling for `count`, which takes no `QueryOptions` and so has no
/// caller-supplied deadline to honour. See the call site for why it needs one.
const COUNT_TIMEOUT_MS: u64 = 10_000;

/// Apply a server-side statement timeout to one pooled connection.
///
/// The knob is spelled differently on the two engines this backend serves —
/// MySQL 5.7.8+ has `max_execution_time` (milliseconds), MariaDB has
/// `max_statement_time` (seconds, fractional) — and setting the one the server
/// does not know is an error, so both are tried and whichever lands wins. Must
/// run on the SAME connection as the statement it is meant to bound; a session
/// variable set on some other connection in the pool bounds some other request.
///
/// Best-effort by design: failing to set a ceiling should not fail the query,
/// but it must be visible, because it means only the control-plane deadline is
/// in play and the server will keep working after the client gives up.
async fn apply_statement_timeout(
    conn: &mut sqlx::pool::PoolConnection<sqlx::MySql>,
    timeout_ms: u64,
    database_name: &str,
) {
    let mysql_set = sqlx::query(&format!("SET SESSION max_execution_time = {timeout_ms}"))
        .execute(&mut **conn)
        .await
        .is_ok();
    let mariadb_set = sqlx::query(&format!(
        "SET SESSION max_statement_time = {}",
        timeout_ms as f64 / 1000.0
    ))
    .execute(&mut **conn)
    .await
    .is_ok();

    if !mysql_set && !mariadb_set {
        warn!(
            database = %database_name,
            "Neither max_execution_time nor max_statement_time is supported by this server; \
             data browser statements are bounded only by the control-plane deadline"
        );
    }
}

fn strip_sql_string_literals(sql: &str) -> Result<String> {
    let mut result = String::with_capacity(sql.len());
    let mut in_string = false;
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        // SECURITY: backtick-quoted identifiers must be normalised before the
        // shared structural check runs, and they must collapse to the *same*
        // placeholder the PostgreSQL stripper emits for `"..."`.
        //
        // `reject_subqueries_and_function_calls` was written against PostgreSQL
        // lexing: it recognises a function call either by a quote character
        // (`"` or `'`) immediately before `(`, or by walking back over
        // identifier characters. A backtick is neither — it is not a quote it
        // knows about, and it terminates the identifier walk — so the guard saw
        // an empty identifier before the paren and allowed the call through.
        // The `sleep(`/`benchmark(` denylist entries missed it independently,
        // because a backticked call contains "sleep`(" rather than "sleep(".
        // One character defeated both layers, yielding a CPU/connection-hold
        // primitive on the operator's database reachable from the agent's
        // documented `--filter`.
        //
        // Emitting `""` means `` `benchmark`(...) `` reaches the shared check as
        // `""(...)`, which it already rejects as a quoted function call. A
        // backtick-quoted identifier in ordinary use (`` `order` = 1 ``) becomes
        // `"" = 1` and still validates. Only the validator sees this rewrite;
        // the server is always sent the caller's original text.
        if c == '`' && !in_string {
            let mut terminated = false;
            while let Some(inner) = chars.next() {
                if inner == '`' {
                    // MySQL escapes a literal backtick inside a quoted
                    // identifier by doubling it.
                    if chars.peek() == Some(&'`') {
                        chars.next();
                        continue;
                    }
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(DataError::InvalidQuery(
                    "Unterminated backtick-quoted identifier in the data browser filter"
                        .to_string(),
                ));
            }
            result.push_str("\"\"");
            continue;
        }

        // SECURITY: refuse double quotes rather than guess what they mean.
        //
        // Under MySQL/MariaDB's default sql_mode `"..."` is a *string literal*;
        // under ANSI_QUOTES it is a quoted identifier. The server's mode is not
        // knowable from here, and the two readings disagree about where the
        // literal ends — so any stripper that picks one desynchronises from the
        // server whenever the other applies. That is exactly the class of bug
        // the backslash rule below was added for: the stripper swallows a span
        // the server executes, and every later check runs on a sanitised
        // remainder.
        //
        // Single quotes delimit strings in both modes and backticks delimit
        // identifiers in both modes, so a legitimate filter never needs `"`.
        if c == '"' && !in_string {
            return Err(DataError::InvalidQuery(
                "Double quotes are not allowed in the data browser filter — use single quotes \
                 for string values and backticks for identifiers"
                    .to_string(),
            ));
        }

        if in_string {
            // SECURITY: MySQL/MariaDB honour backslash escapes inside string
            // literals unless NO_BACKSLASH_ESCAPES is set, and the server's
            // sql_mode is not knowable from here. This stripper only
            // understood the doubled-quote form, so `'\''` desynchronised it
            // from the server: it read the escaped quote as the first half of
            // a `''` pair, stayed "inside" the string, and discarded the rest
            // of the clause — including the payload. Every downstream check
            // then ran against a sanitised remainder and passed.
            //
            //   1='\'' union select 1 from mysql.user where 'a'='a'
            //
            // Rather than emulate a mode we cannot observe, refuse the input.
            // A legitimate data-browser filter has no need of one.
            if c == '\\' {
                return Err(DataError::InvalidQuery(
                    "Backslash escapes are not allowed inside string literals in the data \
                     browser filter"
                        .to_string(),
                ));
            }
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                } else {
                    in_string = false;
                    result.push('\'');
                }
            }
        } else if c == '\'' {
            in_string = true;
            result.push('\'');
        } else {
            result.push(c);
        }
    }

    if in_string {
        return Err(DataError::InvalidQuery(
            "Unterminated string literal in the data browser filter".to_string(),
        ));
    }

    Ok(result)
}

fn validate_where_clause(sql: &str) -> Result<()> {
    let sql_lower = sql.trim().to_ascii_lowercase();

    if sql_lower.is_empty() {
        return Err(DataError::InvalidQuery(
            "WHERE clause cannot be empty".to_string(),
        ));
    }

    // SECURITY: normalise whitespace before any keyword matching. The list
    // below matches literal prefixes like `"union "`, so a separator it does
    // not enumerate (\r, \f, \v — all whitespace to MySQL's lexer) walked
    // straight past it.
    let without_strings =
        temps_query_postgres::normalize_sql_whitespace(&strip_sql_string_literals(&sql_lower)?);

    if without_strings.contains(';') {
        return Err(DataError::InvalidQuery(
            "Multiple SQL statements are not allowed".to_string(),
        ));
    }

    if without_strings.contains("--")
        || without_strings.contains("/*")
        || without_strings.contains('#')
    {
        return Err(DataError::InvalidQuery(
            "SQL comments are not allowed in the data browser".to_string(),
        ));
    }

    // STRUCTURAL CHECKS — shared with the PostgreSQL validator rather than
    // reimplemented. The local copy drifted: it inspected only the byte
    // immediately before `(` (so `sleep (5)` and `extractvalue (1, user ())`
    // passed) and treated `select` as the only subquery starter (so
    // `id IN (TABLE mysql.user)` passed on MySQL 8.0.19+, which this backend
    // also serves). One implementation, one set of tests.
    temps_query_postgres::reject_subqueries_and_function_calls(&without_strings)?;

    let dangerous_keywords = [
        "drop ",
        "truncate ",
        "alter ",
        "create ",
        "grant ",
        "revoke ",
        "insert ",
        "update ",
        "delete ",
        "replace ",
        "load ",
        // One space suffices now that whitespace is normalised above.
        "union ",
        "intersect ",
        "except ",
        // These two keep the paren attached deliberately. The real guard for
        // function calls is the structural check above, which rejects *any*
        // identifier before `(` — including the backtick-quoted form, now that
        // the stripper normalises it. Matching bare "sleep"/"benchmark" here
        // would add nothing the structural check does not already catch, while
        // rejecting ordinary columns like `sleep_minutes` or `benchmark_id`.
        "sleep(",
        "benchmark(",
        "load_file",
        " into ",
        "outfile",
        "dumpfile",
        "execute ",
        "prepare ",
        "call ",
        "handler ",
        "lock ",
        "unlock ",
        "set ",
        "begin ",
        "commit ",
        "rollback ",
        "savepoint ",
    ];

    // Match on token boundaries rather than raw substrings. The trailing space
    // in each entry was doing that job badly: `payload = 'x'` matched `"load "`
    // and `charset = 'utf8'` matched `"set "`, so two very ordinary column
    // names were unfilterable. A validator that blocks legitimate queries is a
    // validator someone eventually switches off.
    for keyword in &dangerous_keywords {
        let keyword = keyword.trim();
        // `sleep(`/`benchmark(` keep their paren and stay substring matches —
        // they are not identifiers, and the structural check above is the real
        // guard for the function-call forms.
        let hit = if keyword.ends_with('(') {
            without_strings.contains(keyword)
        } else {
            temps_query_postgres::contains_sql_token(&without_strings, keyword)
        };
        if hit {
            return Err(DataError::InvalidQuery(format!(
                "SQL operation '{}' is not allowed in the data browser",
                keyword
            )));
        }
    }

    // SECURITY: no regex operators.
    //
    // The denylist runs on string-stripped input, so a regex pattern — which
    // lives entirely inside a string literal — is invisible to it.
    // `1=1 and 'aaaaaaaaaaaaaaaaaaaa' regexp '(a+)+b'` strips to
    // `1=1 and '' regexp ''` and passes every check. MariaDB's engine is PCRE,
    // which backtracks, so that is the same CPU-burn primitive on the
    // operator's database that the `benchmark(`/backtick fix closed, reached
    // through a door the fix cannot see. Not fixable by inspecting the payload:
    // the payload is opaque to us by construction.
    for operator in ["regexp", "rlike"] {
        if temps_query_postgres::contains_sql_token(&without_strings, operator) {
            return Err(DataError::InvalidQuery(
                "Regular-expression operators (REGEXP, RLIKE) are not allowed in the data \
                 browser — use LIKE instead"
                    .to_string(),
            ));
        }
    }

    // SECURITY: no system-variable reads.
    //
    // `@@datadir`, `@@version`, `@@secure_file_priv` and friends need no
    // parentheses, so the function-call guard never sees them, and they give
    // blind boolean extraction of the server's configuration through an
    // ordinary filter. Same for the parenless information functions.
    if without_strings.contains("@@") {
        return Err(DataError::InvalidQuery(
            "System variables (@@...) are not allowed in the data browser".to_string(),
        ));
    }
    for token in [
        "current_user",
        "session_user",
        "system_user",
        "current_role",
    ] {
        if temps_query_postgres::contains_sql_token(&without_strings, token) {
            return Err(DataError::InvalidQuery(format!(
                "'{token}' is not allowed in the data browser"
            )));
        }
    }

    Ok(())
}

pub(crate) fn is_mariadb_compatible_image(image: &str) -> bool {
    let lower = image.to_ascii_lowercase();
    lower.contains("mariadb") || lower.split(['/', ':']).any(|part| part == "mysql")
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::{
        core::{ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    fn container_runtime_unavailable(error: &str) -> bool {
        let message = error.to_ascii_lowercase();
        [
            "hyper legacy client: client error (connect)",
            "failed to connect to docker",
            "error connecting to docker",
            "docker daemon is unavailable",
            "docker client is unavailable",
            "could not find docker environment",
            "docker socket",
        ]
        .iter()
        .any(|marker| message.contains(marker))
    }

    #[test]
    fn generated_query_guards_encoded_row_before_wire_transfer() {
        let columns = vec![
            MariaQueryColumn {
                name: "display_name".to_string(),
                data_type: "longtext".to_string(),
            },
            MariaQueryColumn {
                name: "avatar".to_string(),
                data_type: "longblob".to_string(),
            },
            MariaQueryColumn {
                name: "settings".to_string(),
                data_type: "json".to_string(),
            },
        ];
        let budget = QueryBudget {
            max_bytes: 262_144,
            max_cell_bytes: 65_536,
            ..QueryBudget::default()
        };
        let sql = with_wire_row_budget(
            "SELECT * FROM `app`.`users` LIMIT ? OFFSET ?",
            &columns,
            budget,
        )
        .expect("supported schema should build a bounded query");

        assert!(sql.contains("OCTET_LENGTH(__temps_source.`display_name`) * 6"));
        assert!(sql.contains("OCTET_LENGTH(__temps_source.`avatar`) * 5"));
        assert!(sql.contains("JSON_STORAGE_SIZE(__temps_source.`settings`) * 6"));
        assert!(sql.contains("<= 65536"));
        assert!(sql.contains("<= 262144"));
        assert!(sql.contains("THEN JSON_OBJECT("));
        assert!(sql.contains("JSON_OBJECT("));
        assert_eq!(sql.matches("JSON_OBJECT(").count(), 1);
        assert!(sql.contains("TO_BASE64(__temps_source.`avatar`)"));
        assert!(sql.contains("FROM `app`.`users` LIMIT ? OFFSET ?"));
    }

    #[test]
    fn spatial_and_bit_types_use_bounded_json_encodings() {
        let sql = with_wire_row_budget(
            "SELECT * FROM `app`.`places`",
            &[
                MariaQueryColumn {
                    name: "shape".to_string(),
                    data_type: "geometry".to_string(),
                },
                MariaQueryColumn {
                    name: "flags".to_string(),
                    data_type: "bit".to_string(),
                },
            ],
            QueryBudget::default(),
        )
        .expect("spatial and bit columns should remain browsable through bounded encodings");
        assert!(sql.contains("TO_BASE64(__temps_source.`shape`)"));
        assert!(sql.contains("CAST(__temps_source.`flags` AS UNSIGNED)"));
    }

    #[test]
    fn unknown_mariadb_types_are_rejected_before_query_execution() {
        let error = with_wire_row_budget(
            "SELECT * FROM `app`.`places`",
            &[MariaQueryColumn {
                name: "shape".to_string(),
                data_type: "unregistered_extension_type".to_string(),
            }],
            QueryBudget::default(),
        )
        .expect_err("unknown encodings cannot be safely admitted");
        assert!(matches!(error, DataError::OperationNotSupported(_)));
    }

    #[tokio::test]
    async fn real_mariadb_enforces_wire_budget_and_preserves_rows() -> anyhow::Result<()> {
        let container = match GenericImage::new("mariadb", "11.4")
            .with_exposed_port(ContainerPort::Tcp(3306))
            .with_wait_for(WaitFor::message_on_stderr("ready for connections"))
            .with_env_var("MARIADB_ROOT_PASSWORD", "test")
            .with_env_var("MARIADB_DATABASE", "app")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) if container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping Docker-dependent MariaDB budget test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let host = container.get_host().await?.to_string();
        let port = container.get_host_port_ipv4(3306).await?;
        let mut source = None;
        let mut last_connect_error = None;
        // The image readiness message can precede the host-published socket
        // becoming reachable on loaded GitHub runners. Give that final
        // network hand-off a bounded 30-second window.
        let readiness_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while tokio::time::Instant::now() < readiness_deadline {
            let remaining =
                readiness_deadline.saturating_duration_since(tokio::time::Instant::now());
            let attempt_timeout = remaining.min(std::time::Duration::from_secs(3));
            match tokio::time::timeout(
                attempt_timeout,
                MariaDbSource::connect(&host, port, "root", "test", "app"),
            )
            .await
            {
                Ok(Ok(connected)) => {
                    source = Some(connected);
                    break;
                }
                Ok(Err(error)) => {
                    last_connect_error = Some(error.to_string());
                }
                Err(_) => {
                    last_connect_error = Some(format!(
                        "connection attempt exceeded {}ms",
                        attempt_timeout.as_millis()
                    ))
                }
            }
            let remaining =
                readiness_deadline.saturating_duration_since(tokio::time::Instant::now());
            if !remaining.is_zero() {
                tokio::time::sleep(remaining.min(std::time::Duration::from_millis(500))).await;
            }
        }
        let source = source.ok_or_else(|| {
            anyhow::anyhow!(
                "MariaDB at {}:{} did not become reachable within 30s: {}",
                host,
                port,
                last_connect_error
                    .as_deref()
                    .unwrap_or("no connection error captured")
            )
        })?;
        sqlx::query("CREATE TABLE rows_budget (id BIGINT PRIMARY KEY, payload LONGTEXT NOT NULL)")
            .execute(&source.pool)
            .await?;
        sqlx::query("INSERT INTO rows_budget VALUES (1, 'safe'), (2, REPEAT('x', 100000))")
            .execute(&source.pool)
            .await?;

        let path = ContainerPath::from_slice(&["app"]);
        let safe = source
            .query(
                &path,
                "rows_budget",
                Some(serde_json::json!({"where": "id = 1"})),
                QueryOptions::default(),
            )
            .await?;
        assert_eq!(safe.rows[0]["payload"], serde_json::json!("safe"));

        let error = source
            .query(
                &path,
                "rows_budget",
                Some(serde_json::json!({"where": "id = 2"})),
                QueryOptions {
                    limit: Some(1),
                    budget: QueryBudget {
                        max_bytes: 16 * 1024,
                        max_cell_bytes: 8 * 1024,
                        ..QueryBudget::default()
                    },
                    ..QueryOptions::default()
                },
            )
            .await
            .expect_err("oversized MariaDB row must be rejected before JSON transfer");
        assert!(matches!(
            error,
            DataError::ResultLimitExceeded {
                limit_kind: "wire_cell_bytes",
                ..
            }
        ));

        sqlx::query(
            "CREATE TABLE compatibility_types (id BIGINT PRIMARY KEY, shape POINT, flags BIT(4))",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query("INSERT INTO compatibility_types VALUES (1, POINT(1, 2), B'1010')")
            .execute(&source.pool)
            .await?;
        let compatible = source
            .query(&path, "compatibility_types", None, QueryOptions::default())
            .await?;
        assert!(compatible.rows[0]["shape"]
            .as_str()
            .is_some_and(|encoded| !encoded.is_empty()));
        assert_eq!(compatible.rows[0]["flags"], serde_json::json!(10));

        Ok(())
    }

    fn assert_where_rejected(clause: &str) {
        assert!(
            validate_where_clause(clause).is_err(),
            "expected rejection: {clause}"
        );
    }

    #[test]
    fn where_clause_rejects_backslash_escaped_quote() {
        // The stripper understood only the doubled-quote escape, so a
        // backslash-escaped quote desynchronised it from the server: it read
        // the escape as the first half of a `''` pair, stayed inside the
        // string, and discarded the payload. Every later check then ran on a
        // sanitised remainder and passed.
        assert_where_rejected(r"1='\'' union select 1 from mysql.user where 'a'='a'");
        assert_where_rejected(r"name = 'a\' or 1=1 -- '");
        // An unterminated literal is equally untrustworthy.
        assert_where_rejected("name = 'unterminated");
    }

    #[test]
    fn where_clause_rejects_function_calls_with_space_before_paren() {
        // The local check looked only at the byte immediately before `(`, so a
        // single space defeated it. MySQL accepts `sleep (5)` for every
        // function that isn't also a parser keyword.
        assert_where_rejected("1=1 or sleep (5)");
        assert_where_rejected("extractvalue (1, user ())");
        assert_where_rejected("updatexml (1, concat (0x7e, version ()), 1)");
    }

    #[test]
    fn where_clause_rejects_non_select_query_expressions() {
        // MySQL 8.0.19+ accepts TABLE and VALUES in subquery position, and
        // this backend serves MySQL images too.
        assert_where_rejected("id in (table mysql.user)");
        assert_where_rejected("id in (values row(1))");
        assert_where_rejected("id = (with x as (select 1) select * from x)");
    }

    #[test]
    fn where_clause_rejects_subqueries() {
        // This validator was a pure denylist, unlike the Postgres one. Without
        // structural checks these gave blind extraction of any table the
        // connection user could read.
        assert_where_rejected("1 = (SELECT LENGTH(authentication_string) FROM mysql.user LIMIT 1)");
        assert_where_rejected("id IN (SELECT user_id FROM admin_users)");
        assert_where_rejected("EXISTS ( SELECT 1 FROM mysql.user )");
    }

    #[test]
    fn where_clause_rejects_function_calls() {
        // Error-based extraction returned the value straight back in the 400
        // body, which is faster than blind and equally unblocked before.
        assert_where_rejected(
            "extractvalue(1, concat(0x7e, (select authentication_string from mysql.user limit 1)))",
        );
        assert_where_rejected("updatexml(1,concat(0x7e,version()),1)");
        assert_where_rejected("length(password) > 1");
    }

    #[test]
    fn where_clause_rejects_union_with_exotic_whitespace() {
        // The denylist enumerated "union ", "union\t", "union\n" only.
        assert_where_rejected("false UNION\rSELECT user, authentication_string FROM mysql.user");
        assert_where_rejected("false UNION\u{000C}SELECT 1");
        assert_where_rejected("1=1 INTERSECT\rSELECT 1");
    }

    #[test]
    fn where_clause_rejects_backtick_quoted_function_calls() {
        // A backtick defeated BOTH layers at once. The shared structural check
        // recognises a function call by a quote character (`"`/`'`) before `(`
        // or by walking back over identifier characters; a backtick is neither,
        // so it saw an empty identifier and allowed the call. The denylist
        // missed it independently because the text contains "sleep`(" rather
        // than the "sleep(" it matches on. `benchmark` in particular is a CPU
        // burn on the operator's own database, reachable from `--filter` and so
        // prompt-injectable.
        assert_where_rejected("1=1 and `sleep`(5)");
        assert_where_rejected("1=1 and `benchmark`(50000000, sha1('x'))");
        assert_where_rejected("`extractvalue`(1, `user`())");
        // Qualified and spaced variants of the same trick.
        assert_where_rejected("1=1 and `mysql`.`sleep` (5)");
        // An unterminated identifier is as untrustworthy as an unterminated
        // string: we cannot know where the server thinks it ends.
        assert_where_rejected("`unterminated");
    }

    #[test]
    fn where_clause_rejects_double_quotes() {
        // Under default sql_mode `"..."` is a string literal; under ANSI_QUOTES
        // it is an identifier. The stripper cannot observe the mode, and the
        // two readings disagree about where the span ends — the same
        // stripper/server desync the backslash rule above exists to prevent,
        // reached through a different quote character.
        assert_where_rejected(r#"name = "value""#);
        assert_where_rejected(r#"1=1 or "it's" = 'x'"#);
    }

    #[test]
    fn where_clause_rejects_regex_and_system_variables() {
        // The regex pattern hides inside a string literal, so the stripper
        // removes it before any keyword check runs. MariaDB's PCRE backtracks.
        assert_where_rejected("1=1 and 'aaaaaaaaaaaaaaaaaaaaaaaa' regexp '(a+)+b'");
        assert_where_rejected("name rlike '(a+)+b'");
        // System variables need no parens, so the function-call guard never
        // sees them, and they leak server configuration a filter away.
        assert_where_rejected("1=1 and @@datadir like 'a%'");
        assert_where_rejected("@@version like '8%'");
        assert_where_rejected("@@secure_file_priv is null");
        assert_where_rejected("current_user like 'root%'");
    }

    #[test]
    fn where_clause_still_accepts_ordinary_filters() {
        // The structural checks must not break the filters this exists to run.
        // Grouping parens are preceded by whitespace or an operator, so they
        // are not mistaken for function calls.
        for clause in [
            "plan = 'pro'",
            "id > 5 AND status = 'active'",
            "(plan = 'pro' OR plan = 'scale') AND amount_cents > 100",
            "name LIKE '%test%'",
            "id IN (1, 2, 3)",
            "created_at BETWEEN '2025-01-01' AND '2025-02-01'",
            // Backtick-quoted identifiers are ordinary MySQL and must survive
            // normalisation — including reserved words, which are the whole
            // reason the quoting exists.
            "`order` = 1",
            "`user`.`status` = 'active' AND `order` > 2",
            // Columns whose names merely contain a denylisted function name.
            // Matching bare "sleep"/"benchmark" would reject these.
            "sleep_minutes > 30",
            "benchmark_id = 7",
            // Substring matching on the denylist rejected both of these:
            // `payload` contains `load `, `charset` contains `set `. Very
            // ordinary column names, and the kind of false positive that gets a
            // validator switched off rather than fixed.
            "payload = 'x'",
            "charset = 'utf8'",
            "download_count > 3",
            "offset_seconds = 1",
        ] {
            assert!(
                validate_where_clause(clause).is_ok(),
                "should have been accepted: {clause}"
            );
        }
    }

    #[test]
    fn maps_mysql_types() {
        assert_eq!(MariaDbSource::map_mysql_type("int"), FieldType::Int32);
        assert_eq!(MariaDbSource::map_mysql_type("bigint"), FieldType::Int64);
        assert_eq!(MariaDbSource::map_mysql_type("varchar"), FieldType::String);
        assert_eq!(
            MariaDbSource::map_mysql_type("datetime"),
            FieldType::Timestamp
        );
        assert_eq!(MariaDbSource::map_mysql_type("json"), FieldType::Json);
        assert_eq!(MariaDbSource::map_mysql_type("blob"), FieldType::Bytes);
    }

    #[test]
    fn validates_identifiers() {
        assert!(validate_identifier("database", "app_prod").is_ok());
        assert!(validate_identifier("database", "1bad").is_err());
        assert!(validate_identifier("database", "bad-name").is_err());
        assert!(validate_identifier("database", "bad`name").is_err());
    }

    #[test]
    fn validates_where_clause() {
        assert!(validate_where_clause("status = 'active' AND age >= 18").is_ok());
        assert!(validate_where_clause("id IN (1, 2, 3)").is_ok());
        assert!(validate_where_clause("name LIKE '%drop table%'").is_ok());
        assert!(validate_where_clause("1=1; DROP TABLE users").is_err());
        assert!(validate_where_clause("id = 1 UNION SELECT password FROM users").is_err());
        assert!(validate_where_clause("name = 'x' -- comment").is_err());
    }

    #[test]
    fn detects_mariadb_compatible_images() {
        assert!(is_mariadb_compatible_image("mariadb:lts"));
        assert!(is_mariadb_compatible_image("library/mysql:8.4"));
        assert!(is_mariadb_compatible_image("mysql:8"));
        assert!(!is_mariadb_compatible_image("postgres:18"));
    }
}
