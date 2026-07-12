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
    DeploymentTokenPermission, Entity as DeploymentTokenEntity, Model as DeploymentTokenModel,
};
use thiserror::Error;
use tracing::warn;

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
    pub deployment_id: Option<i32>,
    pub name: String,
    pub permissions: Vec<DeploymentTokenPermission>,
}

pub struct DeploymentTokenValidationService {
    db: Arc<DbConnection>,
    /// Throttles `last_used_at` writes so repeated validations of the same
    /// token within the window skip the DB write entirely. See
    /// [`crate::last_used_throttle`].
    last_used_throttle: crate::last_used_throttle::LastUsedThrottle,
}

impl DeploymentTokenValidationService {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self {
            db,
            last_used_throttle: crate::last_used_throttle::LastUsedThrottle::new(
                crate::last_used_throttle::LAST_USED_UPDATE_INTERVAL,
            ),
        }
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
        // SECURITY: NULL permissions defaults to empty (no access) instead of FullAccess.
        // Tokens must explicitly include "*" (FullAccess) in their permissions array.
        let permissions = if let Some(ref perms_json) = token_model.permissions {
            let perm_strings: Vec<String> =
                serde_json::from_value(perms_json.clone()).unwrap_or_default();

            perm_strings
                .iter()
                .filter_map(|s| DeploymentTokenPermission::from_str(s))
                .collect()
        } else {
            warn!(
                "Deployment token {} has NULL permissions, defaulting to no access",
                token_model.id
            );
            vec![]
        };

        // Update last_used_at, throttled so repeated validations of the
        // same token within LAST_USED_UPDATE_INTERVAL skip the DB write.
        // Best-effort: errors are swallowed so a write failure never blocks
        // validation.
        if self.last_used_throttle.should_update(token_model.id) {
            let _ = self.update_last_used(&token_model).await;
        }

        Ok(ValidatedDeploymentToken {
            token_id: token_model.id,
            project_id: token_model.project_id,
            environment_id: token_model.environment_id,
            deployment_id: token_model.deployment_id,
            name: token_model.name,
            permissions,
        })
    }

    /// Update the last_used_at timestamp, reusing the already-fetched
    /// token row instead of re-querying it.
    async fn update_last_used(
        &self,
        token_model: &DeploymentTokenModel,
    ) -> Result<(), sea_orm::DbErr> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::deployment_tokens::ActiveModel;

        let mut active: ActiveModel = token_model.clone().into();
        active.last_used_at = Set(Some(Utc::now()));
        active.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Hash a token using SHA256
    fn hash_token(&self, token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
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
        let expected_hash = hex::encode(hasher.finalize());

        // Hash should be 64 chars (SHA256 hex)
        assert_eq!(expected_hash.len(), 64);
    }

    #[test]
    fn test_hash_deterministic() {
        // Same input should always produce same hash
        let token = "dt_testtoken123456";

        let mut hasher1 = Sha256::new();
        hasher1.update(token.as_bytes());
        let hash1 = hex::encode(hasher1.finalize());

        let mut hasher2 = Sha256::new();
        hasher2.update(token.as_bytes());
        let hash2 = hex::encode(hasher2.finalize());

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_different_tokens_different_hashes() {
        let token1 = "dt_token1";
        let token2 = "dt_token2";

        let mut hasher1 = Sha256::new();
        hasher1.update(token1.as_bytes());
        let hash1 = hex::encode(hasher1.finalize());

        let mut hasher2 = Sha256::new();
        hasher2.update(token2.as_bytes());
        let hash2 = hex::encode(hasher2.finalize());

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
        let token_hash = hex::encode(hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: Some(20),
                deployment_id: None,
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
                encrypted_token: None,
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
        let token_hash = hex::encode(hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: Some(20),
                deployment_id: None,
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
                encrypted_token: None,
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
        let token_hash = hex::encode(hasher.finalize());

        let model = temps_entities::deployment_tokens::Model {
            id: 42,
            project_id: 100,
            environment_id: Some(200),
            deployment_id: None,
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
            encrypted_token: None,
        };

        // Postgres supports `UPDATE ... RETURNING`, so `ActiveModel::update`
        // goes through the query path (not `execute`/exec_results) — one
        // query batch for the lookup, one for the update-and-return-updated.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model.clone()], vec![model]])
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
    async fn test_validate_token_repeat_call_skips_last_used_update() {
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::days(30);
        let token = "dt_validtoken12345678";
        let token_prefix: String = token.chars().take(8).collect();

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = hex::encode(hasher.finalize());

        let model = || temps_entities::deployment_tokens::Model {
            id: 42,
            project_id: 100,
            environment_id: Some(200),
            deployment_id: None,
            name: "valid-token".to_string(),
            token_hash: token_hash.clone(),
            token_prefix: token_prefix.clone(),
            permissions: Some(serde_json::json!(["visitors:enrich", "emails:send"])),
            is_active: true,
            expires_at: Some(expires_at),
            last_used_at: None,
            created_at: now,
            updated_at: now,
            created_by: Some(1),
            encrypted_token: None,
        };

        // Errors from the last_used_at write are swallowed by validate_token
        // (`let _ = self.update_last_used(...).await;`), so a call
        // succeeding doesn't prove a write was attempted or skipped —
        // asserting on the mock's transaction log is the only reliable way
        // to verify the throttle actually prevented a second write.
        //
        // Statements needed if the throttle works correctly: call 1's
        // lookup, call 1's update-and-return-updated (RETURNING), call 2's
        // lookup. Call 2's update must NOT happen — if it did, it would
        // need a 4th query batch that isn't provided, and (since that
        // statement is logged before the mock realizes its results buffer
        // is empty) the transaction log would show 4 entries instead of 3.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model()], vec![model()], vec![model()]])
                .into_connection(),
        );

        let service = DeploymentTokenValidationService::new(db.clone());

        assert!(service.validate_token(token).await.is_ok());
        assert!(service.validate_token(token).await.is_ok());

        drop(service);
        let statement_count = Arc::try_unwrap(db)
            .expect("service dropped, so this is the only remaining ref")
            .into_transaction_log()
            .len();
        assert_eq!(
            statement_count, 3,
            "expected exactly 2 lookups + 1 throttled last_used_at write, got {statement_count} statements"
        );
    }

    #[tokio::test]
    async fn test_validate_token_no_expiry() {
        // Token with no expiry date should be valid
        let now = chrono::Utc::now();
        let token = "dt_noexpirytoken1234";
        let token_prefix: String = token.chars().take(8).collect();

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = hex::encode(hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
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
                encrypted_token: None,
            }]])
            // For finding token again to update last_used_at
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
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
                encrypted_token: None,
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
        let token_hash = hex::encode(hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
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
                encrypted_token: None,
            }]])
            // For finding token again to update last_used_at
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 1,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
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
                encrypted_token: None,
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
        // SECURITY: NULL permissions should default to empty (no access), not FullAccess
        assert!(
            validated.permissions.is_empty(),
            "NULL permissions should result in empty permissions (no access), got {:?}",
            validated.permissions
        );
    }

    #[tokio::test]
    async fn test_validate_token_explicit_full_access() {
        let now = Utc::now();
        let token_prefix = "dt_fullac";
        let token = format!("{}12345678901234567890123456789012", token_prefix);

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = hex::encode(hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 2,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
                name: "explicit-full-access".to_string(),
                token_hash,
                token_prefix: token_prefix.to_string(),
                permissions: Some(serde_json::json!(["*"])), // Explicit FullAccess
                is_active: true,
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
                encrypted_token: None,
            }]])
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 2,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
                name: "explicit-full-access".to_string(),
                token_hash: "hash".to_string(),
                token_prefix: token_prefix.to_string(),
                permissions: Some(serde_json::json!(["*"])),
                is_active: true,
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
                encrypted_token: None,
            }]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 2,
                rows_affected: 1,
            }])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token(&token).await;
        assert!(result.is_ok());

        let validated = result.unwrap();
        // Explicit FullAccess should be granted
        assert_eq!(validated.permissions.len(), 1);
        assert!(matches!(
            validated.permissions[0],
            DeploymentTokenPermission::FullAccess
        ));
    }

    #[tokio::test]
    async fn test_validate_token_null_permissions_denies_access() {
        let now = Utc::now();
        let token_prefix = "dt_noperm";
        let token = format!("{}12345678901234567890123456789012", token_prefix);

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = hex::encode(hasher.finalize());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 3,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
                name: "no-perms-token".to_string(),
                token_hash,
                token_prefix: token_prefix.to_string(),
                permissions: None, // NULL
                is_active: true,
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
                encrypted_token: None,
            }]])
            .append_query_results(vec![vec![temps_entities::deployment_tokens::Model {
                id: 3,
                project_id: 10,
                environment_id: None,
                deployment_id: None,
                name: "no-perms-token".to_string(),
                token_hash: "hash".to_string(),
                token_prefix: token_prefix.to_string(),
                permissions: None,
                is_active: true,
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
                created_by: Some(1),
                encrypted_token: None,
            }]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 3,
                rows_affected: 1,
            }])
            .into_connection();

        let service = create_service_with_mock(db);

        let result = service.validate_token(&token).await;
        assert!(result.is_ok());

        let validated = result.unwrap();
        // Verify that NULL permissions result in no access -
        // none of the specific permissions should be granted
        assert!(validated.permissions.is_empty());
        assert!(!validated
            .permissions
            .iter()
            .any(|p| matches!(p, DeploymentTokenPermission::FullAccess)));
        assert!(!validated
            .permissions
            .iter()
            .any(|p| matches!(p, DeploymentTokenPermission::EventsWrite)));
        assert!(!validated
            .permissions
            .iter()
            .any(|p| matches!(p, DeploymentTokenPermission::AnalyticsRead)));
    }
}
