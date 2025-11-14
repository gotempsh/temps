use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_query::{ContainerInfo, ContainerPath, EntityInfo, QueryOptions};
use utoipa::ToSchema;

use super::types::AppState;

// ============================================================================
// Request/Response Types
// ============================================================================

#[derive(Debug, Serialize, ToSchema)]
pub struct ContainerResponse {
    /// Container name
    #[schema(example = "mydb")]
    pub name: String,
    /// Container type (database, schema, keyspace, bucket, etc.)
    #[schema(example = "database")]
    pub container_type: String,
    /// Can this container hold other containers?
    #[schema(example = true)]
    pub can_contain_containers: bool,
    /// Can this container hold entities (tables, collections, etc.)?
    #[schema(example = false)]
    pub can_contain_entities: bool,
    /// Type of child containers (if can_contain_containers is true)
    #[schema(example = "schema")]
    pub child_container_type: Option<String>,
    /// Label for entity type (if can_contain_entities is true)
    #[schema(example = "table")]
    pub entity_type_label: Option<String>,
    /// Additional metadata
    pub metadata: serde_json::Value,
}

impl From<ContainerInfo> for ContainerResponse {
    fn from(info: ContainerInfo) -> Self {
        Self {
            name: info.name,
            container_type: info.container_type.to_string(),
            can_contain_containers: info.capabilities.can_contain_containers,
            can_contain_entities: info.capabilities.can_contain_entities,
            child_container_type: info
                .capabilities
                .child_container_type
                .map(|t| t.to_string()),
            entity_type_label: info.capabilities.entity_type_label,
            metadata: serde_json::to_value(info.metadata).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EntityResponse {
    /// Entity name (table/collection)
    #[schema(example = "users")]
    pub name: String,
    /// Entity type (table, view, collection, etc.)
    #[schema(example = "table")]
    pub entity_type: String,
    /// Approximate row count
    #[schema(example = 1234)]
    pub row_count: Option<usize>,
}

impl From<EntityInfo> for EntityResponse {
    fn from(info: EntityInfo) -> Self {
        Self {
            name: info.name,
            entity_type: info.entity_type,
            row_count: info.row_count,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FieldResponse {
    /// Field name
    #[schema(example = "id")]
    pub name: String,
    /// Field type (Int32, String, Timestamp, etc.)
    #[schema(example = "Int64")]
    pub field_type: String,
    /// Whether the field is nullable
    #[schema(example = false)]
    pub nullable: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EntityInfoResponse {
    /// Full container path
    #[schema(example = json!(["mydb", "public"]))]
    pub container_path: Vec<String>,
    /// Entity name
    #[schema(example = "users")]
    pub entity: String,
    /// Entity type
    #[schema(example = "table")]
    pub entity_type: String,
    /// Field definitions
    pub fields: Vec<FieldResponse>,
    /// JSON Schema for sort options (if supported)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_schema: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct QueryDataRequest {
    /// JSON filters (backend-specific format)
    #[serde(default)]
    pub filters: Option<serde_json::Value>,
    /// Maximum number of rows to return
    #[schema(example = 100)]
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Number of rows to skip
    #[schema(example = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Sort by field name
    #[serde(default)]
    pub sort_by: Option<String>,
    /// Sort order (asc/desc)
    #[serde(default)]
    pub sort_order: Option<String>,
}

fn default_limit() -> usize {
    100
}

/// Describes a level in the data source hierarchy
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct HierarchyLevel {
    /// Level number (0 = root)
    #[schema(example = 0)]
    pub level: u32,
    /// Human-readable name for this level
    #[schema(example = "root")]
    pub name: String,
    /// Type of container at this level
    #[schema(example = "database")]
    pub container_type: String,
    /// Can list containers at this level?
    #[schema(example = true)]
    pub can_list_containers: bool,
    /// Can list entities at this level?
    #[schema(example = false)]
    pub can_list_entities: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExplorerSupportResponse {
    /// Whether the service supports query explorer functionality
    #[schema(example = true)]
    pub supported: bool,
    /// Service type
    #[schema(example = "postgres")]
    pub service_type: String,
    /// Capabilities supported by this service
    #[schema(example = json!(["sql"]))]
    pub capabilities: Vec<String>,
    /// Hierarchy levels (describes the navigation structure)
    pub hierarchy: Vec<HierarchyLevel>,
    /// JSON Schema for filter format with embedded UI hints (if supported)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_schema: Option<serde_json::Value>,
    /// Reason why explorer is not supported (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QueryDataResponse {
    /// Field definitions
    pub fields: Vec<FieldResponse>,
    /// Data rows (array of JSON objects)
    pub rows: Vec<serde_json::Value>,
    /// Total number of rows matching the query (before limit/offset)
    #[schema(example = 1234)]
    pub total_count: u64,
    /// Number of rows returned in this response
    #[schema(example = 100)]
    pub returned_count: usize,
    /// Query execution time in milliseconds
    #[schema(example = 45)]
    pub execution_time_ms: u64,
}

// ============================================================================
// Handler Functions
// ============================================================================

/// Check if a service supports query explorer functionality
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/query/explorer-support",
    tag = "External Services - Query",
    responses(
        (status = 200, description = "Explorer support information", body = ExplorerSupportResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn check_explorer_support(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(service_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    // Get service configuration
    let service = app_state
        .external_service_manager
        .get_service_config(service_id)
        .await
        .map_err(|e| {
            temps_core::problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Service Not Found")
                .with_detail(format!("Service with ID {} not found: {}", service_id, e))
        })?;

    // Check if service type supports querying
    let (supported, capabilities, filter_schema, hierarchy, reason) = match service.service_type {
        crate::externalsvc::ServiceType::Postgres => {
            // Get filter schema from QueryService
            let filter_schema = app_state
                .query_service
                .get_filter_schema(service_id)
                .await
                .ok();

            // PostgreSQL has 3 levels: root -> database -> schema -> tables
            let hierarchy = vec![
                HierarchyLevel {
                    level: 0,
                    name: "root".to_string(),
                    container_type: "database".to_string(),
                    can_list_containers: true,
                    can_list_entities: false,
                },
                HierarchyLevel {
                    level: 1,
                    name: "database".to_string(),
                    container_type: "schema".to_string(),
                    can_list_containers: true,
                    can_list_entities: false,
                },
                HierarchyLevel {
                    level: 2,
                    name: "schema".to_string(),
                    container_type: "table".to_string(),
                    can_list_containers: false,
                    can_list_entities: true,
                },
            ];

            (
                true,
                vec!["sql".to_string()],
                filter_schema,
                hierarchy,
                None,
            )
        }
        crate::externalsvc::ServiceType::S3 => {
            // S3 has 2 levels: root -> buckets -> objects
            let hierarchy = vec![
                HierarchyLevel {
                    level: 0,
                    name: "root".to_string(),
                    container_type: "bucket".to_string(),
                    can_list_containers: true,
                    can_list_entities: false,
                },
                HierarchyLevel {
                    level: 1,
                    name: "bucket".to_string(),
                    container_type: "object".to_string(),
                    can_list_containers: false,
                    can_list_entities: true,
                },
            ];

            (
                true,
                vec!["object-store".to_string()],
                None,
                hierarchy,
                None,
            )
        }
        crate::externalsvc::ServiceType::Mongodb => {
            // MongoDB: root -> databases -> collections -> documents
            let hierarchy = vec![
                HierarchyLevel {
                    level: 0,
                    name: "root".to_string(),
                    container_type: "database".to_string(),
                    can_list_containers: true,
                    can_list_entities: false,
                },
                HierarchyLevel {
                    level: 1,
                    name: "database".to_string(),
                    container_type: "collection".to_string(),
                    can_list_containers: false,
                    can_list_entities: true,
                },
            ];

            (true, vec!["document".to_string()], None, hierarchy, None)
        }
        crate::externalsvc::ServiceType::Redis => {
            // Redis: flat key-value store (1 level)
            let hierarchy = vec![HierarchyLevel {
                level: 0,
                name: "root".to_string(),
                container_type: "key".to_string(),
                can_list_containers: false,
                can_list_entities: true,
            }];

            (true, vec!["key-value".to_string()], None, hierarchy, None)
        }
    };

    let response = ExplorerSupportResponse {
        supported,
        service_type: format!("{:?}", service.service_type).to_lowercase(),
        capabilities,
        hierarchy,
        filter_schema,
        reason,
    };

    Ok(Json(response))
}

/// List containers at the root level (databases, keyspaces, etc.)
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/query/containers",
    tag = "External Services - Query",
    responses(
        (status = 200, description = "List of root containers", body = Vec<ContainerResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_root_containers(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(service_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let path = ContainerPath::root();
    let containers = app_state
        .query_service
        .list_containers(service_id, &path)
        .await
        .map_err(|e| {
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Query Error")
                .with_detail(format!("Failed to list containers: {}", e))
        })?;

    let response: Vec<ContainerResponse> = containers.into_iter().map(Into::into).collect();
    Ok(Json(response))
}

/// List containers at a specific path
/// Path segments are separated by forward slashes
/// Example: /external-services/1/query/containers/mydb lists schemas in database "mydb"
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/query/containers/{path}",
    tag = "External Services - Query",
    responses(
        (status = 200, description = "List of containers", body = Vec<ContainerResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service or container not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_containers_at_path(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((service_id, path_str)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    // Parse path from URL (segments separated by /)
    let segments: Vec<String> = path_str.split('/').map(String::from).collect();
    let path = ContainerPath::new(segments);

    let containers = app_state
        .query_service
        .list_containers(service_id, &path)
        .await
        .map_err(|e| {
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Query Error")
                .with_detail(format!("Failed to list containers: {}", e))
        })?;

    let response: Vec<ContainerResponse> = containers.into_iter().map(Into::into).collect();
    Ok(Json(response))
}

/// Get information about a specific container
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/query/containers/{path}/info",
    tag = "External Services - Query",
    responses(
        (status = 200, description = "Container information", body = ContainerResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service or container not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_info(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((service_id, path_str)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let segments: Vec<String> = path_str.split('/').map(String::from).collect();
    let path = ContainerPath::new(segments);

    let container = app_state
        .query_service
        .get_container_info(service_id, &path)
        .await
        .map_err(|e| {
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Query Error")
                .with_detail(format!("Failed to get container info: {}", e))
        })?;

    Ok(Json(ContainerResponse::from(container)))
}

/// List entities (tables, collections, etc.) in a container
/// Example: /external-services/1/query/containers/mydb/public/entities lists tables in the public schema
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/query/containers/{path}/entities",
    tag = "External Services - Query",
    responses(
        (status = 200, description = "List of entities", body = Vec<EntityResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service or container not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_entities(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((service_id, path_str)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let segments: Vec<String> = path_str.split('/').map(String::from).collect();
    let path = ContainerPath::new(segments);

    let entities = app_state
        .query_service
        .list_entities(service_id, &path)
        .await
        .map_err(|e| {
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Query Error")
                .with_detail(format!("Failed to list entities: {}", e))
        })?;

    let response: Vec<EntityResponse> = entities.into_iter().map(Into::into).collect();
    Ok(Json(response))
}

/// Get detailed information about an entity (table schema)
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/query/containers/{path}/entities/{entity}",
    tag = "External Services - Query",
    responses(
        (status = 200, description = "Entity details", body = EntityInfoResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service, container, or entity not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_entity_info(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((service_id, path_str, entity)): Path<(i32, String, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let segments: Vec<String> = path_str.split('/').map(String::from).collect();
    let path = ContainerPath::new(segments.clone());

    let entity_info = app_state
        .query_service
        .get_entity_info(service_id, &path, &entity)
        .await
        .map_err(|e| {
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Query Error")
                .with_detail(format!("Failed to get entity info: {}", e))
        })?;

    let fields = entity_info
        .schema
        .map(|s| {
            s.fields
                .into_iter()
                .map(|f| FieldResponse {
                    name: f.name,
                    field_type: format!("{:?}", f.field_type),
                    nullable: f.nullable,
                })
                .collect()
        })
        .unwrap_or_default();

    // Get sort schema from QueryService
    let sort_schema = app_state
        .query_service
        .get_sort_schema(service_id, &path, &entity)
        .await
        .ok();

    let response = EntityInfoResponse {
        container_path: segments,
        entity: entity_info.name,
        entity_type: entity_info.entity_type,
        fields,
        sort_schema,
    };

    Ok(Json(response))
}

/// Query data from an entity with optional filters, pagination, and sorting
#[utoipa::path(
    post,
    path = "/external-services/{service_id}/query/containers/{path}/entities/{entity}/data",
    tag = "External Services - Query",
    request_body = QueryDataRequest,
    responses(
        (status = 200, description = "Query results", body = QueryDataResponse),
        (status = 400, description = "Invalid query"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Service, container, or entity not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn query_data(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((service_id, path_str, entity)): Path<(i32, String, String)>,
    Json(request): Json<QueryDataRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let segments: Vec<String> = path_str.split('/').map(String::from).collect();
    let path = ContainerPath::new(segments);

    let options = QueryOptions {
        limit: Some(request.limit),
        offset: Some(request.offset),
        cursor: None,
        sort_by: request.sort_by,
        sort_order: request.sort_order,
        timeout_ms: None,
        include_nulls: true,
    };

    let result = app_state
        .query_service
        .query_data(service_id, &path, &entity, request.filters, options)
        .await
        .map_err(|e| {
            // Determine if this is a user error (400) or server error (500)
            let (status, title) = match &e {
                temps_query::DataError::QueryFailed(_) => {
                    // Query syntax errors are user errors
                    (StatusCode::BAD_REQUEST, "Query Failed")
                }
                temps_query::DataError::InvalidQuery(_) => {
                    (StatusCode::BAD_REQUEST, "Invalid Query")
                }
                temps_query::DataError::NotFound(_) => (StatusCode::NOT_FOUND, "Not Found"),
                _ => {
                    // Other errors are server errors
                    (StatusCode::INTERNAL_SERVER_ERROR, "Query Error")
                }
            };

            temps_core::problemdetails::new(status)
                .with_title(title)
                .with_detail(e.to_string()) // Use to_string() instead of format! to avoid extra nesting
        })?;

    let response = QueryDataResponse {
        fields: result
            .schema
            .fields
            .into_iter()
            .map(|f| FieldResponse {
                name: f.name,
                field_type: format!("{:?}", f.field_type),
                nullable: f.nullable,
            })
            .collect(),
        rows: result
            .rows
            .into_iter()
            .map(|row| serde_json::to_value(row).unwrap_or_default())
            .collect(),
        total_count: result.stats.total_rows.unwrap_or(result.stats.row_count) as u64,
        returned_count: result.stats.row_count,
        execution_time_ms: result.stats.execution_ms,
    };

    Ok(Json(response))
}

/// Download an object (S3 only) as a streaming response
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/query/containers/{path}/entities/{entity}/download",
    tag = "External Services - Query",
    responses(
        (status = 200, description = "Object data stream", content_type = "application/octet-stream"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Object not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn download_object(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((service_id, path_str, entity)): Path<(i32, String, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    // Parse path
    let segments: Vec<&str> = if path_str.is_empty() {
        vec![]
    } else {
        path_str.split('/').collect()
    };
    let container_path = ContainerPath::from_slice(&segments);

    // Download object stream
    let (stream, content_type) = app_state
        .query_service
        .download(service_id, &container_path, &entity)
        .await
        .map_err(|e| {
            let (status, title) = match &e {
                temps_query::DataError::NotFound(_) => (StatusCode::NOT_FOUND, "Object Not Found"),
                temps_query::DataError::InvalidQuery(_) => {
                    (StatusCode::BAD_REQUEST, "Invalid Request")
                }
                temps_query::DataError::OperationNotSupported(_) => {
                    (StatusCode::BAD_REQUEST, "Operation Not Supported")
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "Download Error"),
            };
            temps_core::problemdetails::new(status)
                .with_title(title)
                .with_detail(e.to_string())
        })?;

    // Convert stream to Axum Body
    let body = Body::from_stream(stream);

    // Set response headers
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        content_type
            .unwrap_or_else(|| "application/octet-stream".to_string())
            .parse()
            .unwrap(),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", entity)
            .parse()
            .unwrap(),
    );

    Ok((headers, body))
}

// ============================================================================
// Route Configuration
// ============================================================================

pub fn configure_query_routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        // Explorer support check
        .route(
            "/external-services/{service_id}/query/explorer-support",
            axum::routing::get(check_explorer_support),
        )
        // Root containers
        .route(
            "/external-services/{service_id}/query/containers",
            axum::routing::get(list_root_containers),
        )
        // Containers at path
        .route(
            "/external-services/{service_id}/query/containers/{path}",
            axum::routing::get(list_containers_at_path),
        )
        // Container info
        .route(
            "/external-services/{service_id}/query/containers/{path}/info",
            axum::routing::get(get_container_info),
        )
        // Entities in container
        .route(
            "/external-services/{service_id}/query/containers/{path}/entities",
            axum::routing::get(list_entities),
        )
        // Entity info
        .route(
            "/external-services/{service_id}/query/containers/{path}/entities/{entity}",
            axum::routing::get(get_entity_info),
        )
        // Query entity data
        .route(
            "/external-services/{service_id}/query/containers/{path}/entities/{entity}/data",
            axum::routing::post(query_data),
        )
        // Download object (S3 only)
        .route(
            "/external-services/{service_id}/query/containers/{path}/entities/{entity}/download",
            axum::routing::get(download_object),
        )
}
