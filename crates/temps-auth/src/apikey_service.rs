// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::permissions::{Permission, Role};
use chrono::Utc;
use rand::RngExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::UtcDateTime;
use temps_database::DbConnection;
use temps_entities::api_keys::{ActiveModel as ApiKeyActiveModel, Entity as ApiKeyEntity};
use temps_entities::users;
use thiserror::Error;
use tracing::warn;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

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

impl From<temps_entities::api_keys::Model> for ApiKeyResponse {
    fn from(model: temps_entities::api_keys::Model) -> Self {
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
    pub name: String,
    #[schema(example = "admin")]
    pub role_type: String,
    #[schema(example = json!(["projects:read", "deployments:read"]))]
    pub permissions: Option<Vec<String>>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    /// Immutable optional deployment resource scope.
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateApiKeyRequest {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    #[schema(example = json!(["projects:read", "deployments:read"]))]
    pub permissions: Option<Vec<String>>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
}

#[derive(Error, Debug)]
pub enum ApiKeyServiceError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Internal server error: {0}")]
    InternalServerError(String),
}

impl ApiKeyServiceError {
    pub fn to_problem(&self) -> Problem {
        match self {
            ApiKeyServiceError::DatabaseError(e) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/database-error")
                    .title("Database Error")
                    .detail(format!("A database error occurred: {}", e))
                    .value("error_code", "DATABASE_ERROR")
                    .build()
            }
            ApiKeyServiceError::NotFound(msg) => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/api-key-not-found")
                .title("API Key Not Found")
                .detail(msg.clone())
                .value("error_code", "API_KEY_NOT_FOUND")
                .build(),
            ApiKeyServiceError::ValidationError(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .type_("https://temps.sh/probs/validation-error")
                .title("Validation Error")
                .detail(msg.clone())
                .value("error_code", "VALIDATION_ERROR")
                .build(),
            ApiKeyServiceError::Unauthorized(msg) => ErrorBuilder::new(StatusCode::UNAUTHORIZED)
                .type_("https://temps.sh/probs/unauthorized")
                .title("Unauthorized")
                .detail(msg.clone())
                .value("error_code", "UNAUTHORIZED")
                .build(),
            ApiKeyServiceError::Conflict(msg) => ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/conflict")
                .title("Conflict")
                .detail(msg.clone())
                .value("error_code", "CONFLICT")
                .build(),
            ApiKeyServiceError::InternalServerError(msg) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/internal-server-error")
                    .title("Internal Server Error")
                    .detail(msg.clone())
                    .value("error_code", "INTERNAL_SERVER_ERROR")
                    .build()
            }
        }
    }
}

/// A validated key retains its immutable scope through authentication adapters.
pub struct ValidatedApiKey {
    pub user: users::Model,
    pub role: Option<Role>,
    pub permissions: Option<Vec<Permission>>,
    pub name: String,
    pub id: i32,
    pub scope: Option<temps_authz::ApiKeyScope>,
}

impl ValidatedApiKey {
    pub fn into_auth_context(self) -> crate::context::AuthContext {
        crate::context::AuthContext::new_api_key(
            self.user,
            self.role,
            self.permissions,
            self.name,
            self.id,
        )
        .with_api_key_scope(self.scope)
    }
}

pub struct ApiKeyService {
    db: Arc<DbConnection>,
    /// Throttles `last_used_at` writes so repeated validations of the same
    /// key within the window skip the DB write entirely. See
    /// [`crate::last_used_throttle`].
    last_used_throttle: crate::last_used_throttle::LastUsedThrottle,
}

impl ApiKeyService {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self {
            db,
            last_used_throttle: crate::last_used_throttle::LastUsedThrottle::new(
                crate::last_used_throttle::LAST_USED_UPDATE_INTERVAL,
            ),
        }
    }

    async fn validate_scope<C: sea_orm::ConnectionTrait>(
        connection: &C,
        key_name: &str,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        lock: bool,
    ) -> Result<(), ApiKeyServiceError> {
        let Some(project_id) = project_id else {
            if let Some(environment_id) = environment_id {
                return Err(ApiKeyServiceError::ValidationError(format!(
                    "API key '{key_name}' needs a project scope for environment {environment_id}"
                )));
            }
            return Ok(());
        };
        if project_id <= 0 || environment_id.is_some_and(|id| id <= 0) {
            return Err(ApiKeyServiceError::ValidationError(format!(
                "API key '{key_name}' requires positive project and environment IDs"
            )));
        }
        let query = temps_entities::projects::Entity::find_by_id(project_id)
            .filter(temps_entities::projects::Column::DeletedAt.is_null())
            .filter(temps_entities::projects::Column::IsDeleted.eq(false));
        let query = if lock { query.lock_shared() } else { query };
        if query.one(connection).await?.is_none() {
            return Err(ApiKeyServiceError::NotFound(format!(
                "Project {project_id} for API key '{key_name}' was not found"
            )));
        }
        if let Some(environment_id) = environment_id {
            let query = temps_entities::environments::Entity::find_by_id(environment_id)
                .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
                .filter(temps_entities::environments::Column::DeletedAt.is_null());
            let query = if lock { query.lock_shared() } else { query };
            if query.one(connection).await?.is_none() {
                return Err(ApiKeyServiceError::NotFound(format!("Active environment {environment_id} in project {project_id} for API key '{key_name}' was not found")));
            }
        }
        Ok(())
    }

    pub async fn create_api_key(
        &self,
        user_id: i32,
        request: CreateApiKeyRequest,
    ) -> Result<CreateApiKeyResponse, ApiKeyServiceError> {
        // Validate role type and permissions
        let permissions_json = if request.role_type == "custom" {
            // For custom role, validate that permissions are provided
            let permissions = match request.permissions.as_ref() {
                Some(permissions) if !permissions.is_empty() => permissions,
                _ => {
                    return Err(ApiKeyServiceError::ValidationError(
                        "Custom role requires at least one permission".to_string(),
                    ));
                }
            };

            // Validate each permission
            for perm_str in permissions {
                if Permission::from_str(perm_str).is_none() {
                    return Err(ApiKeyServiceError::ValidationError(format!(
                        "Invalid permission: {}",
                        perm_str
                    )));
                }
            }

            // Store permissions as JSON string
            Some(serde_json::to_string(&permissions).map_err(|e| {
                ApiKeyServiceError::InternalServerError(format!(
                    "Failed to serialize permissions for API key '{}' of user {}: {}",
                    request.name, user_id, e
                ))
            })?)
        } else {
            // For predefined roles, validate the role exists
            if Role::from_str(&request.role_type).is_none() {
                return Err(ApiKeyServiceError::ValidationError(
                    format!("Invalid role type: {}. Valid roles are: admin, platform_admin, user, reader, api_reader, or custom", request.role_type)
                ));
            }
            None
        };

        if request.project_id.is_some()
            && request
                .expires_at
                .is_some_and(|expiry| expiry <= Utc::now())
        {
            return Err(ApiKeyServiceError::ValidationError(format!(
                "Expiry for API key '{}' must be in the future",
                request.name
            )));
        }
        temps_authz::ApiKeyScope::from_ids(request.project_id, request.environment_id).map_err(
            |error| {
                ApiKeyServiceError::ValidationError(format!("API key '{}': {error}", request.name))
            },
        )?;
        let txn = self.db.begin().await?;
        Self::validate_scope(
            &txn,
            &request.name,
            request.project_id,
            request.environment_id,
            true,
        )
        .await?;
        // Check if name is unique for this user
        let existing_key = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .filter(temps_entities::api_keys::Column::Name.eq(&request.name))
            .one(&txn)
            .await?;

        if existing_key.is_some() {
            return Err(ApiKeyServiceError::Conflict(
                "API key with this name already exists".to_string(),
            ));
        }

        // Generate API key
        let api_key = self.generate_api_key_for_scope(request.project_id);
        let key_hash = self.hash_api_key(&api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        let now = Utc::now();
        let expires_at = request.expires_at.or_else(|| {
            // Default expiration: 1 year from now
            Some(now + chrono::Duration::days(365))
        });

        let new_api_key = ApiKeyActiveModel {
            project_id: Set(request.project_id),
            environment_id: Set(request.environment_id),
            name: Set(request.name.clone()),
            key_hash: Set(key_hash),
            key_prefix: Set(key_prefix.clone()),
            user_id: Set(user_id),
            role_type: Set(request.role_type.clone()),
            permissions: Set(permissions_json),
            is_active: Set(true),
            expires_at: Set(expires_at),
            last_used_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        let api_key_model = new_api_key.insert(&txn).await?;
        txn.commit().await?;

        Ok(CreateApiKeyResponse {
            id: api_key_model.id,
            project_id: api_key_model.project_id,
            environment_id: api_key_model.environment_id,
            name: api_key_model.name,
            key_prefix,
            role_type: api_key_model.role_type,
            permissions: api_key_model
                .permissions
                .and_then(|p| serde_json::from_str(&p).ok()),
            api_key, // Only returned on creation
            expires_at: api_key_model.expires_at,
            created_at: api_key_model.created_at,
        })
    }

    pub async fn list_api_keys(
        &self,
        user_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<ApiKeyListResponse, ApiKeyServiceError> {
        let paginator = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .order_by_desc(temps_entities::api_keys::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let api_keys_models = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;

        let api_keys = api_keys_models
            .into_iter()
            .map(ApiKeyResponse::from)
            .collect();

        Ok(ApiKeyListResponse { api_keys, total })
    }

    pub async fn get_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("API key not found".to_string()))?;

        Ok(ApiKeyResponse::from(api_key))
    }

    pub async fn update_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
        request: UpdateApiKeyRequest,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("API key not found".to_string()))?;

        // Check if new name conflicts with existing keys
        if let Some(ref new_name) = request.name {
            if new_name != &api_key.name {
                let existing_key = ApiKeyEntity::find()
                    .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
                    .filter(temps_entities::api_keys::Column::Name.eq(new_name))
                    .filter(temps_entities::api_keys::Column::Id.ne(api_key_id))
                    .one(self.db.as_ref())
                    .await?;

                if existing_key.is_some() {
                    return Err(ApiKeyServiceError::Conflict(
                        "API key with this name already exists".to_string(),
                    ));
                }
            }
        }

        let mut api_key_active: ApiKeyActiveModel = api_key.into();

        if let Some(name) = request.name {
            api_key_active.name = Set(name);
        }
        if let Some(is_active) = request.is_active {
            api_key_active.is_active = Set(is_active);
        }
        if let Some(expires_at) = request.expires_at {
            api_key_active.expires_at = Set(Some(expires_at));
        }
        // Note: We no longer support custom permissions, only standard roles
        api_key_active.updated_at = Set(Utc::now());

        let updated_api_key = api_key_active.update(self.db.as_ref()).await?;

        Ok(ApiKeyResponse::from(updated_api_key))
    }

    /// Delete one of `user_id`'s keys, returning the deleted key's
    /// (secret-free) description so the caller can audit what was removed.
    pub async fn delete_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApiKeyServiceError::NotFound(format!(
                    "API key {api_key_id} not found for user {user_id}"
                ))
            })?;

        ApiKeyEntity::delete_by_id(api_key.id)
            .exec(self.db.as_ref())
            .await?;

        Ok(ApiKeyResponse::from(api_key))
    }

    pub async fn validate_api_key(
        &self,
        api_key: &str,
    ) -> Result<
        (
            users::Model,
            Option<Role>,
            Option<Vec<Permission>>,
            String,
            i32,
        ),
        ApiKeyServiceError,
    > {
        let validated = self.validate_api_key_with_scope(api_key).await?;
        if validated.scope.is_some() {
            return Err(ApiKeyServiceError::Unauthorized(
                "Scoped API keys require a scope-aware authentication adapter".into(),
            ));
        }
        Ok((
            validated.user,
            validated.role,
            validated.permissions,
            validated.name,
            validated.id,
        ))
    }

    pub async fn validate_api_key_with_scope(
        &self,
        api_key: &str,
    ) -> Result<ValidatedApiKey, ApiKeyServiceError> {
        let key_hash = self.hash_api_key(api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        let api_key_model = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::KeyHash.eq(&key_hash))
            .filter(temps_entities::api_keys::Column::KeyPrefix.eq(&key_prefix))
            .filter(temps_entities::api_keys::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::Unauthorized("Invalid API key".to_string()))?;

        let scope = temps_authz::ApiKeyScope::from_ids(
            api_key_model.project_id,
            api_key_model.environment_id,
        )
        .map_err(|error| {
            ApiKeyServiceError::Unauthorized(format!("API key {}: {error}", api_key_model.id))
        })?;
        if api_key.starts_with("tkp_") != scope.is_some() {
            return Err(ApiKeyServiceError::Unauthorized(format!(
                "API key {} has an incompatible scope prefix",
                api_key_model.id
            )));
        }
        Self::validate_scope(
            self.db.as_ref(),
            &api_key_model.name,
            api_key_model.project_id,
            api_key_model.environment_id,
            false,
        )
        .await?;

        // Check if expired
        if let Some(expires_at) = api_key_model.expires_at {
            if expires_at <= Utc::now() {
                return Err(ApiKeyServiceError::Unauthorized(
                    "API key has expired".to_string(),
                ));
            }
        }

        // Get user, excluding soft-deleted users
        let user = users::Entity::find_by_id(api_key_model.user_id)
            .filter(users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("User not found or deleted".to_string()))?;

        // Parse role or permissions based on role_type
        let (role, permissions) = if api_key_model.role_type == "custom" {
            // For custom role, parse permissions from JSON
            let perms = if let Some(perms_json) = &api_key_model.permissions {
                let perm_strings: Vec<String> = serde_json::from_str(perms_json).map_err(|_| {
                    ApiKeyServiceError::InternalServerError(
                        "Invalid permissions in database".to_string(),
                    )
                })?;

                let mut permissions = Vec::new();
                for perm_str in perm_strings {
                    if let Some(perm) = Permission::from_str(&perm_str) {
                        permissions.push(perm);
                    }
                }
                Some(permissions)
            } else {
                return Err(ApiKeyServiceError::InternalServerError(
                    "Custom role but no permissions defined".to_string(),
                ));
            };
            (None, perms)
        } else {
            // For predefined roles
            let role = Role::from_str(&api_key_model.role_type).ok_or_else(|| {
                ApiKeyServiceError::InternalServerError("Invalid role type in database".to_string())
            })?;
            (Some(role), None)
        };

        // Bound the stored grant by what the owner holds now, not what they
        // held when the key was minted.
        let owner_role = current_owner_role(self.db.as_ref(), user.id).await?;
        let (role, permissions) = cap_api_key_grant(role, permissions, &owner_role);

        self.record_last_used(&api_key_model).await;

        Ok(ValidatedApiKey {
            user,
            role,
            permissions,
            name: api_key_model.name,
            id: api_key_model.id,
            scope,
        })
    }

    /// Record that `api_key` just authenticated a request.
    ///
    /// Writes are throttled per key id so repeated validations within
    /// `LAST_USED_UPDATE_INTERVAL` skip the DB write, except a key's first
    /// use (`last_used_at` still empty), which is always written: the AI
    /// harness onboarding waits on exactly that timestamp, and a throttle
    /// primed under the same id (by the previous secret of a rotated key)
    /// must not hide it. The write is conditioned on the validated secret's
    /// hash. A failed write never fails the request; it is logged and the
    /// throttle entry dropped, so the next request retries.
    async fn record_last_used(&self, api_key: &temps_entities::api_keys::Model) {
        let first_use = api_key.last_used_at.is_none();
        // Always consult the throttle so a first use also starts the window.
        let throttle_allows = self.last_used_throttle.should_update(api_key.id);
        if !(first_use || throttle_allows) {
            return;
        }
        // Bound to the secret that was validated, not just the row: if the key
        // was rotated after validation read it, this matches no row, so the
        // replacement secret is never stamped as used by a request that sent
        // the old one. Zero rows affected is not an error for the request.
        let result = ApiKeyEntity::update_many()
            .col_expr(
                temps_entities::api_keys::Column::LastUsedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(temps_entities::api_keys::Column::Id.eq(api_key.id))
            .filter(temps_entities::api_keys::Column::KeyHash.eq(&api_key.key_hash))
            .exec(self.db.as_ref())
            .await;
        if let Err(error) = result {
            self.last_used_throttle.forget(api_key.id);
            warn!(
                api_key_id = api_key.id,
                user_id = api_key.user_id,
                key_prefix = %api_key.key_prefix,
                first_use,
                error = %error,
                "Failed to record last_used_at for API key {}; the request stays authenticated and the next one retries the write",
                api_key.id
            );
        }
    }

    /// The caller's AI harness connection status.
    ///
    /// Two indexed lookups on the caller's own keys: the dedicated
    /// [`AI_HARNESS_KEY_NAME`](crate::ai_harness::AI_HARNESS_KEY_NAME) key
    /// (names are unique per user), and, only while that key is unused, the
    /// most recently used device-login key created after it. Unlike the
    /// paginated key list it never misses the dedicated key behind newer ones.
    pub async fn ai_harness_status(
        &self,
        user_id: i32,
    ) -> Result<crate::ai_harness::AiHarnessStatusResponse, ApiKeyServiceError> {
        use temps_entities::api_keys::Column;

        let harness_key = ApiKeyEntity::find()
            .filter(Column::UserId.eq(user_id))
            .filter(Column::Name.eq(crate::ai_harness::AI_HARNESS_KEY_NAME))
            .one(self.db.as_ref())
            .await?;

        // The most recently used agent credential (device login or MCP
        // client key) of this user. With a dedicated key, only one created
        // after it counts (see `derive_ai_harness_status`); a used dedicated
        // key or a deactivated one decides on its own.
        let agent_key_query = || {
            ApiKeyEntity::find()
                .filter(Column::UserId.eq(user_id))
                .filter(Column::IsActive.eq(true))
                .filter(Column::ProjectId.is_null())
                .filter(
                    Condition::any()
                        .add(Column::Name.starts_with(crate::ai_harness::CLI_DEVICE_KEY_PREFIX))
                        .add(Column::Name.starts_with(crate::ai_harness::MCP_CLIENT_KEY_PREFIX)),
                )
                .filter(Column::LastUsedAt.is_not_null())
                .order_by_desc(Column::LastUsedAt)
        };
        let agent_key = match &harness_key {
            Some(key) if key.is_active && key.last_used_at.is_none() => {
                agent_key_query()
                    .filter(Column::CreatedAt.gte(key.created_at))
                    .one(self.db.as_ref())
                    .await?
            }
            Some(_) => None,
            None => agent_key_query().one(self.db.as_ref()).await?,
        };

        Ok(crate::ai_harness::derive_ai_harness_status(
            harness_key.as_ref(),
            agent_key.as_ref(),
        ))
    }

    pub async fn deactivate_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let request = UpdateApiKeyRequest {
            name: None,
            is_active: Some(false),
            permissions: None,
            expires_at: None,
        };
        self.update_api_key(user_id, api_key_id, request).await
    }

    pub async fn activate_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let request = UpdateApiKeyRequest {
            name: None,
            is_active: Some(true),
            permissions: None,
            expires_at: None,
        };
        self.update_api_key(user_id, api_key_id, request).await
    }

    /// Rotate an API key's secret in place.
    ///
    /// Generates a brand new plaintext secret for the same key record (same
    /// id, name, role/permissions, and expiry policy), overwrites the stored
    /// hash so the old secret stops authenticating immediately, and returns
    /// the new plaintext secret exactly once -- mirroring `create_api_key`'s
    /// "only shown at creation/rotation time" contract.
    pub async fn rotate_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<CreateApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApiKeyServiceError::NotFound(format!("API key {} not found", api_key_id))
            })?;

        let new_secret = self.generate_api_key_for_scope(api_key.project_id);
        let key_hash = self.hash_api_key(&new_secret);
        let key_prefix = new_secret.chars().take(8).collect::<String>();

        let mut api_key_active: ApiKeyActiveModel = api_key.into();
        api_key_active.key_hash = Set(key_hash);
        api_key_active.key_prefix = Set(key_prefix.clone());
        // The old secret is gone, so any "last used" timestamp recorded
        // against it no longer means anything for the new secret.
        api_key_active.last_used_at = Set(None);
        api_key_active.updated_at = Set(Utc::now());

        let updated = api_key_active.update(self.db.as_ref()).await?;

        Ok(CreateApiKeyResponse {
            id: updated.id,
            project_id: updated.project_id,
            environment_id: updated.environment_id,
            name: updated.name,
            key_prefix,
            role_type: updated.role_type,
            permissions: updated
                .permissions
                .and_then(|p| serde_json::from_str(&p).ok()),
            api_key: new_secret, // Only returned on rotation
            expires_at: updated.expires_at,
            created_at: updated.created_at,
        })
    }

    /// Generate a service-scoped metrics ingest key (`si_` prefix).
    ///
    /// The key is tied to the given `service_id` and uses the
    /// `metrics_ingest` role.  It never expires — it lives until the
    /// external service is deleted (cascade via FK).
    ///
    /// Returns the plaintext key (caller must inject into the container;
    /// it is never stored in plaintext).
    pub async fn create_service_ingest_key(
        &self,
        service_id: i32,
        service_name: &str,
        system_user_id: i32,
    ) -> Result<String, ApiKeyServiceError> {
        let random_part: String = rand::rng()
            .sample_iter(&rand::distr::Alphanumeric)
            .take(40)
            .map(char::from)
            .collect();
        let api_key = format!("si_{}", random_part);

        let key_hash = self.hash_api_key(&api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        temps_entities::api_keys::ActiveModel {
            name: Set(format!("metrics-ingest-{}", service_name)),
            key_hash: Set(key_hash),
            key_prefix: Set(key_prefix),
            user_id: Set(system_user_id),
            role_type: Set("metrics_ingest".to_string()),
            is_active: Set(true),
            service_id: Set(Some(service_id)),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(ApiKeyServiceError::DatabaseError)?;

        Ok(api_key)
    }

    /// Revoke all metrics ingest keys for a given service.
    /// Called when the service is being deleted (belt-and-suspenders alongside CASCADE).
    pub async fn revoke_service_ingest_keys(
        &self,
        service_id: i32,
    ) -> Result<(), ApiKeyServiceError> {
        temps_entities::api_keys::Entity::delete_many()
            .filter(temps_entities::api_keys::Column::ServiceId.eq(service_id))
            .exec(self.db.as_ref())
            .await
            .map_err(ApiKeyServiceError::DatabaseError)?;
        Ok(())
    }

    fn generate_api_key_for_scope(&self, project_id: Option<i32>) -> String {
        let secret = self.generate_api_key();
        if project_id.is_some() {
            // Older servers refuse this prefix instead of dropping the scope.
            secret.replacen("tk_", "tkp_", 1)
        } else {
            secret
        }
    }

    fn generate_api_key(&self) -> String {
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::rng();

        let prefix = "tk_";
        let random_part: String = (0..40)
            .map(|_| {
                let idx = rng.random_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();

        format!("{}{}", prefix, random_part)
    }

    pub(crate) fn hash_api_key(&self, api_key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(api_key.as_bytes());
        hex::encode(hasher.finalize())
    }
}

/// The role a user holds *right now*, resolved from `user_roles` on every
/// call and never cached on a credential.
///
/// Delegates to `user_service::current_session_role`, the mapping the
/// session middleware applies to browser sessions, so an API key is bounded by
/// exactly what its owner would get by signing in. Public so crates that
/// re-resolve a key outside this service (plugin host bridge, AI tool
/// refresh) apply the same ceiling.
pub async fn current_owner_role<C: sea_orm::ConnectionTrait>(
    db: &C,
    user_id: i32,
) -> Result<Role, sea_orm::DbErr> {
    crate::user_service::current_session_role(db, user_id).await
}

/// Narrow an API key's stored grant to what its owner's current role still
/// grants, so a key can never exceed the user who owns it.
///
/// The role/permission set is copied onto the key row when it is minted and
/// is not rewritten when the owner is later demoted. Without this cap an
/// `admin` key (including the one `temps login` mints) keeps admin access
/// for its whole lifetime after `DELETE /users/{id}/roles/admin`.
///
/// - Predefined role whose permissions are all still held by the owner: the
///   role is kept as-is, so role checks such as `is_admin()` keep working.
/// - Predefined role that now exceeds the owner: the key degrades to a
///   custom grant of the intersection. `effective_role` becomes `Custom`, so
///   role-based checks (`is_admin`, `is_instance_admin`) no longer pass.
/// - Custom permission list: filtered to the permissions the owner holds.
///
/// Service ingest keys (`metrics_ingest`) carry an empty permission set, so
/// they are always a subset of any owner and pass through unchanged; they
/// authenticate only on the OTel `si_` path, which is bound to a service
/// row, not to user permissions. Deployment tokens (`dt_`) are not owned by
/// a user and never reach this function.
pub fn cap_api_key_grant(
    role: Option<Role>,
    permissions: Option<Vec<Permission>>,
    owner_role: &Role,
) -> (Option<Role>, Option<Vec<Permission>>) {
    let owner_permissions = owner_role.permissions();
    match role {
        Some(role) => {
            let role_permissions = role.permissions();
            if role_permissions
                .iter()
                .all(|permission| owner_permissions.contains(permission))
            {
                (Some(role), permissions)
            } else {
                let capped = role_permissions
                    .iter()
                    .filter(|permission| owner_permissions.contains(permission))
                    .copied()
                    .collect();
                (None, Some(capped))
            }
        }
        None => {
            let capped = permissions
                .unwrap_or_default()
                .into_iter()
                .filter(|permission| owner_permissions.contains(permission))
                .collect();
            (None, Some(capped))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
    use temps_entities::{api_keys, users};
    use temps_test_utils::TestDatabase;

    async fn setup_test_env() -> (TestDatabase, ApiKeyService, users::Model) {
        let db = TestDatabase::with_migrations().await.unwrap();

        // Generate unique email to avoid conflicts in parallel tests
        let test_id = uuid::Uuid::new_v4();
        let email = format!("test_{}@example.com", test_id);

        // Create a test user with unique email
        let user = users::ActiveModel {
            email: Set(email),
            name: Set("Test User".to_string()),
            email_verified: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            mfa_enabled: Set(false),
            ..Default::default()
        };
        let user = user.insert(db.db.as_ref()).await.unwrap();

        // Keys are capped by their owner's current role, so the fixture owner
        // must actually hold the admin role the tests mint keys for.
        let user_service = crate::user_service::UserService::new(
            db.db.clone(),
            crate::mfa_guard::test_support::encryption(),
        );
        user_service.initialize_roles().await.unwrap();
        user_service
            .assign_role_by_type(user.id, temps_entities::types::RoleType::Admin)
            .await
            .unwrap();

        let api_key_service = ApiKeyService::new(db.db.clone());
        (db, api_key_service, user)
    }

    async fn create_test_api_key(
        db: &Arc<DbConnection>,
        user_id: i32,
        name: &str,
        role_type: &str,
    ) -> api_keys::Model {
        // Generate unique key_hash and key_prefix to avoid conflicts
        let unique_id = uuid::Uuid::new_v4();
        let api_key = api_keys::ActiveModel {
            name: Set(name.to_string()),
            key_hash: Set(format!("test_hash_{}", unique_id)),
            key_prefix: Set(format!("tk_{}", &unique_id.to_string()[..8])),
            user_id: Set(user_id),
            role_type: Set(role_type.to_string()),
            permissions: Set(None),
            is_active: Set(true),
            expires_at: Set(Some(Utc::now() + Duration::days(365))),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        api_key.insert(db.as_ref()).await.unwrap()
    }

    #[tokio::test]
    async fn api_key_scope_validation_precedes_database_and_retains_database_errors() {
        let service = ApiKeyService::new(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ));
        for (project_id, environment_id) in [(None, Some(9)), (Some(0), None)] {
            let error = service
                .create_api_key(
                    1,
                    CreateApiKeyRequest {
                        name: "invalid-scope".into(),
                        role_type: "reader".into(),
                        permissions: None,
                        expires_at: None,
                        project_id,
                        environment_id,
                    },
                )
                .await
                .unwrap_err();
            assert!(matches!(error, ApiKeyServiceError::ValidationError(_)));
        }
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("project lookup failed".into())])
            .into_connection();
        let service = ApiKeyService::new(Arc::new(db));
        let error = service
            .create_api_key(
                1,
                CreateApiKeyRequest {
                    name: "project-scope".into(),
                    role_type: "reader".into(),
                    permissions: None,
                    expires_at: None,
                    project_id: Some(7),
                    environment_id: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ApiKeyServiceError::DatabaseError(_)));
    }

    #[tokio::test]
    async fn scoped_api_key_lifecycle_preserves_scope_and_permissions() {
        let (db, service, user) = setup_test_env().await;
        db.db.execute_unprepared("INSERT INTO projects(id,name,slug,repo_name,repo_owner,directory,main_branch,preset,created_at,updated_at) VALUES(7001,'scope-fixture','scope-fixture','app','example','.','main','static',now(),now()); INSERT INTO environments(id,name,slug,subdomain,host,project_id,created_at,updated_at,upstreams) VALUES(9001,'preview','preview','scope-preview','scope.example',7001,now(),now(),'[]')").await.unwrap();
        let make_request = |name: &str, project_id, environment_id| CreateApiKeyRequest {
            name: name.into(),
            role_type: "custom".into(),
            permissions: Some(vec!["deployments:create".into()]),
            expires_at: Some(Utc::now() + Duration::days(1)),
            project_id,
            environment_id,
        };
        let key = service
            .create_api_key(user.id, make_request("scoped", Some(7001), Some(9001)))
            .await
            .unwrap();
        assert!(key.api_key.starts_with("tkp_"));
        assert!(service.validate_api_key(&key.api_key).await.is_err());
        let auth = service
            .validate_api_key_with_scope(&key.api_key)
            .await
            .unwrap()
            .into_auth_context();
        assert_eq!(auth.project_id(), Some(7001));
        assert_eq!(auth.api_key_scope().unwrap().environment_id, Some(9001));
        assert!(auth.has_permission(&Permission::DeploymentsCreate));
        assert!(!auth.has_permission(&Permission::ProjectsWrite));
        assert!(!auth.is_instance_admin());
        let scoped_admin = crate::AuthContext::new_api_key(
            auth.user.clone().unwrap(),
            Some(Role::Admin),
            None,
            "scoped-admin".into(),
            99,
        )
        .with_api_key_scope(auth.api_key_scope().copied());
        assert!(scoped_admin.is_admin());
        assert!(!scoped_admin.is_instance_admin());
        let serialized = serde_json::to_string(&auth).unwrap();
        let restored: crate::AuthContext = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.api_key_scope(), auth.api_key_scope());

        assert!(
            temps_authz::enforce_api_key_path(&auth, "/api/projects/7001/trigger-pipeline").is_ok()
        );
        assert!(
            temps_authz::enforce_api_key_path(&auth, "/api/projects/7002/trigger-pipeline")
                .is_err()
        );
        use axum::extract::FromRequestParts;
        let mut parts = axum::http::Request::builder()
            .uri("/projects/7001/trigger-pipeline")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        parts.extensions.insert(auth.clone());
        assert!(
            temps_authz::RequireAuth::from_request_parts(&mut parts, &())
                .await
                .is_ok()
        );
        parts
            .extensions
            .insert(axum::extract::OriginalUri("/api/settings".parse().unwrap()));
        assert_eq!(
            temps_authz::RequireAuth::from_request_parts(&mut parts, &())
                .await
                .err()
                .unwrap()
                .status_code,
            StatusCode::FORBIDDEN
        );
        let rotated = service.rotate_api_key(user.id, key.id).await.unwrap();
        assert!(rotated.api_key.starts_with("tkp_"));
        assert_eq!(
            (rotated.project_id, rotated.environment_id),
            (Some(7001), Some(9001))
        );
        assert!(service
            .validate_api_key_with_scope(&key.api_key)
            .await
            .is_err());
        assert!(service
            .validate_api_key_with_scope(&rotated.api_key)
            .await
            .is_ok());
        for (name, project, env) in [
            ("no-project", None, Some(9001)),
            ("missing", Some(7002), None),
            ("wrong-project", Some(7001), Some(9002)),
            ("invalid", Some(0), None),
        ] {
            assert!(
                service
                    .create_api_key(user.id, make_request(name, project, env))
                    .await
                    .is_err(),
                "{name}"
            );
        }
        db.db
            .execute_unprepared("UPDATE environments SET deleted_at=now() WHERE id=9001")
            .await
            .unwrap();
        assert!(service
            .validate_api_key_with_scope(&rotated.api_key)
            .await
            .is_err());
        assert!(service
            .create_api_key(user.id, make_request("deleted", Some(7001), Some(9001)))
            .await
            .is_err());
    }

    // API Key Creation Tests

    #[tokio::test]
    async fn test_create_api_key_with_admin_role() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Admin API Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: Some(Utc::now() + Duration::days(30)),
        };

        let response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        assert_eq!(response.name, "Admin API Key");
        assert_eq!(response.role_type, "admin");
        assert!(response.api_key.starts_with("tk_"));
        assert_eq!(response.api_key.len(), 43); // tk_ + 40 chars
        assert!(response.permissions.is_none());
    }

    #[tokio::test]
    async fn test_create_api_key_with_custom_role() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Custom API Key".to_string(),
            role_type: "custom".to_string(),
            permissions: Some(vec![
                "projects:read".to_string(),
                "deployments:read".to_string(),
            ]),
            expires_at: None,
        };

        let response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        assert_eq!(response.name, "Custom API Key");
        assert_eq!(response.role_type, "custom");
        assert!(response.permissions.is_some());
        let perms = response.permissions.unwrap();
        assert_eq!(perms.len(), 2);
        assert!(perms.contains(&"projects:read".to_string()));
    }

    #[tokio::test]
    async fn test_create_api_key_custom_without_permissions_fails() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Invalid Custom Key".to_string(),
            role_type: "custom".to_string(),
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
    }

    #[tokio::test]
    async fn test_create_api_key_with_invalid_role() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Invalid Role Key".to_string(),
            role_type: "invalid_role".to_string(),
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
    }

    #[tokio::test]
    async fn test_create_api_key_duplicate_name_fails() {
        let (_db, api_key_service, user) = setup_test_env().await;

        // Create first key
        let request1 = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Duplicate Name".to_string(),
            role_type: "reader".to_string(),
            permissions: None,
            expires_at: None,
        };

        api_key_service
            .create_api_key(user.id, request1)
            .await
            .unwrap();

        // Try to create second key with same name
        let request2 = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Duplicate Name".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request2).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Conflict(_));
    }

    #[tokio::test]
    async fn test_create_api_key_with_invalid_permission() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Invalid Permission Key".to_string(),
            role_type: "custom".to_string(),
            permissions: Some(vec!["invalid:permission".to_string()]),
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
    }

    // API Key Listing Tests

    #[tokio::test]
    async fn test_list_api_keys() {
        let (db, api_key_service, user) = setup_test_env().await;

        // Create multiple API keys
        create_test_api_key(&db.db, user.id, "Key 1", "admin").await;
        create_test_api_key(&db.db, user.id, "Key 2", "reader").await;
        create_test_api_key(&db.db, user.id, "Key 3", "user").await;

        let response = api_key_service.list_api_keys(user.id, 1, 10).await.unwrap();

        assert_eq!(response.total, 3);
        assert_eq!(response.api_keys.len(), 3);
        // Should be ordered by created_at DESC
        assert_eq!(response.api_keys[0].name, "Key 3");
        assert_eq!(response.api_keys[1].name, "Key 2");
        assert_eq!(response.api_keys[2].name, "Key 1");
    }

    #[tokio::test]
    async fn test_list_api_keys_pagination() {
        let (db, api_key_service, user) = setup_test_env().await;

        // Create 5 API keys
        for i in 1..=5 {
            create_test_api_key(&db.db, user.id, &format!("Key {}", i), "reader").await;
        }

        // Get first page
        let page1 = api_key_service.list_api_keys(user.id, 1, 2).await.unwrap();
        assert_eq!(page1.total, 5);
        assert_eq!(page1.api_keys.len(), 2);

        // Get second page
        let page2 = api_key_service.list_api_keys(user.id, 2, 2).await.unwrap();
        assert_eq!(page2.total, 5);
        assert_eq!(page2.api_keys.len(), 2);

        // Get third page
        let page3 = api_key_service.list_api_keys(user.id, 3, 2).await.unwrap();
        assert_eq!(page3.total, 5);
        assert_eq!(page3.api_keys.len(), 1);
    }

    #[tokio::test]
    async fn test_list_api_keys_empty() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let response = api_key_service.list_api_keys(user.id, 1, 10).await.unwrap();

        assert_eq!(response.total, 0);
        assert_eq!(response.api_keys.len(), 0);
    }

    // API Key Retrieval Tests

    #[tokio::test]
    async fn test_get_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let response = api_key_service
            .get_api_key(user.id, api_key.id)
            .await
            .unwrap();

        assert_eq!(response.id, api_key.id);
        assert_eq!(response.name, "Test Key");
        assert_eq!(response.role_type, "admin");
    }

    #[tokio::test]
    async fn test_get_api_key_not_found() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let result = api_key_service.get_api_key(user.id, 999999).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    #[tokio::test]
    async fn test_get_api_key_wrong_user() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        // Try to get key with different user ID
        let result = api_key_service.get_api_key(user.id + 1, api_key.id).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    // API Key Update Tests

    #[tokio::test]
    async fn test_update_api_key_name() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Old Name", "admin").await;

        let request = UpdateApiKeyRequest {
            name: Some("New Name".to_string()),
            is_active: None,
            permissions: None,
            expires_at: None,
        };

        let response = api_key_service
            .update_api_key(user.id, api_key.id, request)
            .await
            .unwrap();

        assert_eq!(response.name, "New Name");
        assert!(response.is_active);
    }

    #[tokio::test]
    async fn test_update_api_key_deactivate() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let request = UpdateApiKeyRequest {
            name: None,
            is_active: Some(false),
            permissions: None,
            expires_at: None,
        };

        let response = api_key_service
            .update_api_key(user.id, api_key.id, request)
            .await
            .unwrap();

        assert!(!response.is_active);
    }

    #[tokio::test]
    async fn test_update_api_key_expiration() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let new_expiry = Utc::now() + Duration::days(7);
        let request = UpdateApiKeyRequest {
            name: None,
            is_active: None,
            permissions: None,
            expires_at: Some(new_expiry),
        };

        let response = api_key_service
            .update_api_key(user.id, api_key.id, request)
            .await
            .unwrap();

        assert!(response.expires_at.is_some());
        let expires_at = response.expires_at.unwrap();
        assert!((expires_at.timestamp() - new_expiry.timestamp()).abs() < 2);
    }

    #[tokio::test]
    async fn test_update_api_key_duplicate_name_fails() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key1 = create_test_api_key(&db.db, user.id, "Key 1", "admin").await;
        let _api_key2 = create_test_api_key(&db.db, user.id, "Key 2", "reader").await;

        let request = UpdateApiKeyRequest {
            name: Some("Key 2".to_string()), // Try to rename to existing name
            is_active: None,
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service
            .update_api_key(user.id, api_key1.id, request)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Conflict(_));
    }

    #[tokio::test]
    async fn test_update_api_key_not_found() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = UpdateApiKeyRequest {
            name: Some("New Name".to_string()),
            is_active: None,
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service
            .update_api_key(user.id, 999999, request)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    // API Key Deletion Tests

    #[tokio::test]
    async fn test_delete_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        api_key_service
            .delete_api_key(user.id, api_key.id)
            .await
            .unwrap();

        // Verify key is deleted
        let result = api_key_service.get_api_key(user.id, api_key.id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_api_key_not_found() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let result = api_key_service.delete_api_key(user.id, 999999).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    #[tokio::test]
    async fn test_delete_api_key_wrong_user() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let result = api_key_service
            .delete_api_key(user.id + 1, api_key.id)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    // API Key Rotation Tests

    #[tokio::test]
    async fn test_rotate_api_key_invalidates_old_secret_and_issues_new_one() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let create_response = api_key_service
            .create_api_key(
                user.id,
                CreateApiKeyRequest {
                    project_id: None,
                    environment_id: None,
                    name: "Rotate Me".to_string(),
                    role_type: "admin".to_string(),
                    permissions: None,
                    expires_at: Some(Utc::now() + Duration::days(30)),
                },
            )
            .await
            .unwrap();

        // Old secret works before rotation.
        assert!(api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .is_ok());

        let rotated = api_key_service
            .rotate_api_key(user.id, create_response.id)
            .await
            .unwrap();

        // Same key record: id, name, role, and expiry policy unchanged.
        assert_eq!(rotated.id, create_response.id);
        assert_eq!(rotated.name, create_response.name);
        assert_eq!(rotated.role_type, create_response.role_type);
        assert_eq!(rotated.expires_at, create_response.expires_at);

        // A brand new plaintext secret was minted.
        assert_ne!(rotated.api_key, create_response.api_key);
        assert!(rotated.api_key.starts_with("tk_"));

        // The old secret must no longer authenticate ...
        let old_result = api_key_service
            .validate_api_key(&create_response.api_key)
            .await;
        assert!(old_result.is_err());
        assert!(matches!(
            old_result.unwrap_err(),
            ApiKeyServiceError::Unauthorized(_)
        ));

        // ... while the new secret does.
        let (validated_user, role, _, _, key_id) = api_key_service
            .validate_api_key(&rotated.api_key)
            .await
            .unwrap();
        assert_eq!(validated_user.id, user.id);
        assert_eq!(role, Some(Role::Admin));
        assert_eq!(key_id, create_response.id);
    }

    #[tokio::test]
    async fn test_rotate_api_key_not_found() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let result = api_key_service.rotate_api_key(user.id, 999999).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ApiKeyServiceError::NotFound(_)
        ));
    }

    #[tokio::test]
    async fn test_rotate_api_key_wrong_user_is_not_found() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let result = api_key_service
            .rotate_api_key(user.id + 1, api_key.id)
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ApiKeyServiceError::NotFound(_)
        ));
    }

    // API Key Validation Tests

    #[tokio::test]
    async fn test_validate_api_key_success() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Valid Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: Some(Utc::now() + Duration::days(30)),
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        let (validated_user, role, permissions, key_name, key_id) = api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        assert_eq!(validated_user.id, user.id);
        assert!(role.is_some());
        assert_eq!(role.unwrap(), Role::Admin);
        assert!(permissions.is_none());
        assert_eq!(key_name, "Valid Key");
        assert_eq!(key_id, create_response.id);
    }

    #[tokio::test]
    async fn test_validate_api_key_custom_permissions() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Custom Key".to_string(),
            role_type: "custom".to_string(),
            permissions: Some(vec!["projects:read".to_string()]),
            expires_at: None,
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        let (validated_user, role, permissions, _, _) = api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        assert_eq!(validated_user.id, user.id);
        assert!(role.is_none());
        assert!(permissions.is_some());
        let perms = permissions.unwrap();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0], Permission::ProjectsRead);
    }

    #[tokio::test]
    async fn test_validate_api_key_invalid() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let result = api_key_service
            .validate_api_key("tk_invalidkey123456")
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
    }

    #[tokio::test]
    async fn test_validate_api_key_expired() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Expired Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: Some(Utc::now() - Duration::days(1)), // Already expired
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        let result = api_key_service
            .validate_api_key(&create_response.api_key)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
    }

    #[tokio::test]
    async fn test_validate_api_key_inactive() {
        let (_db, api_key_service, user) = setup_test_env().await;

        // Create and then deactivate key
        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Inactive Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        api_key_service
            .deactivate_api_key(user.id, create_response.id)
            .await
            .unwrap();

        let result = api_key_service
            .validate_api_key(&create_response.api_key)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
    }

    #[tokio::test]
    async fn test_validate_api_key_updates_last_used() {
        let (db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Track Usage Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        // Validate the key
        api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        // Check that last_used_at was updated
        let api_key = api_keys::Entity::find_by_id(create_response.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert!(api_key.last_used_at.is_some());
    }

    #[tokio::test]
    async fn test_validate_api_key_repeat_call_skips_last_used_update() {
        let (db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: "Throttled Usage Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        let after_first_call = api_keys::Entity::find_by_id(create_response.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap()
            .last_used_at
            .expect("first validation should write last_used_at");

        // Immediately validate again — within LAST_USED_UPDATE_INTERVAL, the
        // throttle should skip the write entirely, so last_used_at must be
        // byte-identical to what the first call wrote.
        api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        let after_second_call = api_keys::Entity::find_by_id(create_response.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap()
            .last_used_at
            .expect("last_used_at should still be set");

        assert_eq!(
            after_first_call, after_second_call,
            "second validate_api_key call within the throttle window must not rewrite last_used_at"
        );
    }

    /// Rotation keeps the key id and resets `last_used_at`. When the old
    /// secret was used moments before, the per-id throttle was already
    /// primed, and before the first-use bypass the new secret's first request
    /// was never recorded: the AI harness onboarding then waited forever.
    #[tokio::test]
    async fn first_use_of_a_rotated_secret_is_recorded_inside_the_throttle_window() {
        let (db, service, user) = setup_test_env().await;
        let created = service
            .create_api_key(user.id, admin_key_request("Rotated harness key"))
            .await
            .unwrap();
        service.validate_api_key(&created.api_key).await.unwrap();

        let rotated = service.rotate_api_key(user.id, created.id).await.unwrap();
        let reset = api_keys::Entity::find_by_id(created.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reset.last_used_at, None, "rotation resets last_used_at");

        service.validate_api_key(&rotated.api_key).await.unwrap();
        let after = api_keys::Entity::find_by_id(created.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(
            after.last_used_at.is_some(),
            "the new secret's first use must be written even though the throttle window is open"
        );
    }

    /// Validation read the old secret; the key was rotated before the usage
    /// write ran. The write must not mark the replacement secret as used.
    #[tokio::test]
    async fn usage_write_racing_a_rotation_does_not_stamp_the_new_secret() {
        let (db, service, user) = setup_test_env().await;
        let created = service
            .create_api_key(user.id, admin_key_request("Interleaved rotation key"))
            .await
            .unwrap();
        let validated_row = api_keys::Entity::find_by_id(created.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(validated_row.last_used_at, None);

        service.rotate_api_key(user.id, created.id).await.unwrap();
        service.record_last_used(&validated_row).await;

        let after = api_keys::Entity::find_by_id(created.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(after.key_hash, validated_row.key_hash);
        assert_eq!(
            after.last_used_at, None,
            "a request that sent the rotated-out secret must not mark the new one used"
        );
    }

    /// A failed bookkeeping write must not fail authentication, and must not
    /// leave the throttle primed: the next request retries the write.
    #[tokio::test]
    async fn failed_last_used_write_is_retried_on_the_next_request() {
        // No results queued: every statement, including the UPDATE, errors.
        let service = ApiKeyService::new(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ));
        let now = Utc::now();
        let key = api_keys::Model {
            id: 41,
            name: "write-fails".to_string(),
            key_hash: "hash".to_string(),
            key_prefix: "tk_abcde".to_string(),
            user_id: 3,
            role_type: "admin".to_string(),
            permissions: None,
            is_active: true,
            expires_at: None,
            last_used_at: Some(now),
            created_at: now,
            updated_at: now,
            service_id: None,
            project_id: None,
            environment_id: None,
        };

        service.record_last_used(&key).await;

        assert!(
            service.last_used_throttle.should_update(key.id),
            "a failed write must clear the throttle entry so the next request retries"
        );
    }

    fn admin_key_request(name: &str) -> CreateApiKeyRequest {
        CreateApiKeyRequest {
            project_id: None,
            environment_id: None,
            name: name.to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        }
    }

    async fn set_created_at(
        db: &Arc<DbConnection>,
        key_id: i32,
        created_at: chrono::DateTime<Utc>,
    ) {
        let key = api_keys::Entity::find_by_id(key_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        let mut active: api_keys::ActiveModel = key.into();
        active.created_at = Set(created_at);
        active.update(db.as_ref()).await.unwrap();
    }

    #[tokio::test]
    async fn ai_harness_status_is_missing_without_a_dedicated_key() {
        let (_db, service, user) = setup_test_env().await;
        let status = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(status.status, crate::ai_harness::AiHarnessState::Missing);
        assert_eq!(status.key_id, None);
    }

    #[tokio::test]
    async fn ai_harness_status_waits_then_connects_when_the_dedicated_key_is_used() {
        let (_db, service, user) = setup_test_env().await;
        let created = service
            .create_api_key(
                user.id,
                admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
            )
            .await
            .unwrap();

        let waiting = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(waiting.status, crate::ai_harness::AiHarnessState::Waiting);
        assert_eq!(waiting.key_id, Some(created.id));

        service.validate_api_key(&created.api_key).await.unwrap();

        let connected = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(
            connected.status,
            crate::ai_harness::AiHarnessState::Connected
        );
        let via = connected.connected_via.unwrap();
        assert_eq!(via.id, created.id);
        assert_eq!(
            via.kind,
            crate::ai_harness::AiHarnessCredentialKind::DedicatedKey
        );
    }

    /// The reported case: the copied connect command fell through to the
    /// browser device login, which mints its own key. The harness works with
    /// that key and never sends the dedicated one.
    #[tokio::test]
    async fn ai_harness_status_connects_through_a_later_device_login() {
        let (db, service, user) = setup_test_env().await;
        let harness = service
            .create_api_key(
                user.id,
                admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
            )
            .await
            .unwrap();
        set_created_at(&db.db, harness.id, Utc::now() - Duration::minutes(10)).await;
        let cli = service
            .create_api_key(user.id, admin_key_request("cli-device:laptop-1767225600"))
            .await
            .unwrap();

        // Minted but not used yet: still waiting.
        let before = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(before.status, crate::ai_harness::AiHarnessState::Waiting);

        service.validate_api_key(&cli.api_key).await.unwrap();

        let status = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(status.status, crate::ai_harness::AiHarnessState::Connected);
        let via = status.connected_via.unwrap();
        assert_eq!(via.id, cli.id);
        assert_eq!(
            via.kind,
            crate::ai_harness::AiHarnessCredentialKind::CliLogin
        );
        assert_eq!(status.key_id, Some(harness.id));
    }

    /// The welcome step never creates the dedicated key: a device login, or
    /// the Claude app's MCP key, connecting is what it waits for.
    #[tokio::test]
    async fn ai_harness_status_connects_without_a_dedicated_key_once_an_agent_key_is_used() {
        let (_db, service, user) = setup_test_env().await;
        let mut request = admin_key_request("mcp-claude-app-2026-11-25T10-00-00Z");
        request.role_type = "reader".to_string();
        let mcp = service.create_api_key(user.id, request).await.unwrap();
        // A hand-made key in use proves nothing.
        let manual = service
            .create_api_key(user.id, admin_key_request("deploy from CI"))
            .await
            .unwrap();
        service.validate_api_key(&manual.api_key).await.unwrap();

        let before = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(before.status, crate::ai_harness::AiHarnessState::Missing);

        service.validate_api_key(&mcp.api_key).await.unwrap();

        let status = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(status.status, crate::ai_harness::AiHarnessState::Connected);
        let via = status.connected_via.unwrap();
        assert_eq!(via.id, mcp.id);
        assert_eq!(
            via.kind,
            crate::ai_harness::AiHarnessCredentialKind::McpClient
        );
        assert_eq!(status.key_id, None);
    }

    #[tokio::test]
    async fn ai_harness_status_ignores_earlier_and_other_users_device_logins() {
        let (db, service, user) = setup_test_env().await;
        // A device login from before onboarding started, in active use.
        let earlier = service
            .create_api_key(user.id, admin_key_request("cli-device:desktop-1700000000"))
            .await
            .unwrap();
        set_created_at(&db.db, earlier.id, Utc::now() - Duration::days(2)).await;
        service.validate_api_key(&earlier.api_key).await.unwrap();

        service
            .create_api_key(
                user.id,
                admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
            )
            .await
            .unwrap();

        // Another user's fresh device login, also in use.
        let other = users::ActiveModel {
            email: Set(format!("other_{}@example.com", uuid::Uuid::new_v4())),
            name: Set("Other User".to_string()),
            email_verified: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            mfa_enabled: Set(false),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        let foreign = create_test_api_key(&db.db, other.id, "cli-device:other-1", "admin").await;
        let mut foreign: api_keys::ActiveModel = foreign.into();
        foreign.last_used_at = Set(Some(Utc::now()));
        foreign.update(db.db.as_ref()).await.unwrap();

        let status = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(status.status, crate::ai_harness::AiHarnessState::Waiting);
    }

    /// The console used to look for the dedicated key in the first 100 keys
    /// (newest first); every `temps login` mints a key, so it fell off that
    /// page. The status lookup does not paginate.
    #[tokio::test]
    async fn ai_harness_status_finds_the_dedicated_key_behind_a_full_page_of_newer_keys() {
        let (db, service, user) = setup_test_env().await;
        let harness = create_test_api_key(
            &db.db,
            user.id,
            crate::ai_harness::AI_HARNESS_KEY_NAME,
            "admin",
        )
        .await;
        set_created_at(&db.db, harness.id, Utc::now() - Duration::days(1)).await;
        let mut used: api_keys::ActiveModel = harness.clone().into();
        used.last_used_at = Set(Some(Utc::now()));
        used.update(db.db.as_ref()).await.unwrap();
        for index in 0..100 {
            create_test_api_key(&db.db, user.id, &format!("newer key {index}"), "admin").await;
        }

        let first_page = service.list_api_keys(user.id, 1, 100).await.unwrap();
        assert!(
            first_page.api_keys.iter().all(|key| key.id != harness.id),
            "precondition: the dedicated key is not on the first page of 100"
        );

        let status = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(status.status, crate::ai_harness::AiHarnessState::Connected);
        assert_eq!(status.key_id, Some(harness.id));
    }

    #[tokio::test]
    async fn ai_harness_status_reports_a_deactivated_key_instead_of_offering_a_conflicting_create()
    {
        let (_db, service, user) = setup_test_env().await;
        let created = service
            .create_api_key(
                user.id,
                admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
            )
            .await
            .unwrap();
        service
            .deactivate_api_key(user.id, created.id)
            .await
            .unwrap();

        let status = service.ai_harness_status(user.id).await.unwrap();
        assert_eq!(status.status, crate::ai_harness::AiHarnessState::Missing);
        assert_eq!(status.key_id, Some(created.id));
        assert_eq!(
            status.setup_path,
            Some(format!("/settings/keys/{}", created.id))
        );
    }

    #[tokio::test]
    async fn ai_harness_status_surfaces_database_errors() {
        let service = ApiKeyService::new(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("api_keys lookup failed".into())])
                .into_connection(),
        ));
        let error = service.ai_harness_status(5).await.unwrap_err();
        assert!(matches!(error, ApiKeyServiceError::DatabaseError(_)));
    }

    #[tokio::test]
    async fn delete_returns_the_deleted_key_and_names_the_missing_id() {
        let (_db, service, user) = setup_test_env().await;
        let created = service
            .create_api_key(user.id, admin_key_request("Delete and describe"))
            .await
            .unwrap();
        let deleted = service.delete_api_key(user.id, created.id).await.unwrap();
        assert_eq!(deleted.id, created.id);
        assert_eq!(deleted.name, "Delete and describe");

        let error = service
            .delete_api_key(user.id, created.id)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ApiKeyServiceError::NotFound(message) if message.contains(&created.id.to_string())),
            "{error}"
        );
    }

    // Activation/Deactivation Tests

    #[tokio::test]
    async fn test_deactivate_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let response = api_key_service
            .deactivate_api_key(user.id, api_key.id)
            .await
            .unwrap();

        assert!(!response.is_active);
    }

    #[tokio::test]
    async fn test_activate_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        // Create inactive key
        let unique_id = uuid::Uuid::new_v4();
        let api_key = api_keys::ActiveModel {
            name: Set("Inactive Key".to_string()),
            key_hash: Set(format!("test_hash_{}", unique_id)),
            key_prefix: Set(format!("tk_{}", &unique_id.to_string()[..8])),
            user_id: Set(user.id),
            role_type: Set("admin".to_string()),
            is_active: Set(false), // Start as inactive
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let api_key = api_key.insert(db.db.as_ref()).await.unwrap();

        let response = api_key_service
            .activate_api_key(user.id, api_key.id)
            .await
            .unwrap();

        assert!(response.is_active);
    }

    // Helper Method Tests

    #[tokio::test]
    async fn test_generate_api_key_format() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let key = api_key_service.generate_api_key();

        assert!(key.starts_with("tk_"));
        assert_eq!(key.len(), 43); // tk_ + 40 random chars

        // Check that it only contains valid characters
        let valid_chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for c in key[3..].chars() {
            assert!(valid_chars.contains(c));
        }
    }

    #[tokio::test]
    async fn test_hash_api_key_consistency() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let api_key = "tk_testkey123456";

        let hash1 = api_key_service.hash_api_key(api_key);
        let hash2 = api_key_service.hash_api_key(api_key);

        // Same input should produce same hash
        assert_eq!(hash1, hash2);

        // Hash should be 64 chars (SHA256 hex)
        assert_eq!(hash1.len(), 64);
    }

    #[tokio::test]
    async fn test_hash_api_key_different_inputs() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let hash1 = api_key_service.hash_api_key("tk_key1");
        let hash2 = api_key_service.hash_api_key("tk_key2");

        // Different inputs should produce different hashes
        assert_ne!(hash1, hash2);
    }

    // ── Service ingest key ───────────────────────────────────────────────────────

    #[test]
    fn test_si_token_has_correct_prefix() {
        // Verify the prefix used for service ingest tokens
        let token = "si_ABCDEFGHIJ1234567890abcdefghij12345678901234567890";
        assert!(token.starts_with("si_"));
        assert!(token.len() > 40);
    }

    #[test]
    fn test_service_ingest_key_name_format() {
        let service_name = "my-rustfs";
        let key_name = format!("metrics-ingest-{}", service_name);
        assert_eq!(key_name, "metrics-ingest-my-rustfs");
    }

    #[test]
    fn test_service_ingest_role_type_string() {
        // The role_type stored in DB for service keys
        use crate::permissions::Role;
        let role = Role::MetricsIngest;
        assert_eq!(role.to_string(), "metrics_ingest");
        assert!(Role::from_str("metrics_ingest").is_some());
    }

    #[test]
    fn test_metrics_ingest_role_has_no_permissions() {
        use crate::permissions::Role;
        let perms = Role::MetricsIngest.permissions();
        assert!(
            perms.is_empty(),
            "MetricsIngest role should have no UI permissions"
        );
    }

    // Owner-role ceiling tests

    #[test]
    fn admin_role_holds_every_other_predefined_role_permission() {
        // The cap keeps a predefined role intact only while the owner's role
        // covers it. Admin-owned keys of any predefined role must therefore
        // survive the cap unchanged, or every admin's restricted keys would
        // silently degrade to custom grants.
        for role in Role::all() {
            for permission in role.permissions() {
                assert!(
                    Role::Admin.has_permission(permission),
                    "Admin must hold {permission:?} (granted by {role:?})"
                );
            }
        }
    }

    #[test]
    fn cap_keeps_a_role_the_owner_still_covers() {
        let (role, permissions) = cap_api_key_grant(Some(Role::Admin), None, &Role::Admin);
        assert_eq!(role, Some(Role::Admin));
        assert!(permissions.is_none());

        let (role, permissions) = cap_api_key_grant(Some(Role::PlatformAdmin), None, &Role::Admin);
        assert_eq!(role, Some(Role::PlatformAdmin));
        assert!(permissions.is_none());
    }

    #[test]
    fn cap_degrades_an_admin_key_owned_by_a_demoted_user() {
        let (role, permissions) = cap_api_key_grant(Some(Role::Admin), None, &Role::User);
        assert_eq!(role, None, "an admin key must lose its admin role");
        let permissions = permissions.expect("a capped key carries an explicit grant");
        for permission in &permissions {
            assert!(Role::User.has_permission(permission));
        }
        assert!(!permissions.contains(&Permission::UsersManage));
        assert!(!permissions.contains(&Permission::SettingsWrite));

        let ctx = crate::context::AuthContext::new_api_key(
            users::Model {
                id: 1,
                name: "demoted".into(),
                email: "demoted@example.com".into(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            role,
            Some(permissions),
            "demoted-key".into(),
            1,
        );
        assert!(!ctx.is_admin());
        assert!(!ctx.is_instance_admin());
        assert!(!ctx.has_permission(&Permission::UsersManage));
    }

    #[test]
    fn cap_filters_custom_permissions_to_the_owner() {
        let (role, permissions) = cap_api_key_grant(
            None,
            Some(vec![Permission::ProjectsRead, Permission::UsersManage]),
            &Role::User,
        );
        assert_eq!(role, None);
        assert_eq!(permissions, Some(vec![Permission::ProjectsRead]));
    }

    #[test]
    fn cap_leaves_service_ingest_keys_unchanged() {
        // metrics_ingest keys carry no permission set; any owner covers them.
        let (role, permissions) = cap_api_key_grant(Some(Role::MetricsIngest), None, &Role::User);
        assert_eq!(role, Some(Role::MetricsIngest));
        assert!(permissions.is_none());
    }

    async fn mint_key(service: &ApiKeyService, user_id: i32, name: &str, role: &str) -> String {
        service
            .create_api_key(
                user_id,
                CreateApiKeyRequest {
                    project_id: None,
                    environment_id: None,
                    name: name.to_string(),
                    role_type: role.to_string(),
                    permissions: None,
                    expires_at: Some(Utc::now() + Duration::days(30)),
                },
            )
            .await
            .unwrap()
            .api_key
    }

    #[tokio::test]
    async fn admin_key_loses_admin_grant_when_owner_is_demoted() {
        let (db, api_key_service, user) = setup_test_env().await;
        let admin_key = mint_key(&api_key_service, user.id, "admin-key", "admin").await;
        let user_key = mint_key(&api_key_service, user.id, "user-key", "user").await;

        let (_, role, permissions, _, _) =
            api_key_service.validate_api_key(&admin_key).await.unwrap();
        assert_eq!(role, Some(Role::Admin));
        assert!(permissions.is_none());

        crate::user_service::UserService::new(
            db.db.clone(),
            crate::mfa_guard::test_support::encryption(),
        )
        .remove_role_from_user(user.id, temps_entities::types::RoleType::Admin)
        .await
        .unwrap();

        let (_, role, permissions, _, _) =
            api_key_service.validate_api_key(&admin_key).await.unwrap();
        assert_eq!(role, None, "the demoted owner's admin key keeps no role");
        let permissions = permissions.unwrap();
        assert!(!permissions.contains(&Permission::UsersManage));
        assert!(permissions
            .iter()
            .all(|permission| Role::User.has_permission(permission)));

        // A key that never exceeded the owner keeps working unchanged.
        let (_, role, permissions, _, _) =
            api_key_service.validate_api_key(&user_key).await.unwrap();
        assert_eq!(role, Some(Role::User));
        assert!(permissions.is_none());

        // Re-promotion restores the stored grant: nothing was rewritten.
        crate::user_service::UserService::new(
            db.db.clone(),
            crate::mfa_guard::test_support::encryption(),
        )
        .assign_role_by_type(user.id, temps_entities::types::RoleType::Admin)
        .await
        .unwrap();
        let (_, role, _, _, _) = api_key_service.validate_api_key(&admin_key).await.unwrap();
        assert_eq!(role, Some(Role::Admin));
    }

    // Error Conversion Tests

    #[tokio::test]
    async fn test_error_to_problem_conversion() {
        let db_error =
            ApiKeyServiceError::DatabaseError(sea_orm::DbErr::RecordNotFound("test".to_string()));
        let problem = db_error.to_problem();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);

        let not_found = ApiKeyServiceError::NotFound("Key not found".to_string());
        let problem = not_found.to_problem();
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);

        let validation = ApiKeyServiceError::ValidationError("Invalid input".to_string());
        let problem = validation.to_problem();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);

        let unauthorized = ApiKeyServiceError::Unauthorized("Invalid key".to_string());
        let problem = unauthorized.to_problem();
        assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);

        let conflict = ApiKeyServiceError::Conflict("Name exists".to_string());
        let problem = conflict.to_problem();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }
}
