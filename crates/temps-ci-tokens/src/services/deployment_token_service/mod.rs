// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment Token Service
//!
//! Manages deployment tokens that provide API access credentials
//! for deployed applications via TEMPS_API_URL and TEMPS_API_TOKEN
//! environment variables.

use axum::http::StatusCode;
use chrono::Utc;
use rand::RngExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use std::sync::Arc;
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::EncryptionService;
use temps_core::UtcDateTime;
use temps_database::DbConnection;
use temps_entities::deployment_tokens::{
    ActiveModel as DeploymentTokenActiveModel, DeploymentTokenPermission,
    Entity as DeploymentTokenEntity, Model as DeploymentTokenModel,
};
use thiserror::Error;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Response DTOs

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeploymentTokenResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: String,
    pub token_prefix: String,
    pub permissions: Option<Vec<String>>,
    pub is_active: bool,
    #[schema(value_type = Option<String>, format = "date-time", example = "2030-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub last_used_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    pub created_by: Option<i32>,
}

impl From<DeploymentTokenModel> for DeploymentTokenResponse {
    fn from(model: DeploymentTokenModel) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            deployment_id: model.deployment_id,
            name: model.name,
            token_prefix: model.token_prefix,
            permissions: model
                .permissions
                .and_then(|p| serde_json::from_value(p).ok()),
            is_active: model.is_active,
            expires_at: model.expires_at,
            last_used_at: model.last_used_at,
            created_at: model.created_at,
            created_by: model.created_by,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateDeploymentTokenResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: String,
    pub token_prefix: String,
    pub permissions: Option<Vec<String>>,
    /// The full token value - only returned on creation
    pub token: String,
    #[schema(value_type = Option<String>, format = "date-time", example = "2030-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeploymentTokenListResponse {
    pub tokens: Vec<DeploymentTokenResponse>,
    pub total: u64,
}

// Request DTOs

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateDeploymentTokenRequest {
    /// Up to 255 characters, unique within the project. Surrounding
    /// whitespace is trimmed; a blank name is refused.
    pub name: String,
    /// Optional environment of this project - if not set, token applies to
    /// all environments. An environment of another project is refused.
    pub environment_id: Option<i32>,
    /// Optional deployment of this project (and of `environment_id`, when
    /// set) - if set, token is scoped to a specific deployment.
    pub deployment_id: Option<i32>,
    /// List of permissions (e.g., ["visitors:enrich", "emails:send"]).
    /// If not provided, defaults to full access (`["*"]`). An empty list is
    /// refused: the token could do nothing.
    #[schema(example = json!(["visitors:enrich", "emails:send"]))]
    pub permissions: Option<Vec<String>>,
    /// When the token stops working. Must be in the future; leave it out for
    /// a token that does not expire.
    #[schema(value_type = Option<String>, format = "date-time", example = "2030-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateDeploymentTokenRequest {
    /// New name. Leave it out to keep the current one.
    pub name: Option<String>,
    pub is_active: Option<bool>,
    /// Replaces the token's permissions. Leave it out to keep them; an empty
    /// list is refused.
    #[schema(example = json!(["visitors:enrich", "emails:send"]))]
    pub permissions: Option<Vec<String>>,
    /// A future time sets a new expiry, `null` removes it (the token no
    /// longer expires), and leaving the field out, or sending the current
    /// expiry back unchanged (even one that has passed), keeps it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "explicit_null"
    )]
    #[schema(value_type = Option<String>, format = "date-time", example = "2030-12-31T23:59:59Z")]
    pub expires_at: Option<Option<UtcDateTime>>,
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn explicit_null<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Longest accepted token name, in characters.
pub const TOKEN_NAME_MAX_CHARS: usize = 255;

#[derive(Error, Debug)]
pub enum DeploymentTokenServiceError {
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

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error(
        "Environment {environment_id} was not found in project {project_id}; a deployment \
         token can only be scoped to an environment of its own project"
    )]
    EnvironmentNotInProject {
        project_id: i32,
        environment_id: i32,
    },

    #[error(
        "Deployment {deployment_id} was not found in project {project_id}; a deployment \
         token can only be scoped to a deployment of its own project"
    )]
    DeploymentNotInProject { project_id: i32, deployment_id: i32 },

    #[error(
        "Deployment {deployment_id} belongs to environment {deployment_environment_id}, not \
         to the requested environment {environment_id}"
    )]
    DeploymentNotInEnvironment {
        deployment_id: i32,
        environment_id: i32,
        deployment_environment_id: i32,
    },

    #[error("Deployment token name for project {project_id} is invalid: {reason}")]
    InvalidName {
        project_id: i32,
        reason: &'static str,
    },

    #[error(
        "expires_at {expires_at} is in the past; use a future time, or leave expires_at out \
         for a token that does not expire"
    )]
    ExpiryInPast { expires_at: UtcDateTime },

    #[error(
        "permissions for a deployment token of project {project_id} is empty, so the token \
         could do nothing; list at least one permission, or leave permissions out for full access"
    )]
    EmptyPermissions { project_id: i32 },
}

impl DeploymentTokenServiceError {
    pub fn to_problem(&self) -> Problem {
        match self {
            DeploymentTokenServiceError::DatabaseError(e) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/database-error")
                    .title("Database Error")
                    .detail(format!("A database error occurred: {}", e))
                    .value("error_code", "DATABASE_ERROR")
                    .build()
            }
            DeploymentTokenServiceError::NotFound(msg) => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/deployment-token-not-found")
                .title("Deployment Token Not Found")
                .detail(msg.clone())
                .value("error_code", "DEPLOYMENT_TOKEN_NOT_FOUND")
                .build(),
            DeploymentTokenServiceError::ValidationError(msg) => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/validation-error")
                    .title("Validation Error")
                    .detail(msg.clone())
                    .value("error_code", "VALIDATION_ERROR")
                    .build()
            }
            DeploymentTokenServiceError::Unauthorized(msg) => {
                ErrorBuilder::new(StatusCode::UNAUTHORIZED)
                    .type_("https://temps.sh/probs/unauthorized")
                    .title("Unauthorized")
                    .detail(msg.clone())
                    .value("error_code", "UNAUTHORIZED")
                    .build()
            }
            DeploymentTokenServiceError::Conflict(msg) => ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/conflict")
                .title("Conflict")
                .detail(msg.clone())
                .value("error_code", "CONFLICT")
                .build(),
            DeploymentTokenServiceError::InternalServerError(msg) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/internal-server-error")
                    .title("Internal Server Error")
                    .detail(msg.clone())
                    .value("error_code", "INTERNAL_SERVER_ERROR")
                    .build()
            }
            DeploymentTokenServiceError::ProjectNotFound { .. } => {
                ErrorBuilder::new(StatusCode::NOT_FOUND)
                    .type_("https://temps.sh/probs/project-not-found")
                    .title("Project Not Found")
                    .detail(self.to_string())
                    .value("error_code", "PROJECT_NOT_FOUND")
                    .build()
            }
            DeploymentTokenServiceError::EnvironmentNotInProject { .. } => {
                ErrorBuilder::new(StatusCode::NOT_FOUND)
                    .type_("https://temps.sh/probs/environment-not-found")
                    .title("Environment Not Found")
                    .detail(self.to_string())
                    .value("error_code", "ENVIRONMENT_NOT_FOUND")
                    .build()
            }
            DeploymentTokenServiceError::DeploymentNotInProject { .. } => {
                ErrorBuilder::new(StatusCode::NOT_FOUND)
                    .type_("https://temps.sh/probs/deployment-not-found")
                    .title("Deployment Not Found")
                    .detail(self.to_string())
                    .value("error_code", "DEPLOYMENT_NOT_FOUND")
                    .build()
            }
            DeploymentTokenServiceError::DeploymentNotInEnvironment { .. }
            | DeploymentTokenServiceError::InvalidName { .. }
            | DeploymentTokenServiceError::ExpiryInPast { .. }
            | DeploymentTokenServiceError::EmptyPermissions { .. } => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/validation-error")
                    .title("Validation Error")
                    .detail(self.to_string())
                    .value("error_code", "VALIDATION_ERROR")
                    .build()
            }
        }
    }
}

/// Trims a requested token name and checks it is usable.
fn normalize_token_name(
    project_id: i32,
    name: &str,
) -> Result<String, DeploymentTokenServiceError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DeploymentTokenServiceError::InvalidName {
            project_id,
            reason: "the name is blank",
        });
    }
    if name.chars().count() > TOKEN_NAME_MAX_CHARS {
        return Err(DeploymentTokenServiceError::InvalidName {
            project_id,
            reason: "the name is longer than 255 characters",
        });
    }
    Ok(name.to_string())
}

/// Refuses an expiry that has already passed: the token could never work.
fn require_future_expiry(expires_at: UtcDateTime) -> Result<(), DeploymentTokenServiceError> {
    if expires_at <= Utc::now() {
        return Err(DeploymentTokenServiceError::ExpiryInPast { expires_at });
    }
    Ok(())
}

/// Refuses an explicit empty permission list: the token could do nothing.
fn require_permissions(
    project_id: i32,
    permissions: &[String],
) -> Result<(), DeploymentTokenServiceError> {
    if permissions.is_empty() {
        return Err(DeploymentTokenServiceError::EmptyPermissions { project_id });
    }
    Ok(())
}

pub struct DeploymentTokenService {
    db: Arc<DbConnection>,
    encryption_service: Arc<EncryptionService>,
}

impl DeploymentTokenService {
    pub fn new(db: Arc<DbConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Checks that the project exists and that the requested environment and
    /// deployment belong to it, so a token never spans two projects' scopes.
    async fn validate_scope(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> Result<(), DeploymentTokenServiceError> {
        let project = temps_entities::projects::Entity::find_by_id(project_id)
            .filter(temps_entities::projects::Column::IsDeleted.eq(false))
            .filter(temps_entities::projects::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?;
        if project.is_none() {
            return Err(DeploymentTokenServiceError::ProjectNotFound { project_id });
        }
        if let Some(environment_id) = environment_id {
            let environment = temps_entities::environments::Entity::find_by_id(environment_id)
                .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
                .filter(temps_entities::environments::Column::DeletedAt.is_null())
                .one(self.db.as_ref())
                .await?;
            if environment.is_none() {
                return Err(DeploymentTokenServiceError::EnvironmentNotInProject {
                    project_id,
                    environment_id,
                });
            }
        }
        if let Some(deployment_id) = deployment_id {
            let deployment = temps_entities::deployments::Entity::find_by_id(deployment_id)
                .filter(temps_entities::deployments::Column::ProjectId.eq(project_id))
                .one(self.db.as_ref())
                .await?
                .ok_or(DeploymentTokenServiceError::DeploymentNotInProject {
                    project_id,
                    deployment_id,
                })?;
            if let Some(environment_id) = environment_id {
                if deployment.environment_id != environment_id {
                    return Err(DeploymentTokenServiceError::DeploymentNotInEnvironment {
                        deployment_id,
                        environment_id,
                        deployment_environment_id: deployment.environment_id,
                    });
                }
            }
        }
        Ok(())
    }

    /// Create a new deployment token for a project
    pub async fn create_token(
        &self,
        project_id: i32,
        user_id: Option<i32>,
        request: CreateDeploymentTokenRequest,
    ) -> Result<CreateDeploymentTokenResponse, DeploymentTokenServiceError> {
        let name = normalize_token_name(project_id, &request.name)?;
        if let Some(expires_at) = request.expires_at {
            require_future_expiry(expires_at)?;
        }
        // Validate permissions if provided
        let permissions_json = if let Some(ref perms) = request.permissions {
            require_permissions(project_id, perms)?;
            for perm_str in perms {
                if DeploymentTokenPermission::from_str(perm_str).is_none() {
                    return Err(DeploymentTokenServiceError::ValidationError(format!(
                        "Invalid permission: {}. Valid permissions are: visitors:enrich, emails:send, analytics:read, events:write, errors:read, * (full access)",
                        perm_str
                    )));
                }
            }
            Some(serde_json::to_value(perms).map_err(|e| {
                DeploymentTokenServiceError::InternalServerError(format!(
                    "Failed to serialize permissions: {}",
                    e
                ))
            })?)
        } else {
            // Default to full access
            Some(serde_json::json!(["*"]))
        };

        self.validate_scope(project_id, request.environment_id, request.deployment_id)
            .await?;

        // Check if name is unique for this project
        let existing = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .filter(temps_entities::deployment_tokens::Column::Name.eq(&name))
            .one(self.db.as_ref())
            .await?;

        if existing.is_some() {
            return Err(DeploymentTokenServiceError::Conflict(
                "Deployment token with this name already exists for this project".to_string(),
            ));
        }

        // Generate token
        let token = self.generate_token();
        let token_hash = self.hash_token(&token);
        let token_prefix = token.chars().take(8).collect::<String>();

        // Encrypt the token for storage (so we can retrieve it for deployments)
        let encrypted_token = self
            .encryption_service
            .encrypt_string(&token)
            .map_err(|e| {
                DeploymentTokenServiceError::InternalServerError(format!(
                    "Failed to encrypt token: {}",
                    e
                ))
            })?;

        let now = Utc::now();
        // Without an expiry the token never expires.
        let expires_at = request.expires_at;

        let new_token = DeploymentTokenActiveModel {
            project_id: Set(project_id),
            environment_id: Set(request.environment_id),
            deployment_id: Set(request.deployment_id),
            name: Set(name),
            token_hash: Set(token_hash),
            token_prefix: Set(token_prefix.clone()),
            encrypted_token: Set(Some(encrypted_token)),
            permissions: Set(permissions_json.clone()),
            is_active: Set(true),
            expires_at: Set(expires_at),
            last_used_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            created_by: Set(user_id),
            ..Default::default()
        };

        let token_model = new_token.insert(self.db.as_ref()).await?;

        Ok(CreateDeploymentTokenResponse {
            id: token_model.id,
            project_id: token_model.project_id,
            environment_id: token_model.environment_id,
            deployment_id: token_model.deployment_id,
            name: token_model.name,
            token_prefix,
            permissions: permissions_json.and_then(|p| serde_json::from_value(p).ok()),
            token, // Only returned on creation
            expires_at: token_model.expires_at,
            created_at: token_model.created_at,
        })
    }

    /// List all deployment tokens for a project
    pub async fn list_tokens(
        &self,
        project_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<DeploymentTokenListResponse, DeploymentTokenServiceError> {
        // A missing project is a 404, like a create under it, not an empty
        // list a client cannot tell apart from "no tokens yet".
        if !temps_entities::projects::live_project_exists(self.db.as_ref(), project_id).await? {
            return Err(DeploymentTokenServiceError::ProjectNotFound { project_id });
        }
        let paginator = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .order_by_desc(temps_entities::deployment_tokens::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let tokens_models = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;

        let tokens = tokens_models
            .into_iter()
            .map(DeploymentTokenResponse::from)
            .collect();

        Ok(DeploymentTokenListResponse { tokens, total })
    }

    /// Get a specific deployment token
    pub async fn get_token(
        &self,
        project_id: i32,
        token_id: i32,
    ) -> Result<DeploymentTokenResponse, DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::NotFound("Deployment token not found".to_string())
            })?;

        Ok(DeploymentTokenResponse::from(token))
    }

    /// Update a deployment token
    pub async fn update_token(
        &self,
        project_id: i32,
        token_id: i32,
        request: UpdateDeploymentTokenRequest,
    ) -> Result<DeploymentTokenResponse, DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::NotFound("Deployment token not found".to_string())
            })?;

        let name = request
            .name
            .as_deref()
            .map(|name| normalize_token_name(project_id, name))
            .transpose()?;
        // A client that sends the whole token back resends its stored expiry,
        // even one that has passed; only a new expiry must be in the future.
        if let Some(Some(expires_at)) = request.expires_at {
            if token.expires_at != Some(expires_at) {
                require_future_expiry(expires_at)?;
            }
        }
        if let Some(ref perms) = request.permissions {
            require_permissions(project_id, perms)?;
        }

        // Check if new name conflicts
        if let Some(ref new_name) = name {
            if new_name != &token.name {
                let existing = DeploymentTokenEntity::find()
                    .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
                    .filter(temps_entities::deployment_tokens::Column::Name.eq(new_name))
                    .filter(temps_entities::deployment_tokens::Column::Id.ne(token_id))
                    .one(self.db.as_ref())
                    .await?;

                if existing.is_some() {
                    return Err(DeploymentTokenServiceError::Conflict(
                        "Deployment token with this name already exists".to_string(),
                    ));
                }
            }
        }

        let stored_expiry = token.expires_at;
        let mut token_active: DeploymentTokenActiveModel = token.into();

        if let Some(name) = name {
            token_active.name = Set(name);
        }
        if let Some(is_active) = request.is_active {
            token_active.is_active = Set(is_active);
        }
        // `Some(None)` is an explicit `null`: the token no longer expires.
        // Only a change is written: resending the stored expiry must not
        // overwrite one another request changed in the meantime.
        if let Some(expires_at) = request.expires_at {
            if expires_at != stored_expiry {
                token_active.expires_at = Set(expires_at);
            }
        }
        if let Some(ref perms) = request.permissions {
            // Validate permissions
            for perm_str in perms {
                if DeploymentTokenPermission::from_str(perm_str).is_none() {
                    return Err(DeploymentTokenServiceError::ValidationError(format!(
                        "Invalid permission: {}",
                        perm_str
                    )));
                }
            }
            token_active.permissions = Set(Some(serde_json::to_value(perms).map_err(|e| {
                DeploymentTokenServiceError::InternalServerError(format!(
                    "Failed to serialize permissions: {}",
                    e
                ))
            })?));
        }
        token_active.updated_at = Set(Utc::now());

        let updated_token = token_active.update(self.db.as_ref()).await?;

        Ok(DeploymentTokenResponse::from(updated_token))
    }

    /// Delete a deployment token
    pub async fn delete_token(
        &self,
        project_id: i32,
        token_id: i32,
    ) -> Result<(), DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::NotFound("Deployment token not found".to_string())
            })?;

        DeploymentTokenEntity::delete_by_id(token.id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    /// Rotate a deployment token's secret in place.
    ///
    /// Generates a brand new plaintext token for the same row (same id,
    /// name, project/environment/deployment scope, and permissions),
    /// overwrites the stored hash and encrypted value so the old secret
    /// stops validating immediately, and returns the new plaintext token
    /// exactly once -- mirroring `create_token`'s "only shown once" contract.
    pub async fn rotate_token(
        &self,
        project_id: i32,
        token_id: i32,
    ) -> Result<CreateDeploymentTokenResponse, DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::NotFound(format!(
                    "Deployment token {} not found in project {}",
                    token_id, project_id
                ))
            })?;

        let new_token = self.generate_token();
        let new_hash = self.hash_token(&new_token);
        let new_prefix = new_token.chars().take(8).collect::<String>();

        let encrypted_token = self
            .encryption_service
            .encrypt_string(&new_token)
            .map_err(|e| {
                DeploymentTokenServiceError::InternalServerError(format!(
                    "Failed to encrypt rotated token for deployment token {}: {}",
                    token_id, e
                ))
            })?;

        let mut token_active: DeploymentTokenActiveModel = token.into();
        token_active.token_hash = Set(new_hash);
        token_active.token_prefix = Set(new_prefix.clone());
        token_active.encrypted_token = Set(Some(encrypted_token));
        // The old secret is gone, so any "last used" timestamp recorded
        // against it no longer means anything for the new secret.
        token_active.last_used_at = Set(None);
        token_active.updated_at = Set(Utc::now());

        let updated = token_active.update(self.db.as_ref()).await?;

        Ok(CreateDeploymentTokenResponse {
            id: updated.id,
            project_id: updated.project_id,
            environment_id: updated.environment_id,
            deployment_id: updated.deployment_id,
            name: updated.name,
            token_prefix: new_prefix,
            permissions: updated
                .permissions
                .and_then(|p| serde_json::from_value(p).ok()),
            token: new_token, // Only returned on rotation
            expires_at: updated.expires_at,
            created_at: updated.created_at,
        })
    }

    /// Revoke a token immediately by setting its expiry to the current time.
    ///
    /// Used by the agent executor to invalidate run-scoped tokens the moment a
    /// run completes, fails, or is cancelled, so that the remaining time in the
    /// original expiry window cannot be exploited.
    ///
    /// Silently succeeds if the token does not exist — the caller (executor
    /// cleanup) should not fail a completed run because revocation missed.
    pub async fn revoke_token_by_id(
        &self,
        token_id: i32,
    ) -> Result<(), DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .one(self.db.as_ref())
            .await?;

        let Some(token) = token else {
            // Already deleted or never existed — treat as success.
            return Ok(());
        };

        let mut active: DeploymentTokenActiveModel = token.into();
        // Set expires_at to now() so every subsequent validate_token call
        // hits the "token has expired" branch and returns Unauthorized.
        active.expires_at = Set(Some(Utc::now()));
        active.is_active = Set(false);
        active.updated_at = Set(Utc::now());
        active.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Validate a deployment token and return project info and permissions
    /// Returns (project_id, environment_id, permissions)
    pub async fn validate_token(
        &self,
        token: &str,
    ) -> Result<(i32, Option<i32>, Vec<DeploymentTokenPermission>), DeploymentTokenServiceError>
    {
        let token_hash = self.hash_token(token);
        let token_prefix = token.chars().take(8).collect::<String>();

        let token_model = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::TokenHash.eq(&token_hash))
            .filter(temps_entities::deployment_tokens::Column::TokenPrefix.eq(&token_prefix))
            .filter(temps_entities::deployment_tokens::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::Unauthorized("Invalid deployment token".to_string())
            })?;

        // Check if expired
        if let Some(expires_at) = token_model.expires_at {
            if expires_at <= Utc::now() {
                return Err(DeploymentTokenServiceError::Unauthorized(
                    "Deployment token has expired".to_string(),
                ));
            }
        }

        // Parse permissions
        // SECURITY: NULL permissions defaults to empty (no access) instead of FullAccess.
        // Tokens must explicitly include "*" (FullAccess) in their permissions array.
        let permissions = if let Some(ref perms_json) = token_model.permissions {
            let perm_strings: Vec<String> =
                serde_json::from_value(perms_json.clone()).map_err(|_| {
                    DeploymentTokenServiceError::InternalServerError(
                        "Invalid permissions in database".to_string(),
                    )
                })?;

            perm_strings
                .iter()
                .filter_map(|s| DeploymentTokenPermission::from_str(s))
                .collect()
        } else {
            tracing::warn!(
                "Deployment token {} has NULL permissions, defaulting to no access",
                token_model.id
            );
            vec![]
        };

        // Update last_used_at
        let mut token_active: DeploymentTokenActiveModel = token_model.clone().into();
        token_active.last_used_at = Set(Some(Utc::now()));
        let _ = token_active.update(self.db.as_ref()).await; // Don't fail if this fails

        Ok((
            token_model.project_id,
            token_model.environment_id,
            permissions,
        ))
    }

    /// Get the active deployment token for a project/environment/deployment combination
    /// Used during deployment to inject TEMPS_API_TOKEN
    /// Returns the token value if one exists, or creates a default one
    pub async fn get_or_create_deployment_token(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> Result<String, DeploymentTokenServiceError> {
        // First, try to find an existing active token for this project/environment
        let mut query = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .filter(temps_entities::deployment_tokens::Column::IsActive.eq(true));

        // If environment_id is provided, prefer tokens for that specific environment
        // Otherwise, look for project-wide tokens (environment_id is NULL)
        if let Some(env_id) = environment_id {
            query = query.filter(
                temps_entities::deployment_tokens::Column::EnvironmentId
                    .eq(env_id)
                    .or(temps_entities::deployment_tokens::Column::EnvironmentId.is_null()),
            );
        } else {
            query =
                query.filter(temps_entities::deployment_tokens::Column::EnvironmentId.is_null());
        }

        // Only reuse PERMANENT tokens (expires_at IS NULL).
        //
        // The deploy pipeline burns this token into the container's
        // `TEMPS_API_TOKEN` env var at create time. That value never gets
        // refreshed for the life of the container, so the token has to
        // outlive every redeploy of every app in the project.
        //
        // Previously the filter was `is_null().or(gt(now))`, which happily
        // returned short-lived tokens (`workflow-run-*` with 2h expiry,
        // `workspace-session-*` with 6h expiry) as long as they hadn't
        // expired YET. Containers deployed during one of those windows got
        // a token that died a couple hours later and 401'd ever after.
        //
        // Restricting to `expires_at IS NULL` means we only reuse tokens we
        // ourselves auto-generated for the deploy path below — those are
        // the only ones that ever carry no expiry. Anything else falls
        // through to the create branch and we mint a fresh permanent row.
        query = query.filter(temps_entities::deployment_tokens::Column::ExpiresAt.is_null());

        // Order by: prefer environment-specific tokens, then by creation date
        query = query
            .order_by_desc(temps_entities::deployment_tokens::Column::EnvironmentId)
            .order_by_desc(temps_entities::deployment_tokens::Column::CreatedAt);

        let existing_token = query.one(self.db.as_ref()).await?;

        if let Some(token_model) = existing_token {
            // Decrypt and return the stored token value
            if let Some(ref encrypted_token) = token_model.encrypted_token {
                let decrypted_token = self
                    .encryption_service
                    .decrypt_string(encrypted_token)
                    .map_err(|e| {
                        DeploymentTokenServiceError::InternalServerError(format!(
                            "Failed to decrypt token: {}",
                            e
                        ))
                    })?;
                return Ok(decrypted_token);
            } else {
                // Token exists but encrypted_token is not stored (legacy token)
                // We need to regenerate and update the token
                tracing::warn!(
                    "Token ID {} exists but has no encrypted value stored. Regenerating token.",
                    token_model.id
                );

                // Generate new token
                let new_token = self.generate_token();
                let new_hash = self.hash_token(&new_token);
                let new_prefix = new_token.chars().take(8).collect::<String>();
                let encrypted_token =
                    self.encryption_service
                        .encrypt_string(&new_token)
                        .map_err(|e| {
                            DeploymentTokenServiceError::InternalServerError(format!(
                                "Failed to encrypt token: {}",
                                e
                            ))
                        })?;

                // Update the existing token with new values
                let mut token_active: DeploymentTokenActiveModel = token_model.into();
                token_active.token_hash = Set(new_hash);
                token_active.token_prefix = Set(new_prefix);
                token_active.encrypted_token = Set(Some(encrypted_token));
                token_active.updated_at = Set(Utc::now());
                token_active.update(self.db.as_ref()).await?;

                return Ok(new_token);
            }
        }

        // No token exists, create a default one
        let token = self.generate_token();
        let token_hash = self.hash_token(&token);
        let token_prefix = token.chars().take(8).collect::<String>();

        // Encrypt the token for storage
        let encrypted_token = self
            .encryption_service
            .encrypt_string(&token)
            .map_err(|e| {
                DeploymentTokenServiceError::InternalServerError(format!(
                    "Failed to encrypt token: {}",
                    e
                ))
            })?;

        let default_name = if let Some(env_id) = environment_id {
            format!("Auto-generated for environment {}", env_id)
        } else {
            "Auto-generated project token".to_string()
        };

        let new_token = DeploymentTokenActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            deployment_id: Set(deployment_id),
            name: Set(default_name),
            token_hash: Set(token_hash),
            token_prefix: Set(token_prefix),
            encrypted_token: Set(Some(encrypted_token)),
            permissions: Set(Some(serde_json::json!(["*"]))), // Full access by default
            is_active: Set(true),
            expires_at: Set(None), // Never expires
            last_used_at: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            created_by: Set(None), // System-generated
            ..Default::default()
        };

        new_token.insert(self.db.as_ref()).await?;

        Ok(token)
    }

    /// Generate a deployment token with prefix "dt_" (deployment token)
    fn generate_token(&self) -> String {
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::rng();

        let prefix = "dt_";
        let random_part: String = (0..40)
            .map(|_| {
                let idx = rng.random_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();

        format!("{}{}", prefix, random_part)
    }

    /// Hash a token using SHA256
    pub(crate) fn hash_token(&self, token: &str) -> String {
        temps_core::crypto::sha256_hex(token)
    }
}

#[cfg(test)]
mod tests;
