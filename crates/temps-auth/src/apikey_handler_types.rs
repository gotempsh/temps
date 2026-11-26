// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use temps_entities::api_keys::Model;
use utoipa::ToSchema;

// Response DTOs
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyResponse {
    pub id: i32,
    pub name: String,
    pub key_prefix: String,
    pub role_type: String,
    pub permissions: Option<Vec<String>>,
    pub is_active: bool,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub last_used_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    /// Immutable optional deployment resource scope.
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

impl From<Model> for ApiKeyResponse {
    fn from(model: Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            name: model.name,
            key_prefix: model.key_prefix,
            role_type: model.role_type,
            permissions: model
                .permissions
                .and_then(|p| serde_json::from_str(&p).ok()),
            is_active: model.is_active,
            expires_at: model.expires_at,
            last_used_at: model.last_used_at,
            created_at: model.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateApiKeyResponse {
    pub id: i32,
    pub name: String,
    pub key_prefix: String,
    pub role_type: String,
    pub permissions: Option<Vec<String>>,
    pub api_key: String, // Only returned on creation
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    /// Immutable optional deployment resource scope.
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyListResponse {
    pub api_keys: Vec<ApiKeyResponse>,
    pub total: u64,
}

// Request DTOs
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    /// Up to 255 characters, unique among the caller's keys. Surrounding
    /// whitespace is trimmed; a blank name is refused.
    pub name: String,
    /// A predefined role (`admin`, `platform_admin`, `user`, `reader`,
    /// `api_reader`) or `custom` for an explicit permission list.
    #[schema(example = "admin")]
    pub role_type: String,
    /// The key's permissions, required with `role_type: custom` and refused
    /// with any other role, which carries its own. Fixed once the key is
    /// created.
    #[schema(example = json!(["projects:read", "deployments:read"]))]
    pub permissions: Option<Vec<String>>,
    /// When the key stops working. Must be in the future. Defaults to one
    /// year after creation when left out: API keys always expire.
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    /// Immutable optional deployment resource scope.
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateApiKeyRequest {
    /// New name, trimmed; a blank name is refused. Leave it out to keep the
    /// current one.
    pub name: Option<String>,
    pub is_active: Option<bool>,
    /// A key's permissions are fixed when it is created. The key's current
    /// permissions are accepted unchanged; any other list is refused with 400.
    /// To change them, create a new key and delete this one.
    #[schema(example = json!(["projects:read", "deployments:read"]))]
    pub permissions: Option<Vec<String>>,
    /// A new expiry, which must be in the future. API keys always expire, so
    /// `null` is refused for a key that has an expiry. Leave it out to keep
    /// the current one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::apikey_service::explicit_null"
    )]
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<Option<UtcDateTime>>,
}

impl From<UpdateApiKeyRequest> for crate::apikey_service::UpdateApiKeyRequest {
    fn from(request: UpdateApiKeyRequest) -> Self {
        Self {
            name: request.name,
            is_active: request.is_active,
            permissions: request.permissions,
            expires_at: request.expires_at,
        }
    }
}

impl From<CreateApiKeyRequest> for crate::apikey_service::CreateApiKeyRequest {
    fn from(request: CreateApiKeyRequest) -> Self {
        Self {
            project_id: request.project_id,
            environment_id: request.environment_id,
            name: request.name,
            role_type: request.role_type,
            permissions: request.permissions,
            expires_at: request.expires_at,
        }
    }
}
