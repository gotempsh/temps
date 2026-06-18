use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use std::sync::Arc;
use temps_entities::domains;
use temps_entities::on_demand_cert_attempts;
use temps_entities::tls_acme_certificates;
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::tls::{
    CertificateProvider, CertificateRepository, ChallengeType, ProvisioningResult, RepositoryError,
    TlsError,
};

#[derive(Error, Debug)]
pub enum DomainServiceError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Domain not found: {0}")]
    NotFound(String),
    #[error("Invalid domain: {0}")]
    InvalidDomain(String),
    #[error("Challenge error: {0}")]
    Challenge(String),
    #[error("TLS error: {0}")]
    Tls(#[from] TlsError),
    #[error("Provider error: {0}")]
    Provider(#[from] crate::tls::ProviderError),
    #[error("Repository error: {0}")]
    Repository(#[from] RepositoryError),
    #[error("Internal error: {0}")]
    Internal(String),

    /// On-demand HTTP-01 issuance for `hostname` hit a Let's Encrypt rate limit
    /// (`urn:ietf:params:acme:error:rateLimited`). `retry_after` carries the LE
    /// `Retry-After` window when the ACME problem document exposed one, else
    /// `None` (the caller falls back to `now + 1h` for the negative-cache
    /// backoff — see ADR-018 §4 Layer 4).
    #[error("On-demand TLS rate limited for {hostname} by Let's Encrypt: {detail}")]
    OnDemandRateLimited {
        hostname: String,
        detail: String,
        retry_after: Option<DateTime<Utc>>,
    },

    /// On-demand HTTP-01 issuance for `hostname` failed for a non-rate-limit
    /// reason. `category` is the coarse `error_category` written to the
    /// `on_demand_cert_attempts` audit row (`dns_failure`, `acme_order_expired`,
    /// `challenge_mismatch`, `timeout`, or `internal`); `error_chain` is the full
    /// `source()` chain of the underlying error.
    #[error("On-demand TLS issuance failed for {hostname} ({category}): {error_chain}")]
    OnDemandIssuanceFailed {
        hostname: String,
        category: String,
        error_chain: String,
    },
}

pub struct DomainService {
    db: Arc<DatabaseConnection>,
    cert_provider: Arc<dyn CertificateProvider>,
    repository: Arc<dyn CertificateRepository>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl DomainService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        cert_provider: Arc<dyn CertificateProvider>,
        repository: Arc<dyn CertificateRepository>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            cert_provider,
            repository,
            encryption_service,
        }
    }

    /// Determine the status a domain should fall back to when a renewal/order is
    /// abandoned (cancelled or failed). If the domain still holds a usable
    /// certificate (present key + cert that has not yet expired) we must return it
    /// to `"active"` so the proxy keeps serving the live HTTPS cert — only the
    /// `tls_cert_loader` query filters on `status = "active"`, so leaving a valid
    /// cert under any other status silently takes the domain offline. When there
    /// is no usable certificate we fall back to `"pending"`.
    fn fallback_status_for(domain: &domains::Model) -> &'static str {
        let has_material = domain.certificate.as_ref().is_some_and(|c| !c.is_empty())
            && domain.private_key.as_ref().is_some_and(|k| !k.is_empty());

        // Treat a missing expiration as not-usable: we can't prove it's still valid.
        let not_expired = domain.expiration_time.is_some_and(|exp| exp > Utc::now());

        if has_material && not_expired {
            "active"
        } else {
            "pending"
        }
    }

    /// Step 1: Create a domain record in the database
    pub async fn create_domain(
        &self,
        domain_name: &str,
        challenge_type: &str,
    ) -> Result<domains::Model, DomainServiceError> {
        info!(
            "Creating domain: {} with challenge type: {}",
            domain_name, challenge_type
        );

        // Validate domain format
        if !self.is_valid_domain(domain_name) {
            return Err(DomainServiceError::InvalidDomain(format!(
                "Invalid domain format: {}",
                domain_name
            )));
        }

        // Validate challenge type
        let verification_method = match challenge_type {
            "http-01" | "dns-01" => challenge_type.to_string(),
            _ => {
                warn!(
                    "Invalid challenge type '{}' specified, defaulting to http-01",
                    challenge_type
                );
                "http-01".to_string()
            }
        };

        // Check if domain already exists
        if let Some(_existing) = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain_name))
            .one(self.db.as_ref())
            .await?
        {
            return Err(DomainServiceError::InvalidDomain(format!(
                "Domain {} already exists",
                domain_name
            )));
        }

        // Create new domain record with specified challenge type
        let new_domain = domains::ActiveModel {
            domain: Set(domain_name.to_string()),
            status: Set("pending".to_string()),
            is_wildcard: Set(domain_name.starts_with("*.")),
            verification_method: Set(verification_method),
            dns_challenge_token: Set(None),
            dns_challenge_value: Set(None),
            http_challenge_token: Set(None),
            http_challenge_key_authorization: Set(None),
            certificate: Set(None),
            private_key: Set(None),
            expiration_time: Set(None),
            last_renewed: Set(None),
            last_error: Set(None),
            last_error_type: Set(None),
            ..Default::default()
        };

        let domain = new_domain.insert(self.db.as_ref()).await?;

        debug!(
            "Domain created successfully: {} with ID: {} using {} challenge",
            domain_name, domain.id, challenge_type
        );
        Ok(domain)
    }

    /// Step 2: Request a Let's Encrypt challenge for the domain
    pub async fn request_challenge(
        &self,
        domain_name: &str,
        user_email: &str,
    ) -> Result<ChallengeData, DomainServiceError> {
        info!(
            "Requesting Let's Encrypt challenge for domain: {} with email: {}",
            domain_name, user_email
        );

        // Validate email is provided
        if user_email.is_empty() {
            return Err(DomainServiceError::InvalidDomain(
                "User email is required for Let's Encrypt certificate provisioning".to_string(),
            ));
        }

        // Find the domain
        let mut domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain_name))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DomainServiceError::NotFound(domain_name.to_string()))?;

        // Clean up any existing order for this domain (important for renewals)
        // This ensures we always start fresh with a new challenge
        if let Some(existing_order) = self.repository.find_acme_order_by_domain(domain.id).await? {
            info!(
                "Deleting existing ACME order for domain: {} (order_url: {})",
                domain_name, existing_order.order_url
            );
            self.repository
                .delete_acme_order(&existing_order.order_url)
                .await?;
        }

        // Determine challenge type from domain's verification method
        let challenge_type = match domain.verification_method.as_str() {
            "http-01" => ChallengeType::Http01,
            "dns-01" => ChallengeType::Dns01,
            _ => ChallengeType::Http01, // Default to HTTP-01
        };

        // Request challenge from Let's Encrypt
        match self
            .cert_provider
            .provision(domain_name, challenge_type, user_email)
            .await?
        {
            ProvisioningResult::Challenge(challenge_data) => {
                // Save challenge data to acme_orders table
                let challenge_type_str = match challenge_data.challenge_type {
                    ChallengeType::Http01 => "http-01",
                    ChallengeType::Dns01 => "dns-01",
                };

                // Create ACME order record with challenge data stored in JSON
                let identifiers = serde_json::json!([{
                    "type": "dns",
                    "value": domain_name
                }]);

                // Store authorizations as array of DNS TXT records
                let authorizations = serde_json::json!({
                    "challenge_type": challenge_type_str,
                    "token": challenge_data.token,
                    "key_authorization": challenge_data.key_authorization,
                    "dns_txt_records": challenge_data.dns_txt_records,
                    "validation_url": challenge_data.validation_url
                });

                let order = crate::tls::models::AcmeOrder {
                    id: 0, // Will be set by database
                    order_url: challenge_data.order_url.clone().unwrap_or_default(),
                    domain_id: domain.id,
                    email: user_email.to_string(),
                    status: "pending".to_string(),
                    identifiers,
                    authorizations: Some(authorizations),
                    finalize_url: None,
                    certificate_url: None,
                    error: None,
                    error_type: None,
                    token: Some(challenge_data.token.clone()), // For fast HTTP-01 lookups
                    key_authorization: Some(challenge_data.key_authorization.clone()), // For fast HTTP-01 lookups
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    expires_at: Some(Utc::now() + chrono::Duration::days(7)), // ACME orders typically expire in 7 days
                };

                self.repository.save_acme_order(order).await?;

                // Update domain status based on challenge type
                let mut domain_active: domains::ActiveModel = domain.into();
                domain_active.status = Set("challenge_requested".to_string());

                match challenge_data.challenge_type {
                    ChallengeType::Http01 => {
                        domain_active.http_challenge_token =
                            Set(Some(challenge_data.token.clone()));
                        domain_active.http_challenge_key_authorization =
                            Set(Some(challenge_data.key_authorization.clone()));
                        info!("HTTP-01 challenge requested for domain: {}. Place {} at /.well-known/acme-challenge/{}",
                              domain_name,
                              challenge_data.key_authorization,
                              challenge_data.token);
                    }
                    ChallengeType::Dns01 => {
                        // Store first DNS TXT value for backward compatibility
                        let first_txt_record = challenge_data.dns_txt_records.first();
                        domain_active.dns_challenge_token = Set(Some(challenge_data.token.clone()));
                        domain_active.dns_challenge_value =
                            Set(first_txt_record.map(|r| r.value.clone()));

                        if !challenge_data.dns_txt_records.is_empty() {
                            info!(
                                "DNS-01 challenge requested for domain: {}. Add {} TXT record(s):",
                                domain_name,
                                challenge_data.dns_txt_records.len()
                            );
                            for (i, txt_record) in challenge_data.dns_txt_records.iter().enumerate()
                            {
                                info!("  [{}] {} = {}", i + 1, txt_record.name, txt_record.value);
                            }
                        }
                    }
                }

                domain = domain_active.update(self.db.as_ref()).await?;

                Ok(ChallengeData {
                    domain: domain.domain.to_string(),
                    challenge_type: challenge_type_str.to_string(),
                    token: challenge_data.token,
                    key_authorization: challenge_data.key_authorization,
                    txt_records: challenge_data.dns_txt_records,
                    validation_url: challenge_data.validation_url.unwrap_or_default(),
                    status: "pending".to_string(),
                })
            }
            ProvisioningResult::Certificate(cert_data) => {
                // If we receive a certificate immediately, store it and mark domain as active
                info!(
                    "Certificate provisioned immediately for domain: {}",
                    domain_name
                );

                // Encrypt private key before storing
                let encrypted_private_key = self
                    .encryption_service
                    .encrypt_string(&cert_data.private_key_pem)
                    .map_err(|e| {
                        DomainServiceError::Internal(format!(
                            "Failed to encrypt private key: {}",
                            e
                        ))
                    })?;

                let mut domain_active: domains::ActiveModel = domain.into();
                domain_active.status = Set("active".to_string());
                domain_active.certificate = Set(Some(cert_data.certificate_pem.clone()));
                domain_active.private_key = Set(Some(encrypted_private_key));
                domain_active.expiration_time = Set(Some(cert_data.expiration_time));
                domain_active.last_error = Set(None);
                domain_active.last_error_type = Set(None);

                let domain = domain_active.update(self.db.as_ref()).await?;

                // Return challenge data indicating immediate completion
                Ok(ChallengeData {
                    domain: domain.domain.to_string(),
                    challenge_type: cert_data.verification_method.clone(),
                    token: "".to_string(),
                    key_authorization: "".to_string(),
                    txt_records: vec![],
                    validation_url: "".to_string(),
                    status: "completed".to_string(),
                })
            }
        }
    }

    /// Step 3: Complete the challenge (after user has added DNS record)
    pub async fn complete_challenge(
        &self,
        domain_name: &str,
        user_email: &str,
    ) -> Result<domains::Model, DomainServiceError> {
        debug!(
            "Completing challenge for domain: {} with email: {}",
            domain_name, user_email
        );

        // Validate email is provided
        if user_email.is_empty() {
            return Err(DomainServiceError::InvalidDomain(
                "User email is required for Let's Encrypt certificate provisioning".to_string(),
            ));
        }

        // Find the domain
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain_name))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DomainServiceError::NotFound(domain_name.to_string()))?;

        // Find the ACME order for this domain
        let order = self.repository.find_acme_order_by_domain(domain.id).await?
            .ok_or_else(|| DomainServiceError::Challenge(
                format!("No ACME order found for domain: {}. Please create an order first using POST /domains/{}/order",
                    domain_name, domain.id)
            ))?;

        // Check if order is in a valid state
        if order.status != "pending" && order.status != "ready" {
            return Err(DomainServiceError::Challenge(
                format!("ACME order is in '{}' state and cannot be finalized. The authorization may have expired or failed. \
                         Please cancel this order (DELETE /domains/{}/order) and create a new one (POST /domains/{}/order).",
                    order.status, domain.id, domain.id)
            ));
        }

        // Extract challenge data from authorizations JSON
        let authorizations = order.authorizations.clone().unwrap_or_default();
        let challenge_type_str = authorizations["challenge_type"]
            .as_str()
            .unwrap_or("http-01");
        let challenge_type = match challenge_type_str {
            "http-01" => ChallengeType::Http01,
            "dns-01" => ChallengeType::Dns01,
            _ => ChallengeType::Http01,
        };

        // Parse DNS TXT records from authorizations (for DNS-01)
        let dns_txt_records = if let Some(records_json) = authorizations
            .get("dns_txt_records")
            .and_then(|v| v.as_array())
        {
            records_json
                .iter()
                .filter_map(|rec| {
                    Some(crate::tls::models::DnsTxtRecord {
                        name: rec["name"].as_str()?.to_string(),
                        value: rec["value"].as_str()?.to_string(),
                        validation_url: rec["validation_url"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        } else {
            vec![]
        };

        // Extract validation URL (used for both HTTP-01 and DNS-01)
        let validation_url = authorizations["validation_url"].as_str().map(String::from);

        let challenge = crate::tls::models::ChallengeData {
            challenge_type: challenge_type.clone(),
            domain: domain_name.to_string(),
            token: order.token.clone().unwrap_or_default(),
            key_authorization: order.key_authorization.clone().unwrap_or_default(),
            validation_url,
            dns_txt_records,
            order_url: Some(order.order_url.clone()),
        };

        debug!(
            "Completing {:?} challenge for domain {} with validation URL: {:?}",
            challenge_type.clone(),
            domain_name,
            challenge.validation_url
        );

        // Complete the challenge with Let's Encrypt
        match self
            .cert_provider
            .complete_challenge(domain_name, &challenge, user_email)
            .await
        {
            Ok(certificate) => {
                // Encrypt private key before storing
                let encrypted_private_key = self
                    .encryption_service
                    .encrypt_string(&certificate.private_key_pem)
                    .map_err(|e| {
                        DomainServiceError::Internal(format!(
                            "Failed to encrypt private key: {}",
                            e
                        ))
                    })?;

                // Save certificate to tls_acme_certificates table
                let acme_cert = tls_acme_certificates::ActiveModel {
                    domain: Set(domain_name.to_string()),
                    certificate: Set(certificate.certificate_pem.clone()),
                    private_key: Set(encrypted_private_key.clone()),
                    expires_at: Set(certificate.expiration_time),
                    issued_at: Set(Utc::now()),
                    ..Default::default()
                };

                acme_cert.insert(self.db.as_ref()).await?;

                // Capture domain ID before move
                let domain_id = domain.id;

                // Update domain record
                let mut domain_active: domains::ActiveModel = domain.into();
                domain_active.status = Set("active".to_string());
                domain_active.certificate = Set(Some(certificate.certificate_pem));
                domain_active.private_key = Set(Some(encrypted_private_key));
                domain_active.expiration_time = Set(Some(certificate.expiration_time));
                domain_active.last_renewed = Set(Some(Utc::now()));
                domain_active.last_error = Set(None);
                domain_active.last_error_type = Set(None);

                let updated_domain = domain_active.update(self.db.as_ref()).await?;

                // Clean up ACME order
                if let Some(order) = self.repository.find_acme_order_by_domain(domain_id).await? {
                    self.repository.delete_acme_order(&order.order_url).await?;
                }

                info!(
                    "Challenge completed successfully for domain: {}",
                    domain_name
                );
                Ok(updated_domain)
            }
            Err(e) => {
                error!(
                    "Failed to complete challenge for domain {}: {}",
                    domain_name, e
                );

                // A failed renewal must not take down a domain that is still serving a
                // valid certificate: keep it "active" when usable cert material remains,
                // otherwise mark it "failed". (The proxy only loads certs for "active"
                // domains, so "failed" with a live cert silently breaks HTTPS.)
                let fallback_status = Self::fallback_status_for(&domain);

                // Persist the error so the failure is visible, and actually write it —
                // the previous implementation built this ActiveModel but never called
                // update(), silently dropping the status/error change.
                let mut domain_active: domains::ActiveModel = domain.into();
                domain_active.status = Set(fallback_status.to_string());
                domain_active.last_error = Set(Some(e.to_string()));
                domain_active.last_error_type = Set(Some("challenge_completion".to_string()));

                if let Err(update_err) = domain_active.update(self.db.as_ref()).await {
                    error!(
                        "Failed to persist challenge-failure status for domain {}: {}",
                        domain_name, update_err
                    );
                }

                Err(DomainServiceError::Challenge(format!(
                    "Failed to complete challenge: {}.",
                    e
                )))
            }
        }
    }

    /// Get domain by name
    pub async fn get_domain(
        &self,
        domain_name: &str,
    ) -> Result<Option<domains::Model>, DomainServiceError> {
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain_name))
            .one(self.db.as_ref())
            .await?;
        Ok(domain)
    }

    /// Get domain by ID
    pub async fn get_domain_by_id(
        &self,
        id: i32,
    ) -> Result<Option<domains::Model>, DomainServiceError> {
        let domain = domains::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?;
        Ok(domain)
    }

    /// List all domains
    pub async fn list_domains(&self) -> Result<Vec<domains::Model>, DomainServiceError> {
        let domains = domains::Entity::find().all(self.db.as_ref()).await?;
        Ok(domains)
    }

    /// List domains with pagination
    pub async fn list_domains_paginated(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<domains::Model>, DomainServiceError> {
        let domains = domains::Entity::find()
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;
        Ok(domains)
    }

    /// List domains with pagination, search, and total count
    pub async fn list_domains_with_total(
        &self,
        page: u64,
        page_size: u64,
        search: Option<&str>,
    ) -> Result<(Vec<domains::Model>, u64), DomainServiceError> {
        let mut query = domains::Entity::find();

        if let Some(search) = search {
            if !search.is_empty() {
                query = query.filter(domains::Column::Domain.contains(search));
            }
        }

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let domains = paginator.fetch_page(page - 1).await?;
        Ok((domains, total))
    }

    /// Get challenge status for a domain
    pub async fn get_challenge_status(
        &self,
        domain_name: &str,
    ) -> Result<Option<ChallengeData>, DomainServiceError> {
        // Get domain to find its ID
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain_name))
            .one(self.db.as_ref())
            .await?;

        if let Some(domain) = domain {
            // Find ACME order
            if let Some(order) = self.repository.find_acme_order_by_domain(domain.id).await? {
                let authorizations = order.authorizations.unwrap_or_default();

                // Parse DNS TXT records from authorizations
                let txt_records = if let Some(records_json) = authorizations
                    .get("dns_txt_records")
                    .and_then(|v| v.as_array())
                {
                    records_json
                        .iter()
                        .filter_map(|rec| {
                            Some(crate::tls::models::DnsTxtRecord {
                                name: rec["name"].as_str()?.to_string(),
                                value: rec["value"].as_str()?.to_string(),
                                validation_url: rec["validation_url"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string(),
                            })
                        })
                        .collect()
                } else {
                    vec![]
                };

                return Ok(Some(ChallengeData {
                    domain: domain_name.to_string(),
                    challenge_type: authorizations["challenge_type"]
                        .as_str()
                        .unwrap_or("http-01")
                        .to_string(),
                    token: order.token.unwrap_or_default(),
                    key_authorization: order.key_authorization.unwrap_or_default(),
                    txt_records,
                    validation_url: authorizations["validation_url"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    status: order.status,
                }));
            }
        }
        Ok(None)
    }

    /// Delete a domain
    pub async fn delete_domain(&self, domain_name: &str) -> Result<(), DomainServiceError> {
        info!("Deleting domain: {}", domain_name);

        // Delete from domains table
        let result = domains::Entity::delete_many()
            .filter(domains::Column::Domain.eq(domain_name))
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(DomainServiceError::NotFound(domain_name.to_string()));
        }

        // Clean up related data - acme_orders will be deleted via ON DELETE CASCADE
        tls_acme_certificates::Entity::delete_many()
            .filter(tls_acme_certificates::Column::Domain.eq(domain_name))
            .exec(self.db.as_ref())
            .await?;

        info!("Domain deleted successfully: {}", domain_name);
        Ok(())
    }

    /// Cancel an existing ACME order for a domain and allow creating a new one
    /// This clears all challenge data and resets the domain status
    pub async fn cancel_order(
        &self,
        domain_name: &str,
    ) -> Result<domains::Model, DomainServiceError> {
        info!("Canceling order and resetting domain: {}", domain_name);

        // Find the domain
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain_name))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DomainServiceError::NotFound(domain_name.to_string()))?;

        // Clean up ACME order if exists
        if let Some(order) = self.repository.find_acme_order_by_domain(domain.id).await? {
            self.repository.delete_acme_order(&order.order_url).await?;
        }

        // Cancelling an in-flight renewal must NOT take down a domain that is still
        // serving a valid certificate. The cert/key columns are left untouched here,
        // but the proxy only loads certs for domains with status = "active", so if we
        // blindly reset to "pending" the live cert stops being served. Fall back to
        // "active" when a usable cert is still present, otherwise "pending".
        let fallback_status = Self::fallback_status_for(&domain);
        info!(
            "Cancelling order for domain {}: resetting status to '{}' (existing certificate {})",
            domain_name,
            fallback_status,
            if fallback_status == "active" {
                "preserved"
            } else {
                "absent or expired"
            }
        );

        // Reset domain status and clear challenge fields
        let mut domain_active: domains::ActiveModel = domain.into();
        domain_active.status = Set(fallback_status.to_string());
        domain_active.dns_challenge_token = Set(None);
        domain_active.dns_challenge_value = Set(None);
        domain_active.http_challenge_token = Set(None);
        domain_active.http_challenge_key_authorization = Set(None);
        domain_active.last_error = Set(Some("Order cancelled by user".to_string()));
        domain_active.last_error_type = Set(Some("cancelled".to_string()));

        let updated_domain = domain_active.update(self.db.as_ref()).await?;

        // Call provider's cancel_order (mostly for logging)
        let _ = self.cert_provider.cancel_order(domain_name).await;

        info!(
            "Order cancelled successfully for domain: {}. Ready to create new order.",
            domain_name
        );
        Ok(updated_domain)
    }

    /// Decrypt private key for a domain
    pub async fn get_decrypted_private_key(
        &self,
        domain_name: &str,
    ) -> Result<Option<String>, DomainServiceError> {
        let domain_opt = self.get_domain(domain_name).await?;

        if let Some(domain) = domain_opt {
            if let Some(encrypted_key) = domain.private_key {
                let decrypted = self
                    .encryption_service
                    .decrypt_string(&encrypted_key)
                    .map_err(|e| {
                        DomainServiceError::Internal(format!(
                            "Failed to decrypt private key: {}",
                            e
                        ))
                    })?;
                Ok(Some(decrypted))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// ADR-018 Layer 2 — drive the full HTTP-01 ACME flow for a single STABLE
    /// hostname, triggered lazily by the proxy's `certificate_callback` when no
    /// active cert exists.
    ///
    /// This is the on-demand counterpart of the order-based renewal path
    /// (`TlsService::handle_http01_renewal_order_based`): it reuses the same
    /// `request_challenge` → wait → `complete_challenge` pipeline (and therefore
    /// the same `LetsEncryptProvider` / `CertificateRepository`), so no ACME
    /// client logic is duplicated here. The proxy already serves the HTTP-01
    /// token from `domains.http_challenge_token`, so the happy path needs no
    /// operator action.
    ///
    /// State machine (persisted on the `domains` row, ADR-018 §3):
    /// `on_demand_pending` → `on_demand_issuing` → `active` | `on_demand_failed`.
    ///
    /// Every call writes an `on_demand_cert_attempts` audit row at the outcome
    /// (start is logged via `tracing`). On failure the row carries the full
    /// `source()` chain and a coarse `error_category`, and the `domains` row gets
    /// an exponential `on_demand_backoff_until` (5m → 15m → 1h → 4h → 24h cap)
    /// plus `last_error` / `last_error_type`, so the negative cache and the
    /// console "Certificates" surface both have a queryable signal.
    ///
    /// `email` is the ACME account contact; the caller (proxy) resolves it from
    /// `letsencrypt.email` settings (falling back to the first user) before
    /// calling, mirroring `TlsService::get_acme_email`.
    ///
    /// # Caller invariant (security)
    ///
    /// This method enforces only `is_valid_domain` + wildcard rejection. It does
    /// NOT re-check the on-demand zone suffix or that `hostname` maps to a
    /// cert-eligible (stable, non-ephemeral) route — those are the allowlist
    /// controls and they live in the proxy's `OnDemandCertManager::try_enqueue`
    /// gate (ADR-018 §2). **Every caller MUST therefore have passed that gate.**
    /// The only production caller is the gated background issuance consumer. Do
    /// not wire a new caller (CLI force-issue, admin endpoint, retry job) to this
    /// method without first re-applying the zone + cert-eligibility checks, or it
    /// would issue a real Let's Encrypt cert for any syntactically-valid
    /// non-wildcard hostname. (ADR-018 security review, LOW.)
    pub async fn provision_on_demand(
        &self,
        hostname: &str,
        email: &str,
    ) -> Result<(), DomainServiceError> {
        let started = Utc::now();
        info!(
            hostname = %hostname,
            transition = "none->on_demand_issuing",
            "on-demand TLS: starting HTTP-01 issuance"
        );

        if email.trim().is_empty() {
            // No ACME contact configured — record a skipped/internal attempt and
            // bail without touching Let's Encrypt.
            let err = DomainServiceError::OnDemandIssuanceFailed {
                hostname: hostname.to_string(),
                category: "internal".to_string(),
                error_chain: "no ACME contact email configured for on-demand issuance".to_string(),
            };
            self.record_on_demand_outcome(hostname, false, false, None, Some(&err), started)
                .await;
            return Err(err);
        }

        // 1. Create or reuse the domains row, transitioning into the issuing state.
        //    HTTP-01 is the only supported on-demand challenge (wildcards/DNS-01
        //    are explicitly out of scope per ADR §2).
        if let Err(e) = self.begin_on_demand_issuing(hostname).await {
            self.record_on_demand_outcome(hostname, false, false, None, Some(&e), started)
                .await;
            return Err(e);
        }

        // 2. Run the ACME order: request_challenge persists the order + serves the
        //    token, complete_challenge finalizes and stores cert+key as "active".
        match self.run_on_demand_acme(hostname, email).await {
            Ok(()) => {
                let duration_ms = Self::elapsed_ms(started);
                info!(
                    hostname = %hostname,
                    transition = "on_demand_issuing->active",
                    outcome = "issued",
                    duration_ms,
                    "on-demand TLS: certificate issued"
                );
                self.record_on_demand_outcome(
                    hostname,
                    true,
                    true,
                    Some("200".to_string()),
                    None,
                    started,
                )
                .await;
                Ok(())
            }
            Err(e) => {
                // Persist the failure on the domains row (status + backoff + error)
                // before recording the audit row and returning.
                let (category, retry_after) = Self::categorize_on_demand_error(&e);
                let error_chain = error_chain_string(&e);
                if let Err(persist_err) = self
                    .mark_on_demand_failed(hostname, &error_chain, &category, retry_after)
                    .await
                {
                    error!(
                        hostname = %hostname,
                        "on-demand TLS: failed to persist failure state: {}",
                        persist_err
                    );
                }
                let duration_ms = Self::elapsed_ms(started);
                warn!(
                    hostname = %hostname,
                    transition = "on_demand_issuing->on_demand_failed",
                    outcome = "failed",
                    error_category = %category,
                    error_chain = %error_chain,
                    duration_ms,
                    "on-demand TLS: issuance failed"
                );
                // acme_request_sent is true whenever we reached request_challenge.
                let acme_status = Self::acme_status_for(&e);
                self.record_on_demand_outcome(hostname, true, true, acme_status, Some(&e), started)
                    .await;
                Err(e)
            }
        }
    }

    /// Create the `domains` row for `hostname` if absent, then move it into the
    /// `on_demand_issuing` state. Existing rows that already hold a cert are not
    /// clobbered beyond the status flip — `request_challenge` rebuilds the order.
    async fn begin_on_demand_issuing(
        &self,
        hostname: &str,
    ) -> Result<domains::Model, DomainServiceError> {
        if !self.is_valid_domain(hostname) {
            return Err(DomainServiceError::InvalidDomain(format!(
                "Invalid on-demand hostname: {hostname}"
            )));
        }
        if hostname.starts_with("*.") {
            // Wildcards require DNS-01 and are never on-demand certed (ADR §2).
            return Err(DomainServiceError::InvalidDomain(format!(
                "Wildcard hostname {hostname} cannot use on-demand HTTP-01 TLS"
            )));
        }

        match domains::Entity::find()
            .filter(domains::Column::Domain.eq(hostname))
            .one(self.db.as_ref())
            .await?
        {
            Some(existing) => {
                let mut active: domains::ActiveModel = existing.into();
                active.status = Set("on_demand_issuing".to_string());
                active.verification_method = Set("http-01".to_string());
                active.on_demand_backoff_until = Set(None);
                active.last_error = Set(None);
                active.last_error_type = Set(None);
                let updated = active.update(self.db.as_ref()).await?;
                Ok(updated)
            }
            None => {
                let new_domain = domains::ActiveModel {
                    domain: Set(hostname.to_string()),
                    status: Set("on_demand_issuing".to_string()),
                    is_wildcard: Set(false),
                    verification_method: Set("http-01".to_string()),
                    on_demand_backoff_until: Set(None),
                    ..Default::default()
                };
                let created = new_domain.insert(self.db.as_ref()).await?;
                Ok(created)
            }
        }
    }

    /// Run the two-step ACME order against Let's Encrypt, reusing the existing
    /// `request_challenge` / `complete_challenge` pipeline. Errors are surfaced
    /// untouched so the caller can categorize them.
    async fn run_on_demand_acme(
        &self,
        hostname: &str,
        email: &str,
    ) -> Result<(), DomainServiceError> {
        // Step 1: create + persist a fresh ACME order. This sets the HTTP-01
        // token on the domains row, which the proxy serves.
        let challenge = self
            .request_challenge(hostname, email)
            .await
            .map_err(|e| Self::map_acme_error(hostname, e))?;

        // A cached/valid authorization can yield a certificate immediately —
        // request_challenge already stored it as "active".
        if challenge.status == "completed" {
            return Ok(());
        }

        // Step 2: give Let's Encrypt a moment to fetch the served token, then
        // accept the challenge and finalize.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        self.complete_challenge(hostname, email)
            .await
            .map_err(|e| Self::map_acme_error(hostname, e))?;
        Ok(())
    }

    /// Promote a Let's Encrypt `rateLimited` failure (ADR §4 Layer 4) into the
    /// typed `OnDemandRateLimited` variant so the caller can surface a specific
    /// console message and set the backoff from the LE `Retry-After` window. The
    /// `instant_acme::Problem` is flattened into a string at the provider
    /// boundary, so we match on the ACME error URN inside the error chain. All
    /// other errors pass through unchanged for generic categorization.
    fn map_acme_error(hostname: &str, err: DomainServiceError) -> DomainServiceError {
        let chain = error_chain_string(&err);
        let lower = chain.to_lowercase();
        let is_rate_limited = lower.contains("urn:ietf:params:acme:error:ratelimited")
            || lower.contains("ratelimited");
        if !is_rate_limited {
            return err;
        }

        // Parse a `retry after <RFC3339>` hint from the LE problem detail if it
        // exposed one; otherwise the caller falls back to now + 1h.
        let retry_after = parse_retry_after(&chain);
        DomainServiceError::OnDemandRateLimited {
            hostname: hostname.to_string(),
            detail: chain,
            retry_after,
        }
    }

    /// Persist a failed on-demand attempt on the `domains` row: status
    /// `on_demand_failed`, the full error chain, an `on_demand` error type, and
    /// an exponential `on_demand_backoff_until` (ADR §4 Layer 2). For a rate
    /// limit, `retry_after` (when present) overrides the exponential delay.
    async fn mark_on_demand_failed(
        &self,
        hostname: &str,
        error_chain: &str,
        category: &str,
        retry_after: Option<DateTime<Utc>>,
    ) -> Result<(), DomainServiceError> {
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(hostname))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DomainServiceError::NotFound(hostname.to_string()))?;

        let backoff_until = match retry_after {
            Some(ts) => ts,
            None => {
                let prev = domain.on_demand_backoff_until;
                Utc::now() + Self::next_backoff_delay(prev, domain.updated_at)
            }
        };

        let mut active: domains::ActiveModel = domain.into();
        active.status = Set("on_demand_failed".to_string());
        active.last_error = Set(Some(error_chain.to_string()));
        active.last_error_type = Set(Some(category.to_string()));
        active.on_demand_backoff_until = Set(Some(backoff_until));
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Compute the next exponential backoff delay: 5m → 15m → 1h → 4h → 24h
    /// (capped). The previous step is inferred from the gap between the last
    /// `on_demand_backoff_until` and the row's `updated_at` (the time that
    /// backoff was set); a missing/short gap restarts at 5m.
    fn next_backoff_delay(
        prev_backoff_until: Option<DateTime<Utc>>,
        prev_set_at: DateTime<Utc>,
    ) -> Duration {
        const LADDER_MINS: [i64; 5] = [5, 15, 60, 240, 1440];
        let prev_delay_mins = prev_backoff_until
            .map(|until| (until - prev_set_at).num_minutes())
            .unwrap_or(0);

        // Find the next rung strictly greater than the previous delay; cap at 24h.
        let next = LADDER_MINS
            .iter()
            .copied()
            .find(|&m| m > prev_delay_mins)
            .unwrap_or(1440);
        Duration::minutes(next)
    }

    /// Map a `DomainServiceError` from the on-demand flow to a coarse
    /// `error_category` for the audit row plus an optional rate-limit
    /// `Retry-After` deadline. Categories: `rate_limited`, `dns_failure`,
    /// `acme_order_expired`, `challenge_mismatch`, `timeout`, `internal`.
    fn categorize_on_demand_error(err: &DomainServiceError) -> (String, Option<DateTime<Utc>>) {
        if let DomainServiceError::OnDemandRateLimited { retry_after, .. } = err {
            // ADR §4 Layer 4: honor the LE Retry-After window when present, else
            // fall back to now + 1h (NOT the exponential ladder).
            let deadline = retry_after.unwrap_or_else(|| Utc::now() + Duration::hours(1));
            return ("rate_limited".to_string(), Some(deadline));
        }

        let chain = error_chain_string(err).to_lowercase();
        let category = if chain.contains("ratelimited") || chain.contains("rate limit") {
            "rate_limited"
        } else if chain.contains("dns") {
            "dns_failure"
        } else if chain.contains("expired") {
            "acme_order_expired"
        } else if chain.contains("timed out") || chain.contains("timeout") {
            "timeout"
        } else if chain.contains("incorrect")
            || chain.contains("invalid response")
            || chain.contains("key authorization")
            || chain.contains("challenge")
        {
            "challenge_mismatch"
        } else {
            "internal"
        };
        (category.to_string(), None)
    }

    /// Best-effort ACME response status string for the audit row: the LE problem
    /// type for a rate limit, otherwise `None`.
    fn acme_status_for(err: &DomainServiceError) -> Option<String> {
        match err {
            DomainServiceError::OnDemandRateLimited { .. } => {
                Some("urn:ietf:params:acme:error:rateLimited".to_string())
            }
            _ => None,
        }
    }

    fn elapsed_ms(started: DateTime<Utc>) -> i32 {
        (Utc::now() - started)
            .num_milliseconds()
            .clamp(0, i32::MAX as i64) as i32
    }

    /// Append a single `on_demand_cert_attempts` audit row. Failure to write the
    /// audit row is logged but never fails the issuance (the `domains` row is the
    /// authoritative state; this table is the append-only observability log).
    async fn record_on_demand_outcome(
        &self,
        hostname: &str,
        challenge_served: bool,
        acme_request_sent: bool,
        acme_response_status: Option<String>,
        error: Option<&DomainServiceError>,
        started: DateTime<Utc>,
    ) {
        let (outcome, error_chain, error_category) = match error {
            None => ("issued".to_string(), None, None),
            Some(e) => {
                let (category, _) = Self::categorize_on_demand_error(e);
                (
                    "failed".to_string(),
                    Some(error_chain_string(e)),
                    Some(category),
                )
            }
        };

        let row = on_demand_cert_attempts::ActiveModel {
            hostname: Set(hostname.to_string()),
            trigger: Set("tls_callback".to_string()),
            challenge_served: Set(Some(challenge_served)),
            acme_request_sent: Set(Some(acme_request_sent)),
            acme_response_status: Set(acme_response_status),
            outcome: Set(outcome),
            error_chain: Set(error_chain),
            error_category: Set(error_category),
            duration_ms: Set(Some(Self::elapsed_ms(started))),
            ..Default::default()
        };

        if let Err(e) = row.insert(self.db.as_ref()).await {
            error!(
                hostname = %hostname,
                "on-demand TLS: failed to write cert-attempt audit row: {}",
                e
            );
        }
    }

    /// Fetch the most recent on-demand cert attempt for a hostname (used by the
    /// console "Certificates" surface and `temps domain cert-status`).
    pub async fn latest_on_demand_attempt(
        &self,
        hostname: &str,
    ) -> Result<Option<on_demand_cert_attempts::Model>, DomainServiceError> {
        let row = on_demand_cert_attempts::Entity::find()
            .filter(on_demand_cert_attempts::Column::Hostname.eq(hostname))
            .order_by_desc(on_demand_cert_attempts::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?;
        Ok(row)
    }

    /// Paginated list of on-demand cert attempts, newest first, each enriched
    /// with the current authoritative state of its `domains` row (ADR-018 §5
    /// console "Certificates" surface).
    ///
    /// The audit table (`on_demand_cert_attempts`) is append-only and carries
    /// the per-attempt forensic detail (challenge_served, acme_response_status,
    /// error_chain). The current cert state — `status`, `on_demand_backoff_until`,
    /// `expiration_time` — lives on the `domains` row. This method returns both
    /// so the UI can show "what is the cert doing right now" alongside "what
    /// happened on the last attempt". A hostname may have an attempt row but no
    /// `domains` row yet (e.g. a `skipped_gate` attempt that never created one),
    /// hence the `Option<domains::Model>`.
    ///
    /// Returns `(rows, total)` where `total` is the count across all attempts.
    pub async fn list_on_demand_attempts(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<OnDemandAttemptWithDomain>, u64), DomainServiceError> {
        let paginator = on_demand_cert_attempts::Entity::find()
            .order_by_desc(on_demand_cert_attempts::Column::CreatedAt)
            .order_by_desc(on_demand_cert_attempts::Column::Id)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let attempts = paginator.fetch_page(page.saturating_sub(1)).await?;

        if attempts.is_empty() {
            return Ok((Vec::new(), total));
        }

        // Resolve the current `domains` row for each distinct hostname in a
        // single query (avoid N+1). Hostnames map to `domains.domain`.
        let hostnames: Vec<String> = {
            let mut seen: Vec<String> = Vec::with_capacity(attempts.len());
            for a in &attempts {
                if !seen.contains(&a.hostname) {
                    seen.push(a.hostname.clone());
                }
            }
            seen
        };

        let domain_rows = domains::Entity::find()
            .filter(domains::Column::Domain.is_in(hostnames))
            .all(self.db.as_ref())
            .await?;

        let rows = attempts
            .into_iter()
            .map(|attempt| {
                let domain = domain_rows
                    .iter()
                    .find(|d| d.domain == attempt.hostname)
                    .cloned();
                OnDemandAttemptWithDomain { attempt, domain }
            })
            .collect();

        Ok((rows, total))
    }

    /// Current on-demand cert status for a single hostname: the authoritative
    /// `domains` row state (if any) plus the most recent audit attempt (if any).
    /// Backs `GET /domains/by-host/{hostname}/cert-status` and
    /// `temps domain cert-status` (ADR-018 §5).
    pub async fn on_demand_cert_status(
        &self,
        hostname: &str,
    ) -> Result<OnDemandCertStatus, DomainServiceError> {
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(hostname))
            .one(self.db.as_ref())
            .await?;
        let latest_attempt = self.latest_on_demand_attempt(hostname).await?;
        Ok(OnDemandCertStatus {
            domain,
            latest_attempt,
        })
    }

    fn is_valid_domain(&self, domain: &str) -> bool {
        // Basic domain validation
        if domain.is_empty() || domain.len() > 253 {
            return false;
        }

        // Allow wildcard domains
        let domain_to_check = domain.strip_prefix("*.").unwrap_or(domain);

        // Basic checks
        if domain_to_check.starts_with('.') || domain_to_check.ends_with('.') {
            return false;
        }

        // Split by dots and validate each part
        let parts: Vec<&str> = domain_to_check.split('.').collect();
        if parts.len() < 2 {
            return false;
        }

        for part in parts {
            if part.is_empty() || part.len() > 63 {
                return false;
            }

            // Check characters (alphanumeric and hyphens, but not starting/ending with hyphen)
            if !part.chars().all(|c| c.is_alphanumeric() || c == '-') {
                return false;
            }

            if part.starts_with('-') || part.ends_with('-') {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone)]
pub struct ChallengeData {
    pub domain: String,
    pub challenge_type: String,
    pub token: String,
    pub key_authorization: String,
    /// Array of DNS TXT records to add. For wildcards, multiple records are required.
    pub txt_records: Vec<crate::tls::models::DnsTxtRecord>,
    pub validation_url: String,
    pub status: String,
}

/// One on-demand cert attempt enriched with the current authoritative state of
/// its `domains` row (ADR-018 §5). `domain` is `None` when no `domains` row
/// exists for the attempt's hostname (e.g. an attempt that was skipped at the
/// gate before any row was created).
#[derive(Debug, Clone)]
pub struct OnDemandAttemptWithDomain {
    pub attempt: on_demand_cert_attempts::Model,
    pub domain: Option<domains::Model>,
}

/// Current on-demand cert status for a single hostname: the authoritative
/// `domains` row (if any) plus the most recent audit attempt (if any).
#[derive(Debug, Clone)]
pub struct OnDemandCertStatus {
    pub domain: Option<domains::Model>,
    pub latest_attempt: Option<on_demand_cert_attempts::Model>,
}

/// Render the full `Display` chain of an error — the top-level message plus every
/// `source()` level joined by `: ` — for the `on_demand_cert_attempts.error_chain`
/// audit column (ADR-018 §5). This is the operator's first-line diagnostic, so it
/// must preserve every nested cause rather than the top-level message alone.
fn error_chain_string(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(cause) = source {
        let msg = cause.to_string();
        // Skip a redundant tail when an outer layer already embedded the cause's
        // Display (common with `#[error("...: {0}")]` thiserror wrappers).
        if !parts.last().is_some_and(|last| last.contains(&msg)) {
            parts.push(msg);
        }
        source = cause.source();
    }
    parts.join(": ")
}

/// Best-effort extraction of a `Retry-After` deadline from a Let's Encrypt
/// `rateLimited` problem detail. LE typically phrases this as
/// "retry after 2026-06-25T00:00:00Z"; we parse the first RFC3339 timestamp that
/// follows the word "retry". Returns `None` when no parseable timestamp is found
/// (the caller then falls back to now + 1h).
fn parse_retry_after(detail: &str) -> Option<DateTime<Utc>> {
    let lower = detail.to_lowercase();
    let after = lower.find("retry")?;
    // Scan tokens after the "retry" marker for the first parseable RFC3339 value.
    for token in detail[after..].split(|c: char| c.is_whitespace() || c == ',') {
        let trimmed = token.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != ':' && c != '-' && c != '+' && c != '.'
        });
        if trimmed.len() >= 20 {
            if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
                return Some(parsed.with_timezone(&Utc));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use temps_core::EncryptionService;

    use super::*;
    use chrono::Datelike;
    use std::sync::Arc;

    struct MockProvider;

    #[async_trait::async_trait]
    impl CertificateProvider for MockProvider {
        async fn provision(
            &self,
            _domain: &str,
            _challenge: ChallengeType,
            _email: &str,
        ) -> Result<ProvisioningResult, crate::tls::ProviderError> {
            unimplemented!()
        }

        async fn complete_challenge(
            &self,
            _domain: &str,
            _challenge_data: &crate::tls::models::ChallengeData,
            _email: &str,
        ) -> Result<crate::tls::models::Certificate, crate::tls::ProviderError> {
            unimplemented!()
        }

        fn supported_challenges(&self) -> Vec<ChallengeType> {
            vec![ChallengeType::Dns01]
        }

        async fn validate_prerequisites(
            &self,
            _domain: &str,
            _email: &str,
        ) -> Result<crate::tls::models::ValidationResult, crate::tls::ProviderError> {
            unimplemented!()
        }

        async fn cancel_order(&self, _domain: &str) -> Result<(), crate::tls::ProviderError> {
            Ok(())
        }
    }

    /// Configurable provider for the on-demand issuance tests.
    ///
    /// On the success path we return `ProvisioningResult::Certificate` directly
    /// from `provision`, which mirrors Let's Encrypt's cached-authorization fast
    /// path: `request_challenge` stores the cert as `"active"` and reports
    /// `status="completed"`, so `provision_on_demand` never needs to call
    /// `complete_challenge` (and the test never waits on the 5s validation
    /// sleep). On the failure path `provision` returns the configured
    /// `ProviderError`.
    enum OnDemandMockMode {
        ImmediateCert,
        Fail(String),
    }

    struct OnDemandMockProvider {
        mode: OnDemandMockMode,
    }

    fn mock_certificate(domain: &str) -> crate::tls::models::Certificate {
        crate::tls::models::Certificate {
            id: 1,
            domain: domain.to_string(),
            certificate_pem: "-----BEGIN CERTIFICATE-----\nMOCK\n-----END CERTIFICATE-----"
                .to_string(),
            private_key_pem: "-----BEGIN PRIVATE KEY-----\nMOCK\n-----END PRIVATE KEY-----"
                .to_string(),
            expiration_time: Utc::now() + chrono::Duration::days(90),
            last_renewed: Some(Utc::now()),
            is_wildcard: false,
            verification_method: "acme".to_string(),
            status: crate::tls::CertificateStatus::Active,
        }
    }

    #[async_trait::async_trait]
    impl CertificateProvider for OnDemandMockProvider {
        async fn provision(
            &self,
            domain: &str,
            _challenge: ChallengeType,
            _email: &str,
        ) -> Result<ProvisioningResult, crate::tls::ProviderError> {
            match &self.mode {
                OnDemandMockMode::ImmediateCert => {
                    Ok(ProvisioningResult::Certificate(mock_certificate(domain)))
                }
                OnDemandMockMode::Fail(msg) => Err(crate::tls::ProviderError::Acme(msg.clone())),
            }
        }

        async fn complete_challenge(
            &self,
            domain: &str,
            _challenge_data: &crate::tls::models::ChallengeData,
            _email: &str,
        ) -> Result<crate::tls::models::Certificate, crate::tls::ProviderError> {
            match &self.mode {
                OnDemandMockMode::ImmediateCert => Ok(mock_certificate(domain)),
                OnDemandMockMode::Fail(msg) => Err(crate::tls::ProviderError::Acme(msg.clone())),
            }
        }

        fn supported_challenges(&self) -> Vec<ChallengeType> {
            vec![ChallengeType::Http01]
        }

        async fn validate_prerequisites(
            &self,
            _domain: &str,
            _email: &str,
        ) -> Result<crate::tls::models::ValidationResult, crate::tls::ProviderError> {
            Ok(crate::tls::models::ValidationResult {
                is_valid: true,
                errors: vec![],
                warnings: vec![],
            })
        }

        async fn cancel_order(&self, _domain: &str) -> Result<(), crate::tls::ProviderError> {
            Ok(())
        }
    }

    /// Build a `DomainService` backed by a real (Docker) Postgres test schema and
    /// the configurable on-demand mock provider. Returns `None` when Docker /
    /// `TEMPS_TEST_DATABASE_URL` is unavailable so the test skips gracefully
    /// (Docker-dependent tests must never be `#[ignore]` per project rules).
    async fn on_demand_service(
        mode: OnDemandMockMode,
    ) -> Option<(
        DomainService,
        Arc<DatabaseConnection>,
        temps_database::test_utils::TestDatabase,
    )> {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping on-demand TLS test: {e}");
                return None;
            }
        };
        let encryption_service = Arc::new(
            EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let repository = Arc::new(crate::tls::repository::DefaultCertificateRepository::new(
            test_db.db.clone(),
            encryption_service.clone(),
        ));
        let service = DomainService::new(
            test_db.db.clone(),
            Arc::new(OnDemandMockProvider { mode }),
            repository,
            encryption_service,
        );
        let db = test_db.db.clone();
        // Return the TestDatabase guard so the caller keeps it alive for the whole
        // test; its Drop reaps the dedicated schema afterwards.
        Some((service, db, test_db))
    }

    #[tokio::test]
    async fn test_provision_on_demand_success_sets_active_and_writes_attempt() {
        let Some((service, db, _guard)) = on_demand_service(OnDemandMockMode::ImmediateCert).await
        else {
            return;
        };
        let hostname = "app.1-2-3-4.sslip.io";

        service
            .provision_on_demand(hostname, "ops@example.com")
            .await
            .expect("on-demand issuance should succeed via cached-auth fast path");

        // domains row is active with cert material populated.
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(hostname))
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("domains row should exist after issuance");
        assert_eq!(domain.status, "active");
        assert!(domain.certificate.is_some());
        assert!(domain.private_key.is_some());
        assert!(domain.on_demand_backoff_until.is_none());

        // An "issued" audit attempt was written.
        let attempt = service
            .latest_on_demand_attempt(hostname)
            .await
            .unwrap()
            .expect("an audit attempt should be written");
        assert_eq!(attempt.outcome, "issued");
        assert_eq!(attempt.trigger, "tls_callback");
        assert_eq!(attempt.challenge_served, Some(true));
        assert_eq!(attempt.acme_request_sent, Some(true));
        assert!(attempt.error_chain.is_none());
        assert!(attempt.error_category.is_none());
    }

    #[tokio::test]
    async fn test_provision_on_demand_failure_sets_backoff_and_failed_attempt() {
        let Some((service, db, _guard)) = on_demand_service(OnDemandMockMode::Fail(
            "challenge failed: incorrect key authorization".to_string(),
        ))
        .await
        else {
            return;
        };
        let hostname = "broken.1-2-3-4.sslip.io";

        let err = service
            .provision_on_demand(hostname, "ops@example.com")
            .await
            .expect_err("on-demand issuance should fail");
        assert!(matches!(
            err,
            DomainServiceError::OnDemandIssuanceFailed { .. }
                | DomainServiceError::Provider(_)
                | DomainServiceError::Challenge(_)
        ));

        // domains row is on_demand_failed with a backoff window and recorded error.
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(hostname))
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("domains row should exist after a failed attempt");
        assert_eq!(domain.status, "on_demand_failed");
        assert!(
            domain.on_demand_backoff_until.is_some(),
            "failure must arm the negative-cache backoff"
        );
        // First failure uses the first ladder rung (5 minutes).
        let backoff = domain.on_demand_backoff_until.unwrap();
        let mins = (backoff - Utc::now()).num_minutes();
        assert!(
            (3..=6).contains(&mins),
            "first backoff should be ~5 minutes, got {mins}"
        );
        assert!(domain.last_error.is_some());
        assert_eq!(
            domain.last_error_type.as_deref(),
            Some("challenge_mismatch")
        );

        // A "failed" audit attempt with the full error chain was written.
        let attempt = service
            .latest_on_demand_attempt(hostname)
            .await
            .unwrap()
            .expect("a failed audit attempt should be written");
        assert_eq!(attempt.outcome, "failed");
        assert_eq!(
            attempt.error_category.as_deref(),
            Some("challenge_mismatch")
        );
        assert!(attempt
            .error_chain
            .as_deref()
            .unwrap_or_default()
            .contains("incorrect key authorization"));
    }

    #[tokio::test]
    async fn test_provision_on_demand_rate_limited_mapping() {
        // LE rateLimited problem, flattened to a string at the provider boundary,
        // with an explicit Retry-After window in the detail.
        let le_detail = "API error: too many certificates already issued, retry after 2099-01-02T03:04:05Z (urn:ietf:params:acme:error:rateLimited)";
        let Some((service, db, _guard)) =
            on_demand_service(OnDemandMockMode::Fail(le_detail.to_string())).await
        else {
            return;
        };
        let hostname = "limited.1-2-3-4.sslip.io";

        let err = service
            .provision_on_demand(hostname, "ops@example.com")
            .await
            .expect_err("rate-limited issuance should fail");
        match err {
            DomainServiceError::OnDemandRateLimited {
                retry_after,
                hostname: h,
                ..
            } => {
                assert_eq!(h, hostname);
                let ra = retry_after.expect("Retry-After should be parsed from the LE detail");
                assert_eq!(ra.year(), 2099);
            }
            other => panic!("expected OnDemandRateLimited, got {other:?}"),
        }

        // domains row carries the rate_limited category and a backoff at the
        // parsed Retry-After deadline (far in the future).
        let domain = domains::Entity::find()
            .filter(domains::Column::Domain.eq(hostname))
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("domains row should exist");
        assert_eq!(domain.status, "on_demand_failed");
        assert_eq!(domain.last_error_type.as_deref(), Some("rate_limited"));
        let backoff = domain
            .on_demand_backoff_until
            .expect("rate limit must arm the backoff");
        assert_eq!(backoff.year(), 2099);

        let attempt = service
            .latest_on_demand_attempt(hostname)
            .await
            .unwrap()
            .expect("a rate-limited audit attempt should be written");
        assert_eq!(attempt.outcome, "failed");
        assert_eq!(attempt.error_category.as_deref(), Some("rate_limited"));
        assert_eq!(
            attempt.acme_response_status.as_deref(),
            Some("urn:ietf:params:acme:error:rateLimited")
        );
    }

    #[tokio::test]
    async fn test_list_on_demand_attempts_joins_domain_state_newest_first() {
        // Issue one success and one failure, then assert the list endpoint feeder
        // returns both newest-first, each joined with its current domains row.
        let Some((service, _db, _guard)) = on_demand_service(OnDemandMockMode::ImmediateCert).await
        else {
            return;
        };
        let ok_host = "ok.1-2-3-4.sslip.io";
        service
            .provision_on_demand(ok_host, "ops@example.com")
            .await
            .expect("success issuance");

        let (rows, total) = service
            .list_on_demand_attempts(1, 20)
            .await
            .expect("list should succeed");
        assert_eq!(total, 1, "exactly one attempt was written");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.attempt.hostname, ok_host);
        assert_eq!(row.attempt.outcome, "issued");
        // Current authoritative state is joined from the domains row.
        let domain = row.domain.as_ref().expect("domains row should be joined");
        assert_eq!(domain.status, "active");
        assert!(domain.on_demand_backoff_until.is_none());
    }

    #[tokio::test]
    async fn test_list_on_demand_attempts_empty_returns_zero() {
        let Some((service, _db, _guard)) = on_demand_service(OnDemandMockMode::ImmediateCert).await
        else {
            return;
        };
        let (rows, total) = service
            .list_on_demand_attempts(1, 20)
            .await
            .expect("list should succeed on an empty table");
        assert_eq!(total, 0);
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_on_demand_cert_status_for_failed_host() {
        let Some((service, _db, _guard)) = on_demand_service(OnDemandMockMode::Fail(
            "challenge failed: incorrect key authorization".to_string(),
        ))
        .await
        else {
            return;
        };
        let hostname = "status.1-2-3-4.sslip.io";
        let _ = service
            .provision_on_demand(hostname, "ops@example.com")
            .await;

        let status = service
            .on_demand_cert_status(hostname)
            .await
            .expect("status query should succeed");
        let domain = status.domain.expect("a domains row should exist");
        assert_eq!(domain.status, "on_demand_failed");
        assert!(domain.on_demand_backoff_until.is_some());
        let attempt = status.latest_attempt.expect("an attempt should exist");
        assert_eq!(attempt.outcome, "failed");
        assert_eq!(
            attempt.error_category.as_deref(),
            Some("challenge_mismatch")
        );
    }

    #[tokio::test]
    async fn test_on_demand_cert_status_unknown_host_is_empty_not_error() {
        let Some((service, _db, _guard)) = on_demand_service(OnDemandMockMode::ImmediateCert).await
        else {
            return;
        };
        // A hostname with no domains row and no attempts must return an empty
        // (but Ok) status so the CLI/UI can render "no attempts recorded".
        let status = service
            .on_demand_cert_status("never-seen.1-2-3-4.sslip.io")
            .await
            .expect("status query should succeed for an unknown host");
        assert!(status.domain.is_none());
        assert!(status.latest_attempt.is_none());
    }

    // ---- Pure unit tests (no DB) for the on-demand helpers ----

    #[test]
    fn test_next_backoff_delay_climbs_the_ladder() {
        let set_at = Utc::now();
        // No previous backoff → first rung (5m).
        assert_eq!(
            DomainService::next_backoff_delay(None, set_at).num_minutes(),
            5
        );
        // Previous was 5m → 15m.
        assert_eq!(
            DomainService::next_backoff_delay(Some(set_at + Duration::minutes(5)), set_at)
                .num_minutes(),
            15
        );
        // Previous was 15m → 1h.
        assert_eq!(
            DomainService::next_backoff_delay(Some(set_at + Duration::minutes(15)), set_at)
                .num_minutes(),
            60
        );
        // Previous was 1h → 4h.
        assert_eq!(
            DomainService::next_backoff_delay(Some(set_at + Duration::minutes(60)), set_at)
                .num_minutes(),
            240
        );
        // Previous was 4h → 24h.
        assert_eq!(
            DomainService::next_backoff_delay(Some(set_at + Duration::minutes(240)), set_at)
                .num_minutes(),
            1440
        );
        // Previous was 24h → stays capped at 24h.
        assert_eq!(
            DomainService::next_backoff_delay(Some(set_at + Duration::minutes(1440)), set_at)
                .num_minutes(),
            1440
        );
    }

    #[test]
    fn test_categorize_on_demand_error_rate_limited_fallback_now_plus_1h() {
        let err = DomainServiceError::OnDemandRateLimited {
            hostname: "x.sslip.io".to_string(),
            detail: "rate limited".to_string(),
            retry_after: None,
        };
        let (category, deadline) = DomainService::categorize_on_demand_error(&err);
        assert_eq!(category, "rate_limited");
        let mins = (deadline.unwrap() - Utc::now()).num_minutes();
        assert!(
            (55..=65).contains(&mins),
            "fallback should be ~1h, got {mins}"
        );
    }

    #[test]
    fn test_categorize_on_demand_error_categories() {
        let cases = [
            ("DNS lookup failed for host", "dns_failure"),
            ("ACME order has expired", "acme_order_expired"),
            ("Order validation timed out after 6 attempts", "timeout"),
            (
                "challenge failed: incorrect key authorization",
                "challenge_mismatch",
            ),
            ("connection reset by peer", "internal"),
        ];
        for (msg, expected) in cases {
            let err =
                DomainServiceError::Provider(crate::tls::ProviderError::Acme(msg.to_string()));
            let (category, _) = DomainService::categorize_on_demand_error(&err);
            assert_eq!(category, expected, "message: {msg}");
        }
    }

    #[test]
    fn test_error_chain_string_walks_sources() {
        // DomainServiceError::Provider wraps ProviderError via #[from]; the chain
        // string must surface the inner ACME detail.
        let err = DomainServiceError::Provider(crate::tls::ProviderError::Acme(
            "boom from let's encrypt".to_string(),
        ));
        let chain = error_chain_string(&err);
        assert!(chain.contains("boom from let's encrypt"), "chain: {chain}");
    }

    #[test]
    fn test_parse_retry_after() {
        assert_eq!(
            parse_retry_after("retry after 2026-06-25T00:00:00Z").map(|d| d.year()),
            Some(2026)
        );
        assert_eq!(
            parse_retry_after(
                "too many certs (urn:ietf:params:acme:error:rateLimited), retry after 2030-12-31T23:59:59Z"
            )
            .map(|d| d.year()),
            Some(2030)
        );
        // No parseable timestamp → None (caller falls back to now+1h).
        assert!(parse_retry_after("rate limited, try again later").is_none());
        assert!(parse_retry_after("no retry hint here").is_none());
    }

    #[tokio::test]
    async fn test_domain_validation() {
        // Create a test database
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let encryption_service = Arc::new(
            EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let repository = Arc::new(crate::tls::repository::DefaultCertificateRepository::new(
            test_db.db.clone(),
            encryption_service.clone(),
        ));
        let service = DomainService::new(
            test_db.db.clone(),
            Arc::new(MockProvider),
            repository,
            encryption_service,
        );

        // Valid domains
        assert!(service.is_valid_domain("example.com"));
        assert!(service.is_valid_domain("subdomain.example.com"));
        assert!(service.is_valid_domain("*.example.com"));
        assert!(service.is_valid_domain("test-site.example.co.uk"));

        // Invalid domains
        assert!(!service.is_valid_domain(""));
        assert!(!service.is_valid_domain(".example.com"));
        assert!(!service.is_valid_domain("example.com."));
        assert!(!service.is_valid_domain("example"));
        assert!(!service.is_valid_domain("-example.com"));
        assert!(!service.is_valid_domain("example-.com"));
    }

    /// Build a minimal domain model for `fallback_status_for` tests, varying only
    /// the certificate-related fields that drive the decision.
    fn domain_with_cert(
        certificate: Option<&str>,
        private_key: Option<&str>,
        expiration_time: Option<chrono::DateTime<Utc>>,
    ) -> domains::Model {
        let now = Utc::now();
        domains::Model {
            id: 1,
            domain: "example.com".to_string(),
            certificate: certificate.map(|s| s.to_string()),
            private_key: private_key.map(|s| s.to_string()),
            expiration_time,
            last_renewed: None,
            status: "challenge_requested".to_string(),
            dns_challenge_token: None,
            dns_challenge_value: None,
            http_challenge_token: None,
            http_challenge_key_authorization: None,
            last_error: None,
            last_error_type: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            on_demand_backoff_until: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn test_fallback_status_preserves_active_for_valid_cert() {
        // Present cert + key, expires in the future → stay "active" so the proxy
        // keeps serving the live certificate.
        let domain = domain_with_cert(
            Some("-----CERT-----"),
            Some("-----KEY-----"),
            Some(Utc::now() + chrono::Duration::days(30)),
        );
        assert_eq!(DomainService::fallback_status_for(&domain), "active");
    }

    #[test]
    fn test_fallback_status_pending_for_expired_cert() {
        // Cert material present but already expired → "pending" (nothing usable to serve).
        let domain = domain_with_cert(
            Some("-----CERT-----"),
            Some("-----KEY-----"),
            Some(Utc::now() - chrono::Duration::days(1)),
        );
        assert_eq!(DomainService::fallback_status_for(&domain), "pending");
    }

    #[test]
    fn test_fallback_status_pending_when_no_cert() {
        // No cert at all (fresh domain whose first order was cancelled) → "pending".
        let domain = domain_with_cert(None, None, None);
        assert_eq!(DomainService::fallback_status_for(&domain), "pending");
    }

    #[test]
    fn test_fallback_status_pending_when_cert_without_expiry() {
        // Cert/key present but no expiration recorded → can't prove validity → "pending".
        let domain = domain_with_cert(Some("-----CERT-----"), Some("-----KEY-----"), None);
        assert_eq!(DomainService::fallback_status_for(&domain), "pending");
    }

    #[test]
    fn test_fallback_status_pending_for_partial_material() {
        // Cert without key (or empty strings) is not usable → "pending".
        let cert_only = domain_with_cert(
            Some("-----CERT-----"),
            None,
            Some(Utc::now() + chrono::Duration::days(30)),
        );
        assert_eq!(DomainService::fallback_status_for(&cert_only), "pending");

        let empty_strings = domain_with_cert(
            Some(""),
            Some(""),
            Some(Utc::now() + chrono::Duration::days(30)),
        );
        assert_eq!(
            DomainService::fallback_status_for(&empty_strings),
            "pending"
        );
    }
}
