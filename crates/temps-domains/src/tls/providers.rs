use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use instant_acme::{
    Account, AccountCredentials, ChallengeType as AcmeChallengeType, Identifier, NewAccount,
    NewOrder, Order, OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use serde_json;
use std::sync::Arc;
use temps_core::UtcDateTime;
use tracing::{debug, error, info};

use super::errors::ProviderError;
use super::models::*;
use super::repository::CertificateRepository;

#[async_trait]
pub trait CertificateProvider: Send + Sync {
    async fn provision(
        &self,
        domain: &str,
        challenge: ChallengeType,
        email: &str,
    ) -> Result<ProvisioningResult, ProviderError>;

    async fn complete_challenge(
        &self,
        domain: &str,
        challenge_data: &ChallengeData,
        email: &str,
    ) -> Result<Certificate, ProviderError>;

    fn supported_challenges(&self) -> Vec<ChallengeType>;

    async fn validate_prerequisites(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<ValidationResult, ProviderError>;

    /// Cancel an existing ACME order for a domain
    /// This allows you to abandon a failed order and create a new one
    async fn cancel_order(&self, domain: &str) -> Result<(), ProviderError>;
}

pub struct LetsEncryptProvider {
    repository: Arc<dyn CertificateRepository>,
    environment: String,
}

impl LetsEncryptProvider {
    pub fn new(repository: Arc<dyn CertificateRepository>) -> Self {
        // Read environment from LETSENCRYPT_MODE env var, default to "production"
        let environment =
            std::env::var("LETSENCRYPT_MODE").unwrap_or_else(|_| "production".to_string());

        Self {
            repository,
            environment,
        }
    }

    fn get_acme_url(&self) -> String {
        // Allow custom ACME directory URL for testing (e.g., Pebble)
        if let Ok(custom_url) = std::env::var("ACME_DIRECTORY_URL") {
            return custom_url;
        }

        if self.environment == "production" {
            instant_acme::LetsEncrypt::Production.url().to_string()
        } else {
            instant_acme::LetsEncrypt::Staging.url().to_string()
        }
    }

    async fn get_or_create_acme_account(
        &self,
        email: &str,
    ) -> Result<(Account, AccountCredentials), ProviderError> {
        info!(
            "Getting or creating ACME account for email: {} environment: {}",
            email, self.environment
        );

        if let Some(account) = self
            .repository
            .find_acme_account(email, &self.environment)
            .await?
        {
            let account_creds: AccountCredentials = serde_json::from_str(&account.credentials)
                .map_err(|e| {
                    ProviderError::Configuration(format!("Failed to deserialize account: {}", e))
                })?;

            let account_creds_clone = serde_json::from_str(&account.credentials).map_err(|e| {
                ProviderError::Configuration(format!("Failed to deserialize account: {}", e))
            })?;

            let acme_account = Account::from_credentials(account_creds)
                .await
                .map_err(|e| ProviderError::Acme(format!("Failed to load account: {}", e)))?;

            Ok((acme_account, account_creds_clone))
        } else {
            let acme_url = self.get_acme_url();
            let (acme_account, credentials) = Account::create(
                &NewAccount {
                    contact: &[format!("mailto:{}", email).as_str()],
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                &acme_url,
                None,
            )
            .await?;

            let account_creds_str = serde_json::to_string(&credentials).map_err(|e| {
                ProviderError::Configuration(format!("Failed to serialize account: {}", e))
            })?;

            let acme_account_data = AcmeAccount {
                email: email.to_string(),
                environment: self.environment.clone(),
                credentials: account_creds_str,
                created_at: Utc::now(),
            };

            self.repository.save_acme_account(acme_account_data).await?;

            Ok((acme_account, credentials))
        }
    }

    async fn generate_certificate_from_order(
        &self,
        domain: &str,
        order: &mut Order,
    ) -> Result<Certificate, ProviderError> {
        // Generate CSR
        // For wildcard domains, include both wildcard and base domain
        let names = if let Some(base_domain) = domain.strip_prefix("*.") {
            vec![domain.to_string(), base_domain.to_string()]
        } else {
            vec![domain.to_string()]
        };
        let mut params = CertificateParams::new(names)?;
        params.distinguished_name = DistinguishedName::new();

        let private_key = KeyPair::generate()?;
        let csr = params.serialize_request(&private_key)?;

        // Finalize order
        order.finalize(csr.der()).await?;

        // Wait for certificate
        let cert_chain_pem = loop {
            match order.certificate().await? {
                Some(cert) => break cert,
                None => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        };

        // Extract expiration time
        let expiration_time = self.extract_expiration_time(&cert_chain_pem)?;

        Ok(Certificate {
            id: 1,
            domain: domain.to_string(),
            certificate_pem: cert_chain_pem,
            private_key_pem: private_key.serialize_pem(),
            expiration_time,
            last_renewed: Some(Utc::now()),
            is_wildcard: domain.starts_with("*."),
            verification_method: "acme".to_string(),
            status: CertificateStatus::Active,
        })
    }

    fn extract_expiration_time(&self, cert_pem: &str) -> Result<UtcDateTime, ProviderError> {
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).map_err(|e| {
            ProviderError::CertificateGeneration(format!("Failed to parse PEM: {}", e))
        })?;

        let x509 = pem.parse_x509().map_err(|e| {
            ProviderError::CertificateGeneration(format!("Failed to parse X509: {}", e))
        })?;

        let not_after = x509.validity().not_after;

        let expiration_time = chrono::Utc
            .timestamp_opt(not_after.timestamp(), 0)
            .single()
            .ok_or_else(|| {
                ProviderError::CertificateGeneration("Invalid expiration timestamp".to_string())
            })?;

        Ok(expiration_time)
    }

    async fn handle_http_challenge(
        &self,
        domain: &str,
        order: &mut Order,
        authorizations: Vec<instant_acme::Authorization>,
    ) -> Result<ChallengeData, ProviderError> {
        let authz = authorizations.first().ok_or_else(|| {
            ProviderError::ValidationFailed("No authorizations found".to_string())
        })?;

        let challenge = authz
            .challenges
            .iter()
            .find(|c| c.r#type == AcmeChallengeType::Http01)
            .ok_or_else(|| {
                ProviderError::UnsupportedChallenge("No HTTP-01 challenge found".to_string())
            })?;

        let key_auth = order.key_authorization(challenge);

        Ok(ChallengeData {
            challenge_type: ChallengeType::Http01,
            domain: domain.to_string(),
            token: challenge.token.clone(),
            key_authorization: key_auth.as_str().to_string(),
            validation_url: Some(challenge.url.clone()),
            dns_txt_records: vec![], // No DNS records for HTTP-01
            order_url: Some(order.url().to_string()),
        })
    }

    async fn handle_dns_challenge(
        &self,
        domain: &str,
        order: &mut Order,
        authorizations: Vec<instant_acme::Authorization>,
    ) -> Result<ChallengeData, ProviderError> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use sha2::{Digest, Sha256};

        if authorizations.is_empty() {
            return Err(ProviderError::ValidationFailed(
                "No authorizations found".to_string(),
            ));
        }

        // Extract base domain for DNS record name
        let dns_record_domain = domain.strip_prefix("*.").unwrap_or(domain);

        // For wildcard domains with base domain, we'll have multiple authorizations
        // Collect ALL DNS TXT records that need to be added
        let mut dns_txt_records = Vec::new();
        let mut first_challenge_url: Option<String> = None;
        let mut first_token = String::new();
        let mut first_key_auth = String::new();

        for authz in &authorizations {
            let challenge = authz
                .challenges
                .iter()
                .find(|c| c.r#type == AcmeChallengeType::Dns01)
                .ok_or_else(|| {
                    ProviderError::UnsupportedChallenge("No DNS-01 challenge found".to_string())
                })?;

            let key_auth = order.key_authorization(challenge);

            // For DNS-01 challenges, ACME requires base64url(SHA256(key_authorization))
            let mut hasher = Sha256::new();
            hasher.update(key_auth.as_str().as_bytes());
            let hash = hasher.finalize();
            let txt_value = URL_SAFE_NO_PAD.encode(hash);

            // Add DNS TXT record with its validation URL
            dns_txt_records.push(DnsTxtRecord {
                name: format!("_acme-challenge.{}", dns_record_domain),
                value: txt_value,
                validation_url: challenge.url.clone(),
            });

            // Store first challenge details for backward compatibility
            if first_challenge_url.is_none() {
                first_challenge_url = Some(challenge.url.clone());
                first_token = challenge.token.clone();
                first_key_auth = key_auth.as_str().to_string();
            }
        }

        info!(
            "DNS-01 challenge for {}: {} TXT record(s) to add to _acme-challenge.{}",
            domain,
            dns_txt_records.len(),
            dns_record_domain
        );

        Ok(ChallengeData {
            challenge_type: ChallengeType::Dns01,
            domain: domain.to_string(),
            token: first_token,
            key_authorization: first_key_auth,
            validation_url: first_challenge_url,
            dns_txt_records,
            order_url: Some(order.url().to_string()),
        })
    }

    async fn wait_for_order_ready(&self, order: &mut Order) -> Result<(), ProviderError> {
        const MAX_ATTEMPTS: u8 = 6;
        const BASE_DELAY_SECS: u64 = 1;
        const MAX_DELAY_SECS: u64 = 30;

        for attempt in 1..=MAX_ATTEMPTS {
            // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s (capped)
            let delay_secs = std::cmp::min(
                BASE_DELAY_SECS * 2u64.pow((attempt - 1) as u32),
                MAX_DELAY_SECS,
            );
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            let state = order.refresh().await?;

            match state.status {
                OrderStatus::Ready => {
                    info!("Order is ready after {} attempt(s)", attempt);
                    return Ok(());
                }
                OrderStatus::Invalid => {
                    let error_msg = format!("Order validation failed after {} attempt(s)", attempt);
                    error!("{}", error_msg);
                    return Err(ProviderError::ChallengeFailed(error_msg));
                }
                _ => {
                    if attempt < MAX_ATTEMPTS {
                        let next_delay = std::cmp::min(
                            BASE_DELAY_SECS * 2u64.pow(attempt as u32),
                            MAX_DELAY_SECS,
                        );
                        info!(
                            "Order not ready yet (attempt {}/{}), retrying in {}s",
                            attempt, MAX_ATTEMPTS, next_delay
                        );
                    } else {
                        let error_msg =
                            format!("Order validation timed out after {} attempts", MAX_ATTEMPTS);
                        error!("{}", error_msg);
                        return Err(ProviderError::ChallengeFailed(error_msg));
                    }
                }
            }
        }

        // This should never be reached due to the loop logic, but added for completeness
        Err(ProviderError::ChallengeFailed(format!(
            "Order validation timed out after {} attempts",
            MAX_ATTEMPTS
        )))
    }
}

#[async_trait]
impl CertificateProvider for LetsEncryptProvider {
    async fn provision(
        &self,
        domain: &str,
        challenge: ChallengeType,
        email: &str,
    ) -> Result<ProvisioningResult, ProviderError> {
        info!(
            "Provisioning certificate for domain: {} using {:?} with email: {}",
            domain, challenge, email
        );

        // Wildcard domains MUST use DNS-01 challenge
        if domain.starts_with("*.") && challenge != ChallengeType::Dns01 {
            return Err(ProviderError::UnsupportedChallenge(
                format!("Wildcard domain '{}' requires DNS-01 challenge. HTTP-01 is not supported for wildcards.", domain)
            ));
        }

        // For wildcard domains, also request the base domain in the same certificate
        // e.g., if domain is "*.example.com", request both "*.example.com" and "example.com"
        let identifiers = if let Some(base_domain) = domain.strip_prefix("*.") {
            // Remove "*." prefix
            info!(
                "Requesting wildcard certificate for {} - including base domain {}",
                domain, base_domain
            );
            vec![
                Identifier::Dns(domain.to_string()),
                Identifier::Dns(base_domain.to_string()),
            ]
        } else {
            vec![Identifier::Dns(domain.to_string())]
        };

        let (acme_account, _) = self.get_or_create_acme_account(email).await?;

        let mut order = acme_account
            .new_order(&NewOrder {
                identifiers: &identifiers,
            })
            .await?;

        // Check if order is already ready (renewal case)
        if order.state().status == OrderStatus::Ready {
            info!("Order is already ready, generating certificate");
            let cert = self
                .generate_certificate_from_order(domain, &mut order)
                .await?;
            return Ok(ProvisioningResult::Certificate(cert));
        }

        let authorizations = order.authorizations().await?;

        match challenge {
            ChallengeType::Http01 => {
                let challenge_data = self
                    .handle_http_challenge(domain, &mut order, authorizations)
                    .await?;
                Ok(ProvisioningResult::Challenge(challenge_data))
            }
            ChallengeType::Dns01 => {
                let challenge_data = self
                    .handle_dns_challenge(domain, &mut order, authorizations)
                    .await?;
                Ok(ProvisioningResult::Challenge(challenge_data))
            }
        }
    }

    async fn complete_challenge(
        &self,
        domain: &str,
        challenge_data: &ChallengeData,
        email: &str,
    ) -> Result<Certificate, ProviderError> {
        debug!(
            "Completing {:?} challenge for domain: {} with email: {}",
            challenge_data.challenge_type, domain, email
        );

        let (acme_account, _) = self.get_or_create_acme_account(email).await?;

        // Load the existing order using the stored order URL
        let order_url = challenge_data.order_url.as_ref().ok_or_else(|| {
            ProviderError::Configuration("Order URL not found in challenge data".to_string())
        })?;

        debug!("Loading existing ACME order from URL: {}", order_url);
        let mut order = acme_account.order(order_url.clone()).await?;

        // Handle different challenge types
        match challenge_data.challenge_type {
            ChallengeType::Http01 => {
                // For HTTP-01, use the validation_url from challenge_data
                if let Some(validation_url) = &challenge_data.validation_url {
                    debug!(
                        "Setting HTTP-01 challenge ready for domain: {} (URL: {})",
                        domain, validation_url
                    );
                    order.set_challenge_ready(validation_url).await?;
                } else {
                    return Err(ProviderError::Configuration(
                        "HTTP-01 challenge validation URL not found".to_string(),
                    ));
                }
            }
            ChallengeType::Dns01 => {
                // For DNS-01, set ALL challenge URLs as ready (important for wildcards with multiple authorizations)
                // Each DNS TXT record has its own validation URL that must be set ready
                if challenge_data.dns_txt_records.is_empty() {
                    return Err(ProviderError::Configuration(
                        "No DNS TXT records found for DNS-01 challenge".to_string(),
                    ));
                }

                for txt_record in &challenge_data.dns_txt_records {
                    debug!(
                        "Setting challenge ready for DNS record: {} = {} (URL: {})",
                        txt_record.name, txt_record.value, txt_record.validation_url
                    );
                    order
                        .set_challenge_ready(&txt_record.validation_url)
                        .await?;
                }
            }
        }

        // Wait for validation
        self.wait_for_order_ready(&mut order).await?;

        // Generate certificate
        self.generate_certificate_from_order(domain, &mut order)
            .await
    }

    fn supported_challenges(&self) -> Vec<ChallengeType> {
        vec![ChallengeType::Http01, ChallengeType::Dns01]
    }

    async fn validate_prerequisites(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<ValidationResult, ProviderError> {
        let mut result = ValidationResult {
            is_valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        // Check if email is configured
        if email.is_empty() {
            result.is_valid = false;
            result.errors.push("ACME email not configured".to_string());
        }

        // Check domain format
        if domain.is_empty() {
            result.is_valid = false;
            result.errors.push("Domain cannot be empty".to_string());
        }

        // Warn about staging environment
        if self.environment != "production" {
            result.warnings.push(format!(
                "Using {} environment - certificates will not be trusted",
                self.environment
            ));
        }

        Ok(result)
    }

    async fn cancel_order(&self, domain: &str) -> Result<(), ProviderError> {
        info!("Canceling ACME order for domain: {}", domain);

        // Note: We can't directly access the DB from the provider, so we'll just
        // return success. The actual cancellation happens when creating a new order.
        // ACME doesn't require explicit order cancellation - orders expire after some time.

        info!(
            "ACME order cancellation requested for domain: {}. New order can be created.",
            domain
        );
        Ok(())
    }
}

// Additional methods specific to LetsEncryptProvider
impl LetsEncryptProvider {
    /// Fetch live challenge validation status from Let's Encrypt
    /// This fetches the current state of the challenge directly from the ACME server
    pub async fn get_challenge_status(
        &self,
        order_url: &str,
        email: &str,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        debug!("Fetching live challenge status for order: {}", order_url);

        let (acme_account, _) = self.get_or_create_acme_account(email).await?;

        // Load the order from Let's Encrypt
        let mut order = acme_account.order(order_url.to_string()).await?;

        // Get authorizations
        let authorizations = order.authorizations().await?;

        if authorizations.is_empty() {
            debug!("No authorizations found for order");
            return Ok(None);
        }

        // Get the first authorization's challenges (typically there's one for single domain)
        let first_auth = &authorizations[0];
        let challenges = &first_auth.challenges;

        if challenges.is_empty() {
            debug!("No challenges found in authorization");
            return Ok(None);
        }

        // Find the first HTTP-01 or DNS-01 challenge
        let challenge = challenges.iter().find(|c| {
            matches!(
                c.r#type,
                AcmeChallengeType::Http01 | AcmeChallengeType::Dns01
            )
        });

        if let Some(challenge) = challenge {
            // Convert challenge to JSON format matching Let's Encrypt response
            let challenge_json = serde_json::json!({
                "type": match challenge.r#type {
                    AcmeChallengeType::Http01 => "http-01",
                    AcmeChallengeType::Dns01 => "dns-01",
                    _ => "unknown"
                },
                "url": challenge.url,
                "status": format!("{:?}", challenge.status).to_lowercase(),
                "error": challenge.error.as_ref().map(|e| serde_json::json!({
                    "type": e.r#type,
                    "detail": e.detail,
                    "status": e.status
                })),
                "token": challenge.token
            });

            Ok(Some(challenge_json))
        } else {
            debug!("No HTTP-01 or DNS-01 challenge found");
            Ok(None)
        }
    }
}

impl From<super::errors::RepositoryError> for ProviderError {
    fn from(err: super::errors::RepositoryError) -> Self {
        use super::errors::RepositoryError;
        match err {
            RepositoryError::NotFound(msg) => ProviderError::Internal(msg),
            RepositoryError::Database(msg) => ProviderError::Internal(msg),
            _ => ProviderError::Internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::repository::test_utils::MockCertificateRepository;

    #[tokio::test]
    async fn test_letsencrypt_provider_validation() {
        std::env::set_var("LETSENCRYPT_MODE", "staging");
        let repo = Arc::new(MockCertificateRepository::new());
        let provider = LetsEncryptProvider::new(repo);

        let result = provider
            .validate_prerequisites("example.com", "test@example.com")
            .await
            .unwrap();
        assert!(result.is_valid);
        assert_eq!(result.warnings.len(), 1); // Staging environment warning
    }

    #[tokio::test]
    async fn test_supported_challenges() {
        let repo = Arc::new(MockCertificateRepository::new());
        let provider = LetsEncryptProvider::new(repo);

        let challenges = provider.supported_challenges();
        assert_eq!(challenges.len(), 2);
        assert!(challenges.contains(&ChallengeType::Http01));
    }
}
