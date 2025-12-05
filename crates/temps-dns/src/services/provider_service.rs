//! DNS Provider service for managing provider configurations
//!
//! This service handles:
//! - Creating and managing DNS provider configurations
//! - Storing encrypted credentials
//! - Creating provider instances from stored configurations
//! - Testing provider connections

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{dns_managed_domains, dns_providers};
use tracing::{debug, error, info};

use crate::errors::DnsError;
use crate::providers::{
    CloudflareProvider, DnsProvider, DnsProviderType, ManualDnsProvider, NamecheapProvider,
    ProviderCredentials,
};

/// Service for managing DNS providers
#[derive(Clone)]
pub struct DnsProviderService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
}

/// Request to create a new DNS provider
#[derive(Debug, Clone)]
pub struct CreateProviderRequest {
    pub name: String,
    pub provider_type: DnsProviderType,
    pub credentials: ProviderCredentials,
    pub description: Option<String>,
}

/// Request to update an existing DNS provider
#[derive(Debug, Clone)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub credentials: Option<ProviderCredentials>,
    pub description: Option<String>,
    pub is_active: Option<bool>,
}

/// Request to add a domain to be managed by a provider
#[derive(Debug, Clone)]
pub struct AddManagedDomainRequest {
    pub domain: String,
    pub auto_manage: bool,
}

impl DnsProviderService {
    pub fn new(db: Arc<DatabaseConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Create a new DNS provider
    pub async fn create(
        &self,
        request: CreateProviderRequest,
    ) -> Result<dns_providers::Model, DnsError> {
        debug!(
            "Creating DNS provider: {} ({})",
            request.name, request.provider_type
        );

        // Serialize credentials to JSON
        let credentials_json = serde_json::to_string(&request.credentials)?;

        // Encrypt credentials
        let encrypted_credentials = self
            .encryption_service
            .encrypt_string(&credentials_json)
            .map_err(|e| DnsError::Encryption(e.to_string()))?;

        let provider = dns_providers::ActiveModel {
            name: Set(request.name),
            provider_type: Set(request.provider_type.to_string()),
            credentials: Set(encrypted_credentials),
            is_active: Set(true),
            description: Set(request.description),
            ..Default::default()
        };

        let result = provider.insert(self.db.as_ref()).await?;

        info!("Created DNS provider with id: {}", result.id);

        Ok(result)
    }

    /// Test credentials before creating a provider
    /// Returns Ok(()) if the credentials are valid, otherwise returns an error
    pub async fn test_credentials(
        &self,
        provider_type: &DnsProviderType,
        credentials: &ProviderCredentials,
    ) -> Result<(), DnsError> {
        debug!("Testing credentials for provider type: {}", provider_type);

        // Create a temporary provider instance to test the connection
        let instance: Box<dyn DnsProvider> = match provider_type {
            DnsProviderType::Cloudflare => match credentials {
                ProviderCredentials::Cloudflare(cf_creds) => {
                    let cf_provider = CloudflareProvider::new(cf_creds.clone()).map_err(|e| {
                        error!("Failed to create Cloudflare provider for testing: {}", e);
                        e
                    })?;
                    Box::new(cf_provider)
                }
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected Cloudflare credentials".to_string(),
                    ))
                }
            },
            DnsProviderType::Namecheap => match credentials {
                ProviderCredentials::Namecheap(nc_creds) => {
                    let nc_provider = NamecheapProvider::new(nc_creds.clone()).map_err(|e| {
                        error!("Failed to create Namecheap provider for testing: {}", e);
                        e
                    })?;
                    Box::new(nc_provider)
                }
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected Namecheap credentials".to_string(),
                    ))
                }
            },
            DnsProviderType::Route53 => {
                return Err(DnsError::NotSupported(
                    "Route53 provider not yet implemented".to_string(),
                ))
            }
            DnsProviderType::DigitalOcean => {
                return Err(DnsError::NotSupported(
                    "DigitalOcean provider not yet implemented".to_string(),
                ))
            }
            DnsProviderType::Manual => {
                // Manual provider doesn't need connection testing
                debug!("Manual provider - skipping connection test");
                return Ok(());
            }
        };

        // Test the connection
        let result = instance.test_connection().await?;

        if result {
            info!(
                "Credentials test successful for provider type: {}",
                provider_type
            );
            Ok(())
        } else {
            Err(DnsError::ConnectionFailed(
                "Connection test failed - credentials may be invalid".to_string(),
            ))
        }
    }

    /// Get a provider by ID
    pub async fn get(&self, id: i32) -> Result<dns_providers::Model, DnsError> {
        dns_providers::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(DnsError::ProviderNotFound(id))
    }

    /// List all providers
    pub async fn list(&self) -> Result<Vec<dns_providers::Model>, DnsError> {
        let providers = dns_providers::Entity::find()
            .order_by_desc(dns_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(providers)
    }

    /// List only active providers
    pub async fn list_active(&self) -> Result<Vec<dns_providers::Model>, DnsError> {
        let providers = dns_providers::Entity::find()
            .filter(dns_providers::Column::IsActive.eq(true))
            .order_by_desc(dns_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(providers)
    }

    /// Update a provider
    pub async fn update(
        &self,
        id: i32,
        request: UpdateProviderRequest,
    ) -> Result<dns_providers::Model, DnsError> {
        let provider = self.get(id).await?;

        let mut active_model: dns_providers::ActiveModel = provider.into();

        if let Some(name) = request.name {
            active_model.name = Set(name);
        }

        if let Some(credentials) = request.credentials {
            let credentials_json = serde_json::to_string(&credentials)?;
            let encrypted = self
                .encryption_service
                .encrypt_string(&credentials_json)
                .map_err(|e| DnsError::Encryption(e.to_string()))?;
            active_model.credentials = Set(encrypted);
        }

        if let Some(description) = request.description {
            active_model.description = Set(Some(description));
        }

        if let Some(is_active) = request.is_active {
            active_model.is_active = Set(is_active);
        }

        let result = active_model.update(self.db.as_ref()).await?;

        debug!("Updated DNS provider with id: {}", id);

        Ok(result)
    }

    /// Delete a provider
    pub async fn delete(&self, id: i32) -> Result<(), DnsError> {
        let provider = self.get(id).await?;

        dns_providers::Entity::delete_by_id(provider.id)
            .exec(self.db.as_ref())
            .await?;

        info!("Deleted DNS provider with id: {}", id);

        Ok(())
    }

    /// Set provider active status
    pub async fn set_active(
        &self,
        id: i32,
        is_active: bool,
    ) -> Result<dns_providers::Model, DnsError> {
        let provider = self.get(id).await?;

        let mut active_model: dns_providers::ActiveModel = provider.into();
        active_model.is_active = Set(is_active);

        let result = active_model.update(self.db.as_ref()).await?;

        debug!(
            "Updated DNS provider {} active status to: {}",
            id, is_active
        );

        Ok(result)
    }

    /// Create a DNS provider instance from a database model
    pub fn create_provider_instance(
        &self,
        provider: &dns_providers::Model,
    ) -> Result<Box<dyn DnsProvider>, DnsError> {
        // Decrypt credentials
        let credentials_json = self
            .encryption_service
            .decrypt_string(&provider.credentials)
            .map_err(|e| DnsError::Decryption(e.to_string()))?;

        let provider_type = DnsProviderType::from_str(&provider.provider_type)?;

        match provider_type {
            DnsProviderType::Cloudflare => {
                let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;
                match credentials {
                    ProviderCredentials::Cloudflare(cf_creds) => {
                        let cf_provider = CloudflareProvider::new(cf_creds).map_err(|e| {
                            error!("Failed to create Cloudflare provider: {}", e);
                            e
                        })?;
                        Ok(Box::new(cf_provider))
                    }
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected Cloudflare credentials".to_string(),
                    )),
                }
            }
            DnsProviderType::Namecheap => {
                let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;
                match credentials {
                    ProviderCredentials::Namecheap(nc_creds) => {
                        let nc_provider = NamecheapProvider::new(nc_creds).map_err(|e| {
                            error!("Failed to create Namecheap provider: {}", e);
                            e
                        })?;
                        Ok(Box::new(nc_provider))
                    }
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected Namecheap credentials".to_string(),
                    )),
                }
            }
            DnsProviderType::Route53 => {
                // Route53 implementation would go here
                Err(DnsError::NotSupported(
                    "Route53 provider not yet implemented".to_string(),
                ))
            }
            DnsProviderType::DigitalOcean => {
                // DigitalOcean implementation would go here
                Err(DnsError::NotSupported(
                    "DigitalOcean provider not yet implemented".to_string(),
                ))
            }
            DnsProviderType::Manual => Ok(Box::new(ManualDnsProvider::new())),
        }
    }

    /// Test a provider's connection
    pub async fn test_connection(&self, id: i32) -> Result<bool, DnsError> {
        let provider = self.get(id).await?;
        let instance = self.create_provider_instance(&provider)?;

        let result = instance.test_connection().await?;

        // Update last_used_at on success, or last_error on failure
        let mut active_model: dns_providers::ActiveModel = provider.into();
        if result {
            active_model.last_used_at = Set(Some(chrono::Utc::now()));
            active_model.last_error = Set(None);
        } else {
            active_model.last_error = Set(Some("Connection test failed".to_string()));
        }
        active_model.update(self.db.as_ref()).await?;

        Ok(result)
    }

    /// Get masked credentials for display
    pub fn get_masked_credentials(
        &self,
        provider: &dns_providers::Model,
    ) -> Result<serde_json::Value, DnsError> {
        let credentials_json = self
            .encryption_service
            .decrypt_string(&provider.credentials)
            .map_err(|e| DnsError::Decryption(e.to_string()))?;

        let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;

        Ok(credentials.masked())
    }

    // ========================================
    // Managed Domains Operations
    // ========================================

    /// Add a domain to be managed by a provider
    pub async fn add_managed_domain(
        &self,
        provider_id: i32,
        request: AddManagedDomainRequest,
    ) -> Result<dns_managed_domains::Model, DnsError> {
        // Verify provider exists
        let _provider = self.get(provider_id).await?;

        // Check if domain is already managed
        let existing = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::Domain.eq(&request.domain))
            .one(self.db.as_ref())
            .await?;

        if existing.is_some() {
            return Err(DnsError::Validation(format!(
                "Domain {} is already managed by another provider",
                request.domain
            )));
        }

        let managed_domain = dns_managed_domains::ActiveModel {
            provider_id: Set(provider_id),
            domain: Set(request.domain.clone()),
            auto_manage: Set(request.auto_manage),
            verified: Set(false),
            ..Default::default()
        };

        let result = managed_domain.insert(self.db.as_ref()).await?;

        info!(
            "Added managed domain {} to provider {}",
            request.domain, provider_id
        );

        Ok(result)
    }

    /// Remove a managed domain
    pub async fn remove_managed_domain(
        &self,
        provider_id: i32,
        domain: &str,
    ) -> Result<(), DnsError> {
        let deleted = dns_managed_domains::Entity::delete_many()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(domain))
            .exec(self.db.as_ref())
            .await?;

        if deleted.rows_affected == 0 {
            return Err(DnsError::DomainNotFound(domain.to_string()));
        }

        info!(
            "Removed managed domain {} from provider {}",
            domain, provider_id
        );

        Ok(())
    }

    /// List managed domains for a provider
    pub async fn list_managed_domains(
        &self,
        provider_id: i32,
    ) -> Result<Vec<dns_managed_domains::Model>, DnsError> {
        let domains = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .order_by_asc(dns_managed_domains::Column::Domain)
            .all(self.db.as_ref())
            .await?;

        Ok(domains)
    }

    /// Verify a managed domain (check if provider can access it)
    pub async fn verify_managed_domain(
        &self,
        provider_id: i32,
        domain: &str,
    ) -> Result<bool, DnsError> {
        let provider = self.get(provider_id).await?;
        let instance = self.create_provider_instance(&provider)?;

        // Check if provider can manage this domain
        let can_manage = instance.can_manage_domain(domain).await;

        // Update verification status
        let managed_domain = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(domain.to_string()))?;

        let mut active_model: dns_managed_domains::ActiveModel = managed_domain.into();
        active_model.verified = Set(can_manage);
        active_model.verified_at = Set(Some(chrono::Utc::now()));

        if can_manage {
            active_model.verification_error = Set(None);

            // Try to get and cache the zone ID
            if let Ok(Some(zone)) = instance.get_zone(domain).await {
                active_model.zone_id = Set(Some(zone.id));
            }
        } else {
            active_model.verification_error =
                Set(Some("Provider cannot access this domain".to_string()));
        }

        active_model.update(self.db.as_ref()).await?;

        info!(
            "Verified managed domain {} for provider {}: {}",
            domain, provider_id, can_manage
        );

        Ok(can_manage)
    }

    /// Find the provider that manages a specific domain
    pub async fn find_provider_for_domain(
        &self,
        domain: &str,
    ) -> Result<Option<(dns_providers::Model, dns_managed_domains::Model)>, DnsError> {
        // Extract base domain
        let base_domain = Self::extract_base_domain(domain);

        let managed_domain = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::Domain.eq(&base_domain))
            .filter(dns_managed_domains::Column::Verified.eq(true))
            .filter(dns_managed_domains::Column::AutoManage.eq(true))
            .one(self.db.as_ref())
            .await?;

        if let Some(managed) = managed_domain {
            let provider = self.get(managed.provider_id).await?;
            if provider.is_active {
                return Ok(Some((provider, managed)));
            }
        }

        Ok(None)
    }

    /// Extract base domain from a full domain name
    fn extract_base_domain(domain: &str) -> String {
        let parts: Vec<&str> = domain.split('.').collect();
        if parts.len() >= 2 {
            parts[parts.len() - 2..].join(".")
        } else {
            domain.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_base_domain() {
        assert_eq!(
            DnsProviderService::extract_base_domain("example.com"),
            "example.com"
        );
        assert_eq!(
            DnsProviderService::extract_base_domain("sub.example.com"),
            "example.com"
        );
        assert_eq!(
            DnsProviderService::extract_base_domain("deep.sub.example.com"),
            "example.com"
        );
    }
}
