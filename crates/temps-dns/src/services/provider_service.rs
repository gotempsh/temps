// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS Provider service for managing provider configurations
//!
//! This service handles:
//! - Creating and managing DNS provider configurations
//! - Storing encrypted credentials
//! - Creating provider instances from stored configurations
//! - Testing provider connections

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, SqlErr,
    TransactionTrait,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{
    dns_managed_domains, dns_managed_record_states, dns_providers, dns_reconciliation_runs,
    domain_delivery_bindings,
};
use tracing::{debug, error, info};

use crate::errors::DnsError;
use crate::providers::{
    AzureProvider, BunnyProvider, CloudflareProvider, DigitalOceanProvider, DnsProvider,
    DnsProviderType, GcpProvider, ManualDnsProvider, NamecheapProvider, PebbleDnsProvider,
    ProviderCredentials, Route53Provider,
};
use crate::services::hostname_sync::{self, ConflictDecisions, HostnameModeResult};
use temps_core::{AppSettings, PublicHostnameStrategy};

/// Rows per `INSERT` when replacing generated-hostname record states.
pub(crate) const GENERATED_RECORD_STATE_INSERT_BATCH: usize = 500;

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
    pub proxied_by_default: bool,
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
    pub proxied_by_default: Option<bool>,
}

impl DnsProviderService {
    // A valid DNS name has at most 127 labels. Keeping the candidate set capped
    // also bounds malformed input before it reaches an IN predicate.
    const MAX_AUTHORITATIVE_SUFFIX_CANDIDATES: usize = 127;
    const NORMALIZED_MANAGED_DOMAIN_SQL: &'static str =
        "LOWER(REGEXP_REPLACE(RTRIM(BTRIM(\"dns_managed_domains\".\"domain\"), '.'), '^((\\*\\.)+)', ''))";
    /// A delivery binding's zone in the form [`Self::normalize_domain`]
    /// produces (trimmed, lowercase, no root dot), so a binding matches its
    /// managed zone however either was spelled.
    const NORMALIZED_BINDING_ZONE_SQL: &'static str =
        "LOWER(RTRIM(BTRIM(\"domain_delivery_bindings\".\"zone\"), '.'))";

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

    pub fn parse_requested_hostname_mode(value: &str) -> Result<PublicHostnameStrategy, DnsError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "standard" => Ok(PublicHostnameStrategy::Standard),
            "flat" => Ok(PublicHostnameStrategy::Flat),
            _ => Err(DnsError::Validation(format!(
                "Invalid generated hostname mode '{value}'; expected 'standard' or 'flat'"
            ))),
        }
    }

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
            DnsProviderType::Bunny => match credentials {
                ProviderCredentials::Bunny(creds) => Box::new(BunnyProvider::new(creds.clone())?),
                _ => {
                    return Err(DnsError::InvalidCredentials(
                        "Expected Bunny DNS credentials".into(),
                    ))
                }
            },
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

    /// Update a provider.
    ///
    /// Runs on one transaction that first locks the provider row (see
    /// [`Self::lock_provider`]), so the deactivation guard's binding count
    /// and the write that follows it are atomic with respect to binding
    /// creation.
    pub async fn update(
        &self,
        id: i32,
        request: UpdateProviderRequest,
    ) -> Result<dns_providers::Model, DnsError> {
        let transaction = self.db.begin().await?;
        let provider = Self::lock_provider(&transaction, id).await?;
        if request.is_active == Some(false) && provider.is_active {
            Self::ensure_no_bindings_before_deactivation(&transaction, &provider).await?;
        }

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

        let result = active_model.update(&transaction).await?;
        transaction.commit().await?;

        debug!("Updated DNS provider with id: {}", id);

        Ok(result)
    }

    /// Delete a provider.
    ///
    /// The binding check and the delete run on one transaction holding the
    /// provider row lock (see [`Self::lock_provider`]), so no binding can be
    /// created between them.
    pub async fn delete(&self, id: i32) -> Result<(), DnsError> {
        let transaction = self.db.begin().await?;
        let provider = Self::lock_provider(&transaction, id).await?;

        // Domain delivery bindings own DNS records written through this
        // provider and reference it with ON DELETE RESTRICT. Refuse with an
        // actionable conflict instead of surfacing the raw foreign-key error.
        let binding_count =
            Self::count_provider_delivery_bindings(&transaction, provider.id).await?;
        if binding_count > 0 {
            return Err(Self::provider_in_use(&provider, binding_count));
        }

        dns_providers::Entity::delete_by_id(provider.id)
            .exec(&transaction)
            .await
            .map_err(|error| match error.sql_err() {
                // The foreign key remains the backstop for a binding written
                // by a path that does not take the provider row lock; report
                // it the same way.
                Some(SqlErr::ForeignKeyConstraintViolation(_)) => {
                    Self::provider_in_use(&provider, 1)
                }
                _ => DnsError::Database(error),
            })?;
        transaction.commit().await?;

        info!("Deleted DNS provider with id: {}", id);

        Ok(())
    }

    fn provider_in_use(provider: &dns_providers::Model, binding_count: u64) -> DnsError {
        DnsError::ResourceInUse {
            resource: "DNS provider",
            id: provider.id,
            name: provider.name.clone(),
            reason: format!(
                "{binding_count} domain delivery binding(s) still use it; remove those bindings from their projects' traffic delivery settings before deleting the provider"
            ),
        }
    }

    /// Lock the provider row for the rest of `transaction`
    /// (`SELECT … FOR UPDATE`).
    ///
    /// Binding creation holds `FOR SHARE` on this row while it reserves a
    /// binding, so a guard that takes this lock before counting bindings
    /// cannot interleave with one being created: the count, and the write
    /// it allows, see every binding committed before the lock was granted,
    /// and a binding started later waits for this transaction. Only this row
    /// is locked, which keeps the global lock order (custom domain →
    /// provider → managed zone).
    async fn lock_provider(
        transaction: &DatabaseTransaction,
        id: i32,
    ) -> Result<dns_providers::Model, DnsError> {
        dns_providers::Entity::find_by_id(id)
            .lock_exclusive()
            .one(transaction)
            .await?
            .ok_or(DnsError::ProviderNotFound(id))
    }

    /// Number of domain delivery bindings whose DNS records were written
    /// through `provider_id`: one COUNT on the indexed foreign key.
    async fn count_provider_delivery_bindings<C: ConnectionTrait>(
        connection: &C,
        provider_id: i32,
    ) -> Result<u64, DnsError> {
        Ok(domain_delivery_bindings::Entity::find()
            .filter(domain_delivery_bindings::Column::DnsProviderId.eq(provider_id))
            .count(connection)
            .await?)
    }

    /// Refuse to deactivate a provider that domain delivery bindings still
    /// use. An inactive provider cannot be instantiated, so the DNS records
    /// those bindings own could no longer be updated or cleaned up, and the
    /// bindings would keep their domain, project, environment, delivery
    /// profile and this provider undeletable. Activation is never refused.
    ///
    /// Must run on the transaction holding [`Self::lock_provider`].
    async fn ensure_no_bindings_before_deactivation(
        transaction: &DatabaseTransaction,
        provider: &dns_providers::Model,
    ) -> Result<(), DnsError> {
        let binding_count =
            Self::count_provider_delivery_bindings(transaction, provider.id).await?;
        if binding_count == 0 {
            return Ok(());
        }
        Err(DnsError::ResourceInUse {
            resource: "DNS provider",
            id: provider.id,
            name: provider.name.clone(),
            reason: format!(
                "{binding_count} domain delivery binding(s) still use it, so it cannot be deactivated without leaving the DNS records they own impossible to update or clean up; remove CDN/DNS delivery for those hostnames first"
            ),
        })
    }

    /// Set provider active status. Deactivation is refused while domain
    /// delivery bindings use the provider, with the same row lock as
    /// [`Self::update`].
    pub async fn set_active(
        &self,
        id: i32,
        is_active: bool,
    ) -> Result<dns_providers::Model, DnsError> {
        let transaction = self.db.begin().await?;
        let provider = Self::lock_provider(&transaction, id).await?;
        if !is_active && provider.is_active {
            Self::ensure_no_bindings_before_deactivation(&transaction, &provider).await?;
        }

        let mut active_model: dns_providers::ActiveModel = provider.into();
        active_model.is_active = Set(is_active);

        let result = active_model.update(&transaction).await?;
        transaction.commit().await?;

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
            DnsProviderType::Bunny => {
                match serde_json::from_str::<ProviderCredentials>(&credentials_json)? {
                    ProviderCredentials::Bunny(creds) => Ok(Box::new(BunnyProvider::new(creds)?)),
                    _ => Err(DnsError::InvalidCredentials(
                        "Expected Bunny DNS credentials".into(),
                    )),
                }
            }
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
        let provider = self.get(provider_id).await?;
        let normalized_domain = Self::normalize_domain(&request.domain);
        if normalized_domain.is_empty() {
            return Err(DnsError::Validation(
                "Managed domain cannot be empty".to_string(),
            ));
        }
        if request.proxied_by_default {
            let instance = self.create_provider_instance(&provider)?;
            if !instance.capabilities().proxy {
                return Err(DnsError::ProxyNotSupportedByProvider {
                    provider: provider.name,
                });
            }
        }

        // Check the same canonical form used by authoritative lookup. New rows
        // are stored canonically, so the existing raw unique constraint also
        // closes the race between this preflight query and the insert.
        let existing = dns_managed_domains::Entity::find()
            .filter(sea_orm::sea_query::Expr::cust_with_values(
                format!("{} = $1", Self::NORMALIZED_MANAGED_DOMAIN_SQL),
                [normalized_domain.clone()],
            ))
            .limit(1)
            .one(self.db.as_ref())
            .await?;

        if let Some(existing) = existing {
            return Err(DnsError::ManagedDomainAlreadyExists {
                requested_domain: request.domain,
                canonical_domain: normalized_domain,
                existing_managed_domain_id: existing.id,
                existing_provider_id: existing.provider_id,
            });
        }

        let mode = Self::parse_requested_hostname_mode(
            request
                .generated_hostname_mode
                .as_deref()
                .unwrap_or("standard"),
        )?
        .as_db_str()
        .to_string();

        let managed_domain = dns_managed_domains::ActiveModel {
            provider_id: Set(provider_id),
            domain: Set(normalized_domain.clone()),
            auto_manage: Set(request.auto_manage),
            proxied_by_default: Set(request.proxied_by_default),
            verified: Set(false),
            generated_hostname_mode: Set(mode),
            sync_generated_records: Set(request.sync_generated_records),
            ..Default::default()
        };

        let result = managed_domain.insert(self.db.as_ref()).await?;

        info!(
            "Added managed domain {} to provider {}",
            normalized_domain, provider_id
        );

        Ok(result)
    }

    /// Remove a managed domain.
    ///
    /// Refused while domain delivery bindings still use the zone: their
    /// cleanup resolves the zone through this row to delete the DNS records
    /// they own, so removing it under them would strand those records and
    /// keep the bindings' domain, project, environment, delivery profile and
    /// provider undeletable — the same reason provider deletion is refused.
    /// The check and the delete run on one transaction holding the zone row
    /// lock (see [`Self::lock_managed_domain`]).
    pub async fn remove_managed_domain(
        &self,
        provider_id: i32,
        domain: &str,
    ) -> Result<(), DnsError> {
        let normalized_domain = Self::normalize_domain(domain);
        let transaction = self.db.begin().await?;
        let managed =
            Self::lock_managed_domain(&transaction, provider_id, &normalized_domain).await?;
        Self::refuse_if_zone_has_delivery_bindings(&transaction, &managed, |binding_count| {
            format!(
                "{binding_count} domain delivery binding(s) on DNS provider {provider_id} still use it and need it to clean up the DNS records they own; remove CDN/DNS delivery for those hostnames first"
            )
        })
        .await?;

        let deleted = dns_managed_domains::Entity::delete_by_id(managed.id)
            .exec(&transaction)
            .await?;

        if deleted.rows_affected == 0 {
            return Err(DnsError::DomainNotFound(normalized_domain));
        }
        transaction.commit().await?;

        info!(
            "Removed managed domain {} from provider {}",
            domain, provider_id
        );

        Ok(())
    }

    /// Lock a managed zone row for the rest of `transaction`
    /// (`SELECT … FOR UPDATE`), matched exactly as the row is stored.
    ///
    /// Binding creation holds `FOR SHARE` on the zone row while it reserves
    /// a binding, so a guard that takes this lock before counting bindings
    /// cannot interleave with one being created (see
    /// [`Self::lock_provider`]). Only this row is locked, which keeps the
    /// global lock order (custom domain → provider → managed zone).
    async fn lock_managed_domain(
        transaction: &DatabaseTransaction,
        provider_id: i32,
        normalized_domain: &str,
    ) -> Result<dns_managed_domains::Model, DnsError> {
        dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(normalized_domain))
            .lock_exclusive()
            .one(transaction)
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(normalized_domain.to_string()))
    }

    /// Number of domain delivery bindings whose DNS records live in `zone`
    /// on `provider_id`: one COUNT, narrowed by the provider foreign-key
    /// index, with both zone spellings compared in normalized form.
    async fn count_zone_delivery_bindings<C: ConnectionTrait>(
        connection: &C,
        provider_id: i32,
        zone: &str,
    ) -> Result<u64, DnsError> {
        Ok(domain_delivery_bindings::Entity::find()
            .filter(domain_delivery_bindings::Column::DnsProviderId.eq(provider_id))
            .filter(sea_orm::sea_query::Expr::cust_with_values(
                format!("{} = $1", Self::NORMALIZED_BINDING_ZONE_SQL),
                [Self::normalize_domain(zone)],
            ))
            .count(connection)
            .await?)
    }

    /// Refuse a change to `managed` while domain delivery bindings still use
    /// its zone; `reason` turns the binding count into the explanation.
    ///
    /// Must run on the transaction holding [`Self::lock_managed_domain`].
    async fn refuse_if_zone_has_delivery_bindings(
        transaction: &DatabaseTransaction,
        managed: &dns_managed_domains::Model,
        reason: impl FnOnce(u64) -> String,
    ) -> Result<(), DnsError> {
        let binding_count =
            Self::count_zone_delivery_bindings(transaction, managed.provider_id, &managed.domain)
                .await?;
        if binding_count == 0 {
            return Ok(());
        }
        Err(DnsError::ResourceInUse {
            resource: "managed DNS zone",
            id: managed.id,
            name: managed.domain.clone(),
            reason: reason(binding_count),
        })
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
        let normalized_domain = Self::normalize_domain(domain);
        let provider = self.get(provider_id).await?;
        let instance = self.create_provider_instance(&provider)?;

        // Distinguish "token lacks zone access" (PermissionDenied) from "zone
        // absent" so the UI can flag an incorrectly scoped token.
        let access = instance.check_zone_access(&normalized_domain).await;
        let (zone_access_ok, zone_access_error) = match &access {
            Ok(()) => (Some(true), None),
            Err(DnsError::PermissionDenied(msg)) => (Some(false), Some(msg.clone())),
            Err(e) => (Some(false), Some(e.to_string())),
        };
        // Zone lookups on some providers resolve a subdomain to its parent
        // zone. A managed domain must be the zone apex itself, otherwise
        // record names would be computed relative to the wrong origin.
        let (provider_zone, apex_error) = if access.is_ok() {
            match instance.get_zone(&normalized_domain).await {
                Ok(Some(zone)) => {
                    let error =
                        Self::zone_apex_mismatch(provider_id, &normalized_domain, &zone.name);
                    (Some(zone), error)
                }
                Ok(None) => (
                    None,
                    Some(format!(
                        "DNS provider {provider_id} reported access to '{normalized_domain}' but returned no zone for it"
                    )),
                ),
                Err(e) => (
                    None,
                    Some(format!(
                        "DNS provider {provider_id} zone lookup for '{normalized_domain}' failed: {e}"
                    )),
                ),
            }
        } else {
            (None, None)
        };
        let can_manage = access.is_ok() && apex_error.is_none();

        // Update verification status
        let managed_domain = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(&normalized_domain))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(normalized_domain.clone()))?;

        let mut active_model: dns_managed_domains::ActiveModel = managed_domain.into();
        active_model.verified = Set(can_manage);
        active_model.verified_at = Set(Some(chrono::Utc::now()));
        active_model.zone_access_ok = Set(zone_access_ok);
        active_model.zone_access_error = Set(zone_access_error);

        if can_manage {
            active_model.verification_error = Set(None);
            if let Some(zone) = provider_zone {
                active_model.zone_id = Set(Some(zone.id));
            }
        } else {
            active_model.verification_error =
                Set(Some(apex_error.unwrap_or_else(|| {
                    "Provider cannot access this domain".to_string()
                })));
        }

        active_model.update(self.db.as_ref()).await?;

        info!(
            "Verified managed domain {} for provider {}: {}",
            domain, provider_id, can_manage
        );

        Ok(can_manage)
    }

    /// Returns why verification must fail when `managed_domain` is not the
    /// apex of the zone the provider resolved it to (case-insensitive,
    /// trailing dot ignored).
    fn zone_apex_mismatch(
        provider_id: i32,
        managed_domain: &str,
        provider_zone: &str,
    ) -> Option<String> {
        let managed = Self::normalize_domain(managed_domain);
        let zone = provider_zone
            .trim()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if managed == zone {
            None
        } else {
            Some(format!(
                "Managed domain '{managed}' is not a DNS zone apex at provider {provider_id}: the provider hosts it in zone '{zone}'. Manage '{zone}' instead"
            ))
        }
    }

    /// Update a managed domain's settings (hostname mode, sync opt-in,
    /// auto-manage). Persists the values only; switching the mode does NOT
    /// recompute existing hostnames — callers use [`apply_hostname_mode`] for
    /// that.
    ///
    /// Runs on one transaction holding the zone row lock (see
    /// [`Self::lock_managed_domain`]), so the auto-manage guard's binding
    /// count and the write that follows it are atomic with respect to
    /// binding creation.
    ///
    /// The hostname mode is part of the zone's generated-hostname state, so a
    /// request that changes it first takes the zone operation lock (see
    /// [`hostname_sync::ZoneOperationLock`]): it fails fast with
    /// [`DnsError::ZoneOperationInProgress`] while a hostname-mode apply runs
    /// on the zone, instead of landing between that apply's DNS writes and its
    /// own mode save.
    pub async fn update_managed_domain(
        &self,
        provider_id: i32,
        domain: &str,
        request: UpdateManagedDomainRequest,
    ) -> Result<dns_managed_domains::Model, DnsError> {
        let normalized_domain = Self::normalize_domain(domain);
        let mode = request
            .generated_hostname_mode
            .as_deref()
            .map(Self::parse_requested_hostname_mode)
            .transpose()?;
        let transaction = self.db.begin().await?;
        if mode.is_some() {
            hostname_sync::lock_zone_operation(&transaction, provider_id, &normalized_domain)
                .await?;
        }
        let managed =
            Self::lock_managed_domain(&transaction, provider_id, &normalized_domain).await?;

        // Domain delivery writes and cleans up records in its zone
        // automatically; taking the zone out of automatic management under
        // live bindings would leave them unable to do either.
        if request.auto_manage == Some(false) && managed.auto_manage {
            Self::refuse_if_zone_has_delivery_bindings(&transaction, &managed, |binding_count| {
                format!(
                    "{binding_count} domain delivery binding(s) on DNS provider {provider_id} still manage records in it, so automatic DNS management cannot be turned off; remove CDN/DNS delivery for those hostnames first"
                )
            })
            .await?;
        }

        if request.proxied_by_default == Some(true) {
            // A plain read on the same transaction: it takes no row lock, and
            // it does not hold the zone lock while waiting for a second
            // pooled connection.
            let provider = dns_providers::Entity::find_by_id(provider_id)
                .one(&transaction)
                .await?
                .ok_or(DnsError::ProviderNotFound(provider_id))?;
            let instance = self.create_provider_instance(&provider)?;
            if !instance.capabilities().proxy {
                return Err(DnsError::ProxyNotSupportedByProvider {
                    provider: provider.name,
                });
            }
        }

        let mut active: dns_managed_domains::ActiveModel = managed.into();
        if let Some(mode) = mode {
            active.generated_hostname_mode = Set(mode.as_db_str().to_string());
        }
        if let Some(sync) = request.sync_generated_records {
            active.sync_generated_records = Set(sync);
        }
        if let Some(auto) = request.auto_manage {
            active.auto_manage = Set(auto);
        }
        if let Some(proxied_by_default) = request.proxied_by_default {
            active.proxied_by_default = Set(proxied_by_default);
        }

        let updated = active.update(&transaction).await?;
        transaction.commit().await?;
        Ok(updated)
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
        self.hostname_mode_operation(
            provider_id,
            domain,
            target,
            want_sync,
            true,
            None,
            &ConflictDecisions::default(),
        )
        .await
    }

    /// Apply a hostname-mode change: persist the mode and (when `sync_dns`)
    /// reconcile the provider's DNS zone. Returns the changes that were applied.
    /// The caller is responsible for triggering a route reload so derived
    /// hostnames take effect.
    ///
    /// `decisions` resolve the conflicts a preview reported: each adopts the
    /// record at a conflicting hostname, or leaves the hostname untouched.
    /// The sync refuses to change anything while a conflict is unresolved, or
    /// when a decision no longer matches the zone.
    pub async fn apply_hostname_mode(
        &self,
        provider_id: i32,
        domain: &str,
        target: PublicHostnameStrategy,
        sync_dns: bool,
        decisions: &ConflictDecisions,
        actor_user_id: i32,
    ) -> Result<HostnameModeResult, DnsError> {
        if !sync_dns && !decisions.is_empty() {
            return Err(DnsError::Validation(format!(
                "Adopt and skip decisions resolve DNS record conflicts, but this hostname-mode apply for zone '{domain}' (DNS provider {provider_id}) does not sync DNS records; set sync_dns, or send no decisions"
            )));
        }
        self.hostname_mode_operation(
            provider_id,
            domain,
            target,
            sync_dns,
            false,
            Some(actor_user_id),
            decisions,
        )
        .await
    }

    /// Shared preview/apply implementation. With `dry_run` it computes the
    /// changes without writing; otherwise it persists the mode and executes the
    /// DNS reconciliation.
    ///
    /// An apply rewrites the zone's generated-hostname state — its DNS
    /// records, its record states (the proxy's origin-certificate allowlist)
    /// and its hostname mode — so it holds the zone's
    /// [`hostname_sync::ZoneOperationLock`] from planning to its last write. A
    /// concurrent apply, reconciliation or mode update on the zone fails fast
    /// with [`DnsError::ZoneOperationInProgress`] instead of interleaving. The
    /// record states and the mode are saved together, and an apply that stops
    /// part-way saves what it changed (see
    /// [`hostname_sync::apply_hostname_mode_plan`]). A preview writes nothing
    /// and takes no lock.
    #[allow(clippy::too_many_arguments)]
    async fn hostname_mode_operation(
        &self,
        provider_id: i32,
        domain: &str,
        target: PublicHostnameStrategy,
        sync_dns: bool,
        dry_run: bool,
        actor_user_id: Option<i32>,
        decisions: &ConflictDecisions,
    ) -> Result<HostnameModeResult, DnsError> {
        if dry_run {
            return self
                .hostname_mode_steps(
                    provider_id,
                    domain,
                    target,
                    sync_dns,
                    None,
                    actor_user_id,
                    decisions,
                )
                .await;
        }
        let zone_lock =
            hostname_sync::ZoneOperationLock::acquire(self.db.as_ref(), provider_id, domain)
                .await?;
        let outcome = self
            .hostname_mode_steps(
                provider_id,
                domain,
                target,
                sync_dns,
                Some(&zone_lock),
                actor_user_id,
                decisions,
            )
            .await;
        zone_lock.finish(outcome).await
    }

    /// The steps of [`Self::hostname_mode_operation`]. `zone_lock` is `None`
    /// for a preview, which writes nothing; an apply passes the zone lock it
    /// holds until its last write.
    #[allow(clippy::too_many_arguments)]
    async fn hostname_mode_steps(
        &self,
        provider_id: i32,
        domain: &str,
        target: PublicHostnameStrategy,
        sync_dns: bool,
        zone_lock: Option<&hostname_sync::ZoneOperationLock<'_>>,
        actor_user_id: Option<i32>,
        decisions: &ConflictDecisions,
    ) -> Result<HostnameModeResult, DnsError> {
        let dry_run = zone_lock.is_none();
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
            hostname_sync::compute_hostname_changes(self.db.as_ref(), &preview_domain, target)
                .await?
        } else {
            Vec::new()
        };

        if sync_dns && !applies {
            return Err(DnsError::Validation(format!(
                "Managed zone '{domain}' does not govern the configured preview domain '{preview_domain}'"
            )));
        }

        let mut result = HostnameModeResult {
            hostname_changes,
            dns_changes: Vec::new(),
            conflicts: Vec::new(),
            zone_access_ok: None,
        };
        // Whether the DNS sync saved the mode together with its record states.
        let mut mode_saved = false;

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
                let instance_id =
                    crate::services::ManagedDnsRecordService::load_instance_id(self.db.as_ref())
                        .await?;
                let signing_key = self
                    .encryption_service
                    .derive_subkey("temps:dns-ownership:v1");
                let desired = hostname_sync::enumerate_generated_hosts(
                    self.db.as_ref(),
                    &preview_domain,
                    target,
                )
                .await?;
                // Plan from a single zone listing; an apply run executes this
                // exact plan (the one persisted on the run row) instead of
                // listing the zone a second time.
                let plan = hostname_sync::plan_zone_records(
                    instance.as_ref(),
                    domain,
                    &desired,
                    edge_target,
                    hostname_sync::PlanOptions {
                        proxied: managed.proxied_by_default,
                        instance_id: &instance_id,
                        signing_key: &signing_key,
                        decisions,
                    },
                )
                .await?;
                if let Some(zone_lock) = zone_lock {
                    // The run row is written on its own, so it records the
                    // outcome whatever the apply managed to save.
                    let run = dns_reconciliation_runs::ActiveModel {
                        provider_id: Set(provider_id),
                        zone: Set(domain.to_ascii_lowercase()),
                        actor_user_id: Set(actor_user_id.ok_or_else(|| {
                            DnsError::Validation(
                                "DNS reconciliation requires an authenticated actor".to_string(),
                            )
                        })?),
                        controller: Set("generated-hostname".to_string()),
                        status: Set("pending".to_string()),
                        planned_changes: Set(
                            serde_json::to_value(&plan.changes).map_err(DnsError::Serialization)?
                        ),
                        error: Set(None),
                        ..Default::default()
                    }
                    .insert(self.db.as_ref())
                    .await?;

                    let applied = hostname_sync::apply_hostname_mode_plan(
                        self.db.as_ref(),
                        instance.as_ref(),
                        zone_lock,
                        plan,
                        hostname_sync::HostnameModeSwitch {
                            managed: &managed,
                            target,
                            desired: &desired,
                            edge_target,
                        },
                        &instance_id,
                        &signing_key,
                    )
                    .await;
                    match applied {
                        Ok(changes) => {
                            result.dns_changes = changes;
                            mode_saved = true;
                            self.finish_reconciliation_run(run, "applied", None).await;
                        }
                        Err(error) => {
                            self.finish_reconciliation_run(run, "failed", Some(error.to_string()))
                                .await;
                            return Err(error);
                        }
                    }
                } else {
                    result.dns_changes = plan.changes;
                    result.conflicts = plan.conflicts;
                }
            } else if !decisions.is_empty() {
                return Err(DnsError::Validation(format!(
                    "No edge target is configured, so the hostname-mode sync for zone '{domain}' (DNS provider {provider_id}) writes no DNS records and the adopt and skip decisions resolve nothing; configure the edge target, or send no decisions"
                )));
            }
        }

        // Without a DNS sync, the mode is all there is to save.
        if zone_lock.is_some() && !mode_saved {
            let mut active: dns_managed_domains::ActiveModel = managed.into();
            active.generated_hostname_mode = Set(target.as_db_str().to_string());
            if sync_dns {
                active.sync_generated_records = Set(true);
            }
            active.update(self.db.as_ref()).await?;
        }

        Ok(result)
    }

    /// Record how a reconciliation run ended. A failure to record it is
    /// logged and never replaces the outcome the caller reports: the DNS
    /// changes and saved state are the same either way.
    async fn finish_reconciliation_run(
        &self,
        run: dns_reconciliation_runs::Model,
        status: &str,
        error: Option<String>,
    ) {
        let run_id = run.id;
        let mut active: dns_reconciliation_runs::ActiveModel = run.into();
        active.status = Set(status.to_string());
        active.error = Set(error);
        if let Err(update_error) = active.update(self.db.as_ref()).await {
            tracing::error!(
                "Failed to record status '{}' on DNS reconciliation run {}: {}",
                status,
                run_id,
                update_error
            );
        }
    }

    /// Replace the zone's generated-hostname record states on `transaction`,
    /// which the caller commits together with the hostname mode that the new
    /// states describe.
    pub(crate) async fn replace_generated_record_states(
        transaction: &DatabaseTransaction,
        provider_id: i32,
        zone: &str,
        desired: &[hostname_sync::GeneratedHost],
        edge_target: &str,
        proxied: bool,
    ) -> Result<(), DnsError> {
        // Rows are stored with the normalized zone, so the delete must use the
        // same form or a mixed-case/trailing-dot caller would leave stale rows.
        let zone = Self::normalize_domain(zone);
        let suffix = format!(".{zone}");
        let (_, _, record_type) = hostname_sync::desired_content(edge_target);
        let rows = desired
            .iter()
            .map(|host| {
                Ok(dns_managed_record_states::ActiveModel {
                    provider_id: Set(provider_id),
                    zone: Set(zone.clone()),
                    name: Set(hostname_sync::relative_name(&host.fqdn, &suffix)?),
                    fqdn: Set(host.fqdn.to_ascii_lowercase()),
                    record_type: Set(record_type.clone()),
                    controller: Set("generated-hostname".to_string()),
                    proxied: Set(proxied),
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>, DnsError>>()?;

        dns_managed_record_states::Entity::delete_many()
            .filter(dns_managed_record_states::Column::ProviderId.eq(provider_id))
            .filter(dns_managed_record_states::Column::Zone.eq(&zone))
            .filter(dns_managed_record_states::Column::Controller.eq("generated-hostname"))
            .exec(transaction)
            .await?;
        // Batched so a large zone stays well under PostgreSQL's bind-parameter
        // limit while avoiding one round trip per hostname.
        for chunk in rows.chunks(GENERATED_RECORD_STATE_INSERT_BATCH) {
            dns_managed_record_states::Entity::insert_many(chunk.to_vec())
                .exec_without_returning(transaction)
                .await?;
        }
        Ok(())
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

    // Delivery cleanup
    /// Resolve the provider and managed zone a domain delivery binding wrote
    /// its record through, by the binding's own `provider_id` and `zone`.
    ///
    /// Unlike [`Self::find_provider_for_domain`] this does not require the
    /// zone to still be verified or auto-managed: removing a record Temps
    /// already wrote must not be stranded by those flags changing later. The
    /// provider row must still exist; the returned zone is canonical
    /// (trimmed, lowercase, no trailing dot).
    pub async fn find_managed_zone_for_delivery_cleanup(
        &self,
        provider_id: i32,
        zone: &str,
    ) -> Result<(dns_providers::Model, dns_managed_domains::Model), DnsError> {
        let provider = dns_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(DnsError::ProviderNotFound(provider_id))?;
        let canonical_zone = Self::normalize_domain(zone);
        let mut managed = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider_id))
            .filter(sea_orm::sea_query::Expr::cust_with_values(
                format!("{} = $1", Self::NORMALIZED_MANAGED_DOMAIN_SQL),
                [canonical_zone.clone()],
            ))
            .order_by_asc(dns_managed_domains::Column::Id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DnsError::DomainNotManaged(format!(
                    "zone '{canonical_zone}' is not a managed domain of DNS provider {provider_id} ({}), so its delivery record cannot be cleaned up through it",
                    provider.name
                ))
            })?;
        managed.domain = canonical_zone;
        Ok((provider, managed))
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

    /// The canonical form managed zones and their record states are stored
    /// and locked under.
    pub(crate) fn normalize_domain(domain: &str) -> String {
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

    #[test]
    fn normalize_domain_is_case_and_trailing_dot_insensitive() {
        assert_eq!(
            DnsProviderService::normalize_domain(" Example.CO.UK. "),
            "example.co.uk"
        );
    }
}

#[cfg(test)]
mod upstream_tests {
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
            proxied_by_default: false,
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
    fn bunny_factory_decrypts_credentials_and_fully_masks_account_key() {
        let encryption = Arc::new(EncryptionService::new_from_password("bunny-factory-test"));
        let service = DnsProviderService::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            encryption.clone(),
        );
        let mut model = dns_provider(42, "bunny-test", true);
        model.provider_type = "bunny".into();
        let credentials = ProviderCredentials::Bunny(crate::providers::BunnyCredentials {
            api_key: "test-account-key".into(),
        });
        model.credentials = encryption
            .encrypt_string(&serde_json::to_string(&credentials).unwrap())
            .unwrap();
        assert!(!model.credentials.contains("test-account-key"));
        let provider = service.create_provider_instance(&model).unwrap();
        assert_eq!(provider.provider_type(), DnsProviderType::Bunny);
        assert_eq!(
            service.get_masked_credentials(&model).unwrap()["api_key"],
            "***"
        );
        model.credentials = encryption
            .encrypt_string(
                &serde_json::to_string(&ProviderCredentials::Cloudflare(
                    crate::providers::CloudflareCredentials {
                        api_token: "test-token".into(),
                        account_id: None,
                    },
                ))
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            service.create_provider_instance(&model),
            Err(DnsError::InvalidCredentials(_))
        ));
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
                    proxied_by_default: false,
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
    fn zone_apex_mismatch_accepts_apex_and_rejects_subdomain_of_provider_zone() {
        assert_eq!(
            DnsProviderService::zone_apex_mismatch(7, "Example.COM.", "example.com."),
            None
        );
        let error = DnsProviderService::zone_apex_mismatch(7, "app.example.com", "Example.com")
            .expect("subdomain of the provider zone must fail verification");
        assert!(error.contains("'app.example.com'"), "{error}");
        assert!(error.contains("provider 7"), "{error}");
        assert!(error.contains("'example.com'"), "{error}");
    }

    /// Adopt and skip decisions only resolve the conflicts of a DNS sync. An
    /// apply that does not sync is refused before it reads or writes
    /// anything, instead of silently ignoring what the user confirmed.
    #[tokio::test]
    async fn conflict_decisions_without_a_dns_sync_are_refused_before_anything_runs() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = DnsProviderService::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password("decisions-test")),
        );
        let decisions = ConflictDecisions {
            adopt: Vec::new(),
            skip: vec![hostname_sync::SkipRecordDecision {
                name: "pr-1.example.com".into(),
                record_type: "A".into(),
                revision: "revision-1".into(),
            }],
        };

        let error = service
            .apply_hostname_mode(
                7,
                "example.com",
                PublicHostnameStrategy::Flat,
                false,
                &decisions,
                1,
            )
            .await
            .expect_err("decisions need a DNS sync");

        match &error {
            DnsError::Validation(message) => {
                assert!(
                    message.contains("zone 'example.com' (DNS provider 7)"),
                    "{message}"
                );
                assert!(message.contains("set sync_dns"), "{message}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
        drop(service);
        assert!(logged_transactions(db).is_empty());
    }

    #[tokio::test]
    async fn delete_provider_with_delivery_bindings_is_a_conflict_naming_provider_and_count() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![dns_provider(101, "edge-dns", true)]])
                .append_query_results(vec![vec![std::collections::BTreeMap::from([(
                    "num_items".to_string(),
                    sea_orm::Value::BigInt(Some(3)),
                )])]])
                .into_connection(),
        );
        let service = DnsProviderService::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password("provider-in-use-test")),
        );

        let result = service.delete(101).await;

        match result {
            Err(error @ DnsError::ResourceInUse { id: 101, .. }) => {
                let message = error.to_string();
                assert!(message.contains("DNS provider 101"), "{message}");
                assert!(!message.contains("import the record"), "{message}");
                assert!(
                    message.contains("3 domain delivery binding(s)"),
                    "{message}"
                );
            }
            other => panic!("expected a provider-in-use conflict, got {other:?}"),
        }
        drop(service);
        let transactions = logged_transactions(db);
        // Provider row locked, bindings counted, rolled back: no DELETE.
        assert_eq!(
            shapes(&transactions),
            vec![vec!["BEGIN", "SELECT FOR UPDATE", "COUNT", "ROLLBACK"]]
        );
        assert!(
            transactions[0][1].sql.contains(r#"FROM "dns_providers""#),
            "{}",
            transactions[0][1].sql
        );
    }

    #[tokio::test]
    async fn delete_provider_without_bindings_deletes_it() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![dns_provider(101, "edge-dns", true)]])
                .append_query_results(vec![vec![std::collections::BTreeMap::from([(
                    "num_items".to_string(),
                    sea_orm::Value::BigInt(Some(0)),
                )])]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = DnsProviderService::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password("provider-delete-test")),
        );

        assert!(service.delete(101).await.is_ok());
        drop(service);
        assert_eq!(
            shapes(&logged_transactions(db)),
            vec![vec![
                "BEGIN",
                "SELECT FOR UPDATE",
                "COUNT",
                "DELETE",
                "COMMIT"
            ]]
        );
    }

    #[tokio::test]
    async fn generated_record_states_use_normalized_zone_and_one_batched_insert() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(vec![
                    sea_orm::MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 4,
                    },
                    sea_orm::MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 2,
                    },
                ])
                .into_connection(),
        );
        let desired = vec![
            hostname_sync::GeneratedHost {
                kind: "environment",
                owner_id: 1,
                fqdn: "App.Example.com".into(),
            },
            hostname_sync::GeneratedHost {
                kind: "environment",
                owner_id: 2,
                fqdn: "api.example.com".into(),
            },
        ];

        // The caller owns the transaction, as the zone operation lock does.
        let transaction = db.begin().await.expect("begin");
        DnsProviderService::replace_generated_record_states(
            &transaction,
            42,
            "Example.COM.",
            &desired,
            "192.0.2.10",
            false,
        )
        .await
        .expect("record states replaced");
        transaction.commit().await.expect("commit");

        let log = Arc::try_unwrap(db)
            .expect("test must release the database connection")
            .into_transaction_log();
        let statements: Vec<_> = log.iter().flat_map(|txn| txn.statements()).collect();
        let delete = statements
            .iter()
            .find(|statement| statement.sql.starts_with("DELETE"))
            .expect("delete statement");
        assert!(format!("{:?}", delete.values).contains("\"example.com\""));
        let inserts: Vec<_> = statements
            .iter()
            .filter(|statement| statement.sql.starts_with("INSERT"))
            .collect();
        assert_eq!(inserts.len(), 1, "all rows go in one INSERT");
        let values = format!("{:?}", inserts[0].values);
        assert!(values.contains("\"app\""), "{values}");
        assert!(values.contains("\"api\""), "{values}");
        assert!(!values.contains("Example.COM"), "{values}");
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

    // ==================== delivery bindings guard zones and providers ====================

    fn binding_count_row(count: i64) -> std::collections::BTreeMap<String, sea_orm::Value> {
        std::collections::BTreeMap::from([(
            "num_items".to_string(),
            sea_orm::Value::BigInt(Some(count)),
        )])
    }

    fn service_with(db: Arc<sea_orm::DatabaseConnection>, label: &str) -> DnsProviderService {
        DnsProviderService::new(db, Arc::new(EncryptionService::new_from_password(label)))
    }

    /// Every logged transaction with the statements it ran, in order.
    fn logged_transactions(db: Arc<sea_orm::DatabaseConnection>) -> Vec<Vec<sea_orm::Statement>> {
        Arc::try_unwrap(db)
            .expect("test must release the database connection")
            .into_transaction_log()
            .iter()
            .map(|transaction| transaction.statements().to_vec())
            .collect()
    }

    /// The kind of every statement, per transaction, so a test can assert
    /// that a guard locks the row, counts bindings and writes — in that
    /// order and on a single transaction.
    fn shapes(transactions: &[Vec<sea_orm::Statement>]) -> Vec<Vec<&'static str>> {
        transactions
            .iter()
            .map(|statements| statements.iter().map(statement_kind).collect())
            .collect()
    }

    fn statement_kind(statement: &sea_orm::Statement) -> &'static str {
        let sql = statement.sql.as_str();
        match sql {
            "BEGIN" => "BEGIN",
            "COMMIT" => "COMMIT",
            "ROLLBACK" => "ROLLBACK",
            _ if sql.starts_with("SELECT COUNT(*)") => "COUNT",
            _ if sql.starts_with("SELECT") && sql.ends_with("FOR UPDATE") => "SELECT FOR UPDATE",
            _ if sql.starts_with("SELECT") => "SELECT",
            _ if sql.starts_with("UPDATE") => "UPDATE",
            _ if sql.starts_with("DELETE") => "DELETE",
            _ => "OTHER",
        }
    }

    /// A refused guard: row locked, bindings counted, rolled back unwritten.
    const REFUSED: [&str; 4] = ["BEGIN", "SELECT FOR UPDATE", "COUNT", "ROLLBACK"];

    /// The fields of a `ResourceInUse` refusal; panics on anything else.
    fn in_use<T: std::fmt::Debug>(
        result: Result<T, DnsError>,
    ) -> (&'static str, i32, String, String) {
        match result {
            Err(DnsError::ResourceInUse {
                resource,
                id,
                name,
                reason,
            }) => (resource, id, name, reason),
            other => panic!("expected a ResourceInUse refusal, got {other:?}"),
        }
    }

    /// Refusal reasons are read after "… is in use: ", so they lead with
    /// the binding count and end with what to do about it.
    fn assert_reason(reason: &str, leading_clause: &str) {
        assert!(reason.starts_with(leading_clause), "{reason}");
        assert!(
            reason.ends_with("remove CDN/DNS delivery for those hostnames first"),
            "{reason}"
        );
    }

    fn deactivate_request() -> UpdateProviderRequest {
        UpdateProviderRequest {
            name: None,
            credentials: None,
            description: None,
            is_active: Some(false),
        }
    }

    #[tokio::test]
    async fn remove_managed_domain_with_delivery_bindings_is_refused_without_deleting() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![managed_domain(11, 101, "example.com")]])
                .append_query_results(vec![vec![binding_count_row(2)]])
                .into_connection(),
        );
        let service = service_with(db.clone(), "zone-in-use-test");

        let result = service.remove_managed_domain(101, " Example.COM. ").await;

        let (resource, id, name, reason) = in_use(result);
        assert_eq!(
            (resource, id, name.as_str()),
            ("managed DNS zone", 11, "example.com")
        );
        assert_reason(
            &reason,
            "2 domain delivery binding(s) on DNS provider 101 still use it",
        );
        drop(service);
        let transactions = logged_transactions(db);
        // The zone row is locked before the single binding COUNT, and the
        // transaction rolls back without a DELETE.
        assert_eq!(shapes(&transactions), vec![REFUSED.to_vec()]);
        let lock = &transactions[0][1];
        assert!(
            lock.sql.contains(r#"FROM "dns_managed_domains""#),
            "{}",
            lock.sql
        );
        let count = &transactions[0][2];
        assert!(
            count
                .sql
                .contains(r#""domain_delivery_bindings"."dns_provider_id" = $1"#),
            "{}",
            count.sql
        );
        assert!(
            count
                .sql
                .contains(r#"LOWER(RTRIM(BTRIM("domain_delivery_bindings"."zone"), '.')) = $2"#),
            "{}",
            count.sql
        );
        assert_eq!(
            format!("{:?}", count.values),
            "Some(Values([Int(Some(101)), String(Some(\"example.com\"))]))"
        );
    }

    #[tokio::test]
    async fn remove_managed_domain_without_delivery_bindings_deletes_exactly_that_row() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![managed_domain(11, 101, "example.com")]])
                .append_query_results(vec![vec![binding_count_row(0)]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = service_with(db.clone(), "zone-remove-test");

        service
            .remove_managed_domain(101, "example.com")
            .await
            .expect("a zone without delivery bindings is removed");

        drop(service);
        let transactions = logged_transactions(db);
        assert_eq!(
            shapes(&transactions),
            vec![vec![
                "BEGIN",
                "SELECT FOR UPDATE",
                "COUNT",
                "DELETE",
                "COMMIT"
            ]]
        );
        let delete = &transactions[0][3];
        assert!(
            delete
                .sql
                .starts_with(r#"DELETE FROM "dns_managed_domains""#),
            "{}",
            delete.sql
        );
        assert_eq!(
            format!("{:?}", delete.values),
            "Some(Values([Int(Some(11))]))"
        );
    }

    #[tokio::test]
    async fn remove_unknown_managed_domain_is_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<dns_managed_domains::Model>::new()])
                .into_connection(),
        );
        let service = service_with(db.clone(), "zone-missing-test");

        let result = service.remove_managed_domain(101, "Example.COM").await;

        assert!(
            matches!(&result, Err(DnsError::DomainNotFound(domain)) if domain == "example.com"),
            "{result:?}"
        );
        drop(service);
        assert_eq!(
            shapes(&logged_transactions(db)),
            vec![vec!["BEGIN", "SELECT FOR UPDATE", "ROLLBACK"]]
        );
    }

    #[tokio::test]
    async fn turning_off_auto_manage_with_delivery_bindings_is_refused_without_updating() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![managed_domain(11, 101, "example.com")]])
                .append_query_results(vec![vec![binding_count_row(1)]])
                .into_connection(),
        );
        let service = service_with(db.clone(), "auto-manage-in-use-test");

        let result = service
            .update_managed_domain(
                101,
                "example.com",
                UpdateManagedDomainRequest {
                    auto_manage: Some(false),
                    ..Default::default()
                },
            )
            .await;

        let (resource, id, name, reason) = in_use(result);
        assert_eq!(
            (resource, id, name.as_str()),
            ("managed DNS zone", 11, "example.com")
        );
        assert_reason(
            &reason,
            "1 domain delivery binding(s) on DNS provider 101 still manage records in it",
        );
        assert!(
            reason.contains("automatic DNS management cannot be turned off"),
            "{reason}"
        );
        drop(service);
        assert_eq!(shapes(&logged_transactions(db)), vec![REFUSED.to_vec()]);
    }

    #[tokio::test]
    async fn turning_off_auto_manage_without_delivery_bindings_is_saved() {
        let mut updated = managed_domain(11, 101, "example.com");
        updated.auto_manage = false;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![managed_domain(11, 101, "example.com")]])
                .append_query_results(vec![vec![binding_count_row(0)]])
                .append_query_results(vec![vec![updated]])
                .into_connection(),
        );
        let service = service_with(db.clone(), "auto-manage-off-test");

        let managed = service
            .update_managed_domain(
                101,
                "example.com",
                UpdateManagedDomainRequest {
                    auto_manage: Some(false),
                    ..Default::default()
                },
            )
            .await
            .expect("an unbound zone can leave automatic management");

        assert!(!managed.auto_manage);
        drop(service);
        assert_eq!(
            shapes(&logged_transactions(db)),
            vec![vec![
                "BEGIN",
                "SELECT FOR UPDATE",
                "COUNT",
                "UPDATE",
                "COMMIT"
            ]]
        );
    }

    #[tokio::test]
    async fn managed_domain_updates_that_keep_auto_manage_skip_the_binding_count() {
        let mut updated = managed_domain(11, 101, "example.com");
        updated.sync_generated_records = true;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![managed_domain(11, 101, "example.com")]])
                .append_query_results(vec![vec![updated]])
                .into_connection(),
        );
        let service = service_with(db.clone(), "auto-manage-kept-test");

        service
            .update_managed_domain(
                101,
                "example.com",
                UpdateManagedDomainRequest {
                    auto_manage: Some(true),
                    sync_generated_records: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("settings update");

        drop(service);
        assert_eq!(
            shapes(&logged_transactions(db)),
            vec![vec!["BEGIN", "SELECT FOR UPDATE", "UPDATE", "COMMIT"]]
        );
    }

    #[tokio::test]
    async fn enabling_proxied_by_default_reads_the_provider_on_the_zone_transaction() {
        let encryption = Arc::new(EncryptionService::new_from_password("proxied-default-test"));
        let mut provider = dns_provider(101, "edge-dns", true);
        provider.credentials = encryption
            .encrypt_string(
                &serde_json::to_string(&ProviderCredentials::Cloudflare(
                    crate::providers::CloudflareCredentials {
                        api_token: "test-token".into(),
                        account_id: None,
                    },
                ))
                .unwrap(),
            )
            .unwrap();
        let mut updated = managed_domain(11, 101, "example.com");
        updated.proxied_by_default = true;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![managed_domain(11, 101, "example.com")]])
                .append_query_results(vec![vec![provider]])
                .append_query_results(vec![vec![updated]])
                .into_connection(),
        );
        let service = DnsProviderService::new(db.clone(), encryption);

        let managed = service
            .update_managed_domain(
                101,
                "example.com",
                UpdateManagedDomainRequest {
                    proxied_by_default: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("a proxy-capable provider can proxy by default");

        assert!(managed.proxied_by_default);
        drop(service);
        let transactions = logged_transactions(db);
        // The provider is read without a lock, inside the zone transaction:
        // no second pooled connection is held while the zone row is locked.
        assert_eq!(
            shapes(&transactions),
            vec![vec![
                "BEGIN",
                "SELECT FOR UPDATE",
                "SELECT",
                "UPDATE",
                "COMMIT"
            ]]
        );
        assert!(
            transactions[0][2].sql.contains(r#"FROM "dns_providers""#),
            "{}",
            transactions[0][2].sql
        );
    }

    #[tokio::test]
    async fn deactivating_a_provider_with_delivery_bindings_is_refused_on_every_path() {
        for path in ["set_active", "update"] {
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results(vec![vec![dns_provider(101, "edge-dns", true)]])
                    .append_query_results(vec![vec![binding_count_row(3)]])
                    .into_connection(),
            );
            let service = service_with(db.clone(), "provider-deactivate-in-use-test");

            let result = match path {
                "set_active" => service.set_active(101, false).await,
                _ => service.update(101, deactivate_request()).await,
            };

            let (resource, id, name, reason) = in_use(result);
            assert_eq!(
                (resource, id, name.as_str()),
                ("DNS provider", 101, "edge-dns"),
                "{path}"
            );
            assert_reason(&reason, "3 domain delivery binding(s) still use it");
            assert!(reason.contains("cannot be deactivated"), "{path}: {reason}");
            drop(service);
            let transactions = logged_transactions(db);
            // The provider stays active: locked, counted, rolled back.
            assert_eq!(shapes(&transactions), vec![REFUSED.to_vec()], "{path}");
            assert!(
                transactions[0][1].sql.contains(r#"FROM "dns_providers""#),
                "{path}: {}",
                transactions[0][1].sql
            );
        }
    }

    #[tokio::test]
    async fn deactivating_a_provider_without_delivery_bindings_is_saved() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![dns_provider(101, "edge-dns", true)]])
                .append_query_results(vec![vec![binding_count_row(0)]])
                .append_query_results(vec![vec![dns_provider(101, "edge-dns", false)]])
                .into_connection(),
        );
        let service = service_with(db.clone(), "provider-deactivate-test");

        let provider = service
            .set_active(101, false)
            .await
            .expect("an unbound provider can be deactivated");

        assert!(!provider.is_active);
        drop(service);
        assert_eq!(
            shapes(&logged_transactions(db)),
            vec![vec![
                "BEGIN",
                "SELECT FOR UPDATE",
                "COUNT",
                "UPDATE",
                "COMMIT"
            ]]
        );
    }

    #[tokio::test]
    async fn activating_a_provider_never_counts_bindings() {
        for path in ["set_active", "update"] {
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results(vec![vec![dns_provider(101, "edge-dns", false)]])
                    .append_query_results(vec![vec![dns_provider(101, "edge-dns", true)]])
                    .into_connection(),
            );
            let service = service_with(db.clone(), "provider-activate-test");

            let provider = match path {
                "set_active" => service.set_active(101, true).await,
                _ => {
                    service
                        .update(
                            101,
                            UpdateProviderRequest {
                                is_active: Some(true),
                                ..deactivate_request()
                            },
                        )
                        .await
                }
            }
            .unwrap_or_else(|error| panic!("{path}: activation failed: {error}"));

            assert!(provider.is_active, "{path}");
            drop(service);
            assert_eq!(
                shapes(&logged_transactions(db)),
                vec![vec!["BEGIN", "SELECT FOR UPDATE", "UPDATE", "COMMIT"]],
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn hostname_mode_changes_fail_fast_while_a_zone_operation_runs() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/Postgres unavailable; skipping hostname-mode zone lock test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let now = chrono::Utc::now();
        let provider = dns_providers::ActiveModel {
            name: Set("zone-lock-test".into()),
            provider_type: Set("cloudflare".into()),
            credentials: Set("{}".into()),
            is_active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert provider");
        let managed = dns_managed_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set("example.com".into()),
            auto_manage: Set(true),
            proxied_by_default: Set(false),
            verified: Set(true),
            generated_hostname_mode: Set("standard".into()),
            sync_generated_records: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert managed domain");
        let service = DnsProviderService::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password("zone-lock-test")),
        );
        let stored_mode = || async {
            dns_managed_domains::Entity::find_by_id(managed.id)
                .one(db.as_ref())
                .await
                .expect("read managed domain")
                .expect("managed domain exists")
                .generated_hostname_mode
        };

        // Another operation (an apply mid-way through its DNS writes, or a
        // reconciliation) holds the zone.
        let running =
            hostname_sync::ZoneOperationLock::acquire(db.as_ref(), provider.id, "example.com")
                .await
                .expect("the running operation holds the zone");

        let apply = service
            .apply_hostname_mode(
                provider.id,
                "example.com",
                PublicHostnameStrategy::Flat,
                false,
                &ConflictDecisions::default(),
                1,
            )
            .await
            .expect_err("a second hostname-mode apply must not interleave");
        assert!(
            matches!(
                &apply,
                DnsError::ZoneOperationInProgress { provider_id, zone }
                    if *provider_id == provider.id && zone == "example.com"
            ),
            "{apply:?}"
        );
        let update = service
            .update_managed_domain(
                provider.id,
                "Example.COM.",
                UpdateManagedDomainRequest {
                    generated_hostname_mode: Some("flat".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("a mode-only update must not land mid-operation either");
        assert!(
            matches!(update, DnsError::ZoneOperationInProgress { .. }),
            "{update:?}"
        );
        assert_eq!(
            stored_mode().await,
            "standard",
            "refused changes write nothing"
        );

        // Settings that do not touch the generated-hostname state still save.
        service
            .update_managed_domain(
                provider.id,
                "example.com",
                UpdateManagedDomainRequest {
                    sync_generated_records: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("an update without a mode change does not need the zone lock");

        running
            .finish(Ok(()))
            .await
            .expect("the running operation ends");
        service
            .apply_hostname_mode(
                provider.id,
                "example.com",
                PublicHostnameStrategy::Flat,
                false,
                &ConflictDecisions::default(),
                1,
            )
            .await
            .expect("the zone is free once the running operation ends");
        assert_eq!(stored_mode().await, "flat");
    }
}
