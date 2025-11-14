//! # temps-query
//!
//! Core abstractions for querying heterogeneous data sources in Temps.
//!
//! This crate provides a unified interface for accessing data from different backends:
//! - PostgreSQL (SQL)
//! - Redis (Key-Value)
//! - MongoDB (Document)
//! - S3/MinIO (Object Store)
//! - MySQL (SQL)
//!
//! ## Architecture
//!
//! The crate uses a trait-based architecture with capability detection:
//!
//! - **DataSource**: Core trait that all backends must implement
//! - **Introspect**: Optional trait for schema inspection
//! - **Queryable**: Optional trait for filtering and querying
//! - **ItemAccess**: Optional trait for key-based access
//! - **SqlFeature**: Optional trait for raw SQL execution
//! - **Transactional**: Optional trait for transaction support
//! - **Extensible**: Optional trait for backend-specific operations
//!
//! ## Example
//!
//! ```rust
//! use temps_query::{
//!     ConnectionConfig, QueryRegistry, DataSource, Queryable,
//!     EntityRef, QueryOptions, Capability,
//! };
//!
//! # async fn example() -> temps_query::error::Result<()> {
//! // Create registry
//! let registry = QueryRegistry::new();
//!
//! // Create connection config
//! let config = ConnectionConfig::new("postgres")
//!     .with_host("localhost")
//!     .with_port(5432)
//!     .with_database("mydb");
//!
//! // Create data source (requires factory registration first)
//! // let source = registry.create_source("my-postgres", config).await?;
//!
//! // Check capabilities
//! // if source.supports(Capability::Sql) {
//! //     // Use SQL features
//! // }
//!
//! # Ok(())
//! # }
//! ```
//!
//! ## Backend Implementation
//!
//! To implement a new backend:
//!
//! 1. Create a struct that implements `DataSource`
//! 2. Implement optional traits based on backend capabilities
//! 3. Create a `DataSourceFactory` implementation
//! 4. Register the factory with `QueryRegistry`
//!
//! Example backend crates:
//! - `temps-query-postgres` - PostgreSQL implementation
//! - `temps-query-redis` - Redis implementation (future)
//! - `temps-query-mongodb` - MongoDB implementation (future)

pub mod error;
pub mod registry;
pub mod traits;
pub mod types;

// Re-export commonly used items
pub use error::{DataError, Result};
pub use registry::{ConnectionConfig, DataSourceFactory, QueryRegistry};
pub use traits::{
    DataSource, Downloadable, Extensible, Introspect, ItemAccess, QuerySchemaProvider, Queryable,
    SqlFeature, Transactional,
};
pub use types::{
    Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType, DataRow,
    DatabaseInfo, DatasetSchema, EntityInfo, EntityRef, FieldDef, FieldType, NamespaceInfo,
    NamespaceRef, QueryOptions, QueryResult, QueryStats,
};
