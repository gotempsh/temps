use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Capabilities supported by a data source
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum Capability {
    /// SQL-based queries (Postgres, MySQL, etc.)
    Sql,
    /// Document-based (MongoDB, etc.)
    Document,
    /// Key-value pairs (Redis, etc.)
    KeyValue,
    /// Object storage (S3, etc.)
    ObjectStore,
    /// Time-series data
    TimeSeries,
    /// Text search
    TextSearch,
    /// Graph queries
    Graph,
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Capability::Sql => write!(f, "sql"),
            Capability::Document => write!(f, "document"),
            Capability::KeyValue => write!(f, "key-value"),
            Capability::ObjectStore => write!(f, "object-store"),
            Capability::TimeSeries => write!(f, "time-series"),
            Capability::TextSearch => write!(f, "text-search"),
            Capability::Graph => write!(f, "graph"),
        }
    }
}

/// Field data types supported by query results
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    /// NULL value
    Null,
    /// Boolean true/false
    Boolean,
    /// 32-bit signed integer
    Int32,
    /// 64-bit signed integer
    Int64,
    /// 32-bit floating point
    Float32,
    /// 64-bit floating point
    Float64,
    /// UTF-8 string
    String,
    /// Binary data
    Bytes,
    /// ISO 8601 date
    Date,
    /// ISO 8601 timestamp
    Timestamp,
    /// JSON object
    Json,
    /// UUID
    Uuid,
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldType::Null => write!(f, "null"),
            FieldType::Boolean => write!(f, "boolean"),
            FieldType::Int32 => write!(f, "int32"),
            FieldType::Int64 => write!(f, "int64"),
            FieldType::Float32 => write!(f, "float32"),
            FieldType::Float64 => write!(f, "float64"),
            FieldType::String => write!(f, "string"),
            FieldType::Bytes => write!(f, "bytes"),
            FieldType::Date => write!(f, "date"),
            FieldType::Timestamp => write!(f, "timestamp"),
            FieldType::Json => write!(f, "json"),
            FieldType::Uuid => write!(f, "uuid"),
        }
    }
}

/// Definition of a single field in a dataset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    /// Field name
    pub name: String,
    /// Field type
    pub field_type: FieldType,
    /// Whether field is nullable
    pub nullable: bool,
    /// Optional description
    pub description: Option<String>,
}

/// Schema of a dataset or entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetSchema {
    /// Field definitions
    pub fields: Vec<FieldDef>,
    /// Optional partition keys
    pub partitions: Option<Vec<String>>,
    /// Optional primary key
    pub primary_key: Option<Vec<String>>,
}

/// Reference to a namespace (database, schema, keyspace, etc.)
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NamespaceRef {
    pub name: String,
}

impl NamespaceRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// Reference to an entity (table, collection, key pattern, etc.)
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct EntityRef {
    pub namespace: NamespaceRef,
    pub name: String,
}

impl EntityRef {
    pub fn new(namespace: NamespaceRef, name: impl Into<String>) -> Self {
        Self {
            namespace,
            name: name.into(),
        }
    }

    /// Create entity with string namespace
    pub fn from_parts(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: NamespaceRef::new(namespace),
            name: name.into(),
        }
    }
}

impl fmt::Display for EntityRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.namespace.name, self.name)
    }
}

/// Query options for controlling result behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOptions {
    /// Maximum rows to return
    pub limit: Option<usize>,
    /// Number of rows to skip
    pub offset: Option<usize>,
    /// Cursor for pagination (backend-specific format)
    pub cursor: Option<String>,
    /// Sort by field (JSON pointer format: "/field_name")
    pub sort_by: Option<String>,
    /// Sort order: "asc" or "desc"
    pub sort_order: Option<String>,
    /// Timeout in milliseconds
    pub timeout_ms: Option<u64>,
    /// Include null values
    pub include_nulls: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            limit: Some(100),
            offset: Some(0),
            cursor: None,
            sort_by: None,
            sort_order: Some("asc".to_string()),
            timeout_ms: Some(30000),
            include_nulls: true,
        }
    }
}

/// A row of data as key-value pairs
pub type DataRow = HashMap<String, serde_json::Value>;

/// Statistics about query execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryStats {
    /// Number of rows returned
    pub row_count: usize,
    /// Total rows available (if known)
    pub total_rows: Option<usize>,
    /// Query execution time in milliseconds
    pub execution_ms: u64,
    /// Whether there are more results
    pub has_more: bool,
    /// Next cursor for pagination (if applicable)
    pub next_cursor: Option<String>,
}

/// Result of executing a query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    /// Schema of returned data
    pub schema: DatasetSchema,
    /// Rows of data
    pub rows: Vec<DataRow>,
    /// Execution statistics
    pub stats: QueryStats,
}

impl QueryResult {
    /// Create a new query result
    pub fn new(schema: DatasetSchema, rows: Vec<DataRow>, execution_ms: u64) -> Self {
        let row_count = rows.len();
        let has_more = false;

        Self {
            schema,
            rows,
            stats: QueryStats {
                row_count,
                total_rows: Some(row_count as usize),
                execution_ms,
                has_more,
                next_cursor: None,
            },
        }
    }

    /// Create result with pagination info
    pub fn with_pagination(
        schema: DatasetSchema,
        rows: Vec<DataRow>,
        execution_ms: u64,
        total_rows: Option<usize>,
        has_more: bool,
        next_cursor: Option<String>,
    ) -> Self {
        let row_count = rows.len();

        Self {
            schema,
            rows,
            stats: QueryStats {
                row_count,
                total_rows,
                execution_ms,
                has_more,
                next_cursor,
            },
        }
    }
}

/// Type of container in the hierarchy
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerType {
    /// Database (PostgreSQL, MySQL, MongoDB)
    Database,
    /// Schema (PostgreSQL)
    Schema,
    /// Keyspace (Cassandra, ScyllaDB)
    Keyspace,
    /// Bucket (S3, MinIO)
    Bucket,
    /// Namespace (Kubernetes-style)
    Namespace,
}

impl fmt::Display for ContainerType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContainerType::Database => write!(f, "database"),
            ContainerType::Schema => write!(f, "schema"),
            ContainerType::Keyspace => write!(f, "keyspace"),
            ContainerType::Bucket => write!(f, "bucket"),
            ContainerType::Namespace => write!(f, "namespace"),
        }
    }
}

/// Path through the container hierarchy
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ContainerPath {
    /// Segments of the path (e.g., ["mydb", "public"])
    pub segments: Vec<String>,
}

impl ContainerPath {
    pub fn new(segments: Vec<String>) -> Self {
        Self { segments }
    }

    pub fn from_slice(segments: &[&str]) -> Self {
        Self {
            segments: segments.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn root() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    pub fn push(&mut self, segment: impl Into<String>) {
        self.segments.push(segment.into());
    }

    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            None
        } else {
            Some(Self {
                segments: self.segments[..self.segments.len() - 1].to_vec(),
            })
        }
    }

    pub fn depth(&self) -> usize {
        self.segments.len()
    }
}

impl fmt::Display for ContainerPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.segments.join("/"))
    }
}

/// Information about what a container can hold
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerCapabilities {
    /// Can contain sub-containers
    pub can_contain_containers: bool,
    /// Can contain entities (tables, collections, etc.)
    pub can_contain_entities: bool,
    /// Type of sub-containers (if any)
    pub child_container_type: Option<ContainerType>,
    /// Type label for entities (e.g., "table", "collection", "object")
    pub entity_type_label: Option<String>,
}

/// Generic container information (database, schema, keyspace, bucket, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    /// Container name
    pub name: String,
    /// Type of this container
    pub container_type: ContainerType,
    /// What this container can hold
    pub capabilities: ContainerCapabilities,
    /// Optional metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Legacy type aliases for backward compatibility
pub type DatabaseInfo = ContainerInfo;
pub type NamespaceInfo = ContainerInfo;

/// Information about an entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityInfo {
    pub namespace: String,
    pub name: String,
    pub entity_type: String, // "table", "collection", "stream", etc.
    pub row_count: Option<usize>,
    pub size_bytes: Option<u64>,
    pub schema: Option<DatasetSchema>,
    /// Additional backend-specific metadata (content_type, last_modified, etag, etc.)
    pub metadata: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_ref_creation() {
        let entity = EntityRef::from_parts("public", "users");
        assert_eq!(entity.namespace.name, "public");
        assert_eq!(entity.name, "users");
        assert_eq!(entity.to_string(), "public.users");
    }

    #[test]
    fn test_query_options_defaults() {
        let opts = QueryOptions::default();
        assert_eq!(opts.limit, Some(100));
        assert_eq!(opts.offset, Some(0));
        assert_eq!(opts.sort_order, Some("asc".to_string()));
    }

    #[test]
    fn test_field_def_creation() {
        let field = FieldDef {
            name: "id".to_string(),
            field_type: FieldType::Int64,
            nullable: false,
            description: Some("User ID".to_string()),
        };
        assert_eq!(field.field_type, FieldType::Int64);
        assert!(!field.nullable);
    }
}
