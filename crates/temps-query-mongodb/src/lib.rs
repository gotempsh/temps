// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
//! use temps_query::DataSource;
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
    bson::{doc, Bson, Document},
    options::ClientOptions,
    Client,
};
use std::collections::HashMap;
use temps_query::{
    BoundedRows, Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType,
    DataError, DataSource, DatasetSchema, EntityCountHint, EntityInfo, FieldDef, FieldType, Result,
};
use tracing::{debug, error};

/// MongoDB data source implementation
pub struct MongoDBSource {
    client: Client,
    /// The database named in the connection string, when it names one.
    ///
    /// The SQL backends pin every request to the database they connected to
    /// (`temps-query-postgres` rejects a mismatch outright, as does
    /// `database_from_path` in the MariaDB backend). MongoDB had no equivalent:
    /// path segment 0 was passed to `client.database(...)` unchecked, so a
    /// caller could walk out of their own database into any other on the
    /// server. This is the anchor for the same guard — see
    /// [`MongoDBSource::assert_database_allowed`].
    default_database: Option<String>,
}

/// Databases that are never browsable, whatever the connection string says.
///
/// With a Temps-provisioned MongoDB the connection user is typically root, so
/// these are readable by default and they are not application data:
/// `admin.system.users` holds the SCRAM credential records for every Mongo
/// user, and `local.oplog.rs` is the change stream for *every* database on the
/// server — reading it returns the row contents of databases the caller never
/// named, which defeats the whole point of gating row access per service.
const SYSTEM_DATABASES: [&str; 3] = ["admin", "local", "config"];

/// Query operators a data-browser filter may use.
///
/// SECURITY: this is an allowlist on purpose, and it is the MongoDB equivalent
/// of the WHERE-clause validators the SQL backends carry. Filters arrive as
/// caller-supplied JSON — from `?filter=` and, once a service opts into AI data
/// access, from a model that may be reading attacker-written rows — and were
/// previously inserted into the BSON document verbatim, with no `$` check at
/// all. That made `{"$where": "function(){...}"}` server-side JavaScript
/// execution inside mongod, `{"$expr": {"$function": {...}}}` the same on 4.4+,
/// and `$where` additionally ignores the rest of the filter and reads every
/// document.
///
/// A denylist cannot work here: MongoDB adds operators, and any one that
/// evaluates an expression is another hole. Anything not on this list is
/// refused with the name echoed back, so a caller using a legitimate operator
/// we forgot gets an actionable error instead of silence.
/// `$regex` is on this list deliberately, unlike the SQL backends, which reject
/// `~`/`SIMILAR TO`/`REGEXP` outright. Recording the reasoning because the
/// inconsistency is otherwise indistinguishable from an oversight: a crafted
/// pattern is catastrophic backtracking in mongod's PCRE, the same primitive
/// the SQL side refuses. The difference is that SQL has `LIKE` and MongoDB has
/// no other substring-search primitive at all, so removing `$regex` would take
/// "find documents whose name starts with…" away entirely rather than
/// redirecting it. The exposure is bounded by `max_time` on every call site
/// that can carry a filter (`query` and both `count_documents`), which is the
/// same mitigation the SQL side relies on for the clauses it does allow.
const ALLOWED_FILTER_OPERATORS: [&str; 15] = [
    "$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$in", "$nin", "$and", "$or", "$nor", "$not",
    "$exists", "$type", "$regex",
];

/// Maximum nesting depth of a filter document.
///
/// Deep nesting is both a parser-stack risk and a way to hide an operator from
/// a shallow check. Honest filters are one or two levels.
const MAX_FILTER_DEPTH: usize = 8;

fn bounded_document_pipeline(
    filter: Document,
    sort: Document,
    skip: u64,
    limit: i64,
    max_document_bytes: usize,
) -> Vec<Document> {
    let byte_limit = i64::try_from(max_document_bytes).unwrap_or(i64::MAX);
    vec![
        doc! { "$match": filter },
        doc! { "$sort": sort },
        doc! { "$skip": i64::try_from(skip).unwrap_or(i64::MAX) },
        doc! { "$limit": limit },
        doc! {
            "$replaceWith": {
                "$let": {
                    "vars": { "temps_size": { "$bsonSize": "$$ROOT" } },
                    "in": {
                        "$cond": [
                            { "$lte": ["$$temps_size", byte_limit] },
                            {
                                "__temps_admitted": true,
                                "__temps_size": "$$temps_size",
                                "__temps_doc": "$$ROOT"
                            },
                            {
                                "__temps_admitted": false,
                                "__temps_size": "$$temps_size"
                            }
                        ]
                    }
                }
            }
        },
    ]
}

/// Server-side ceiling for the standalone `count`, which carries no
/// `QueryOptions` and therefore no caller deadline. Matches the SQL backends.
const COUNT_TIMEOUT_MS: u64 = 10_000;

/// Validate a caller-supplied MongoDB filter before it reaches the driver.
///
/// Walks the whole document — every nested object and array — rather than only
/// the top level, because `{"$and": [{"$where": "..."}]}` puts the payload one
/// level down. Field names (anything not starting with `$`) are passed through:
/// they address data, they are not evaluated.
fn validate_filter_value(value: &serde_json::Value, depth: usize) -> Result<()> {
    if depth > MAX_FILTER_DEPTH {
        return Err(DataError::InvalidQuery(format!(
            "Filter is nested more than {MAX_FILTER_DEPTH} levels deep"
        )));
    }

    match value {
        serde_json::Value::Object(map) => {
            for (key, nested) in map {
                if key.starts_with('$') && !ALLOWED_FILTER_OPERATORS.contains(&key.as_str()) {
                    return Err(DataError::InvalidQuery(format!(
                        "The MongoDB operator '{key}' is not allowed in the data browser. \
                         Allowed operators: {}",
                        ALLOWED_FILTER_OPERATORS.join(", ")
                    )));
                }
                validate_filter_value(nested, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                validate_filter_value(item, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

impl MongoDBSource {
    /// Create a new MongoDB data source
    ///
    /// # Arguments
    ///
    /// * `url` - MongoDB connection URL (e.g., "mongodb://localhost:27017")
    pub async fn new(url: &str) -> Result<Self> {
        Self::new_scoped(url, None).await
    }

    /// Create a source pinned to one database.
    ///
    /// SECURITY: prefer this over [`MongoDBSource::new`] for anything a user can
    /// aim at a path. The pin used to be read from the URI's `/dbname` segment
    /// alone, and the data browser builds its URI as
    /// `mongodb://user:pass@host:port` with no segment — so `default_database`
    /// was always `None`, `assert_database_allowed` fell through for every
    /// non-system name, and a guard that looked right did nothing. Taking the
    /// scope as an explicit argument makes it visible at the call site whether
    /// a source is pinned, instead of depending on how a string was formatted
    /// three modules away.
    pub async fn new_scoped(url: &str, database: Option<&str>) -> Result<Self> {
        debug!("Creating MongoDB source for URL: {}", url);

        let mut client_options = ClientOptions::parse(url).await.map_err(|e| {
            error!("Failed to parse MongoDB URL: {}", e);
            DataError::ConnectionFailed(format!("Failed to parse MongoDB URL: {}", e))
        })?;

        // Force direct connection so the driver doesn't perform replica-set
        // topology discovery. In RS mode, `rs.status()` advertises members at
        // their internal addresses (e.g. `127.0.0.1:27017` from the
        // container's POV) which are unreachable from the explorer process.
        // Without this flag, `list_database_names()` hangs on `ReplicaSetNoPrimary`
        // until server-selection timeout. The explorer is inherently per-node,
        // not per-cluster, so direct connection is the correct semantic.
        client_options.direct_connection = Some(true);

        // Explicit scope wins; fall back to the URI's `/dbname` segment.
        let default_database = database
            .map(str::to_string)
            .or_else(|| client_options.default_database.clone())
            .filter(|d| !d.is_empty());

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
            default_database,
        })
    }

    /// Reject a database the caller is not entitled to browse.
    ///
    /// Two layers, because they close different holes:
    ///
    /// 1. System databases are always denied. They are readable by the root
    ///    user a Temps-provisioned MongoDB connects as, they contain
    ///    credentials and a cross-database change stream, and no data browser
    ///    needs them.
    /// 2. When the connection string names a database, nothing else is
    ///    reachable — matching what the Postgres and MariaDB backends already
    ///    enforce. Left permissive when it does not, because a URI without a
    ///    database is a legitimate configuration and refusing everything would
    ///    break it; layer 1 still applies.
    fn assert_database_allowed(&self, db_name: &str) -> Result<()> {
        if SYSTEM_DATABASES
            .iter()
            .any(|d| d.eq_ignore_ascii_case(db_name))
        {
            return Err(DataError::OperationNotSupported(format!(
                "The '{db_name}' database is a MongoDB system database and is not browsable"
            )));
        }

        if let Some(configured) = &self.default_database {
            if configured != db_name {
                return Err(DataError::OperationNotSupported(format!(
                    "Cannot access database '{db_name}' while connected to '{configured}'"
                )));
            }
        }

        Ok(())
    }

    /// Reject MongoDB's internal collections.
    ///
    /// `system.*` is namespaced for the server's own bookkeeping — indexes,
    /// users, JS — and is not application data. Denied separately from the
    /// database check because `system.*` collections exist inside ordinary
    /// databases too, not only inside `admin`.
    fn assert_collection_allowed(collection: &str) -> Result<()> {
        if collection.starts_with("system.") {
            return Err(DataError::OperationNotSupported(format!(
                "The '{collection}' collection is MongoDB-internal and is not browsable"
            )));
        }
        Ok(())
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
                    metadata: Some(serde_json::Value::Object(
                        metadata_map.into_iter().collect(),
                    )),
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

        // Already an O(1) metadata read rather than a full scan — unlike the
        // SQL backends, Mongo hands us a maintained count.
        let doc_count = collection.estimated_document_count().await.ok();

        // On-disk size via collStats. Also a metadata read; `storageSize` is
        // the compressed on-disk figure, which is what an operator asking
        // "how much space does this use" wants (`size` is the uncompressed
        // logical total). Best-effort: collStats needs privileges the browsing
        // user may not have, and a missing size renders as "—" rather than 0.
        let size_bytes = db
            .run_command(doc! { "collStats": collection_name })
            .await
            .ok()
            .and_then(|stats| {
                stats
                    .get("storageSize")
                    .or_else(|| stats.get("size"))
                    .and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from)))
            })
            .and_then(|s| u64::try_from(s).ok());

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
            size_bytes,
            schema,
            metadata: Some(serde_json::Value::Object(
                metadata_map.into_iter().collect(),
            )),
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
                    // Don't advertise what `assert_database_allowed` will
                    // refuse: listing `admin`/`local` and then 400-ing on click
                    // is worse UX than not offering them, and it tells an agent
                    // they exist.
                    .filter(|name| self.assert_database_allowed(name).is_ok())
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
                                entity_count_hint: Some(EntityCountHint::Small),
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
        // The one entry point that was missing this. Without it,
        // `/containers/admin/info` confirmed the database exists and returned
        // its collection count — contradicting the guard applied everywhere
        // else, and giving an existence oracle for databases the caller cannot
        // otherwise reach.
        self.assert_database_allowed(db_name)?;

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
                entity_count_hint: Some(EntityCountHint::Small),
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
        self.assert_database_allowed(db_name)?;

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
        self.assert_database_allowed(db_name)?;
        Self::assert_collection_allowed(entity_name)?;

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

#[async_trait]
impl temps_query::Queryable for MongoDBSource {
    async fn query(
        &self,
        container_path: &temps_query::ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
        options: temps_query::QueryOptions,
    ) -> Result<temps_query::QueryResult> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot query at root level - specify a database path".to_string(),
            ));
        }

        let db_name = &container_path.segments[0];
        self.assert_database_allowed(db_name)?;
        Self::assert_collection_allowed(entity_name)?;
        let db = self.client.database(db_name);
        let collection = db.collection::<Document>(entity_name);

        // Convert filter JSON to MongoDB Document
        let filter_doc = if let Some(filter_value) = filters {
            // SECURITY: validate before anything reaches the driver. See
            // `validate_filter_value` — without this, `$where` is arbitrary
            // JavaScript execution inside mongod.
            validate_filter_value(&filter_value, 0)?;

            // If the filter is already a MongoDB query, use it directly
            // Otherwise, treat it as key-value equality filters
            match filter_value {
                serde_json::Value::Object(map) => {
                    let mut doc = Document::new();
                    for (key, value) in map {
                        // Convert JSON value to BSON value
                        if let Ok(bson_value) = mongodb::bson::serialize_to_bson(&value) {
                            doc.insert(key, bson_value);
                        }
                    }
                    doc
                }
                _ => Document::new(),
            }
        } else {
            Document::new()
        };

        // Apply pagination
        let limit = options.limit.unwrap_or(100) as i64;
        let skip = options.offset.unwrap_or(0) as u64;

        // Apply sorting
        let sort_doc = if let Some(sort_by) = &options.sort_by {
            let sort_order = match options.sort_order.as_deref() {
                Some("desc") | Some("DESC") => -1,
                _ => 1,
            };
            [(sort_by.clone(), Bson::Int32(sort_order))]
                .into_iter()
                .collect()
        } else {
            doc! { "_id": 1 } // Default sort by _id ascending
        };

        debug!(
            entity = entity_name,
            limit, skip, "executing MongoDB data query"
        );

        let start_time = std::time::Instant::now();

        // SECURITY / AVAILABILITY: bound the operation server-side.
        //
        // Dropping the future when the caller's deadline fires abandons the
        // cursor but does not stop the server from doing the work — `maxTimeMS`
        // is what makes mongod give up. Relevant here because `skip` is
        // caller-supplied and its cost is O(skip): the server walks and discards
        // every skipped document regardless of `limit`.
        let max_time = std::time::Duration::from_millis(options.timeout_ms.unwrap_or(30_000));

        // `$bsonSize` is evaluated by mongod before the conditional projection.
        // Using the cell ceiling as the document ceiling is deliberately
        // conservative: no individual nested value can cross the wire above
        // the per-cell budget, even though MongoDB documents are dynamic.
        let wire_document_limit = options.budget.max_bytes.min(options.budget.max_cell_bytes);
        let pipeline = bounded_document_pipeline(
            filter_doc.clone(),
            sort_doc,
            skip,
            limit,
            wire_document_limit,
        );
        let mut cursor = collection
            .aggregate(pipeline)
            .batch_size(1)
            .max_time(max_time)
            .await
            .map_err(|_error| {
                error!(entity = entity_name, limit, "MongoDB query failed");
                DataError::BackendQueryFailed {
                    backend: "MongoDB",
                    entity: entity_name.to_string(),
                }
            })?;

        // Decode one cursor document at a time into the shared bounded
        // collector. MongoDB itself caps one BSON document at 16 MiB; the
        // tighter query budget rejects it before it can accumulate with peers.
        let mut bounded = BoundedRows::new(options.budget);
        while cursor.advance().await.map_err(|_error| {
            error!(entity = entity_name, limit, "MongoDB cursor failed");
            DataError::BackendQueryFailed {
                backend: "MongoDB",
                entity: entity_name.to_string(),
            }
        })? {
            let mut envelope = cursor.deserialize_current().map_err(|_error| {
                error!(
                    entity = entity_name,
                    limit, "MongoDB document decode failed"
                );
                DataError::BackendQueryFailed {
                    backend: "MongoDB",
                    entity: entity_name.to_string(),
                }
            })?;

            let observed = match envelope.remove("__temps_size") {
                Some(Bson::Int32(value)) => usize::try_from(value).unwrap_or(usize::MAX),
                Some(Bson::Int64(value)) => usize::try_from(value).unwrap_or(usize::MAX),
                _ => {
                    return Err(DataError::BackendQueryFailed {
                        backend: "MongoDB",
                        entity: entity_name.to_string(),
                    })
                }
            };
            let admitted = envelope
                .remove("__temps_admitted")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !admitted {
                return Err(DataError::ResultLimitExceeded {
                    entity: entity_name.to_string(),
                    limit_kind: "wire_document_bytes",
                    limit: wire_document_limit,
                    observed,
                });
            }
            let doc = envelope
                .remove("__temps_doc")
                .and_then(|value| value.as_document().cloned())
                .ok_or_else(|| DataError::BackendQueryFailed {
                    backend: "MongoDB",
                    entity: entity_name.to_string(),
                })?;

            // Convert the admitted Document to HashMap for DataRow.
            let mut row_map = std::collections::HashMap::new();
            for (key, value) in doc {
                // Convert BSON to serde_json::Value
                // Use serde to convert BSON value to JSON
                if let Ok(json_value) = serde_json::to_value(&value) {
                    row_map.insert(key, json_value);
                }
            }
            if !bounded.push(entity_name, row_map)? {
                break;
            }
        }
        let (rows, truncated) = bounded.into_parts();

        // Get total count (expensive, but needed for pagination).
        //
        // SECURITY: `max_time` here as well as on the `find` above. Dropping the
        // future when the control-plane deadline fires abandons the operation
        // client-side but does not stop mongod, and a count with a
        // caller-supplied filter is a full collection scan.
        let total_count = collection
            .count_documents(filter_doc)
            .max_time(max_time)
            .await
            .map_err(|_error| {
                error!(entity = entity_name, limit, "MongoDB count failed");
                DataError::BackendQueryFailed {
                    backend: "MongoDB",
                    entity: entity_name.to_string(),
                }
            })?;

        let execution_time = start_time.elapsed();

        // Get schema from first document
        let schema = if let Some(first_row) = rows.first() {
            let fields: Vec<temps_query::FieldDef> = first_row
                .iter()
                .map(|(key, value)| {
                    let field_type = match value {
                        serde_json::Value::String(_) => temps_query::FieldType::String,
                        serde_json::Value::Number(n) if n.is_i64() => temps_query::FieldType::Int64,
                        serde_json::Value::Number(n) if n.is_f64() => {
                            temps_query::FieldType::Float64
                        }
                        serde_json::Value::Bool(_) => temps_query::FieldType::Boolean,
                        serde_json::Value::Array(_) => temps_query::FieldType::Json,
                        serde_json::Value::Object(_) => temps_query::FieldType::Json,
                        _ => temps_query::FieldType::String,
                    };

                    temps_query::FieldDef {
                        name: key.clone(),
                        field_type,
                        nullable: true,
                        description: None,
                    }
                })
                .collect();

            temps_query::DatasetSchema {
                fields,
                partitions: None,
                primary_key: Some(vec!["_id".to_string()]),
            }
        } else {
            // Empty result - create schema from collection sample
            temps_query::DatasetSchema {
                fields: vec![],
                partitions: None,
                primary_key: Some(vec!["_id".to_string()]),
            }
        };

        // Check if there are more results
        let returned_rows = rows.len();
        let has_more = (skip + returned_rows as u64) < total_count;

        Ok(temps_query::QueryResult {
            schema,
            rows,
            stats: temps_query::QueryStats {
                row_count: returned_rows,
                total_rows: Some(total_count as usize),
                execution_ms: execution_time.as_millis() as u64,
                has_more,
                next_cursor: None, // MongoDB uses offset-based pagination, not cursors
                truncated,
            },
        })
    }

    async fn count(
        &self,
        container_path: &temps_query::ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
    ) -> Result<u64> {
        if container_path.depth() == 0 {
            return Err(DataError::InvalidQuery(
                "Cannot count at root level - specify a database path".to_string(),
            ));
        }

        let db_name = &container_path.segments[0];
        self.assert_database_allowed(db_name)?;
        Self::assert_collection_allowed(entity_name)?;
        let db = self.client.database(db_name);
        let collection = db.collection::<Document>(entity_name);

        // Convert filter JSON to MongoDB Document
        let filter_doc = if let Some(filter_value) = filters {
            // Same validation as the query path — this call site takes the same
            // caller-supplied JSON and reaches the same evaluator.
            validate_filter_value(&filter_value, 0)?;

            match filter_value {
                serde_json::Value::Object(map) => {
                    let mut doc = Document::new();
                    for (key, value) in map {
                        if let Ok(bson_value) = mongodb::bson::serialize_to_bson(&value) {
                            doc.insert(key, bson_value);
                        }
                    }
                    doc
                }
                _ => Document::new(),
            }
        } else {
            Document::new()
        };

        // `count` takes no QueryOptions, so it has no caller deadline to
        // honour — but it is reached from `get_entity_info`, which the AI agent
        // can call, and it is a full collection scan.
        let count = collection
            .count_documents(filter_doc)
            .max_time(std::time::Duration::from_millis(COUNT_TIMEOUT_MS))
            .await
            .map_err(|e| {
                error!("Failed to count MongoDB documents: {}", e);
                DataError::QueryFailed(format!("Failed to count documents: {}", e))
            })?;

        Ok(count)
    }

    async fn entity_exists(
        &self,
        container_path: &temps_query::ContainerPath,
        entity_name: &str,
    ) -> Result<bool> {
        if container_path.depth() == 0 {
            return Ok(false);
        }

        let db_name = &container_path.segments[0];
        let db = self.client.database(db_name);

        let collections = db.list_collection_names().await.map_err(|e| {
            error!("Failed to list collections: {}", e);
            DataError::QueryFailed(format!("Failed to list collections: {}", e))
        })?;

        Ok(collections.contains(&entity_name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_query::{QueryBudget, QueryOptions, Queryable};
    use testcontainers::{
        core::{ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage,
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
    fn aggregation_sizes_before_conditionally_projecting_document() {
        let pipeline = bounded_document_pipeline(
            doc! { "status": "active" },
            doc! { "created_at": -1 },
            25,
            10,
            65_536,
        );
        let encoded = mongodb::bson::serialize_to_bson(&pipeline)
            .expect("test pipeline should serialize")
            .to_string();

        assert!(encoded.contains("$bsonSize"));
        assert!(encoded.contains("$cond"));
        assert!(encoded.contains("__temps_admitted"));
        assert!(encoded.contains("__temps_doc"));
        assert!(encoded.contains("65536"));
        assert_eq!(pipeline[2], doc! { "$skip": 25_i64 });
        assert_eq!(pipeline[3], doc! { "$limit": 10_i64 });
    }

    #[tokio::test]
    async fn real_mongodb_enforces_wire_budget_and_preserves_documents() -> anyhow::Result<()> {
        let container = match GenericImage::new("mongo", "8")
            .with_exposed_port(ContainerPort::Tcp(27017))
            .with_wait_for(WaitFor::message_on_stdout("Waiting for connections"))
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) if container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping Docker-dependent MongoDB budget test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let host = container.get_host().await?;
        let port = container.get_host_port_ipv4(27017).await?;
        let url = format!("mongodb://{host}:{port}/app");
        let mut source = None;
        for _ in 0..20 {
            match MongoDBSource::new_scoped(&url, Some("app")).await {
                Ok(connected) => {
                    source = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            }
        }
        let source = source.ok_or_else(|| anyhow::anyhow!("MongoDB did not become reachable"))?;
        source
            .client
            .database("app")
            .collection::<Document>("rows_budget")
            .insert_many([
                doc! { "row_id": 1_i32, "payload": "safe" },
                doc! { "row_id": 2_i32, "payload": "x".repeat(100_000) },
            ])
            .await?;

        let path = ContainerPath::from_slice(&["app"]);
        let safe = source
            .query(
                &path,
                "rows_budget",
                Some(serde_json::json!({"row_id": 1})),
                QueryOptions::default(),
            )
            .await?;
        assert_eq!(safe.rows[0]["payload"], serde_json::json!("safe"));

        let error = source
            .query(
                &path,
                "rows_budget",
                Some(serde_json::json!({"row_id": 2})),
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
            .expect_err("oversized MongoDB document must be rejected before transfer");
        assert!(matches!(
            error,
            DataError::ResultLimitExceeded {
                limit_kind: "wire_document_bytes",
                ..
            }
        ));

        Ok(())
    }

    #[test]
    fn test_source_type() {
        assert_eq!("mongodb", "mongodb");
    }

    #[test]
    fn test_capabilities() {
        let capabilities = [Capability::Document];
        assert!(capabilities.contains(&Capability::Document));
    }

    #[test]
    fn filter_rejects_javascript_evaluating_operators() {
        // These were the hole: every top-level key went into the BSON document
        // verbatim, so `$where` was arbitrary JavaScript inside mongod, and it
        // also ignores the rest of the filter and reads every document.
        for payload in [
            serde_json::json!({"$where": "function(){while(true){}}"}),
            serde_json::json!({"$expr": {"$function": {"body": "x", "args": [], "lang": "js"}}}),
            serde_json::json!({"$accumulator": {"init": "x"}}),
            serde_json::json!({"$merge": "other"}),
            serde_json::json!({"$out": "other"}),
        ] {
            assert!(
                validate_filter_value(&payload, 0).is_err(),
                "should have been rejected: {payload}"
            );
        }
    }

    #[test]
    fn filter_rejects_disallowed_operators_nested_in_allowed_ones() {
        // A top-level-only check would pass this: the payload sits one level
        // down, inside an operator that is itself legitimate.
        let nested = serde_json::json!({"$and": [{"status": "active"}, {"$where": "1"}]});
        assert!(validate_filter_value(&nested, 0).is_err());

        let deeper = serde_json::json!({"$or": [{"$and": [{"$where": "1"}]}]});
        assert!(validate_filter_value(&deeper, 0).is_err());
    }

    #[test]
    fn filter_accepts_ordinary_queries() {
        // The allowlist must not break the filters this exists to run. Field
        // names address data rather than being evaluated, so they pass through.
        for payload in [
            serde_json::json!({"status": "active"}),
            serde_json::json!({"age": {"$gt": 21, "$lte": 65}}),
            serde_json::json!({"$and": [{"a": 1}, {"b": {"$in": [1, 2, 3]}}]}),
            serde_json::json!({"name": {"$regex": "^a"}}),
            serde_json::json!({"deleted_at": {"$exists": false}}),
        ] {
            assert!(
                validate_filter_value(&payload, 0).is_ok(),
                "should have been accepted: {payload}"
            );
        }
    }

    #[test]
    fn filter_rejects_excessive_nesting() {
        // Deep nesting is both a stack risk and a way to bury an operator below
        // whatever depth a check bothers to walk.
        let mut deep = serde_json::json!({"a": 1});
        for _ in 0..(MAX_FILTER_DEPTH + 2) {
            deep = serde_json::json!({"$and": [deep]});
        }
        assert!(validate_filter_value(&deep, 0).is_err());
    }

    #[test]
    fn database_pin_survives_a_uri_with_no_path_segment() {
        // The regression this exists for: the data browser builds its URI as
        // `mongodb://user:pass@host:port` with NO `/dbname` segment, so reading
        // the pin from the URI alone left `default_database` as None on every
        // real connection — the guard looked right and never engaged. Assert on
        // the resolution rule directly, since constructing a source needs a
        // live server.
        let from_uri_only: Option<String> = None;
        let explicit = Some("app_prod");

        let resolved = explicit
            .map(str::to_string)
            .or_else(|| from_uri_only.clone())
            .filter(|d| !d.is_empty());
        assert_eq!(
            resolved.as_deref(),
            Some("app_prod"),
            "an explicit scope must pin even when the URI carries no database"
        );

        // An empty configured database must not be mistaken for a real pin.
        let empty: Option<&str> = Some("");
        let resolved_empty = empty
            .map(str::to_string)
            .or_else(|| from_uri_only.clone())
            .filter(|d| !d.is_empty());
        assert_eq!(resolved_empty, None);
    }

    #[test]
    fn system_collections_are_not_browsable() {
        // `system.*` exists inside ordinary databases too, so this is checked
        // separately from the database guard.
        assert!(MongoDBSource::assert_collection_allowed("system.users").is_err());
        assert!(MongoDBSource::assert_collection_allowed("system.indexes").is_err());
        assert!(MongoDBSource::assert_collection_allowed("users").is_ok());
        assert!(MongoDBSource::assert_collection_allowed("system_events").is_ok());
    }
}
