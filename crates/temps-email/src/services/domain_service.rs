//! Domain service for managing email sending domains

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use std::sync::Arc;
use temps_entities::email_domains;
use tracing::{debug, error, info};

use crate::errors::EmailError;
use crate::providers::{DnsRecord, DnsRecordStatus, DomainIdentityDetails, VerificationStatus};
use crate::services::ProviderService;

/// Service for managing email domains
#[derive(Clone)]
pub struct DomainService {
    db: Arc<DatabaseConnection>,
    provider_service: Arc<ProviderService>,
}

/// Request to create a new email domain
#[derive(Debug, Clone)]
pub struct CreateDomainRequest {
    pub provider_id: i32,
    pub domain: String,
}

/// Domain with DNS records for display
#[derive(Debug, Clone)]
pub struct DomainWithDnsRecords {
    pub domain: email_domains::Model,
    pub dns_records: Vec<DnsRecord>,
}

impl DomainService {
    pub fn new(db: Arc<DatabaseConnection>, provider_service: Arc<ProviderService>) -> Self {
        Self {
            db,
            provider_service,
        }
    }

    /// Create a new email domain and register it with the provider
    pub async fn create(
        &self,
        request: CreateDomainRequest,
    ) -> Result<DomainWithDnsRecords, EmailError> {
        debug!(
            "Creating email domain: {} for provider: {}",
            request.domain, request.provider_id
        );

        // Get the provider
        let provider = self.provider_service.get(request.provider_id).await?;

        // Create provider instance
        let provider_instance = self
            .provider_service
            .create_provider_instance(&provider)
            .await?;

        // Register domain with the provider
        let identity = provider_instance
            .create_identity(&request.domain)
            .await
            .map_err(|e| {
                error!("Failed to create domain identity: {}", e);
                e
            })?;

        // Build DNS records list for response
        let mut dns_records = Vec::new();

        if let Some(spf) = &identity.spf_record {
            dns_records.push(spf.clone());
        }

        dns_records.extend(identity.dkim_records.clone());

        if let Some(mx) = &identity.mx_record {
            dns_records.push(mx.clone());
        }

        // Store domain in database
        let domain = email_domains::ActiveModel {
            provider_id: Set(request.provider_id),
            domain: Set(request.domain.clone()),
            status: Set("pending".to_string()),
            spf_record_name: Set(identity.spf_record.as_ref().map(|r| r.name.clone())),
            spf_record_value: Set(identity.spf_record.as_ref().map(|r| r.value.clone())),
            dkim_selector: Set(identity.dkim_selector.clone()),
            dkim_record_name: Set(identity.dkim_records.first().map(|r| r.name.clone())),
            dkim_record_value: Set(identity.dkim_records.first().map(|r| r.value.clone())),
            mx_record_name: Set(identity.mx_record.as_ref().map(|r| r.name.clone())),
            mx_record_value: Set(identity.mx_record.as_ref().map(|r| r.value.clone())),
            mx_record_priority: Set(identity
                .mx_record
                .as_ref()
                .and_then(|r| r.priority.map(|p| p as i16))),
            provider_identity_id: Set(Some(identity.provider_identity_id)),
            ..Default::default()
        };

        let result = domain.insert(self.db.as_ref()).await?;

        info!(
            "Created email domain {} with id: {}",
            request.domain, result.id
        );

        Ok(DomainWithDnsRecords {
            domain: result,
            dns_records,
        })
    }

    /// Get a domain by ID
    pub async fn get(&self, id: i32) -> Result<email_domains::Model, EmailError> {
        email_domains::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(EmailError::DomainNotFound(id))
    }

    /// Find a domain by domain name
    pub async fn find_by_domain_name(
        &self,
        domain_name: &str,
    ) -> Result<Option<email_domains::Model>, EmailError> {
        let domain = email_domains::Entity::find()
            .filter(email_domains::Column::Domain.eq(domain_name))
            .one(self.db.as_ref())
            .await?;

        Ok(domain)
    }

    /// Get a domain with its DNS records (fetches fresh verification status from provider)
    /// The domain status is computed dynamically based on DNS record verification
    pub async fn get_with_dns_records(&self, id: i32) -> Result<DomainWithDnsRecords, EmailError> {
        let mut domain = self.get(id).await?;

        // Get the provider to fetch fresh verification status
        let provider = self.provider_service.get(domain.provider_id).await?;

        // Create provider instance
        let provider_instance = self
            .provider_service
            .create_provider_instance(&provider)
            .await?;

        // Fetch fresh DNS records with verification status from the provider API
        let details = provider_instance
            .get_identity_details(&domain.domain)
            .await
            .map_err(|e| {
                error!(
                    "Failed to get identity details from provider, falling back to stored data: {}",
                    e
                );
                e
            });

        // Build DNS records and compute status based on verification results
        let (dns_records, computed_status) = match details {
            Ok(identity_details) => {
                let mut records = Vec::new();

                if let Some(spf) = identity_details.spf_record.clone() {
                    records.push(spf);
                }

                records.extend(identity_details.dkim_records.clone());

                if let Some(mx) = identity_details.mx_record.clone() {
                    records.push(mx);
                }

                // Compute status based on all DNS records being verified
                let all_verified = Self::are_all_records_verified(&identity_details);
                let status = if all_verified {
                    "verified".to_string()
                } else {
                    // Check if any records failed
                    let any_failed = records.iter().any(|r| r.status == DnsRecordStatus::Failed);
                    if any_failed {
                        "failed".to_string()
                    } else {
                        "pending".to_string()
                    }
                };

                (records, status)
            }
            Err(_) => {
                // Fallback to stored data without status information
                (self.build_dns_records(&domain), domain.status.clone())
            }
        };

        // Override the domain status with computed status
        domain.status = computed_status;

        Ok(DomainWithDnsRecords {
            domain,
            dns_records,
        })
    }

    /// Check if all DNS records are verified
    fn are_all_records_verified(details: &DomainIdentityDetails) -> bool {
        // Check SPF record
        if let Some(ref spf) = details.spf_record {
            if spf.status != DnsRecordStatus::Verified {
                return false;
            }
        }

        // Check DKIM records
        for dkim in &details.dkim_records {
            if dkim.status != DnsRecordStatus::Verified {
                return false;
            }
        }

        // Check MX record (if present)
        if let Some(ref mx) = details.mx_record {
            if mx.status != DnsRecordStatus::Verified {
                return false;
            }
        }

        true
    }

    /// List all domains
    pub async fn list(&self) -> Result<Vec<email_domains::Model>, EmailError> {
        let domains = email_domains::Entity::find()
            .order_by_desc(email_domains::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(domains)
    }

    /// List domains by provider
    pub async fn list_by_provider(
        &self,
        provider_id: i32,
    ) -> Result<Vec<email_domains::Model>, EmailError> {
        let domains = email_domains::Entity::find()
            .filter(email_domains::Column::ProviderId.eq(provider_id))
            .order_by_desc(email_domains::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(domains)
    }

    /// List verified domains
    pub async fn list_verified(&self) -> Result<Vec<email_domains::Model>, EmailError> {
        let domains = email_domains::Entity::find()
            .filter(email_domains::Column::Status.eq("verified"))
            .order_by_desc(email_domains::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(domains)
    }

    /// Verify a domain's DNS configuration and return the domain with DNS records
    pub async fn verify(&self, id: i32) -> Result<DomainWithDnsRecords, EmailError> {
        let domain = self.get(id).await?;

        debug!("Verifying domain: {}", domain.domain);

        // Get the provider
        let provider = self.provider_service.get(domain.provider_id).await?;

        // Create provider instance
        let provider_instance = self
            .provider_service
            .create_provider_instance(&provider)
            .await?;

        // Get identity details with DNS verification
        let identity_details = provider_instance
            .get_identity_details(&domain.domain)
            .await
            .map_err(|e| {
                error!("Failed to get identity details: {}", e);
                e
            })?;

        // Build DNS records list for response
        let mut dns_records = Vec::new();

        if let Some(spf) = &identity_details.spf_record {
            dns_records.push(spf.clone());
        }

        dns_records.extend(identity_details.dkim_records.clone());

        if let Some(mx) = &identity_details.mx_record {
            dns_records.push(mx.clone());
        }

        // Check if all DNS records are verified
        let all_dns_verified = Self::are_all_records_verified(&identity_details);

        // Check if any records failed
        let any_failed = dns_records
            .iter()
            .any(|r| r.status == DnsRecordStatus::Failed);

        // Determine final status based on DNS record verification
        let status = if all_dns_verified {
            debug!("All DNS records verified via DNS lookup, marking domain as verified");
            VerificationStatus::Verified
        } else if any_failed {
            VerificationStatus::Failed("Some DNS records failed verification".to_string())
        } else {
            // Use the provider's overall status for pending/other states
            identity_details.overall_status
        };

        // Update domain status in database
        let mut active_model: email_domains::ActiveModel = domain.into();

        match &status {
            VerificationStatus::Verified => {
                active_model.status = Set("verified".to_string());
                active_model.last_verified_at = Set(Some(chrono::Utc::now()));
                active_model.verification_error = Set(None);
                info!("Domain verified successfully");
            }
            VerificationStatus::Pending => {
                active_model.status = Set("pending".to_string());
                active_model.verification_error = Set(None);
                debug!("Domain verification pending");
            }
            VerificationStatus::Failed(msg) => {
                active_model.status = Set("failed".to_string());
                active_model.verification_error = Set(Some(msg.clone()));
                error!("Domain verification failed: {}", msg);
            }
            VerificationStatus::NotStarted => {
                active_model.status = Set("not_started".to_string());
                active_model.verification_error = Set(None);
            }
            VerificationStatus::TemporaryFailure => {
                active_model.status = Set("temporary_failure".to_string());
                active_model.verification_error =
                    Set(Some("DNS records no longer valid".to_string()));
            }
        }

        let result = active_model.update(self.db.as_ref()).await?;

        Ok(DomainWithDnsRecords {
            domain: result,
            dns_records,
        })
    }

    /// Delete a domain
    pub async fn delete(&self, id: i32) -> Result<(), EmailError> {
        let domain = self.get(id).await?;

        debug!("Deleting domain: {}", domain.domain);

        // Get the provider
        let provider = self.provider_service.get(domain.provider_id).await?;

        // Create provider instance
        let provider_instance = self
            .provider_service
            .create_provider_instance(&provider)
            .await?;

        // Delete from provider (ignore errors - domain might already be deleted)
        if let Err(e) = provider_instance.delete_identity(&domain.domain).await {
            error!(
                "Failed to delete domain from provider (continuing anyway): {}",
                e
            );
        }

        // Delete from database
        email_domains::Entity::delete_by_id(domain.id)
            .exec(self.db.as_ref())
            .await?;

        info!("Deleted email domain: {}", domain.domain);

        Ok(())
    }

    /// Build DNS records from stored domain data (fallback when provider API unavailable)
    fn build_dns_records(&self, domain: &email_domains::Model) -> Vec<DnsRecord> {
        let mut records = Vec::new();

        // SPF record
        if let (Some(name), Some(value)) = (&domain.spf_record_name, &domain.spf_record_value) {
            records.push(DnsRecord {
                record_type: "TXT".to_string(),
                name: name.clone(),
                value: value.clone(),
                priority: None,
                status: DnsRecordStatus::Unknown,
            });
        }

        // DKIM record
        if let (Some(name), Some(value)) = (&domain.dkim_record_name, &domain.dkim_record_value) {
            records.push(DnsRecord {
                record_type: "TXT".to_string(),
                name: name.clone(),
                value: value.clone(),
                priority: None,
                status: DnsRecordStatus::Unknown,
            });
        }

        // MX record
        if let (Some(name), Some(value)) = (&domain.mx_record_name, &domain.mx_record_value) {
            records.push(DnsRecord {
                record_type: "MX".to_string(),
                name: name.clone(),
                value: value.clone(),
                priority: domain.mx_record_priority.map(|p| p as u16),
                status: DnsRecordStatus::Unknown,
            });
        }

        records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{DnsRecordStatus, EmailProviderType, SesCredentials};
    use crate::services::provider_service::{CreateProviderRequest, ProviderCredentials};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::email_providers;

    // Helper to create a test encryption service
    fn create_test_encryption_service() -> Arc<EncryptionService> {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(EncryptionService::new(key).unwrap())
    }

    // Helper to setup test environment with real database
    async fn setup_test_env() -> (TestDatabase, DomainService, ProviderService) {
        let db = TestDatabase::with_migrations().await.unwrap();
        let encryption_service = create_test_encryption_service();
        let provider_service = ProviderService::new(db.db.clone(), encryption_service);
        let domain_service = DomainService::new(db.db.clone(), Arc::new(provider_service.clone()));
        (db, domain_service, provider_service)
    }

    // Helper to create a test provider
    async fn create_test_provider(service: &ProviderService) -> email_providers::Model {
        let request = CreateProviderRequest {
            name: format!("Test Provider {}", uuid::Uuid::new_v4()),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        service.create(request).await.unwrap()
    }

    // ========== Unit Tests (no database required) ==========

    #[test]
    fn test_create_domain_request() {
        let request = CreateDomainRequest {
            provider_id: 1,
            domain: "example.com".to_string(),
        };

        assert_eq!(request.provider_id, 1);
        assert_eq!(request.domain, "example.com");
    }

    #[test]
    fn test_dns_record_creation() {
        let record = DnsRecord {
            record_type: "TXT".to_string(),
            name: "_dmarc.example.com".to_string(),
            value: "v=DMARC1; p=none".to_string(),
            priority: None,
            status: DnsRecordStatus::Unknown,
        };

        assert_eq!(record.record_type, "TXT");
        assert_eq!(record.name, "_dmarc.example.com");
        assert!(record.priority.is_none());
        assert_eq!(record.status, DnsRecordStatus::Unknown);
    }

    #[test]
    fn test_mx_record_with_priority() {
        let record = DnsRecord {
            record_type: "MX".to_string(),
            name: "example.com".to_string(),
            value: "mail.example.com".to_string(),
            priority: Some(10),
            status: DnsRecordStatus::Verified,
        };

        assert_eq!(record.record_type, "MX");
        assert_eq!(record.priority, Some(10));
        assert_eq!(record.status, DnsRecordStatus::Verified);
    }

    #[test]
    fn test_verification_status_variants() {
        let verified = VerificationStatus::Verified;
        let pending = VerificationStatus::Pending;
        let failed = VerificationStatus::Failed("DNS not found".to_string());
        let not_started = VerificationStatus::NotStarted;
        let temp_fail = VerificationStatus::TemporaryFailure;

        // Test display formatting (lowercase with underscores)
        assert_eq!(format!("{}", verified), "verified");
        assert_eq!(format!("{}", pending), "pending");
        assert_eq!(format!("{}", failed), "failed");
        assert_eq!(format!("{}", not_started), "not_started");
        assert_eq!(format!("{}", temp_fail), "temporary_failure");
    }

    #[test]
    fn test_domain_with_dns_records_struct() {
        let now = chrono::Utc::now();
        let domain = email_domains::Model {
            id: 1,
            provider_id: 1,
            domain: "test.com".to_string(),
            status: "verified".to_string(),
            spf_record_name: None,
            spf_record_value: None,
            dkim_selector: None,
            dkim_record_name: None,
            dkim_record_value: None,
            mx_record_name: None,
            mx_record_value: None,
            mx_record_priority: None,
            provider_identity_id: None,
            last_verified_at: None,
            verification_error: None,
            created_at: now,
            updated_at: now,
        };

        let dns_records = vec![DnsRecord {
            record_type: "TXT".to_string(),
            name: "test.com".to_string(),
            value: "v=spf1 -all".to_string(),
            priority: None,
            status: DnsRecordStatus::Pending,
        }];

        let domain_with_records = DomainWithDnsRecords {
            domain: domain.clone(),
            dns_records: dns_records.clone(),
        };

        assert_eq!(domain_with_records.domain.id, 1);
        assert_eq!(domain_with_records.dns_records.len(), 1);
    }

    #[test]
    fn test_build_dns_records() {
        let encryption_service = create_test_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let provider_service = Arc::new(ProviderService::new(db.clone(), encryption_service));
        let service = DomainService::new(db, provider_service);

        let now = chrono::Utc::now();
        let domain = email_domains::Model {
            id: 1,
            provider_id: 1,
            domain: "example.com".to_string(),
            status: "verified".to_string(),
            spf_record_name: Some("example.com".to_string()),
            spf_record_value: Some("v=spf1 include:amazonses.com ~all".to_string()),
            dkim_selector: Some("ses".to_string()),
            dkim_record_name: Some("ses._domainkey.example.com".to_string()),
            dkim_record_value: Some("v=DKIM1; k=rsa; p=PUBLICKEY".to_string()),
            mx_record_name: Some("example.com".to_string()),
            mx_record_value: Some("feedback-smtp.us-east-1.amazonses.com".to_string()),
            mx_record_priority: Some(10),
            provider_identity_id: Some("identity-123".to_string()),
            last_verified_at: Some(now),
            verification_error: None,
            created_at: now,
            updated_at: now,
        };

        let records = service.build_dns_records(&domain);

        assert_eq!(records.len(), 3); // SPF, DKIM, MX

        // Verify SPF record
        let spf = records
            .iter()
            .find(|r| r.name == "example.com" && r.record_type == "TXT");
        assert!(spf.is_some());
        assert!(spf.unwrap().value.contains("spf1"));

        // Verify DKIM record
        let dkim = records.iter().find(|r| r.name.contains("_domainkey"));
        assert!(dkim.is_some());

        // Verify MX record
        let mx = records.iter().find(|r| r.record_type == "MX");
        assert!(mx.is_some());
        assert_eq!(mx.unwrap().priority, Some(10));
    }

    // ========== Integration Tests (require Docker) ==========

    #[tokio::test]
    async fn test_get_domain_not_found() {
        let (_db, domain_service, _provider_service) = setup_test_env().await;

        let result = domain_service.get(999999).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmailError::DomainNotFound(999999)
        ));
    }

    #[tokio::test]
    async fn test_list_domains_empty() {
        let (_db, domain_service, _provider_service) = setup_test_env().await;

        let result = domain_service.list().await;

        assert!(result.is_ok());
        let domains = result.unwrap();
        assert!(domains.is_empty());
    }

    #[tokio::test]
    async fn test_list_verified_domains_empty() {
        let (_db, domain_service, _provider_service) = setup_test_env().await;

        let result = domain_service.list_verified().await;

        assert!(result.is_ok());
        let domains = result.unwrap();
        assert!(domains.is_empty());
    }

    #[tokio::test]
    async fn test_list_by_provider_empty() {
        let (_db, domain_service, provider_service) = setup_test_env().await;

        // Create a provider
        let provider = create_test_provider(&provider_service).await;

        // List domains for that provider (should be empty)
        let result = domain_service.list_by_provider(provider.id).await;

        assert!(result.is_ok());
        let domains = result.unwrap();
        assert!(domains.is_empty());
    }

    #[tokio::test]
    async fn test_provider_exists_check() {
        let (_db, _domain_service, provider_service) = setup_test_env().await;

        // Create a provider
        let provider = create_test_provider(&provider_service).await;

        // Verify provider exists
        let result = provider_service.get(provider.id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, provider.id);
    }
}
