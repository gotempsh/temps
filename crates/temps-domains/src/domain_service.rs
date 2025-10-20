use anyhow::Result;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use std::sync::Arc;
use temps_entities::domains;
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
            challenge_type.clone(), domain_name, challenge.validation_url
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

                // Update domain with error status
                let mut domain_active: domains::ActiveModel = domain.into();
                domain_active.status = Set("failed".to_string());
                domain_active.last_error = Set(Some(e.to_string()));
                domain_active.last_error_type = Set(Some("challenge_completion".to_string()));

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

        // Reset domain status to pending and clear challenge fields
        let mut domain_active: domains::ActiveModel = domain.into();
        domain_active.status = Set("pending".to_string());
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

    fn is_valid_domain(&self, domain: &str) -> bool {
        // Basic domain validation
        if domain.is_empty() || domain.len() > 253 {
            return false;
        }

        // Allow wildcard domains
        let domain_to_check = if domain.starts_with("*.") {
            &domain[2..]
        } else {
            domain
        };

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

#[cfg(test)]
mod tests {
    use temps_core::EncryptionService;

    use super::*;
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

    #[tokio::test]
    async fn test_domain_validation() {
        // Create a test database
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let encryption_service = Arc::new(EncryptionService::new("0000000000000000000000000000000000000000000000000000000000000000").unwrap());
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
}
