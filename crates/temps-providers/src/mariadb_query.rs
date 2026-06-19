use async_trait::async_trait;
use base64::Engine;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column, Row, TypeInfo};
use std::collections::HashMap;
use temps_query::{
    Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType, DataError,
    DataRow, DataSource, DatasetSchema, EntityCountHint, EntityInfo, FieldDef, FieldType,
    Introspect, QueryOptions, QueryResult, QuerySchemaProvider, QueryStats, Queryable, Result,
};
use tracing::{debug, error};

pub struct MariaDbSource {
    pool: MySqlPool,
    database_name: String,
}

impl MariaDbSource {
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

    fn row_to_datarow(row: &MySqlRow) -> Result<DataRow> {
        let mut data_row = HashMap::new();
        for (idx, column) in row.columns().iter().enumerate() {
            let value = Self::extract_value(row, idx)?;
            data_row.insert(column.name().to_string(), value);
        }
        Ok(data_row)
    }

    fn extract_value(row: &MySqlRow, idx: usize) -> Result<serde_json::Value> {
        let column = &row.columns()[idx];
        let type_name = column.type_info().name().to_ascii_lowercase();

        let value = match type_name.as_str() {
            "bool" | "boolean" => row
                .try_get::<Option<bool>, _>(idx)
                .ok()
                .flatten()
                .map(serde_json::Value::Bool)
                .unwrap_or(serde_json::Value::Null),
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "year" => row
                .try_get::<Option<i32>, _>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::Number(v.into()))
                .unwrap_or(serde_json::Value::Null),
            "bigint" => row
                .try_get::<Option<i64>, _>(idx)
                .ok()
                .flatten()
                .map(|v| serde_json::Value::Number(v.into()))
                .or_else(|| {
                    row.try_get::<Option<u64>, _>(idx)
                        .ok()
                        .flatten()
                        .map(|v| serde_json::Value::Number(v.into()))
                })
                .unwrap_or(serde_json::Value::Null),
            "float" => row
                .try_get::<Option<f32>, _>(idx)
                .ok()
                .flatten()
                .and_then(|v| serde_json::Number::from_f64(v as f64))
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            "double" | "real" => row
                .try_get::<Option<f64>, _>(idx)
                .ok()
                .flatten()
                .and_then(serde_json::Number::from_f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            "json" => row
                .try_get::<Option<String>, _>(idx)
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or(serde_json::Value::Null),
            "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => row
                .try_get::<Option<Vec<u8>>, _>(idx)
                .ok()
                .flatten()
                .map(|v| {
                    serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(v))
                })
                .unwrap_or(serde_json::Value::Null),
            _ => row
                .try_get::<Option<String>, _>(idx)
                .ok()
                .flatten()
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null),
        };

        Ok(value)
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

        let row_count = self.count(container_path, entity_name, None).await.ok();

        Ok(EntityInfo {
            namespace: container_path.segments[0].clone(),
            name: entity_name.to_string(),
            entity_type: "table".to_string(),
            row_count: row_count.and_then(|v| usize::try_from(v).ok()),
            size_bytes: None,
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

        debug!("Executing MariaDB query: {}", sql);

        let rows = sqlx::query(&sql)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                error!("MariaDB query failed: {}", e);
                DataError::QueryFailed(format!("{}\n\nQuery: {}", e, sql))
            })?;

        let data_rows: Result<Vec<DataRow>> = rows.iter().map(Self::row_to_datarow).collect();
        let data_rows = data_rows?;
        let schema = self.get_schema(container_path, entity_name).await?;
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

        let row = sqlx::query(&sql)
            .fetch_one(&self.pool)
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
    format!("`{}`", value)
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

fn strip_sql_string_literals(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut in_string = false;
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        if in_string {
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

    result
}

fn validate_where_clause(sql: &str) -> Result<()> {
    let sql_lower = sql.trim().to_ascii_lowercase();

    if sql_lower.is_empty() {
        return Err(DataError::InvalidQuery(
            "WHERE clause cannot be empty".to_string(),
        ));
    }

    let without_strings = strip_sql_string_literals(&sql_lower);

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
        "union ",
        "union\t",
        "union\n",
        "intersect ",
        "except ",
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

    for keyword in &dangerous_keywords {
        if without_strings.contains(keyword) {
            return Err(DataError::InvalidQuery(format!(
                "SQL operation '{}' is not allowed in the data browser",
                keyword.trim()
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
