//! Provider service for managing email provider configurations

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::email_providers;
use tracing::{debug, error};

use crate::errors::EmailError;
use crate::providers::{
    EmailProvider, EmailProviderType, ScalewayCredentials, ScalewayProvider, SesCredentials,
    SesProvider,
};

/// Service for managing email providers
#[derive(Clone)]
pub struct ProviderService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
}

/// Request to create a new email provider
#[derive(Debug, Clone)]
pub struct CreateProviderRequest {
    pub name: String,
    pub provider_type: EmailProviderType,
    pub region: String,
    pub credentials: ProviderCredentials,
}

/// Provider credentials enum
#[derive(Debug, Clone)]
pub enum ProviderCredentials {
    Ses(SesCredentials),
    Scaleway(ScalewayCredentials),
}

impl ProviderService {
    pub fn new(db: Arc<DatabaseConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Create a new email provider
    pub async fn create(
        &self,
        request: CreateProviderRequest,
    ) -> Result<email_providers::Model, EmailError> {
        debug!(
            "Creating email provider: {} ({})",
            request.name, request.provider_type
        );

        // Serialize credentials to JSON
        let credentials_json = match &request.credentials {
            ProviderCredentials::Ses(creds) => serde_json::to_string(creds)?,
            ProviderCredentials::Scaleway(creds) => serde_json::to_string(creds)?,
        };

        // Encrypt credentials
        let encrypted_credentials = self
            .encryption_service
            .encrypt_string(&credentials_json)
            .map_err(|e| EmailError::Encryption(e.to_string()))?;

        let provider = email_providers::ActiveModel {
            name: Set(request.name),
            provider_type: Set(request.provider_type.to_string()),
            region: Set(request.region),
            credentials: Set(encrypted_credentials),
            is_active: Set(true),
            ..Default::default()
        };

        let result = provider.insert(self.db.as_ref()).await?;

        debug!("Created email provider with id: {}", result.id);

        Ok(result)
    }

    /// Get a provider by ID
    pub async fn get(&self, id: i32) -> Result<email_providers::Model, EmailError> {
        email_providers::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(EmailError::ProviderNotFound(id))
    }

    /// List all providers
    pub async fn list(&self) -> Result<Vec<email_providers::Model>, EmailError> {
        let providers = email_providers::Entity::find()
            .order_by_desc(email_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(providers)
    }

    /// List only active providers
    pub async fn list_active(&self) -> Result<Vec<email_providers::Model>, EmailError> {
        let providers = email_providers::Entity::find()
            .filter(email_providers::Column::IsActive.eq(true))
            .order_by_desc(email_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(providers)
    }

    /// Delete a provider
    pub async fn delete(&self, id: i32) -> Result<(), EmailError> {
        let provider = self.get(id).await?;

        email_providers::Entity::delete_by_id(provider.id)
            .exec(self.db.as_ref())
            .await?;

        debug!("Deleted email provider with id: {}", id);

        Ok(())
    }

    /// Update provider active status
    pub async fn set_active(
        &self,
        id: i32,
        is_active: bool,
    ) -> Result<email_providers::Model, EmailError> {
        let provider = self.get(id).await?;

        let mut active_model: email_providers::ActiveModel = provider.into();
        active_model.is_active = Set(is_active);

        let result = active_model.update(self.db.as_ref()).await?;

        debug!(
            "Updated email provider {} active status to: {}",
            id, is_active
        );

        Ok(result)
    }

    /// Create an email provider instance from a database model
    pub async fn create_provider_instance(
        &self,
        provider: &email_providers::Model,
    ) -> Result<Box<dyn EmailProvider>, EmailError> {
        // Decrypt credentials
        let credentials_json = self
            .encryption_service
            .decrypt_string(&provider.credentials)
            .map_err(|e| EmailError::Decryption(e.to_string()))?;

        let provider_type = EmailProviderType::from_str(&provider.provider_type)?;

        match provider_type {
            EmailProviderType::Ses => {
                let credentials: SesCredentials = serde_json::from_str(&credentials_json)?;
                let ses_provider = SesProvider::new(&credentials, &provider.region)
                    .await
                    .map_err(|e| {
                        error!("Failed to create SES provider: {}", e);
                        e
                    })?;
                Ok(Box::new(ses_provider))
            }
            EmailProviderType::Scaleway => {
                let credentials: ScalewayCredentials = serde_json::from_str(&credentials_json)?;
                let scaleway_provider = ScalewayProvider::new(&credentials, &provider.region)
                    .map_err(|e| {
                        error!("Failed to create Scaleway provider: {}", e);
                        e
                    })?;
                Ok(Box::new(scaleway_provider))
            }
        }
    }

    /// Get decrypted credentials for a provider (for display purposes, masked)
    pub fn get_masked_credentials(
        &self,
        provider: &email_providers::Model,
    ) -> Result<serde_json::Value, EmailError> {
        let credentials_json = self
            .encryption_service
            .decrypt_string(&provider.credentials)
            .map_err(|e| EmailError::Decryption(e.to_string()))?;

        let provider_type = EmailProviderType::from_str(&provider.provider_type)?;

        match provider_type {
            EmailProviderType::Ses => {
                let credentials: SesCredentials = serde_json::from_str(&credentials_json)?;
                Ok(serde_json::json!({
                    "access_key_id": mask_string(&credentials.access_key_id),
                    "secret_access_key": "***"
                }))
            }
            EmailProviderType::Scaleway => {
                let credentials: ScalewayCredentials = serde_json::from_str(&credentials_json)?;
                Ok(serde_json::json!({
                    "api_key": "***",
                    "project_id": credentials.project_id
                }))
            }
        }
    }
}

/// Mask a string, showing only first 4 and last 4 characters
fn mask_string(s: &str) -> String {
    if s.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &s[..4], &s[s.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_database::test_utils::TestDatabase;

    // Helper to create a test encryption service
    fn create_test_encryption_service() -> Arc<EncryptionService> {
        // 32-byte hex key for testing
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(EncryptionService::new(key).unwrap())
    }

    // Helper to setup test environment with real database
    async fn setup_test_env() -> (TestDatabase, ProviderService) {
        let db = TestDatabase::with_migrations().await.unwrap();
        let encryption_service = create_test_encryption_service();
        let service = ProviderService::new(db.db.clone(), encryption_service);
        (db, service)
    }

    // ========== Unit Tests (no database required) ==========

    #[test]
    fn test_mask_string() {
        assert_eq!(mask_string("short"), "***");
        assert_eq!(mask_string("AKIAIOSFODNN7EXAMPLE"), "AKIA...MPLE");
        assert_eq!(mask_string("12345678"), "***"); // Exactly 8 chars
        assert_eq!(mask_string("123456789"), "1234...6789"); // 9 chars
    }

    #[test]
    fn test_create_provider_request_ses() {
        let credentials = ProviderCredentials::Ses(SesCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        });

        let request = CreateProviderRequest {
            name: "My SES Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials,
        };

        assert_eq!(request.name, "My SES Provider");
        assert_eq!(request.provider_type, EmailProviderType::Ses);
        assert_eq!(request.region, "us-east-1");
    }

    #[test]
    fn test_create_provider_request_scaleway() {
        let credentials = ProviderCredentials::Scaleway(ScalewayCredentials {
            api_key: "scw-api-key-example".to_string(),
            project_id: "project-123".to_string(),
        });

        let request = CreateProviderRequest {
            name: "My Scaleway Provider".to_string(),
            provider_type: EmailProviderType::Scaleway,
            region: "fr-par".to_string(),
            credentials,
        };

        assert_eq!(request.name, "My Scaleway Provider");
        assert_eq!(request.provider_type, EmailProviderType::Scaleway);
        assert_eq!(request.region, "fr-par");
    }

    #[test]
    fn test_email_provider_type_display() {
        assert_eq!(format!("{}", EmailProviderType::Ses), "ses");
        assert_eq!(format!("{}", EmailProviderType::Scaleway), "scaleway");
    }

    #[test]
    fn test_email_provider_type_from_str() {
        assert_eq!(
            EmailProviderType::from_str("ses").unwrap(),
            EmailProviderType::Ses
        );
        assert_eq!(
            EmailProviderType::from_str("scaleway").unwrap(),
            EmailProviderType::Scaleway
        );
        assert!(EmailProviderType::from_str("invalid").is_err());
    }

    #[test]
    fn test_get_masked_credentials_ses() {
        let encryption_service = create_test_encryption_service();

        // Create and encrypt real credentials
        let credentials = SesCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        };
        let credentials_json = serde_json::to_string(&credentials).unwrap();
        let encrypted = encryption_service
            .encrypt_string(&credentials_json)
            .unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderService::new(Arc::new(db), encryption_service);

        let provider = email_providers::Model {
            id: 1,
            name: "Test".to_string(),
            provider_type: "ses".to_string(),
            region: "us-east-1".to_string(),
            credentials: encrypted,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let result = service.get_masked_credentials(&provider);

        assert!(result.is_ok());
        let masked = result.unwrap();
        assert_eq!(masked["access_key_id"], "AKIA...MPLE");
        assert_eq!(masked["secret_access_key"], "***");
    }

    #[test]
    fn test_get_masked_credentials_scaleway() {
        let encryption_service = create_test_encryption_service();

        // Create and encrypt real credentials
        let credentials = ScalewayCredentials {
            api_key: "scw-api-key-example-12345".to_string(),
            project_id: "project-123".to_string(),
        };
        let credentials_json = serde_json::to_string(&credentials).unwrap();
        let encrypted = encryption_service
            .encrypt_string(&credentials_json)
            .unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderService::new(Arc::new(db), encryption_service);

        let provider = email_providers::Model {
            id: 1,
            name: "Test".to_string(),
            provider_type: "scaleway".to_string(),
            region: "fr-par".to_string(),
            credentials: encrypted,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let result = service.get_masked_credentials(&provider);

        assert!(result.is_ok());
        let masked = result.unwrap();
        assert_eq!(masked["api_key"], "***");
        assert_eq!(masked["project_id"], "project-123");
    }

    // ========== Integration Tests (require Docker) ==========

    #[tokio::test]
    async fn test_create_provider() {
        let (_db, service) = setup_test_env().await;

        let request = CreateProviderRequest {
            name: "Test SES Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            }),
        };

        let result = service.create(request).await;

        assert!(result.is_ok());
        let provider = result.unwrap();
        assert!(provider.id > 0);
        assert_eq!(provider.name, "Test SES Provider");
        assert_eq!(provider.provider_type, "ses");
        assert_eq!(provider.region, "us-east-1");
        assert!(provider.is_active);
    }

    #[tokio::test]
    async fn test_get_provider() {
        let (_db, service) = setup_test_env().await;

        // Create a provider first
        let request = CreateProviderRequest {
            name: "Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            }),
        };
        let created = service.create(request).await.unwrap();

        // Get the provider
        let result = service.get(created.id).await;

        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(provider.id, created.id);
        assert_eq!(provider.name, "Test Provider");
    }

    #[tokio::test]
    async fn test_get_provider_not_found() {
        let (_db, service) = setup_test_env().await;

        let result = service.get(999999).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmailError::ProviderNotFound(999999)
        ));
    }

    #[tokio::test]
    async fn test_list_providers() {
        let (_db, service) = setup_test_env().await;

        // Create multiple providers
        let request1 = CreateProviderRequest {
            name: "Provider 1".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            }),
        };
        service.create(request1).await.unwrap();

        let request2 = CreateProviderRequest {
            name: "Provider 2".to_string(),
            provider_type: EmailProviderType::Scaleway,
            region: "fr-par".to_string(),
            credentials: ProviderCredentials::Scaleway(ScalewayCredentials {
                api_key: "scw-api-key".to_string(),
                project_id: "project-123".to_string(),
            }),
        };
        service.create(request2).await.unwrap();

        // List all providers
        let result = service.list().await;

        assert!(result.is_ok());
        let providers = result.unwrap();
        assert_eq!(providers.len(), 2);
    }

    #[tokio::test]
    async fn test_list_active_providers() {
        let (_db, service) = setup_test_env().await;

        // Create a provider
        let request = CreateProviderRequest {
            name: "Active Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            }),
        };
        let created = service.create(request).await.unwrap();

        // Create another and deactivate it
        let request2 = CreateProviderRequest {
            name: "Inactive Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-west-2".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE2".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY2".to_string(),
            }),
        };
        let created2 = service.create(request2).await.unwrap();
        service.set_active(created2.id, false).await.unwrap();

        // List only active providers
        let result = service.list_active().await;

        assert!(result.is_ok());
        let providers = result.unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, created.id);
        assert!(providers[0].is_active);
    }

    #[tokio::test]
    async fn test_delete_provider() {
        let (_db, service) = setup_test_env().await;

        // Create a provider
        let request = CreateProviderRequest {
            name: "To Delete".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            }),
        };
        let created = service.create(request).await.unwrap();

        // Delete it
        let result = service.delete(created.id).await;
        assert!(result.is_ok());

        // Verify it's gone
        let get_result = service.get(created.id).await;
        assert!(get_result.is_err());
    }

    #[tokio::test]
    async fn test_set_active() {
        let (_db, service) = setup_test_env().await;

        // Create a provider (active by default)
        let request = CreateProviderRequest {
            name: "Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            }),
        };
        let created = service.create(request).await.unwrap();
        assert!(created.is_active);

        // Deactivate it
        let result = service.set_active(created.id, false).await;
        assert!(result.is_ok());
        let updated = result.unwrap();
        assert!(!updated.is_active);

        // Reactivate it
        let result = service.set_active(created.id, true).await;
        assert!(result.is_ok());
        let updated = result.unwrap();
        assert!(updated.is_active);
    }
}
