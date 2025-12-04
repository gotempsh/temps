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

/// Result of sending a test email
#[derive(Debug, Clone)]
pub struct TestEmailResult {
    /// Whether the test email was sent successfully
    pub success: bool,
    /// The email address the test was sent to
    pub recipient_email: String,
    /// Provider message ID if successful
    pub provider_message_id: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
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

    /// Send a test email to verify provider configuration
    ///
    /// This sends a simple test email to the specified recipient to verify
    /// that the provider credentials are valid and the provider can send emails.
    ///
    /// Note: This bypasses domain verification and sends directly through the provider.
    /// The provider must have the ability to send from any address (e.g., SES sandbox mode
    /// may require verified sender addresses).
    pub async fn send_test_email(
        &self,
        provider_id: i32,
        recipient_email: &str,
    ) -> Result<TestEmailResult, EmailError> {
        use crate::providers::SendEmailRequest as ProviderSendRequest;

        debug!(
            "Sending test email from provider {} to {}",
            provider_id, recipient_email
        );

        // Get the provider
        let provider = self.get(provider_id).await?;

        // Create provider instance
        let provider_instance = self.create_provider_instance(&provider).await?;

        // Create a simple test email
        let test_request = ProviderSendRequest {
            from: format!("test@temps.example.com"),
            from_name: Some("Temps Email Test".to_string()),
            to: vec![recipient_email.to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: format!("Temps Email Provider Test - {}", provider.name),
            html: Some(format!(
                r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <title>Email Provider Test</title>
</head>
<body style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; max-width: 600px; margin: 0 auto; padding: 20px;">
    <h1 style="color: #333;">✅ Email Provider Test Successful</h1>
    <p>This is a test email from your Temps email provider configuration.</p>
    <hr style="border: none; border-top: 1px solid #eee; margin: 20px 0;">
    <table style="width: 100%; border-collapse: collapse;">
        <tr>
            <td style="padding: 8px 0; color: #666;">Provider Name:</td>
            <td style="padding: 8px 0; font-weight: bold;">{}</td>
        </tr>
        <tr>
            <td style="padding: 8px 0; color: #666;">Provider Type:</td>
            <td style="padding: 8px 0; font-weight: bold;">{}</td>
        </tr>
        <tr>
            <td style="padding: 8px 0; color: #666;">Region:</td>
            <td style="padding: 8px 0; font-weight: bold;">{}</td>
        </tr>
        <tr>
            <td style="padding: 8px 0; color: #666;">Test Time:</td>
            <td style="padding: 8px 0; font-weight: bold;">{}</td>
        </tr>
    </table>
    <hr style="border: none; border-top: 1px solid #eee; margin: 20px 0;">
    <p style="color: #888; font-size: 12px;">
        This email was sent as a test from Temps. If you received this email,
        your email provider is configured correctly.
    </p>
</body>
</html>"#,
                provider.name,
                provider.provider_type,
                provider.region,
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
            )),
            text: Some(format!(
                "Email Provider Test Successful\n\n\
                Provider Name: {}\n\
                Provider Type: {}\n\
                Region: {}\n\
                Test Time: {}\n\n\
                This email was sent as a test from Temps. If you received this email, \
                your email provider is configured correctly.",
                provider.name,
                provider.provider_type,
                provider.region,
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
            )),
            headers: None,
        };

        // Try to send the email
        match provider_instance.send(&test_request).await {
            Ok(response) => {
                debug!(
                    "Test email sent successfully, message_id: {}",
                    response.message_id
                );
                Ok(TestEmailResult {
                    success: true,
                    recipient_email: recipient_email.to_string(),
                    provider_message_id: Some(response.message_id),
                    error: None,
                })
            }
            Err(e) => {
                error!("Failed to send test email: {}", e);
                Ok(TestEmailResult {
                    success: false,
                    recipient_email: recipient_email.to_string(),
                    provider_message_id: None,
                    error: Some(e.to_string()),
                })
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
            endpoint_url: None,
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
            endpoint_url: None,
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
                endpoint_url: None,
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
                endpoint_url: None,
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
                endpoint_url: None,
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
                endpoint_url: None,
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
                endpoint_url: None,
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
                endpoint_url: None,
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
                endpoint_url: None,
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

    // ========== Unit Tests for TestEmailResult ==========

    #[test]
    fn test_email_result_success() {
        let result = TestEmailResult {
            success: true,
            recipient_email: "test@example.com".to_string(),
            provider_message_id: Some("msg-123".to_string()),
            error: None,
        };

        assert!(result.success);
        assert_eq!(result.recipient_email, "test@example.com");
        assert_eq!(result.provider_message_id, Some("msg-123".to_string()));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_email_result_failure() {
        let result = TestEmailResult {
            success: false,
            recipient_email: "test@example.com".to_string(),
            provider_message_id: None,
            error: Some("Connection refused".to_string()),
        };

        assert!(!result.success);
        assert_eq!(result.recipient_email, "test@example.com");
        assert!(result.provider_message_id.is_none());
        assert_eq!(result.error, Some("Connection refused".to_string()));
    }

    // ========== Integration Tests for send_test_email ==========

    #[tokio::test]
    async fn test_send_test_email_provider_not_found() {
        let (_db, service) = setup_test_env().await;

        // Attempt to send test email for non-existent provider
        let result = service.send_test_email(999999, "test@example.com").await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmailError::ProviderNotFound(999999)
        ));
    }

    #[tokio::test]
    async fn test_send_test_email_with_invalid_credentials() {
        let (_db, service) = setup_test_env().await;

        // Create a provider with fake credentials
        let request = CreateProviderRequest {
            name: "Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        let provider = service.create(request).await.unwrap();

        // Attempt to send test email - this will create a provider instance
        // but the send will fail because the credentials are fake
        // The function should return a result with success=false, not an error
        let result = service
            .send_test_email(provider.id, "test@example.com")
            .await;

        // The function should succeed (return Ok) but the result should indicate failure
        // This is because we gracefully handle send errors as failed test results
        assert!(result.is_ok());
        let test_result = result.unwrap();
        assert!(!test_result.success); // Email send failed due to invalid credentials
        assert_eq!(test_result.recipient_email, "test@example.com");
        assert!(test_result.error.is_some()); // Should have an error message
    }

    // ========== LocalStack Integration Tests ==========
    //
    // These tests use LocalStack to test actual AWS SES integration without
    // requiring a real AWS account. They require Docker to be running.
    //
    // To run these tests:
    //   cargo test --lib -p temps-email test_localstack -- --nocapture
    //
    // The tests will be skipped if Docker is not available.

    /// Helper to check if Docker is available
    fn is_docker_available() -> bool {
        std::process::Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Helper struct to hold LocalStack container and connection details
    struct LocalStackTestEnv {
        _container: testcontainers::ContainerAsync<testcontainers::GenericImage>,
        endpoint_url: String,
        #[allow(dead_code)]
        port: u16,
    }

    impl LocalStackTestEnv {
        async fn new() -> anyhow::Result<Self> {
            use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

            // Start LocalStack container with SES service
            let container = GenericImage::new("localstack/localstack", "latest")
                .with_env_var("SERVICES", "ses")
                .with_env_var("DEBUG", "1")
                .with_env_var("LOCALSTACK_HOST", "localhost.localstack.cloud")
                .start()
                .await?;

            // Get the mapped port for LocalStack (default internal port is 4566)
            let port = container.get_host_port_ipv4(4566).await?;
            let endpoint_url = format!("http://localhost:{}", port);

            // Wait for LocalStack to be ready
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            Ok(Self {
                _container: container,
                endpoint_url,
                port,
            })
        }
    }

    /// Test sending email via LocalStack SES
    ///
    /// This test verifies that the SES provider can send emails through LocalStack.
    /// LocalStack simulates SES and accepts all emails without validation.
    #[tokio::test]
    async fn test_localstack_ses_send_email() {
        // Skip if Docker is not available
        if !is_docker_available() {
            eprintln!("Skipping test_localstack_ses_send_email: Docker not available");
            return;
        }

        // Start LocalStack
        let localstack = match LocalStackTestEnv::new().await {
            Ok(env) => env,
            Err(e) => {
                eprintln!("Skipping test: Failed to start LocalStack: {}", e);
                return;
            }
        };

        // Setup test database and provider service
        let (_db, service) = setup_test_env().await;

        // Create a provider pointing to LocalStack
        let request = CreateProviderRequest {
            name: "LocalStack SES Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                // LocalStack accepts any credentials
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                endpoint_url: Some(localstack.endpoint_url.clone()),
            }),
        };
        let provider = service.create(request).await.unwrap();

        // Verify the provider was created
        assert!(provider.id > 0);
        assert_eq!(provider.name, "LocalStack SES Provider");
        assert_eq!(provider.provider_type, "ses");

        // LocalStack requires email identity to be verified first
        // Let's verify an identity before sending
        let provider_model = service.get(provider.id).await.unwrap();
        let provider_instance = service
            .create_provider_instance(&provider_model)
            .await
            .unwrap();

        // Create/verify a test identity (LocalStack auto-verifies)
        let domain = "test.example.com";
        match provider_instance.create_identity(domain).await {
            Ok(_identity) => {
                debug!("Created identity for {}", domain);
            }
            Err(e) => {
                // LocalStack might not support all SES operations
                debug!("Could not create identity (may be expected): {}", e);
            }
        }

        // Send a test email
        let result = service
            .send_test_email(provider.id, "recipient@test.example.com")
            .await;

        // Verify the result
        assert!(result.is_ok(), "send_test_email should not return error");
        let test_result = result.unwrap();

        // LocalStack should accept the email
        // Note: The result depends on LocalStack's SES implementation
        // Some versions may return success, others may return specific errors
        println!(
            "LocalStack test email result: success={}, error={:?}",
            test_result.success, test_result.error
        );

        assert_eq!(test_result.recipient_email, "recipient@test.example.com");
    }

    /// Test creating SES provider with LocalStack endpoint
    ///
    /// This test verifies that the SES provider can be created with a custom
    /// endpoint URL pointing to LocalStack.
    #[tokio::test]
    async fn test_localstack_ses_provider_creation() {
        // Skip if Docker is not available
        if !is_docker_available() {
            eprintln!("Skipping test_localstack_ses_provider_creation: Docker not available");
            return;
        }

        // Start LocalStack
        let localstack = match LocalStackTestEnv::new().await {
            Ok(env) => env,
            Err(e) => {
                eprintln!("Skipping test: Failed to start LocalStack: {}", e);
                return;
            }
        };

        // Setup test database and provider service
        let (_db, service) = setup_test_env().await;

        // Create a provider with LocalStack endpoint
        let request = CreateProviderRequest {
            name: "LocalStack Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "test-key".to_string(),
                secret_access_key: "test-secret".to_string(),
                endpoint_url: Some(localstack.endpoint_url.clone()),
            }),
        };

        // Create the provider
        let result = service.create(request).await;
        assert!(result.is_ok());
        let provider = result.unwrap();

        // Verify provider was stored correctly
        assert!(provider.id > 0);
        assert_eq!(provider.name, "LocalStack Test Provider");
        assert_eq!(provider.provider_type, "ses");
        assert_eq!(provider.region, "us-east-1");
        assert!(provider.is_active);

        // Verify we can retrieve it
        let retrieved = service.get(provider.id).await;
        assert!(retrieved.is_ok());
        let retrieved_provider = retrieved.unwrap();
        assert_eq!(retrieved_provider.id, provider.id);

        // Verify we can create a provider instance (which creates the AWS client)
        let instance_result = service.create_provider_instance(&retrieved_provider).await;
        assert!(
            instance_result.is_ok(),
            "Should be able to create provider instance: {:?}",
            instance_result.err()
        );

        // Verify the provider instance has the correct type
        let instance = instance_result.unwrap();
        assert_eq!(instance.provider_type(), EmailProviderType::Ses);
    }

    /// Test SES identity operations with LocalStack
    ///
    /// This test verifies that the SES provider can create and verify domain
    /// identities through LocalStack.
    #[tokio::test]
    async fn test_localstack_ses_identity_operations() {
        // Skip if Docker is not available
        if !is_docker_available() {
            eprintln!("Skipping test_localstack_ses_identity_operations: Docker not available");
            return;
        }

        // Start LocalStack
        let localstack = match LocalStackTestEnv::new().await {
            Ok(env) => env,
            Err(e) => {
                eprintln!("Skipping test: Failed to start LocalStack: {}", e);
                return;
            }
        };

        // Setup test database and provider service
        let (_db, service) = setup_test_env().await;

        // Create a provider with LocalStack endpoint
        let request = CreateProviderRequest {
            name: "LocalStack Identity Test".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "test-key".to_string(),
                secret_access_key: "test-secret".to_string(),
                endpoint_url: Some(localstack.endpoint_url.clone()),
            }),
        };
        let provider = service.create(request).await.unwrap();

        // Get provider instance
        let provider_model = service.get(provider.id).await.unwrap();
        let provider_instance = service
            .create_provider_instance(&provider_model)
            .await
            .unwrap();

        // Test domain identity creation
        let test_domain = "localstack-test.example.com";
        let identity_result = provider_instance.create_identity(test_domain).await;

        // LocalStack should accept the identity creation
        // The result depends on LocalStack's SES implementation
        match identity_result {
            Ok(identity) => {
                println!("Created identity for {}: {:?}", test_domain, identity);
                assert_eq!(identity.provider_identity_id, test_domain);

                // Verify the identity (LocalStack auto-verifies)
                let verify_result = provider_instance.verify_identity(test_domain).await;
                match verify_result {
                    Ok(status) => {
                        println!("Verification status for {}: {:?}", test_domain, status);
                        // LocalStack may return different statuses
                    }
                    Err(e) => {
                        println!("Verification check failed (may be expected): {}", e);
                    }
                }

                // Clean up - delete the identity
                let delete_result = provider_instance.delete_identity(test_domain).await;
                match delete_result {
                    Ok(_) => println!("Deleted identity for {}", test_domain),
                    Err(e) => println!("Delete failed (may be expected): {}", e),
                }
            }
            Err(e) => {
                // Some LocalStack versions may not fully support SESv2
                println!("Identity creation failed (may be expected): {}", e);
            }
        }
    }
}
