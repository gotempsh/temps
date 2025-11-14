# temps-query-mongodb

MongoDB implementation of the `temps-query` DataSource trait for document database access.

## Features

- **Generic Hierarchy Support**: Uses the same ContainerPath-based API as other backends
- **Database Listing**: List all databases in MongoDB instance
- **Collection Listing**: List collections within databases
- **Collection Metadata**: Get document counts and inferred schemas
- **Schema Inference**: Automatically infer schema from sample documents

## Hierarchy Structure

MongoDB uses a simple 2-level hierarchy:

- **Depth 0**: List databases
- **Depth 1**: List collections in database

### Examples

```rust
use temps_query_mongodb::MongoDBSource;
use temps_query::{DataSource, ContainerPath};

// Create MongoDB source
let source = MongoDBSource::new("mongodb://localhost:27017").await?;

// List all databases (depth 0)
let databases = source.list_containers(&ContainerPath::root()).await?;

// List collections in a database (depth 1)
let path = ContainerPath::from_slice(&["mydb"]);
let collections = source.list_entities(&path).await?;

// Get collection metadata
let path = ContainerPath::from_slice(&["mydb"]);
let collection_info = source.get_entity_info(&path, "users").await?;

// Get inferred schema
let schema = source.get_schema(&path, "users").await?;
```

## API Endpoints

When integrated with temps-providers, MongoDB sources support these endpoints:

```bash
# List all databases
GET /external-services/{service_id}/query/containers

# Get database info
GET /external-services/{service_id}/query/containers/mydb/info

# List collections in database
GET /external-services/{service_id}/query/containers/mydb/entities

# Get collection metadata
GET /external-services/{service_id}/query/containers/mydb/entities/users
```

## Configuration

When creating an external service for MongoDB in temps-providers, use:

```json
{
  "service_type": "mongodb",
  "parameters": {
    "connection_string": "mongodb://username:password@localhost:27017"
  }
}
```

## Differences from PostgreSQL

| Feature | PostgreSQL | MongoDB |
|---------|-----------|---------|
| **Hierarchy** | 2 levels (database → schema) | 2 levels (database → collection) |
| **Containers** | Databases, Schemas | Databases only |
| **Entities** | Tables | Collections |
| **Schema** | Defined schema with types | Inferred from sample documents |
| **Schema Level** | Yes (public, etc.) | No (flat database structure) |

## Capabilities

```rust
source.capabilities() // Returns: vec![Capability::Document]
```

## Schema Inference

The MongoDB implementation automatically infers schemas by sampling a document from each collection. The inferred types are:

| BSON Type | temps-query FieldType |
|-----------|----------------------|
| String | String |
| Int32 | Int32 |
| Int64 | Int64 |
| Double | Float64 |
| Boolean | Boolean |
| DateTime | Timestamp |
| Array | Json |
| Document | Json |
| ObjectId | String |
| Null | String |

**Note**: Schema inference is based on a single sample document and may not represent all possible field types in the collection.

## Notes

- MongoDB has a flat database → collection structure (no schema level like PostgreSQL)
- Document counts use `estimated_document_count()` for better performance
- Schema inference samples one document per collection
- All fields are marked as nullable since MongoDB is schema-less
- Primary key is always `_id` (MongoDB's default)
