# temps-query-s3

S3/MinIO implementation of the `temps-query` DataSource trait for object storage access.

## Features

- **Generic Hierarchy Support**: Uses the same ContainerPath-based API as other backends
- **S3 & MinIO Compatible**: Works with AWS S3 and self-hosted MinIO servers
- **Bucket Management**: List and inspect S3 buckets
- **Object Listing**: List objects in buckets with optional prefix filtering
- **Object Metadata**: Get detailed information about objects (size, content type, last modified)

## Hierarchy Structure

S3 uses a flat namespace with buckets and objects:

- **Depth 0**: List buckets (root containers)
- **Depth 1+**: List objects in bucket (path segments become object prefix)

### Examples

```rust
use temps_query_s3::S3Source;
use temps_query::{DataSource, ContainerPath};

// Create S3 source
let source = S3Source::new(
    "us-east-1",                       // AWS region
    Some("http://localhost:9000"),    // Optional endpoint (for MinIO)
    "access_key",                      // Access key
    "secret_key",                      // Secret key
).await?;

// List all buckets (depth 0)
let buckets = source.list_containers(&ContainerPath::root()).await?;

// List objects in a bucket (depth 1)
let path = ContainerPath::from_slice(&["my-bucket"]);
let objects = source.list_entities(&path).await?;

// List objects with prefix (depth 2+)
let path = ContainerPath::from_slice(&["my-bucket", "uploads", "2024"]);
let objects = source.list_entities(&path).await?;
// Lists objects with prefix "uploads/2024/"

// Get object metadata
let path = ContainerPath::from_slice(&["my-bucket"]);
let object_info = source.get_entity_info(&path, "file.txt").await?;
```

## API Endpoints

When integrated with temps-providers, S3 sources support these endpoints:

```bash
# List all buckets
GET /external-services/{service_id}/query/containers

# Get bucket info
GET /external-services/{service_id}/query/containers/my-bucket/info

# List objects in bucket
GET /external-services/{service_id}/query/containers/my-bucket/entities

# List objects with prefix
GET /external-services/{service_id}/query/containers/my-bucket%2Fuploads/entities

# Get object metadata
GET /external-services/{service_id}/query/containers/my-bucket/entities/file.txt
```

## Configuration

When creating an external service for S3/MinIO in temps-providers, use:

```json
{
  "service_type": "s3",
  "parameters": {
    "region": "us-east-1",
    "endpoint": "http://localhost:9000",  // Optional, for MinIO
    "access_key": "your-access-key",
    "secret_key": "your-secret-key"
  }
}
```

## Differences from PostgreSQL

| Feature | PostgreSQL | S3 |
|---------|-----------|-----|
| **Hierarchy** | 2 levels (database → schema) | 1 level (bucket) |
| **Containers** | Databases, Schemas | Buckets only |
| **Entities** | Tables | Objects |
| **Prefixes** | Not applicable | Path segments become object prefix |
| **Schemas** | Rich table schemas | Basic metadata (size, type, modified) |

## Capabilities

```rust
source.capabilities() // Returns: vec![Capability::ObjectStore]
```

## Notes

- S3 uses a flat namespace - "folders" are just object key prefixes
- Path segments after the bucket name are joined with "/" to form the prefix
- For example: `ContainerPath::from_slice(&["bucket", "dir", "subdir"])` becomes prefix `"dir/subdir/"`
- Force path-style addressing is enabled for MinIO compatibility
