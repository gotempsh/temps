//! Deployment Token Validation Service
//!
//! Validates deployment tokens for authentication in the middleware.
//! This service handles the read-only validation of tokens, while the
//! full CRUD operations are in temps-deployments.

use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_database::DbConnection;
use temps_entities::deployment_tokens::{
    DeploymentTokenPermission, Entity as DeploymentTokenEntity,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DeploymentTokenValidationError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Invalid token")]
    InvalidToken,

    #[error("Token expired")]
    TokenExpired,

    #[error("Token inactive")]
    TokenInactive,
}

/// Result of successful deployment token validation
#[derive(Debug, Clone)]
pub struct ValidatedDeploymentToken {
    pub token_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub name: String,
    pub permissions: Vec<DeploymentTokenPermission>,
}

pub struct DeploymentTokenValidationService {
    db: Arc<DbConnection>,
}

impl DeploymentTokenValidationService {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self { db }
    }

    /// Validate a deployment token and return its details
    pub async fn validate_token(
        &self,
        token: &str,
    ) -> Result<ValidatedDeploymentToken, DeploymentTokenValidationError> {
        // Deployment tokens start with "dt_"
        if !token.starts_with("dt_") {
            return Err(DeploymentTokenValidationError::InvalidToken);
        }

        let token_hash = self.hash_token(token);
        let token_prefix: String = token.chars().take(8).collect();

        // Find the token by hash and prefix
        let token_model = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::TokenHash.eq(&token_hash))
            .filter(temps_entities::deployment_tokens::Column::TokenPrefix.eq(&token_prefix))
            .one(self.db.as_ref())
            .await?
            .ok_or(DeploymentTokenValidationError::InvalidToken)?;

        // Check if active
        if !token_model.is_active {
            return Err(DeploymentTokenValidationError::TokenInactive);
        }

        // Check if expired
        if let Some(expires_at) = token_model.expires_at {
            if expires_at <= Utc::now() {
                return Err(DeploymentTokenValidationError::TokenExpired);
            }
        }

        // Parse permissions from JSON
        let permissions = if let Some(ref perms_json) = token_model.permissions {
            let perm_strings: Vec<String> =
                serde_json::from_value(perms_json.clone()).unwrap_or_default();

            perm_strings
                .iter()
                .filter_map(|s| DeploymentTokenPermission::from_str(s))
                .collect()
        } else {
            vec![DeploymentTokenPermission::FullAccess]
        };

        // Update last_used_at (fire and forget - don't fail validation if this fails)
        let _ = self.update_last_used(token_model.id).await;

        Ok(ValidatedDeploymentToken {
            token_id: token_model.id,
            project_id: token_model.project_id,
            environment_id: token_model.environment_id,
            name: token_model.name,
            permissions,
        })
    }

    /// Update the last_used_at timestamp
    async fn update_last_used(&self, token_id: i32) -> Result<(), sea_orm::DbErr> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::deployment_tokens::ActiveModel;

        let mut token_active = ActiveModel {
            id: Set(token_id),
            ..Default::default()
        };
        token_active.last_used_at = Set(Some(Utc::now()));

        // Use update, but we need to handle partial updates properly
        // Actually, sea-orm requires all fields for update, so we need to fetch first
        if let Some(existing) = DeploymentTokenEntity::find_by_id(token_id)
            .one(self.db.as_ref())
            .await?
        {
            let mut active: ActiveModel = existing.into();
            active.last_used_at = Set(Some(Utc::now()));
            active.update(self.db.as_ref()).await?;
        }

        Ok(())
    }

    /// Hash a token using SHA256
    fn hash_token(&self, token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    /// Helper to create a service with mock database
    fn create_service_with_mock(
        db: sea_orm::DatabaseConnection,
    ) -> DeploymentTokenValidationService {
        DeploymentTokenValidationService::new(Arc::new(db))
    }

    #[test]
    fn test_hash_consistency() {
        // Test the SHA256 hash directly without needing a service
        let token = "dt_testtoken123456";

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let expected_hash = format!("{:x}", hasher.finalize());

        // Hash should be 64 chars (SHA256 hex)
        assert_eq!(expected_hash.len(), 64);
    }

    #[test]
    fn test_hash_deterministic() {
        // Same input should always produce same hash
        let token = "dt_testtoken123456";

        let mut hasher1 = Sha256::new();
        hasher1.update(token.as_bytes());
        let hash1 = format!("{:x}", hasher1.finalize());

        let mut hasher2 = Sha256::new();
        hasher2.update(token.as_bytes());
        let hash2 = format!("{:x}", hasher2.finalize());

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_different_tokens_different_hashes() {
        let token1 = "dt_token1";
        let token2 = "dt_token2";

        let mut hasher1 = Sha256::new();
        hasher1.update(token1.as_bytes());
        let hash1 = format!("{:x}", hasher1.finalize());

        let mut hasher2 = Sha256::new();
        hasher2.update(token2.as_bytes());
        let hash2 = format!("{:x}", hasher2.finalize());

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_invalid_token_prefix() {
        // Tokens not starting with dt_ should be invalid
        assert!(!("tk_someapikey").starts_with("dt_"));
        assert!(("dt_somedeploymenttoken").starts_with("dt_"));
        assert!(!("bearer_token").starts_with("dt_"));
        assert!(!("").starts_with("dt_"));
    }

    #[test]
    fn test_token_prefix_extraction() {
        let token = "dt_abc123xyz789";
        let prefix: String = token.chars().take(8).collect();
        assert_eq!(prefix, "dt_abc12");
    }

    #[test]
    fn test_error_display() {
        let db_err = DeploymentTokenValidationError::DatabaseError(sea_orm::DbErr::Custom(
            "test".to_string(),
        ));
        assert!(db_err.to_string().contains("Database error"));

        let invalid = DeploymentTokenValidationError::InvalidToken;
        assert_eq!(invalid.to_string(), "Invalid token");

        let expired = DeploymentTokenValidationError::TokenExpired;
        assert_eq!(expired.to_string(), "Token expired");

        let inactive = DeploymentTokenValidationError::TokenInactive;
        assert_eq!(inactive.to_string(), "Token inactive");
    }

    #[tokio::test]
    async fn test_validate_token_invalid_prefix() {
        // Create a mock database (won't be queried)
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = create_service_with_mock(db);

        // Token without dt_ prefix should fail immediately
        let result = service.validate_token("tk_someapikey").await;
        assert!(matches!(
            result,
            Err(DeploymentTokenValidationError::InvalidToken)
        ));

        let result = service.validate_token("invalid_token").await;
        assert!(matches!(
            result,
            Err(DeploymentTokenValidationError::InvalidToken)
        ));
    }

    #[tokio::test]
    async fn test_validate_token_not_found() {
        // Mock database returns empty result
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<temps_entities::deployment_tokens::Model, Vec<_>, _>(vec![
                vec![],
            ])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token("dt_nonexistent12345678").await;
        assert!(matches!(
            result,
            Err(DeploymentTokenValidationError::InvalidToken)
        ));
    }

    #[tokio::test]
    async fn test_validate_token_inactive() {
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::days(30);
        let token = "dt_inactivetoken123456";
        let token_prefix: String = token.chars().take(8).collect();

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = format!("{:x}", hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: Some(20),
                name: "inactive-token".to_string(),
                token_hash,
                token_prefix,
                permissions: Some(serde_json::json!(["visitors:enrich"])),
                is_active: false, // Inactive!
                expires_at: Some(expires_at),
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token(token).await;
        assert!(matches!(
            result,
            Err(DeploymentTokenValidationError::TokenInactive)
        ));
    }

    #[tokio::test]
    async fn test_validate_token_expired() {
        let now = chrono::Utc::now();
        let expired_at = now - chrono::Duration::days(1); // Expired yesterday
        let token = "dt_expiredtoken1234567";
        let token_prefix: String = token.chars().take(8).collect();

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = format!("{:x}", hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: Some(20),
                name: "expired-token".to_string(),
                token_hash,
                token_prefix,
                permissions: Some(serde_json::json!(["visitors:enrich"])),
                is_active: true,
                expires_at: Some(expired_at),
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token(token).await;
        assert!(matches!(
            result,
            Err(DeploymentTokenValidationError::TokenExpired)
        ));
    }

    #[tokio::test]
    async fn test_validate_token_success() {
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::days(30);
        let token = "dt_validtoken12345678";
        let token_prefix: String = token.chars().take(8).collect();

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = format!("{:x}", hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 42,
                project_id: 100,
                environment_id: Some(200),
                name: "valid-token".to_string(),
                token_hash,
                token_prefix,
                permissions: Some(serde_json::json!(["visitors:enrich", "emails:send"])),
                is_active: true,
                expires_at: Some(expires_at),
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            // For finding token again to update last_used_at
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 42,
                project_id: 100,
                environment_id: Some(200),
                name: "valid-token".to_string(),
                token_hash: "hash".to_string(),
                token_prefix: "dt_valid".to_string(),
                permissions: Some(serde_json::json!(["visitors:enrich", "emails:send"])),
                is_active: true,
                expires_at: Some(expires_at),
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            // For the update
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 42,
                rows_affected: 1,
            }])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token(token).await;
        assert!(result.is_ok());

        let validated = result.unwrap();
        assert_eq!(validated.token_id, 42);
        assert_eq!(validated.project_id, 100);
        assert_eq!(validated.environment_id, Some(200));
        assert_eq!(validated.name, "valid-token");
        assert_eq!(validated.permissions.len(), 2);
    }

    #[tokio::test]
    async fn test_validate_token_no_expiry() {
        // Token with no expiry date should be valid
        let now = chrono::Utc::now();
        let token = "dt_noexpirytoken1234";
        let token_prefix: String = token.chars().take(8).collect();

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = format!("{:x}", hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                name: "no-expiry-token".to_string(),
                token_hash,
                token_prefix,
                permissions: Some(serde_json::json!(["*"])), // Full access
                is_active: true,
                expires_at: None, // No expiry
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            // For finding token again to update last_used_at
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                name: "no-expiry-token".to_string(),
                token_hash: "hash".to_string(),
                token_prefix: "dt_noexp".to_string(),
                permissions: Some(serde_json::json!(["*"])),
                is_active: true,
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            // For the update
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token(token).await;
        assert!(result.is_ok());

        let validated = result.unwrap();
        assert_eq!(validated.environment_id, None);
        // Full access permission
        assert!(validated
            .permissions
            .iter()
            .any(|p| matches!(p, DeploymentTokenPermission::FullAccess)));
    }

    #[tokio::test]
    async fn test_validate_token_default_permissions() {
        // Token with null permissions should default to full access
        let now = chrono::Utc::now();
        let token = "dt_defaultperms12345";
        let token_prefix: String = token.chars().take(8).collect();

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = format!("{:x}", hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                name: "default-perms-token".to_string(),
                token_hash,
                token_prefix,
                permissions: None, // Null permissions
                is_active: true,
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            // For finding token again to update last_used_at
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                name: "default-perms-token".to_string(),
                token_hash: "hash".to_string(),
                token_prefix: "dt_defau".to_string(),
                permissions: None,
                is_active: true,
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
            }]])
            // For the update
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token(token).await;
        assert!(result.is_ok());

        let validated = result.unwrap();
        // Should default to FullAccess when no permissions specified
        assert_eq!(validated.permissions.len(), 1);
        assert!(matches!(
            validated.permissions[0],
            DeploymentTokenPermission::FullAccess
        ));
    }
}
