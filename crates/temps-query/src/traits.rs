use crate::error::Result;
use crate::types::*;
use async_trait::async_trait;
use downcast_rs::{impl_downcast, Downcast};

/// Core trait that all data sources must implement
/// Provides basic capability checking and schema inspection
#[async_trait]
pub trait DataSource: Send + Sync + Downcast {
    /// Get the type name of this data source
    fn source_type(&self) -> &'static str;

    /// Get all capabilities supported by this source
    fn capabilities(&self) -> Vec<Capability>;

    /// Check if a specific capability is supported
    fn supports(&self, capability: Capability) -> bool {
        self.capabilities().contains(&capability)
    }

    /// List containers at a specific path in the hierarchy
    /// Empty path means root level (databases, keyspaces, buckets, etc.)
    /// Example paths:
    /// - [] -> list databases
    /// - ["mydb"] -> list schemas in database "mydb"
    /// - ["mydb", "public"] -> would list tables, but use list_entities instead
    async fn list_containers(&self, path: &ContainerPath) -> Result<Vec<ContainerInfo>>;

    /// Get information about a specific container
    async fn get_container_info(&self, path: &ContainerPath) -> Result<ContainerInfo>;

    /// List entities (tables, collections, objects) at a specific container path
    /// The path should point to a container that can hold entities
    /// Example: path=["mydb", "public"] lists tables in the public schema
    async fn list_entities(&self, container_path: &ContainerPath) -> Result<Vec<EntityInfo>>;

    /// Get detailed information about an entity
    /// The container_path points to the parent container, entity_name is the entity within it
    async fn get_entity_info(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo>;

    /// Get the schema of an entity
    async fn get_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<DatasetSchema>;

    /// Close the connection gracefully
    async fn close(&self) -> Result<()>;
}

impl_downcast!(DataSource);

/// Optional trait for sources that support schema inspection
#[async_trait]
pub trait Introspect: DataSource {
    /// Get detailed field information
    async fn inspect_fields(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<Vec<FieldDef>>;

    /// Check if a field exists
    async fn field_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<bool>;

    /// Get field type information
    async fn get_field_type(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<FieldType>;
}

/// Optional trait for sources that support querying with filters
#[async_trait]
pub trait Queryable: DataSource {
    /// Execute a query and return rows with pagination
    /// Filters are backend-specific JSON (SQL WHERE, MongoDB filter, etc.)
    async fn query(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
        options: QueryOptions,
    ) -> Result<QueryResult>;

    /// Count rows matching criteria
    async fn count(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
    ) -> Result<u64>;

    /// Check if an entity exists
    async fn entity_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<bool>;
}

/// Optional trait for sources that support item-level access
#[async_trait]
pub trait ItemAccess: DataSource {
    /// Get a single item by key/ID
    async fn get_item(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        key: &str,
    ) -> Result<Option<DataRow>>;

    /// Get items by multiple keys/IDs
    async fn get_items(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        keys: Vec<String>,
    ) -> Result<Vec<DataRow>>;

    /// Update an item
    async fn update_item(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        key: &str,
        data: DataRow,
    ) -> Result<DataRow>;

    /// Delete an item
    async fn delete_item(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        key: &str,
    ) -> Result<()>;
}

/// Optional trait for SQL-based sources (PostgreSQL, MySQL, etc.)
#[async_trait]
pub trait SqlFeature: DataSource {
    /// Execute raw SQL query
    async fn execute_sql(
        &self,
        sql: &str,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<QueryResult>;

    /// Explain query execution plan
    async fn explain(&self, sql: &str) -> Result<String>;

    /// Validate SQL syntax
    fn validate_sql(&self, sql: &str) -> Result<()>;
}

/// Optional trait for sources supporting transactions
#[async_trait]
pub trait Transactional: DataSource {
    /// Begin a transaction
    async fn begin_transaction(&self) -> Result<String>;

    /// Commit a transaction
    async fn commit_transaction(&self, tx_id: &str) -> Result<()>;

    /// Rollback a transaction
    async fn rollback_transaction(&self, tx_id: &str) -> Result<()>;
}

/// Optional trait for extensibility - allows backends to provide custom methods
#[async_trait]
pub trait Extensible: DataSource {
    /// Execute a backend-specific operation
    async fn execute_custom(
        &self,
        operation: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value>;

    /// Get available custom operations
    fn available_operations(&self) -> Vec<String>;
}

/// Optional trait for sources that support streaming downloads (S3, file systems, etc.)
#[async_trait]
pub trait Downloadable: DataSource {
    /// Download an entity and return its stream
    /// Returns (stream, content_type)
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
    )>;
}

/// Optional trait for sources that can describe their query capabilities via JSON Schema
/// This allows frontends to dynamically generate filter and sort UIs
pub trait QuerySchemaProvider: DataSource {
    /// Get JSON Schema for the filter format
    /// Returns a JSON Schema object describing the expected filter structure
    ///
    /// Example for SQL-based sources:
    /// ```json
    /// {
    ///   "type": "object",
    ///   "properties": {
    ///     "where": {
    ///       "type": "string",
    ///       "description": "SQL WHERE clause"
    ///     }
    ///   }
    /// }
    /// ```
    fn get_filter_schema(&self) -> serde_json::Value;

    /// Get JSON Schema for sort options based on entity schema
    /// Returns a JSON Schema object describing available sort fields and orders
    ///
    /// Example:
    /// ```json
    /// {
    ///   "type": "object",
    ///   "properties": {
    ///     "sort_by": {
    ///       "type": "string",
    ///       "enum": ["id", "name", "created_at"]
    ///     },
    ///     "sort_order": {
    ///       "type": "string",
    ///       "enum": ["asc", "desc"]
    ///     }
    ///   }
    /// }
    /// ```
    fn get_sort_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<serde_json::Value>;

    /// Get UI hints for rendering filter form (React JSON Schema Form format)
    /// Returns optional UI schema with widget preferences and placeholders
    fn get_filter_ui_schema(&self) -> Option<serde_json::Value> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_display() {
        assert_eq!(Capability::Sql.to_string(), "sql");
        assert_eq!(Capability::Document.to_string(), "document");
        assert_eq!(Capability::KeyValue.to_string(), "key-value");
    }
}
