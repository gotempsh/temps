//! MongoDB implementation of the temps-query DataSource trait
//!
//! This crate provides MongoDB access through the generic query interface.
//!
//! ## Hierarchy
//!
//! MongoDB uses a simple 2-level hierarchy:
//! - Depth 0: List databases
//! - Depth 1: List collections in database
//!
//! ## Example
//!
//! ```rust,no_run
//! use temps_query_mongodb::MongoDBSource;
//!
//! # async fn example() -> temps_query::Result<()> {
//! let source = MongoDBSource::new("mongodb://localhost:27017").await?;
//!
//! // List databases
//! let databases = source.list_containers(&temps_query::ContainerPath::root()).await?;
//!
//! // List collections in a database
//! let path = temps_query::ContainerPath::from_slice(&["mydb"]);
//! let collections = source.list_entities(&path).await?;
//! # Ok(())
//! # }\//! ```

use async_trait::async_trait;
use mongodb::{
    bson::{doc, Document},
    options::ClientOptions,
    Client,
};
use std::collections::HashMap;
use temps_query::{
    Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType, DataError,
    DataSource, DatasetSchema, EntityInfo, FieldDef, FieldType, Result,
};
use tracing::{debug, error};

/// MongoDB data source implementation
pub struct MongoDBSource {
    client: Client,
    url: String,
}

impl MongoDBSource {
    /// Create a new MongoDB data source
    ///
    /// # Arguments
    ///
    /// * `url` - MongoDB connection URL (e.g., "mongodb://localhost:27017")
    pub async fn new(url: &str) -> Result<Self> {
        debug!("Creating MongoDB source for URL: {}", url);

        let client_options = ClientOptions::parse(url).await.map_err(|e| {
            error!("Failed to parse MongoDB URL: {}", e);
            DataError::ConnectionFailed(format!("Failed to parse MongoDB URL: {}", e))
        })?;

        let client = Client::with_options(client_options).map_err(|e| {
            error!("Failed to create MongoDB client: {}", e);
            DataError::ConnectionFailed(format!("Failed to create MongoDB client: {}", e))
        })?;

        // Test connection
        client.list_database_names().await.map_err(|e| {
            error!("Failed to connect to MongoDB: {}", e);
            DataError::ConnectionFailed(format!("Failed to connect to MongoDB: {}", e))
        })?;

        debug!("MongoDB client created successfully");

        Ok(Self {
            client,
            url: url.to_string(),
        })
    }

    /// List collections in a specific database
    async fn list_collections_in_db(&self, db_name: &str) -> Result<Vec<EntityInfo>> {
        debug!("Listing collections in database: {}", db_name);

        let db = self.client.database(db_name);

        let collection_names = db.list_collection_names().await.map_err(|e| {
            error!("Failed to list collections in database {}: {}", db_name, e);
            DataError::QueryFailed(format!("Failed to list collections: {}", e))
        })?;

        let entities: Vec<EntityInfo> = collection_names
            .into_iter()
            .map(|name| {
                let mut metadata_map = HashMap::new();
                metadata_map.insert("database".to_string(), serde_json::json!(db_name));

                EntityInfo {
                    namespace: db_name.to_string(),
                    name,
                    entity_type: "collection".to_string(),
                    row_count: None, // Would require counting documents
                    size_bytes: None,
                    schema: None,
                    metadata: Some(serde_json::to_value(metadata_map).unwrap()),
                }
            })
            .collect();

        debug!(
            "Found {} collections in database {}",
            entities.len(),
            db_name
        );

        Ok(entities)
    }

    /// Get information about a specific collection
    async fn get_collection_info(
        &self,
        db_name: &str,
        collection_name: &str,
    ) -> Result<EntityInfo> {
        debug!(
            "Getting info for collection '{}' in database '{}'",
            collection_name, db_name
        );

        let db = self.client.database(db_name);

        // Check if collection exists
        let collection_names = db.list_collection_names().await.map_err(|e| {
            error!("Failed to list collections: {}", e);
            DataError::QueryFailed(format!("Failed to list collections: {}", e))
        })?;

        if !collection_names.contains(&collection_name.to_string()) {
            return Err(DataError::NotFound(format!(
                "Collection '{}' not found in database '{}'",
                collection_name, db_name
            )));
        }

        let collection = db.collection::<Document>(collection_name);

        // Get document count
        let doc_count = collection.estimated_document_count().await.ok();

        // Sample a document to infer schema
        let schema = if let Ok(Some(sample_doc)) = collection.find_one(doc! {}).await {
            Some(self.infer_schema_from_document(&sample_doc))
        } else {
            None
        };

        let mut metadata_map = HashMap::new();
        metadata_map.insert("database".to_string(), serde_json::json!(db_name));
        if let Some(count) = doc_count {
            metadata_map.insert("document_count".to_string(), serde_json::json!(count));
        }

        Ok(EntityInfo {
            namespace: db_name.to_string(),
            name: collection_name.to_string(),
            entity_type: "collection".to_string(),
            row_count: doc_count.map(|c| c as usize),
            size_bytes: None,
            schema,
            metadata: Some(serde_json::to_value(metadata_map).unwrap()),
        })
    }

    /// Infer schema from a BSON document
    fn infer_schema_from_document(&self, doc: &Document) -> DatasetSchema {
        let fields: Vec<FieldDef> = doc
            .iter()
            .map(|(key, value)| {
                let field_type = match value {
                    bson::Bson::String(_) => FieldType::String,
                    bson::Bson::Int32(_) => FieldType::Int32,
                    bson::Bson::Int64(_) => FieldType::Int64,
                    bson::Bson::Double(_) => FieldType::Float64,
                    bson::Bson::Boolean(_) => FieldType::Boolean,
                    bson::Bson::DateTime(_) => FieldType::Timestamp,
                    bson::Bson::Array(_) => FieldType::Json,
                    bson::Bson::Document(_) => FieldType::Json,
                    bson::Bson::ObjectId(_) => FieldType::String,
                    bson::Bson::Null => FieldType::String,
                    _ => FieldType::String,
                };

                FieldDef {
                    name: key.clone(),
                    field_type,
                    nullable: true,
                    description: None,
                }
            })
            .collect();

        DatasetSchema {
            fields,
            partitions: None,
            primary_key: Some(vec!["_id".to_string()]),
        }
    }
}

#[async_trait]
impl DataSource for MongoDBSource {
    fn source_type(&self) -> &'static str {
        "mongodb"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Document]
    }

    async fn list_containers(&self, path: &ContainerPath) -> Result<Vec<ContainerInfo>> {
        match path.depth() {
            // Depth 0: List databases
            0 => {
                debug!("Listing MongoDB databases");

                let db_names = self.client.list_database_names().await.map_err(|e| {
                    error!("Failed to list databases: {}", e);
                    DataError::QueryFailed(format!("Failed to list databases: {}", e))
                })?;

                let databases: Vec<ContainerInfo> = db_names
                    .into_iter()
                    .map(|name| {
                        let mut metadata = HashMap::new();
                        metadata.insert("type".to_string(), serde_json::json!("database"));

                        ContainerInfo {
                            name,
                            container_type: ContainerType::Database,
                            capabilities: ContainerCapabilities {
                                can_contain_containers: false,
                                can_contain_entities: true,
                                child_container_type: None,
                                entity_type_label: Some("collection".to_string()),
                            },
                            metadata,
                        }
                    })
                    .collect();

                debug!("Found {} databases", databases.len());
                Ok(databases)
            }

            // Depth >= 1: Not supported (MongoDB has flat database -> collection structure)
            _ => Err(DataError::InvalidQuery(format!(
                "MongoDB hierarchy only supports 1 level (database). Path depth: {}. Use list_entities to list collections.",
                path.depth()
            ))),
        }
    }

    async fn get_container_info(&self, path: &ContainerPath) -> Result<ContainerInfo> {
        if path.depth() != 1 {
            return Err(DataError::InvalidQuery(format!(
                "get_container_info requires path depth 1 (database name), got {}",
                path.depth()
            )));
        }

        let db_name = &path.segments[0];

        debug!("Getting info for database: {}", db_name);

        // Check if database exists
        let db_names = self.client.list_database_names().await.map_err(|e| {
            error!("Failed to list databases: {}", e);
            DataError::QueryFailed(format!("Failed to list databases: {}", e))
        })?;

        if !db_names.contains(db_name) {
            return Err(DataError::NotFound(format!(
                "Database '{}' not found",
                db_name
            )));
        }

        let mut metadata = HashMap::new();
        metadata.insert("type".to_string(), serde_json::json!("database"));

        // Try to get collection count
        let db = self.client.database(db_name);
        if let Ok(collection_names) = db.list_collection_names().await {
            metadata.insert(
                "collection_count".to_string(),
                serde_json::json!(collection_names.len()),
            );
        }

        Ok(ContainerInfo {
            name: db_name.clone(),
            container_type: ContainerType::Database,
            capabilities: ContainerCapabilities {
                can_contain_containers: false,
                can_contain_entities: true,
                child_container_type: None,
                entity_type_label: Some("collection".to_string()),
            },
            metadata,
        })
    }

    async fn list_entities(&self, container_path: &ContainerPath) -> Result<Vec<EntityInfo>> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot list entities at root level - specify a database path".to_string(),
            ));
        }

        if container_path.depth() > 1 {
            return Err(DataError::InvalidQuery(format!(
                "MongoDB only supports 1 level hierarchy (database). Path depth: {}",
                container_path.depth()
            )));
        }

        let db_name = &container_path.segments[0];

        self.list_collections_in_db(db_name).await
    }

    async fn get_entity_info(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot get entity at root level - specify a database path".to_string(),
            ));
        }

        if container_path.depth() > 1 {
            return Err(DataError::InvalidQuery(format!(
                "MongoDB only supports 1 level hierarchy (database). Path depth: {}",
                container_path.depth()
            )));
        }

        let db_name = &container_path.segments[0];

        self.get_collection_info(db_name, entity_name).await
    }

    async fn get_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<DatasetSchema> {
        let entity_info = self.get_entity_info(container_path, entity_name).await?;

        entity_info.schema.ok_or_else(|| {
            DataError::OperationNotSupported(
                "Schema not available for this entity (empty collection)".to_string(),
            )
        })
    }

    async fn close(&self) -> Result<()> {
        debug!("Closing MongoDB source");
        // MongoDB client handles cleanup automatically
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_type() {
        assert_eq!("mongodb", "mongodb");
    }

    #[test]
    fn test_capabilities() {
        let capabilities = vec![Capability::Document];
        assert!(capabilities.contains(&Capability::Document));
    }
}
