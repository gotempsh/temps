//! Request and response types for KV handlers

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use temps_core::AuditLogger;
use temps_providers::externalsvc::RedisService;
use temps_providers::ExternalServiceManager;
use utoipa::ToSchema;

use crate::KvService;

/// Application state for KV handlers
pub struct KvAppState {
    pub kv_service: Arc<KvService>,
    pub redis_service: Arc<RedisService>,
    pub external_service_manager: Arc<ExternalServiceManager>,
    pub audit_service: Arc<dyn AuditLogger>,
}

// =============================================================================
// Request Types
// =============================================================================

/// Request to get a value by key
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct GetRequest {
    /// The key to retrieve
    #[schema(example = "user:123")]
    pub key: String,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Request to set a value
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetRequest {
    /// The key to set
    #[schema(example = "user:123")]
    pub key: String,

    /// The value to store (can be any JSON value)
    #[schema(example = json!({"name": "John", "email": "john@example.com"}))]
    pub value: Value,

    /// Expire in seconds
    #[schema(example = 3600)]
    pub ex: Option<i64>,

    /// Expire in milliseconds
    pub px: Option<i64>,

    /// Only set if key does not exist
    #[serde(default)]
    pub nx: bool,

    /// Only set if key exists
    #[serde(default)]
    pub xx: bool,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Request to delete keys
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DelRequest {
    /// The key(s) to delete
    #[schema(example = json!(["user:123", "user:456"]))]
    pub keys: Vec<String>,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Request to increment a value
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct IncrRequest {
    /// The key to increment
    #[schema(example = "counter")]
    pub key: String,

    /// Amount to increment by (default: 1)
    #[serde(default = "default_incr_amount")]
    pub amount: Option<i64>,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

fn default_incr_amount() -> Option<i64> {
    Some(1)
}

/// Request to set expiration on a key
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ExpireRequest {
    /// The key to set expiration on
    #[schema(example = "session:abc")]
    pub key: String,

    /// Expiration time in seconds
    #[schema(example = 3600)]
    pub seconds: i64,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Request to get TTL for a key
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TtlRequest {
    /// The key to check TTL for
    #[schema(example = "session:abc")]
    pub key: String,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

/// Request to get keys matching a pattern
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct KeysRequest {
    /// Pattern to match (supports * and ? wildcards)
    #[schema(example = "user:*")]
    pub pattern: String,

    /// Project ID (required for API key/session auth, optional for deployment tokens)
    #[schema(example = 1)]
    pub project_id: Option<i32>,
}

// =============================================================================
// Response Types
// =============================================================================

/// Response for get operation
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GetResponse {
    /// The value, or null if not found
    pub value: Option<Value>,
}

/// Response for set operation
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SetResponse {
    /// Always "OK" on success
    #[schema(example = "OK")]
    pub result: String,
}

/// Response for delete operation
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DelResponse {
    /// Number of keys deleted
    #[schema(example = 2)]
    pub deleted: i64,
}

/// Response for increment operation
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IncrResponse {
    /// New value after increment
    #[schema(example = 42)]
    pub value: i64,
}

/// Response for expire operation
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExpireResponse {
    /// True if expiration was set, false if key doesn't exist
    pub success: bool,
}

/// Response for TTL operation
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TtlResponse {
    /// TTL in seconds, -1 if no expiration, -2 if key doesn't exist
    #[schema(example = 3600)]
    pub ttl: i64,
}

/// Response for keys operation
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct KeysResponse {
    /// List of matching keys
    #[schema(example = json!(["user:1", "user:2", "user:3"]))]
    pub keys: Vec<String>,
}

// =============================================================================
// Service Management Types
// =============================================================================

/// Response for KV service status
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct KvStatusResponse {
    /// Whether the KV service is enabled
    pub enabled: bool,

    /// Whether the underlying Redis service is healthy
    pub healthy: bool,

    /// Service version
    #[schema(example = "7.2")]
    pub version: Option<String>,

    /// Docker image being used
    #[schema(example = "redis:7-alpine")]
    pub docker_image: Option<String>,
}

/// Request to enable the KV service
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EnableKvRequest {
    /// Docker image to use (optional, uses default if not provided)
    #[schema(example = "redis:7-alpine")]
    pub docker_image: Option<String>,

    /// Maximum memory allocation (e.g., "256mb", "1gb")
    #[schema(example = "256mb")]
    pub max_memory: Option<String>,

    /// Enable data persistence
    #[serde(default = "default_persistence")]
    pub persistence: bool,
}

fn default_persistence() -> bool {
    true
}

/// Response after enabling KV service
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnableKvResponse {
    /// Whether the service was successfully enabled
    pub success: bool,

    /// Status message
    #[schema(example = "KV service enabled successfully")]
    pub message: String,

    /// Current service status
    pub status: KvStatusResponse,
}

/// Response after disabling KV service
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DisableKvResponse {
    /// Whether the service was successfully disabled
    pub success: bool,

    /// Status message
    #[schema(example = "KV service disabled successfully")]
    pub message: String,
}
