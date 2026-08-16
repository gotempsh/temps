//! DNS Provider service for managing provider configurations
//!
//! This service handles:
//! - Creating and managing DNS provider configurations
//! - Storing encrypted credentials
//! - Creating provider instances from stored configurations
//! - Testing provider connections

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{dns_managed_domains, dns_providers};
use tracing::{debug, error, info};

use crate::errors::DnsError;
use crate::providers::{
    AzureProvider, CloudflareProvider, DigitalOceanProvider, DnsProvider, DnsProviderType,
    GcpProvider, ManualDnsProvider, NamecheapProvider, PebbleDnsProvider, ProviderCredentials,
    Route53Provider,
};
use crate::services::hostname_sync::{self, HostnameModeResult};
use temps_core::{AppSettings, PublicHostnameStrategy};

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
    /// Optional generated hostname mode (`"standard"`/`"flat"`); defaults to standard.
    pub generated_hostname_mode: Option<String>,
    /// Opt in to reconciling generated hostnames into this domain's DNS zone.
    pub sync_generated_records: bool,
}

/// Request to update a managed domain's settings.
#[derive(Debug, Clone, Default)]
pub struct UpdateManagedDomainRequest {
    pub generated_hostname_mode: Option<String>,
    pub sync_generated_records: Option<bool>,
    pub auto_manage: Option<bool>,
}

impl DnsProviderService {
    // A valid DNS name has at most 127 labels. Keeping the candidate set capped
    // also bounds malformed input before it reaches an IN predicate.
    const MAX_AUTHORITATIVE_SUFFIX_CANDIDATES: usize = 127;
    const NORMALIZED_MANAGED_DOMAIN_SQL: &'static str =
        "LOWER(REGEXP_REPLACE(RTRIM(BTRIM(\"dns_managed_domains\".\"domain\"), '.'), '^((\\*\\.)+)', ''))";

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
            DnsProviderType::Route53 => match credentials {
                ProviderCredentials::Route53(r53_creds) => {
                    let r53_provider = Route53Provider::new(r53_creds.clone()).map_err(|e| {
                        error!("Failed to create Route53 provider for testing: {}", e);
                        e
                    })?;
                    Box::new(r53_provider)
                }
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected Route53 credentials".to_string(),
                    ))
                }
            },
            DnsProviderType::DigitalOcean => match credentials {
                ProviderCredentials::DigitalOcean(do_creds) => {
                    let do_provider = DigitalOceanProvider::new(do_creds.clone()).map_err(|e| {
                        error!("Failed to create DigitalOcean provider for testing: {}", e);
                        e
                    })?;
                    Box::new(do_provider)
                }
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected DigitalOcean credentials".to_string(),
                    ))
                }
            },
            DnsProviderType::Gcp => match credentials {
                ProviderCredentials::Gcp(gcp_creds) => {
                    let gcp_provider = GcpProvider::new(gcp_creds.clone()).map_err(|e| {
                        error!("Failed to create GCP provider for testing: {}", e);
                        e
                    })?;
                    Box::new(gcp_provider)
                }
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected GCP credentials".to_string(),
                    ))
                }
            },
            DnsProviderType::Azure => match credentials {
                ProviderCredentials::Azure(azure_creds) => {
                    let azure_provider = AzureProvider::new(azure_creds.clone()).map_err(|e| {
                        error!("Failed to create Azure provider for testing: {}", e);
                        e
                    })?;
                    Box::new(azure_provider)
                }
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected Azure credentials".to_string(),
                    ))
                }
            },
            DnsProviderType::Manual => {
                // Manual provider doesn't need connection testing
                debug!("Manual provider - skipping connection test");
                return Ok(());
            }
            DnsProviderType::Pebble => match credentials {
                ProviderCredentials::Pebble(pebble_creds) => {
                    let pebble_provider =
                        PebbleDnsProvider::new(pebble_creds.clone()).map_err(|e| {
                            error!("Failed to create Pebble provider for testing: {}", e);
                            e
                        })?;
                    Box::new(pebble_provider)
                }
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected Pebble credentials".to_string(),
                    ))
                }
            },
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
    /// Whether the provider advertises the flat-hostname capability (i.e. its
    /// wildcard TLS only covers one label, like Cloudflare Universal SSL). Used
    /// by the UI to surface and recommend the Flat hostname mode. Returns false
    /// if the provider instance can't be constructed.
    pub fn flat_hostnames_supported(&self, provider: &dns_providers::Model) -> bool {
        self.create_provider_instance(provider)
            .map(|instance| instance.capabilities().flat_hostnames)
            .unwrap_or(false)
    }

    pub fn create_provider_instance(
        &self,
        provider: &dns_providers::Model,
    ) -> Result<Box<dyn DnsProvider>, DnsError> {
        if !provider.is_active {
            return Err(DnsError::ProviderInactive {
                provider_id: provider.id,
                provider_name: provider.name.clone(),
            });
        }

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
                let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;
                match credentials {
                    ProviderCredentials::Route53(r53_creds) => {
                        let r53_provider = Route53Provider::new(r53_creds).map_err(|e| {
                            error!("Failed to create Route53 provider: {}", e);
                            e
                        })?;
                        Ok(Box::new(r53_provider))
                    }
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected Route53 credentials".to_string(),
                    )),
                }
            }
            DnsProviderType::DigitalOcean => {
                let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;
                match credentials {
                    ProviderCredentials::DigitalOcean(do_creds) => {
                        let do_provider = DigitalOceanProvider::new(do_creds).map_err(|e| {
                            error!("Failed to create DigitalOcean provider: {}", e);
                            e
                        })?;
                        Ok(Box::new(do_provider))
                    }
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected DigitalOcean credentials".to_string(),
                    )),
                }
            }
            DnsProviderType::Gcp => {
                let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;
                match credentials {
                    ProviderCredentials::Gcp(gcp_creds) => {
                        let gcp_provider = GcpProvider::new(gcp_creds).map_err(|e| {
                            error!("Failed to create GCP provider: {}", e);
                            e
                        })?;
                        Ok(Box::new(gcp_provider))
                    }
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected GCP credentials".to_string(),
                    )),
                }
            }
            DnsProviderType::Azure => {
                let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;
                match credentials {
                    ProviderCredentials::Azure(azure_creds) => {
                        let azure_provider = AzureProvider::new(azure_creds).map_err(|e| {
                            error!("Failed to create Azure provider: {}", e);
                            e
                        })?;
                        Ok(Box::new(azure_provider))
                    }
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected Azure credentials".to_string(),
                    )),
                }
            }
            DnsProviderType::Manual => Ok(Box::new(ManualDnsProvider::new())),
            DnsProviderType::Pebble => {
                let credentials: ProviderCredentials = serde_json::from_str(&credentials_json)?;
                match credentials {
                    ProviderCredentials::Pebble(pebble_creds) => {
                        let pebble_provider =
                            PebbleDnsProvider::new(pebble_creds).map_err(|e| {
                                error!("Failed to create Pebble provider: {}", e);
                                e
                            })?;
                        Ok(Box::new(pebble_provider))
                    }
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected Pebble credentials".to_string(),
                    )),
                }
            }
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
        if !provider.is_active {
            return Err(DnsError::ProviderInactive {
                provider_id: provider.id,
                provider_name: provider.name.clone(),
            });
        }

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

        let canonical_domain = Self::normalize_domain(&request.domain);
        if canonical_domain.is_empty() {
            return Err(DnsError::Validation(format!(
                "Managed domain '{}' has no DNS labels after canonicalization",
                request.domain
            )));
        }

        // Check the same canonical form used by authoritative lookup. New rows
        // are stored canonically, so the existing raw unique constraint also
        // closes the race between this preflight query and the insert.
        let existing = dns_managed_domains::Entity::find()
            .filter(sea_orm::sea_query::Expr::cust_with_values(
                format!("{} = $1", Self::NORMALIZED_MANAGED_DOMAIN_SQL),
                [canonical_domain.clone()],
            ))
            .limit(1)
            .one(self.db.as_ref())
            .await?;

        if let Some(existing) = existing {
            return Err(DnsError::ManagedDomainAlreadyExists {
                requested_domain: request.domain,
                canonical_domain,
                existing_managed_domain_id: existing.id,
                existing_provider_id: existing.provider_id,
            });
        }

        // Normalize the requested mode; unknown values fall back to standard.
        let mode = temps_core::PublicHostnameStrategy::from_db_str(
            request
                .generated_hostname_mode
                .as_deref()
                .unwrap_or("standard"),
        )
        .as_db_str()
        .to_string();

        let managed_domain = dns_managed_domains::ActiveModel {
            provider_id: Set(provider_id),
            domain: Set(canonical_domain.clone()),
            auto_manage: Set(request.auto_manage),
            verified: Set(false),
            generated_hostname_mode: Set(mode),
            sync_generated_records: Set(request.sync_generated_records),
            ..Default::default()
        };

        let result = managed_domain.insert(self.db.as_ref()).await?;

        info!(
            "Added managed domain {} to provider {}",
            canonical_domain, provider_id
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

    /// Load one managed domain so authorization can be evaluated against the
    /// resulting settings before a handler mutates it.
    pub async fn get_managed_domain(
        &self,
        provider_id: i32,
        domain: &str,
    ) -> Result<dns_managed_domains::Model, DnsError> {
        dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(domain.to_string()))
    }

    /// Verify a managed domain (check if provider can access it)
    pub async fn verify_managed_domain(
        &self,
        provider_id: i32,
        domain: &str,
    ) -> Result<bool, DnsError> {
        let provider = self.get(provider_id).await?;
        let instance = self.create_provider_instance(&provider)?;

        // Distinguish "token lacks zone access" (PermissionDenied) from "zone
        // absent" so the UI can flag an incorrectly scoped token.
        let access = instance.check_zone_access(domain).await;
        let can_manage = access.is_ok();
        let (zone_access_ok, zone_access_error) = match &access {
            Ok(()) => (Some(true), None),
            Err(DnsError::PermissionDenied(msg)) => (Some(false), Some(msg.clone())),
            Err(e) => (Some(false), Some(e.to_string())),
        };

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
        active_model.zone_access_ok = Set(zone_access_ok);
        active_model.zone_access_error = Set(zone_access_error);

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

    /// Update a managed domain's settings (hostname mode, sync opt-in,
    /// auto-manage). Persists the values only; switching the mode does NOT
    /// recompute existing hostnames — callers use [`apply_hostname_mode`] for
    /// that.
    pub async fn update_managed_domain(
        &self,
        provider_id: i32,
        domain: &str,
        request: UpdateManagedDomainRequest,
    ) -> Result<dns_managed_domains::Model, DnsError> {
        let managed = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(domain.to_string()))?;

        let mut active: dns_managed_domains::ActiveModel = managed.into();
        if let Some(mode) = request.generated_hostname_mode {
            active.generated_hostname_mode = Set(PublicHostnameStrategy::from_db_str(&mode)
                .as_db_str()
                .to_string());
        }
        if let Some(sync) = request.sync_generated_records {
            active.sync_generated_records = Set(sync);
        }
        if let Some(auto) = request.auto_manage {
            active.auto_manage = Set(auto);
        }

        Ok(active.update(self.db.as_ref()).await?)
    }

    /// Read the instance-wide preview domain and edge target from the settings
    /// singleton.
    async fn hosting_settings(&self) -> (String, Option<String>) {
        let settings = temps_entities::settings::Entity::find()
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|s| AppSettings::from_json(s.data))
            .unwrap_or_default();
        (settings.preview_domain, settings.edge_target)
    }

    /// Preview a hostname-mode change for a managed domain without writing
    /// anything: the generated hostnames that would change, plus (when
    /// `want_sync`) the DNS records the sync would reconcile, plus the token's
    /// zone-access state.
    pub async fn preview_hostname_mode(
        &self,
        provider_id: i32,
        domain: &str,
        target: PublicHostnameStrategy,
        want_sync: bool,
    ) -> Result<HostnameModeResult, DnsError> {
        self.hostname_mode_operation(provider_id, domain, target, want_sync, true)
            .await
    }

    /// Apply a hostname-mode change: persist the mode and (when `sync_dns`)
    /// reconcile the provider's DNS zone. Returns the changes that were applied.
    /// The caller is responsible for triggering a route reload so derived
    /// hostnames take effect.
    pub async fn apply_hostname_mode(
        &self,
        provider_id: i32,
        domain: &str,
        target: PublicHostnameStrategy,
        sync_dns: bool,
    ) -> Result<HostnameModeResult, DnsError> {
        self.hostname_mode_operation(provider_id, domain, target, sync_dns, false)
            .await
    }

    /// Shared preview/apply implementation. With `dry_run` it computes the
    /// changes without writing; otherwise it persists the mode and executes the
    /// DNS reconciliation.
    async fn hostname_mode_operation(
        &self,
        provider_id: i32,
        domain: &str,
        target: PublicHostnameStrategy,
        sync_dns: bool,
        dry_run: bool,
    ) -> Result<HostnameModeResult, DnsError> {
        // Confirm the domain belongs to this provider.
        let managed = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(domain.to_string()))?;

        let (preview_domain, edge_target) = self.hosting_settings().await;

        // Only domains matching the instance's preview base domain govern
        // generated hostnames; others have no generated hosts to change.
        let base = temps_core::public_base_domain(&preview_domain);
        let applies = base == domain.to_ascii_lowercase()
            || base.ends_with(&format!(".{}", domain.to_ascii_lowercase()));

        let hostname_changes = if applies {
            hostname_sync::compute_hostname_changes(self.db.as_ref(), &preview_domain, target).await
        } else {
            Vec::new()
        };

        let mut result = HostnameModeResult {
            hostname_changes,
            dns_changes: Vec::new(),
            zone_access_ok: None,
        };

        if sync_dns {
            let provider = self.get(provider_id).await?;
            let instance = self.create_provider_instance(&provider)?;

            // Verify token zone access before attempting any record changes.
            match instance.check_zone_access(domain).await {
                Ok(()) => result.zone_access_ok = Some(true),
                Err(DnsError::PermissionDenied(msg)) => {
                    result.zone_access_ok = Some(false);
                    if !dry_run {
                        return Err(DnsError::PermissionDenied(msg));
                    }
                    return Ok(result);
                }
                Err(e) => {
                    result.zone_access_ok = Some(false);
                    if !dry_run {
                        return Err(e);
                    }
                    return Ok(result);
                }
            }

            if let Some(edge_target) = edge_target.as_deref() {
                let desired = hostname_sync::enumerate_generated_hosts(
                    self.db.as_ref(),
                    &preview_domain,
                    target,
                )
                .await;
                result.dns_changes = hostname_sync::reconcile_zone_records(
                    instance.as_ref(),
                    domain,
                    &desired,
                    edge_target,
                    dry_run,
                )
                .await?;
            }
        }

        if !dry_run {
            let mut active: dns_managed_domains::ActiveModel = managed.into();
            active.generated_hostname_mode = Set(target.as_db_str().to_string());
            if sync_dns {
                active.sync_generated_records = Set(true);
            }
            active.update(self.db.as_ref()).await?;
        }

        Ok(result)
    }

    /// Resolve the public hostname strategy for a preview/base domain by
    /// matching it against managed domains. Defaults to `Standard` when no
    /// managed domain matches.
    pub async fn resolve_hostname_strategy(
        &self,
        preview_domain: &str,
    ) -> temps_core::PublicHostnameStrategy {
        let map = self.hostname_strategy_map().await;
        temps_core::public_hostname_resolver::match_strategy(&map, preview_domain)
    }

    /// Load every managed domain's hostname strategy keyed by its (lowercased)
    /// base domain. Errors are swallowed into an empty map so hostname
    /// generation degrades to `Standard` rather than failing.
    pub async fn hostname_strategy_map(
        &self,
    ) -> std::collections::HashMap<String, temps_core::PublicHostnameStrategy> {
        dns_managed_domains::Entity::find()
            .all(self.db.as_ref())
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|d| {
                (
                    d.domain.to_ascii_lowercase(),
                    temps_core::PublicHostnameStrategy::from_db_str(&d.generated_hostname_mode),
                )
            })
            .collect()
    }

    /// Find the provider that manages a specific domain
    pub async fn find_provider_for_domain(
        &self,
        domain: &str,
    ) -> Result<Option<(dns_providers::Model, dns_managed_domains::Model)>, DnsError> {
        self.find_active_managed_domain_candidate(domain, None, true)
            .await
    }

    /// Find the verified zone belonging to `provider_id` that authoritatively
    /// covers `domain`. Human-triggered writes do not require `auto_manage`, but
    /// they must never be allowed to use arbitrary provider credentials.
    pub async fn find_verified_zone_for_provider(
        &self,
        provider_id: i32,
        domain: &str,
    ) -> Result<Option<dns_managed_domains::Model>, DnsError> {
        Ok(self
            .find_active_managed_domain_candidate(domain, Some(provider_id), false)
            .await?
            .map(|(_, managed)| managed))
    }

    /// Find the longest authoritative suffix without loading the managed-zone
    /// table. The normalized equality predicate is backed by
    /// `idx_dns_managed_domains_normalized_domain`, preserving legacy mixed-case,
    /// wildcard, whitespace, and trailing-dot rows without a sequential scan.
    async fn find_active_managed_domain_candidate(
        &self,
        domain: &str,
        provider_id: Option<i32>,
        require_auto_manage: bool,
    ) -> Result<Option<(dns_providers::Model, dns_managed_domains::Model)>, DnsError> {
        let candidates = Self::authoritative_suffix_candidates(domain);
        if candidates.is_empty() {
            return Ok(None);
        }

        let placeholders = (1..=candidates.len())
            .map(|position| format!("${position}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut query = dns_managed_domains::Entity::find()
            .find_also_related(dns_providers::Entity)
            .filter(sea_orm::sea_query::Expr::cust_with_values(
                format!(
                    "{} IN ({placeholders})",
                    Self::NORMALIZED_MANAGED_DOMAIN_SQL
                ),
                candidates,
            ))
            .filter(dns_managed_domains::Column::Verified.eq(true))
            // This predicate must execute in the same SQL statement before
            // LIMIT, otherwise an inactive duplicate can shadow an active zone.
            .filter(dns_providers::Column::IsActive.eq(true));
        if let Some(provider_id) = provider_id {
            query = query.filter(dns_managed_domains::Column::ProviderId.eq(provider_id));
        }
        if require_auto_manage {
            query = query.filter(dns_managed_domains::Column::AutoManage.eq(true));
        }

        let rows = query
            .order_by_desc(sea_orm::sea_query::Expr::cust(format!(
                "CHAR_LENGTH({})",
                Self::NORMALIZED_MANAGED_DOMAIN_SQL
            )))
            .order_by_asc(dns_managed_domains::Column::Id)
            .limit(2)
            .all(self.db.as_ref())
            .await
            .map_err(DnsError::from)?
            .into_iter()
            .filter_map(|(managed, provider)| provider.map(|provider| (provider, managed)))
            .collect::<Vec<_>>();

        let Some((provider, managed)) = rows.first() else {
            return Ok(None);
        };
        let canonical_zone = Self::normalize_domain(&managed.domain);

        if rows.get(1).is_some_and(|(_, candidate)| {
            Self::normalize_domain(&candidate.domain) == canonical_zone
        }) {
            return Err(DnsError::AmbiguousManagedDomain {
                requested_domain: domain.to_string(),
                canonical_zone,
                managed_domain_ids: rows.iter().map(|(_, managed)| managed.id).collect(),
                provider_ids: rows.iter().map(|(provider, _)| provider.id).collect(),
            });
        }

        let mut selected = managed.clone();
        selected.domain = canonical_zone;
        Ok(Some((provider.clone(), selected)))
    }

    fn authoritative_suffix_candidates(domain: &str) -> Vec<String> {
        let normalized = Self::normalize_domain(domain);
        let mut candidates = Vec::new();
        let mut suffix = normalized.as_str();

        while !suffix.is_empty() && candidates.len() < Self::MAX_AUTHORITATIVE_SUFFIX_CANDIDATES {
            candidates.push(suffix.to_string());
            suffix = match suffix.find('.') {
                Some(separator) => &suffix[separator + 1..],
                None => break,
            };
        }

        candidates
    }

    #[cfg(test)]
    fn longest_managed_domain_match(
        domain: &str,
        managed_domains: Vec<dns_managed_domains::Model>,
    ) -> Option<dns_managed_domains::Model> {
        let domain = Self::normalize_domain(domain);
        managed_domains
            .into_iter()
            .filter(|managed| {
                let zone = Self::normalize_domain(&managed.domain);
                domain == zone || domain.ends_with(&format!(".{zone}"))
            })
            .max_by_key(|managed| Self::normalize_domain(&managed.domain).len())
    }

    fn normalize_domain(domain: &str) -> String {
        domain
            .trim()
            .trim_start_matches("*.")
            .trim_end_matches('.')
            .to_ascii_lowercase()
    }
}

#[async_trait::async_trait]
impl temps_core::PublicHostnameResolver for DnsProviderService {
    async fn strategy_for(&self, preview_domain: &str) -> temps_core::PublicHostnameStrategy {
        self.resolve_hostname_strategy(preview_domain).await
    }

    async fn strategy_map(
        &self,
    ) -> std::collections::HashMap<String, temps_core::PublicHostnameStrategy> {
        self.hostname_strategy_map().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn managed_domain(id: i32, provider_id: i32, domain: &str) -> dns_managed_domains::Model {
        let now = chrono::Utc::now();
        dns_managed_domains::Model {
            id,
            provider_id,
            domain: domain.to_string(),
            zone_id: None,
            auto_manage: true,
            verified: true,
            verified_at: Some(now),
            verification_error: None,
            generated_hostname_mode: "standard".to_string(),
            sync_generated_records: false,
            zone_access_ok: Some(true),
            zone_access_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn dns_provider(id: i32, name: &str, is_active: bool) -> dns_providers::Model {
        let now = chrono::Utc::now();
        dns_providers::Model {
            id,
            name: name.to_string(),
            provider_type: "cloudflare".to_string(),
            credentials: "deliberately-not-encrypted".to_string(),
            is_active,
            description: None,
            last_used_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn inactive_provider_is_rejected_before_credentials_are_decrypted() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = DnsProviderService::new(
            db,
            Arc::new(EncryptionService::new_from_password(
                "inactive-provider-test",
            )),
        );

        let result =
            service.create_provider_instance(&dns_provider(42, "disabled-cloudflare", false));

        assert!(matches!(
            result,
            Err(DnsError::ProviderInactive {
                provider_id: 42,
                provider_name
            }) if provider_name == "disabled-cloudflare"
        ));
    }

    #[test]
    fn longest_managed_domain_suffix_wins() {
        let matched = DnsProviderService::longest_managed_domain_match(
            "API.Dev.Example.COM.",
            vec![
                managed_domain(1, 10, "example.com"),
                managed_domain(2, 20, "dev.example.com"),
            ],
        );

        assert_eq!(
            matched.map(|managed| (managed.provider_id, managed.domain)),
            Some((20, "dev.example.com".to_string()))
        );
    }

    #[test]
    fn managed_domain_match_respects_label_boundaries_and_wildcards() {
        let domains = vec![managed_domain(1, 10, "example.com")];
        assert_eq!(
            DnsProviderService::longest_managed_domain_match("*.www.example.com", domains.clone())
                .map(|managed| managed.domain),
            Some("example.com".to_string())
        );
        assert!(
            DnsProviderService::longest_managed_domain_match("notexample.com", domains).is_none()
        );
    }

    #[test]
    fn authoritative_suffix_candidates_are_normalized_longest_first() {
        assert_eq!(
            DnsProviderService::authoritative_suffix_candidates("  *.API.Dev.Example.COM.  "),
            vec![
                "api.dev.example.com",
                "dev.example.com",
                "example.com",
                "com"
            ]
        );
    }

    #[test]
    fn managed_domain_canonicalization_strips_repeated_wildcards_and_root_dots() {
        assert_eq!(
            DnsProviderService::normalize_domain("  *.*.*.API.Example.COM...  "),
            "api.example.com"
        );
    }

    #[test]
    fn authoritative_suffix_candidates_are_bounded_for_malformed_input() {
        let domain = std::iter::repeat_n("label", 200)
            .collect::<Vec<_>>()
            .join(".");

        let candidates = DnsProviderService::authoritative_suffix_candidates(&domain);

        assert_eq!(
            candidates.len(),
            DnsProviderService::MAX_AUTHORITATIVE_SUFFIX_CANDIDATES
        );
        assert_eq!(candidates.first(), Some(&domain));
    }

    #[tokio::test]
    async fn verified_zone_query_binds_candidates_before_provider_filter() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<dns_managed_domains::Model>::new()])
                .into_connection(),
        );
        let service = DnsProviderService::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password("provider-query-test")),
        );

        let result = service
            .find_verified_zone_for_provider(42, "*.API.Dev.Example.COM.")
            .await;

        assert!(matches!(result, Ok(None)));
        drop(service);
        let transaction_log = Arc::try_unwrap(db)
            .expect("test must release the database connection")
            .into_transaction_log();
        let statement = &transaction_log[0].statements()[0];
        assert!(statement.sql.contains(
            "LOWER(REGEXP_REPLACE(RTRIM(BTRIM(\"dns_managed_domains\".\"domain\"), '.'), '^((\\*\\.)+)', '')) IN ($1, $2, $3, $4)"
        ));
        assert!(statement.sql.contains("LEFT JOIN \"dns_providers\""));
        assert!(statement
            .sql
            .contains(r#""dns_providers"."is_active" = $6"#));
        assert!(statement
            .sql
            .contains(r#""dns_managed_domains"."provider_id" = $7"#));
        assert!(statement.sql.contains("LIMIT $8"));
        assert_eq!(
            format!("{:?}", statement.values),
            "Some(Values([String(Some(\"api.dev.example.com\")), String(Some(\"dev.example.com\")), String(Some(\"example.com\")), String(Some(\"com\")), Bool(Some(true)), Bool(Some(true)), Int(Some(42)), BigUnsigned(Some(2))]))"
        );
    }

    #[tokio::test]
    async fn duplicate_best_canonical_zone_fails_closed() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![
                    (
                        managed_domain(11, 101, "Example.COM."),
                        Some(dns_provider(101, "first", true)),
                    ),
                    (
                        managed_domain(12, 202, "*.example.com"),
                        Some(dns_provider(202, "second", true)),
                    ),
                ]])
                .into_connection(),
        );
        let service = DnsProviderService::new(
            db,
            Arc::new(EncryptionService::new_from_password("ambiguous-zone-test")),
        );

        let result = service.find_provider_for_domain("api.example.com").await;

        assert!(matches!(
            result,
            Err(DnsError::AmbiguousManagedDomain {
                requested_domain,
                canonical_zone,
                managed_domain_ids,
                provider_ids,
            }) if requested_domain == "api.example.com"
                && canonical_zone == "example.com"
                && managed_domain_ids == vec![11, 12]
                && provider_ids == vec![101, 202]
        ));
    }

    #[tokio::test]
    async fn more_specific_zone_wins_without_treating_parent_as_ambiguous() {
        let child = managed_domain(11, 101, "*.Dev.Example.COM.");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![
                    (child.clone(), Some(dns_provider(101, "child", true))),
                    (
                        managed_domain(12, 202, "example.com"),
                        Some(dns_provider(202, "parent", true)),
                    ),
                ]])
                .into_connection(),
        );
        let service = DnsProviderService::new(
            db,
            Arc::new(EncryptionService::new_from_password("parent-zone-test")),
        );

        let result = service
            .find_provider_for_domain("api.dev.example.com")
            .await;

        assert!(matches!(
            result,
            Ok(Some((provider, managed))) if provider.id == 101
                && managed.id == child.id
                && managed.domain == "dev.example.com"
        ));
    }

    #[tokio::test]
    async fn add_managed_domain_rejects_canonical_duplicate_using_indexed_predicate() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![dns_provider(101, "provider", true)]])
                .append_query_results(vec![vec![managed_domain(11, 202, "example.com")]])
                .into_connection(),
        );
        let service = DnsProviderService::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password("canonical-add-test")),
        );

        let result = service
            .add_managed_domain(
                101,
                AddManagedDomainRequest {
                    domain: "  *.*.Example.COM...  ".to_string(),
                    auto_manage: true,
                    generated_hostname_mode: None,
                    sync_generated_records: false,
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(DnsError::ManagedDomainAlreadyExists {
                requested_domain,
                canonical_domain,
                existing_managed_domain_id: 11,
                existing_provider_id: 202,
            }) if requested_domain == "  *.*.Example.COM...  "
                && canonical_domain == "example.com"
        ));
        drop(service);
        let transaction_log = Arc::try_unwrap(db)
            .expect("test must release the database connection")
            .into_transaction_log();
        let statement = &transaction_log[1].statements()[0];
        assert!(statement.sql.contains(
            "LOWER(REGEXP_REPLACE(RTRIM(BTRIM(\"dns_managed_domains\".\"domain\"), '.'), '^((\\*\\.)+)', '')) = $1"
        ));
        assert!(statement.sql.contains("LIMIT $2"));
        assert_eq!(
            format!("{:?}", statement.values),
            "Some(Values([String(Some(\"example.com\")), BigUnsigned(Some(1))]))"
        );
    }

    #[test]
    fn masked_credentials_for_inactive_provider_fail_before_decryption() {
        let service = DnsProviderService::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            Arc::new(EncryptionService::new_from_password("inactive-mask-test")),
        );
        let mut provider = dns_provider(101, "inactive", false);
        provider.credentials = "not-valid-ciphertext".to_string();

        let result = service.get_masked_credentials(&provider);

        assert!(matches!(
            result,
            Err(DnsError::ProviderInactive {
                provider_id: 101,
                provider_name,
            }) if provider_name == "inactive"
        ));
    }
}
