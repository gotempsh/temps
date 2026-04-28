//! PostgreSQL driver for temps-query
//!
//! Implements DataSource, Introspect, and Queryable traits for PostgreSQL.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use temps_query::{
    Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType, DataError,
    DataRow, DataSource, DatasetSchema, EntityCountHint, EntityInfo, FieldDef, FieldType,
    Introspect, QueryOptions, QueryResult, QueryStats, Queryable, Result,
};
use tokio_postgres::{Client, NoTls, Row};
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::{debug, error, warn};

/// Escape a SQL identifier by doubling any internal double-quote characters.
/// Prevents identifier injection when used inside `"..."` quoting.
fn escape_ident(name: &str) -> String {
    name.replace('"', "\"\"")
}

/// A certificate verifier that accepts all server certificates (including self-signed).
/// Used for connecting to PostgreSQL clusters with `--ssl-self-signed` certificates.
#[derive(Debug)]
struct AcceptAllVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAllVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// PostgreSQL data source implementation
pub struct PostgresSource {
    client: Arc<Client>,
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

        let client = match Self::connect_with_tls(&config).await {
            Ok(client) => {
                debug!("Connected to PostgreSQL with TLS");
                client
            }
            Err(tls_err) => {
                warn!(
                    "TLS connection failed, falling back to plain connection: {}",
                    format_chain(&tls_err)
                );
                let (client, connection) =
                    tokio_postgres::connect(&config, NoTls).await.map_err(|e| {
                        DataError::ConnectionFailed(format!(
                            "PostgreSQL connection failed (TLS error: {}, plain error: {})",
                            format_chain(&tls_err),
                            format_chain(&e),
                        ))
                    })?;

                tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        error!("PostgreSQL connection error: {}", e);
                    }
                });

                client
            }
        };

        debug!(
            "Successfully connected to PostgreSQL database: {}",
            database
        );

        Ok(Self {
            client: Arc::new(client),
            database_name: database.to_string(),
        })
    }

    /// Execute a raw SQL statement (no result rows expected).
    /// Used for DDL/admin operations like creating roles and granting privileges.
    pub async fn execute_raw(&self, sql: &str) -> Result<()> {
        self.client
            .batch_execute(sql)
            .await
            .map_err(|e| DataError::QueryFailed(format!("Execute failed: {}", e)))?;
        Ok(())
    }

    /// Attempt a TLS connection using rustls configured to accept self-signed certificates.
    /// Returns the Client after spawning the connection task.
    async fn connect_with_tls(config: &str) -> std::result::Result<Client, tokio_postgres::Error> {
        connect_with_self_signed_tls(config).await
    }
}

/// Open a `tokio_postgres::Client` against a libpq-style connection string,
/// negotiating TLS with rustls + a verifier that accepts any server cert
/// (self-signed included). The spawned connection task is detached and
/// runs until the returned `Client` is dropped.
///
/// Public so probes (cluster health-checks, `pg_auto_failover` monitor
/// reads, etc.) outside this crate can reuse the same TLS posture without
/// re-implementing the verifier or wrestling with `MakeRustlsConnect`
/// directly.
/// Walk an error's `source()` chain and join messages with `: `.
/// `tokio_postgres::Error` displays only `"db error"` at the top level
/// — the actual reason (e.g. "password authentication failed",
/// "channel binding required") is one or two layers deeper.
fn format_chain<E: std::error::Error>(err: &E) -> String {
    let mut out = err.to_string();
    let mut cause: Option<&dyn std::error::Error> = err.source();
    while let Some(c) = cause {
        let s = c.to_string();
        if !s.is_empty() && !out.ends_with(&s) {
            out.push_str(": ");
            out.push_str(&s);
        }
        cause = c.source();
    }
    out
}

pub async fn connect_with_self_signed_tls(
    config: &str,
) -> std::result::Result<Client, tokio_postgres::Error> {
    let rustls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAllVerifier))
        .with_no_client_auth();

    let tls = MakeRustlsConnect::new(rustls_config);
    let (client, connection) = tokio_postgres::connect(config, tls).await?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("PostgreSQL TLS connection error: {}", e);
        }
    });

    Ok(client)
}

impl PostgresSource {
    /// Strip SQL string literals to avoid false positives when scanning for dangerous patterns.
    /// Replaces content inside single-quoted strings with empty strings.
    fn strip_sql_string_literals(sql: &str) -> String {
        let mut result = String::with_capacity(sql.len());
        let mut in_string = false;
        let mut chars = sql.chars().peekable();

        while let Some(c) = chars.next() {
            if in_string {
                if c == '\'' {
                    // Check for escaped quote ('')
                    if chars.peek() == Some(&'\'') {
                        chars.next(); // skip the escaped quote
                    } else {
                        in_string = false;
                        result.push('\'');
                    }
                }
                // Skip characters inside string literals
            } else if c == '\'' {
                in_string = true;
                result.push('\'');
            } else {
                result.push(c);
            }
        }

        result
    }

    /// Validate that a sort_by field name is a safe SQL identifier.
    /// Only allows alphanumeric characters, underscores, and optionally
    /// double-quoted identifiers.
    fn validate_sort_field(sort_by: &str) -> Result<()> {
        let trimmed = sort_by.trim();
        if trimmed.is_empty() {
            return Err(DataError::InvalidQuery(
                "Sort field cannot be empty".to_string(),
            ));
        }

        // Allow double-quoted identifiers
        if trimmed.starts_with('"') && trimmed.ends_with('"') {
            let inner = &trimmed[1..trimmed.len() - 1];
            if inner.contains('"') {
                return Err(DataError::InvalidQuery(
                    "Sort field identifier contains invalid characters".to_string(),
                ));
            }
            return Ok(());
        }

        // Allow only valid unquoted SQL identifiers: [a-zA-Z_][a-zA-Z0-9_]*
        // Also allow schema.column format
        for part in trimmed.split('.') {
            if part.is_empty() {
                return Err(DataError::InvalidQuery(
                    "Sort field contains empty path segment".to_string(),
                ));
            }
            if !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(DataError::InvalidQuery(format!(
                    "Sort field '{}' contains invalid characters. Only alphanumeric characters, underscores, and dots are allowed",
                    sort_by
                )));
            }
        }

        Ok(())
    }

    /// Validate SQL input for dangerous operations.
    /// Used to sanitize user-provided WHERE clauses in the data browser.
    ///
    /// Security: Uses both a denylist of dangerous patterns AND structural
    /// validation to prevent SQL injection. The denylist catches known attack
    /// patterns while structural checks block injection vectors like subqueries,
    /// UNION, and function calls that could bypass simple pattern matching.
    fn validate_sql(sql: &str) -> Result<()> {
        let sql_lower = sql.trim().to_lowercase();

        if sql_lower.is_empty() {
            return Err(DataError::InvalidQuery(
                "WHERE clause cannot be empty".to_string(),
            ));
        }

        // Strip string literals to avoid false positives on content inside quotes
        let without_strings = Self::strip_sql_string_literals(&sql_lower);

        // STRUCTURAL CHECKS: Block injection vectors that denylist alone cannot catch

        // Prevent multi-statement execution via semicolons
        if without_strings.contains(';') {
            return Err(DataError::InvalidQuery(
                "Multiple SQL statements are not allowed".to_string(),
            ));
        }

        // Prevent subqueries via parenthesized SELECT
        // This blocks: (SELECT ...), EXISTS (SELECT ...), IN (SELECT ...)
        if without_strings.contains('(') {
            // Allow simple IN lists like: id IN (1, 2, 3) but block any subqueries
            // by checking if SELECT appears after any opening paren
            let paren_content_has_select = without_strings.match_indices('(').any(|(idx, _)| {
                let after_paren = &without_strings[idx..];
                // Check if there's a SELECT between this ( and its matching )
                after_paren
                    .find(')')
                    .map(|close_idx| {
                        let inner = &after_paren[1..close_idx];
                        inner.contains("select")
                    })
                    .unwrap_or(false)
            });
            if paren_content_has_select {
                return Err(DataError::InvalidQuery(
                    "Subqueries are not allowed in the data browser".to_string(),
                ));
            }
        }

        // Block SQL comments which can be used to hide attack payloads
        if without_strings.contains("--") || without_strings.contains("/*") {
            return Err(DataError::InvalidQuery(
                "SQL comments are not allowed in the data browser".to_string(),
            ));
        }

        // DENYLIST: Block dangerous SQL keywords and operations
        // These are checked against the string-stripped version to prevent
        // hiding keywords inside string literals
        let dangerous_keywords = [
            // DDL operations
            "drop ",
            "truncate ",
            "alter ",
            "create ",
            "grant ",
            "revoke ",
            // Data manipulation that shouldn't appear in WHERE
            "insert ",
            "update ",
            "delete ",
            "copy ",
            // Set operations that enable data exfiltration
            "union ",
            "union\t",
            "union\n",
            "intersect ",
            "except ",
            // Dangerous PostgreSQL functions
            "pg_read_file",
            "pg_write_file",
            "pg_ls_dir",
            "pg_read_binary_file",
            "pg_stat_file",
            "lo_import",
            "lo_export",
            "lo_get",
            "lo_put",
            "pg_sleep",
            "pg_terminate_backend",
            "pg_cancel_backend",
            "pg_reload_conf",
            "pg_rotate_logfile",
            "set_config",
            "current_setting",
            "dblink",
            "dblink_connect",
            "dblink_exec",
            // Information disclosure functions
            "pg_ls_logdir",
            "pg_ls_waldir",
            "pg_ls_tmpdir",
            "pg_ls_archive_statusdir",
            // Execute/prepare
            "execute ",
            "prepare ",
            // Transaction control
            "begin ",
            "commit ",
            "rollback ",
            "savepoint ",
            // INTO clause (write results to table/file)
            " into ",
        ];

        for keyword in &dangerous_keywords {
            if without_strings.contains(keyword) {
                return Err(DataError::InvalidQuery(format!(
                    "SQL operation '{}' is not allowed in the data browser",
                    keyword.trim()
                )));
            }
        }

        // Also check for dangerous keywords at the very start of the string
        let dangerous_starts = ["into "];
        for keyword in &dangerous_starts {
            if without_strings.starts_with(keyword) {
                return Err(DataError::InvalidQuery(format!(
                    "SQL operation '{}' is not allowed in the data browser",
                    keyword.trim()
                )));
            }
        }

        Ok(())
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
                .map(serde_json::Value::Bool)
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
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),

            "float8" => row
                .try_get::<_, Option<f64>>(idx)
                .ok()
                .flatten()
                .and_then(serde_json::Number::from_f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),

            "varchar" | "text" | "char" | "bpchar" => row
                .try_get::<_, Option<String>>(idx)
                .ok()
                .flatten()
                .map(serde_json::Value::String)
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
                    .map(serde_json::Value::String)
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
        let client = &self.client;

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
                                entity_count_hint: None,
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
                                entity_count_hint: Some(EntityCountHint::Small),
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
        let client = &self.client;

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
                        entity_count_hint: None,
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
                        entity_count_hint: Some(EntityCountHint::Small),
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

        let client = &self.client;

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
                    metadata: None,
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

        let client = &self.client;

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
            escape_ident(schema_name),
            escape_ident(entity_name)
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
            metadata: None,
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

        let client = &self.client;

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
        let client = &self.client;

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
        let client = &self.client;

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

        let start = std::time::Instant::now();

        // Build SQL query
        let mut sql = format!(
            "SELECT * FROM \"{}\".\"{}\"",
            escape_ident(schema_name),
            escape_ident(entity_name)
        );

        // Add WHERE clause if filters provided
        if let Some(filter_json) = filters {
            if let Some(where_clause) = filter_json.get("where").and_then(|v| v.as_str()) {
                // Validate WHERE clause for dangerous operations
                Self::validate_sql(where_clause)?;
                sql.push_str(" WHERE ");
                sql.push_str(where_clause);
            }
        }

        // Add ORDER BY
        if let Some(sort_by) = &options.sort_by {
            Self::validate_sort_field(sort_by)?;
            let sort_order = match options.sort_order.as_deref() {
                Some("desc") | Some("DESC") => "DESC",
                _ => "ASC",
            };
            // Quote the column name to handle camelCase identifiers correctly
            let quoted_sort = if sort_by.starts_with('"') && sort_by.ends_with('"') {
                sort_by.to_string() // Already quoted
            } else {
                format!("\"{}\"", sort_by)
            };
            sql.push_str(&format!(" ORDER BY {} {}", quoted_sort, sort_order));
        }

        // Add LIMIT and OFFSET
        let limit = options.limit.unwrap_or(100);
        let offset = options.offset.unwrap_or(0);
        sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

        debug!("Executing query: {}", sql);

        // Safety: SQL injection is prevented by validate_sql() for WHERE clauses
        // and escape_ident() for identifiers. The database user should be read-only
        // as defense-in-depth.
        let client = &self.client;

        let rows = client.query(&sql, &[]).await.map_err(|e| {
            error!("PostgreSQL query failed: {}", e);
            error!("Failed SQL: {}", sql);

            // Extract detailed error message from PostgreSQL error
            let error_msg = if let Some(db_error) = e.as_db_error() {
                // Build detailed error message from PostgreSQL error fields
                let mut msg = db_error.message().to_string();

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

        let mut sql = format!(
            "SELECT COUNT(*) FROM \"{}\".\"{}\"",
            escape_ident(schema_name),
            escape_ident(entity_name)
        );

        // Add WHERE clause if filters provided
        if let Some(filter_json) = filters {
            if let Some(where_clause) = filter_json.get("where").and_then(|v| v.as_str()) {
                // Validate WHERE clause for dangerous operations
                Self::validate_sql(where_clause)?;
                sql.push_str(" WHERE ");
                sql.push_str(where_clause);
            }
        }

        let client = &self.client;

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
        let client = &self.client;

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

    // ── SQL Injection Prevention Tests ────────────────────────────────

    // Helper: assert that a WHERE clause is rejected
    fn assert_sql_rejected(sql: &str) {
        let result = PostgresSource::validate_sql(sql);
        assert!(
            result.is_err(),
            "Expected SQL to be rejected but it was accepted: {:?}",
            sql
        );
    }

    // Helper: assert that a WHERE clause is allowed
    fn assert_sql_allowed(sql: &str) {
        let result = PostgresSource::validate_sql(sql);
        assert!(
            result.is_ok(),
            "Expected SQL to be accepted but it was rejected: {:?} — error: {:?}",
            sql,
            result.unwrap_err()
        );
    }

    // ── Legitimate WHERE clauses that MUST be allowed ────────────────

    #[test]
    fn test_sql_valid_simple_equality() {
        assert_sql_allowed("status = 'active'");
    }

    #[test]
    fn test_sql_valid_comparison_operators() {
        assert_sql_allowed("age >= 18 AND country = 'US'");
        assert_sql_allowed("created_at > '2025-01-01'");
        assert_sql_allowed("price < 100.50");
    }

    #[test]
    fn test_sql_valid_like_pattern() {
        assert_sql_allowed("name LIKE '%test%'");
        assert_sql_allowed("email ILIKE '%@example.com'");
    }

    #[test]
    fn test_sql_valid_in_list() {
        assert_sql_allowed("id IN (1, 2, 3)");
        assert_sql_allowed("status IN ('active', 'pending')");
    }

    #[test]
    fn test_sql_valid_is_null() {
        assert_sql_allowed("deleted_at IS NULL");
        assert_sql_allowed("name IS NOT NULL");
    }

    #[test]
    fn test_sql_valid_between() {
        assert_sql_allowed("created_at BETWEEN '2025-01-01' AND '2025-12-31'");
    }

    #[test]
    fn test_sql_valid_boolean_logic() {
        assert_sql_allowed("active = true AND (role = 'admin' OR role = 'user')");
    }

    // ── SQL Injection attacks that MUST be blocked ───────────────────

    #[test]
    fn test_sql_injection_semicolon_multi_statement() {
        assert_sql_rejected("1=1; DROP TABLE users");
        assert_sql_rejected("status = 'active'; DELETE FROM sessions");
    }

    #[test]
    fn test_sql_injection_union_select_data_exfiltration() {
        assert_sql_rejected("1=1 UNION SELECT * FROM users");
        assert_sql_rejected("1=1 union select password from users");
        assert_sql_rejected("id = 1 UNION\tSELECT * FROM secrets");
    }

    #[test]
    fn test_sql_injection_subquery_in_where() {
        assert_sql_rejected("id = (SELECT id FROM users LIMIT 1)");
        assert_sql_rejected("name = (select password from users limit 1)");
    }

    #[test]
    fn test_sql_injection_exists_subquery() {
        // EXISTS with subquery should be blocked by the subquery detection
        assert_sql_rejected("EXISTS (SELECT 1 FROM users WHERE admin = true)");
    }

    #[test]
    fn test_sql_injection_in_subquery() {
        assert_sql_rejected("id IN (SELECT user_id FROM admin_users)");
    }

    #[test]
    fn test_sql_injection_drop_table() {
        assert_sql_rejected("1=1; DROP TABLE users");
        assert_sql_rejected("drop table users");
    }

    #[test]
    fn test_sql_injection_truncate() {
        assert_sql_rejected("1=1; truncate table sessions");
    }

    #[test]
    fn test_sql_injection_alter_table() {
        assert_sql_rejected("alter table users add column backdoor text");
    }

    #[test]
    fn test_sql_injection_create() {
        assert_sql_rejected("1=1; create table evil (data text)");
    }

    #[test]
    fn test_sql_injection_grant_revoke() {
        assert_sql_rejected("grant all on users to evil");
        assert_sql_rejected("revoke select on users from public");
    }

    #[test]
    fn test_sql_injection_insert_update_delete() {
        assert_sql_rejected("1=1; insert into users (email) values ('evil@hack.com')");
        assert_sql_rejected("1=1; update users set role = 'admin'");
        assert_sql_rejected("1=1; delete from sessions");
    }

    #[test]
    fn test_sql_injection_pg_sleep_timing_attack() {
        assert_sql_rejected("pg_sleep(10)");
        assert_sql_rejected("1=1 AND pg_sleep(5) IS NOT NULL");
    }

    #[test]
    fn test_sql_injection_pg_file_read() {
        assert_sql_rejected("pg_read_file('/etc/passwd')");
        assert_sql_rejected("pg_read_binary_file('/etc/shadow')");
        assert_sql_rejected("pg_write_file('/tmp/evil', 'data')");
    }

    #[test]
    fn test_sql_injection_pg_ls_dir() {
        assert_sql_rejected("pg_ls_dir('/etc')");
        assert_sql_rejected("pg_ls_logdir()");
        assert_sql_rejected("pg_ls_waldir()");
    }

    #[test]
    fn test_sql_injection_lo_import_export() {
        assert_sql_rejected("lo_import('/etc/passwd')");
        assert_sql_rejected("lo_export(1234, '/tmp/data')");
    }

    #[test]
    fn test_sql_injection_terminate_backend() {
        assert_sql_rejected("pg_terminate_backend(1234)");
        assert_sql_rejected("pg_cancel_backend(1234)");
    }

    #[test]
    fn test_sql_injection_dblink() {
        assert_sql_rejected("dblink('host=evil.com', 'SELECT * FROM users')");
        assert_sql_rejected("dblink_connect('evil_conn', 'host=evil.com')");
        assert_sql_rejected("dblink_exec('evil_conn', 'DROP TABLE users')");
    }

    #[test]
    fn test_sql_injection_set_config() {
        assert_sql_rejected("set_config('log_statement', 'all', false)");
    }

    #[test]
    fn test_sql_injection_copy() {
        assert_sql_rejected("1=1; copy users to '/tmp/dump'");
    }

    #[test]
    fn test_sql_injection_comment_hiding() {
        assert_sql_rejected("1=1 -- AND admin = false");
        assert_sql_rejected("1=1 /* hidden payload */");
    }

    #[test]
    fn test_sql_injection_into_clause() {
        assert_sql_rejected("1=1 into outfile '/tmp/data'");
    }

    #[test]
    fn test_sql_injection_execute_prepare() {
        assert_sql_rejected("execute evil_plan");
        assert_sql_rejected("prepare evil_plan as select * from users");
    }

    #[test]
    fn test_sql_injection_transaction_control() {
        assert_sql_rejected("begin ; drop table users");
        assert_sql_rejected("commit ; drop table users");
        assert_sql_rejected("rollback ; drop table users");
    }

    #[test]
    fn test_sql_injection_intersect_except() {
        assert_sql_rejected("1=1 intersect select * from admin_users");
        assert_sql_rejected("1=1 except select * from restricted");
    }

    #[test]
    fn test_sql_injection_empty_where() {
        assert_sql_rejected("");
        assert_sql_rejected("   ");
    }

    #[test]
    fn test_sql_injection_keyword_inside_string_literal_allowed() {
        // The word "drop" inside a string literal should NOT trigger rejection
        // because strip_sql_string_literals removes string content before checking
        assert_sql_allowed("description = 'drop this item'");
        assert_sql_allowed("name = 'select the best option'");
        assert_sql_allowed("note = 'please delete me'");
    }

    // ── Sort field validation tests ──────────────────────────────────

    #[test]
    fn test_sort_field_valid_simple() {
        assert!(PostgresSource::validate_sort_field("created_at").is_ok());
        assert!(PostgresSource::validate_sort_field("id").is_ok());
        assert!(PostgresSource::validate_sort_field("user_name").is_ok());
    }

    #[test]
    fn test_sort_field_valid_quoted() {
        assert!(PostgresSource::validate_sort_field("\"created_at\"").is_ok());
    }

    #[test]
    fn test_sort_field_valid_schema_qualified() {
        assert!(PostgresSource::validate_sort_field("schema.column").is_ok());
    }

    #[test]
    fn test_sort_field_injection_blocked() {
        assert!(PostgresSource::validate_sort_field("id; DROP TABLE users--").is_err());
        assert!(PostgresSource::validate_sort_field("").is_err());
        assert!(PostgresSource::validate_sort_field("id OR 1=1").is_err());
    }

    #[test]
    fn test_sort_field_quoted_injection_blocked() {
        // Double quotes inside a quoted identifier should be rejected
        assert!(PostgresSource::validate_sort_field("\"id\"; DROP TABLE users--\"").is_err());
    }
}
