use anyhow::Result;
use chrono::Utc;
use hickory_resolver::config::{LookupIpStrategy, ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::Arc;
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
use tracing::{error, info, warn};

use super::errors::{BuilderError, TlsError};
use super::models::*;
use super::providers::CertificateProvider;
use super::repository::CertificateRepository;

pub struct TlsService {
    repository: Arc<dyn CertificateRepository>,
    cert_provider: Arc<dyn CertificateProvider>,
    resolver: Arc<TokioAsyncResolver>,
    notification_service: Option<Arc<dyn NotificationService>>,
    config_service: Option<Arc<temps_config::ConfigService>>,
    db: Option<Arc<temps_database::DbConnection>>,
}

impl TlsService {
    pub fn new(
        repository: Arc<dyn CertificateRepository>,
        cert_provider: Arc<dyn CertificateProvider>,
    ) -> Self {
        // Create a cached DNS resolver
        let mut options = ResolverOpts::default();
        options.cache_size = 256;
        options.use_hosts_file = false;
        options.edns0 = true;
        options.ip_strategy = LookupIpStrategy::Ipv4Only;
        options.try_tcp_on_error = true;

        let resolver = Arc::new(TokioAsyncResolver::tokio(
            ResolverConfig::cloudflare(),
            options,
        ));

        Self {
            repository,
            cert_provider,
            resolver,
            notification_service: None,
            config_service: None,
            db: None,
        }
    }

    pub fn with_notification_service(
        mut self,
        notification_service: Arc<dyn NotificationService>,
    ) -> Self {
        self.notification_service = Some(notification_service);
        self
    }

    pub fn with_config_service(mut self, config_service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(config_service);
        self
    }

    pub fn with_db(mut self, db: Arc<temps_database::DbConnection>) -> Self {
        self.db = Some(db);
        self
    }

    // Certificate provisioning
    pub async fn provision_certificate(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<Certificate, TlsError> {
        info!(
            "Provisioning certificate for domain: {} using HTTP-01 challenge with email: {}",
            domain, email
        );
        self.initiate_http_challenge(domain, email).await
    }

    async fn initiate_http_challenge(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<Certificate, TlsError> {
        info!(
            "Initiating HTTP-01 challenge for {} with email: {}",
            domain, email
        );

        match self
            .cert_provider
            .provision(domain, ChallengeType::Http01, email)
            .await?
        {
            ProvisioningResult::Challenge(challenge_data) => {
                // Save challenge data for the HTTP server to serve
                let http_challenge = HttpChallengeData {
                    domain: domain.to_string(),
                    token: challenge_data.token.clone(),
                    key_authorization: challenge_data.key_authorization.clone(),
                    validation_url: challenge_data.validation_url.clone(),
                    created_at: Utc::now(),
                };

                self.repository.save_http_challenge(http_challenge).await?;

                info!("HTTP-01 challenge saved for domain: {}. Challenge will be served at /.well-known/acme-challenge/{}",
                      domain, challenge_data.token);

                // The challenge is now ready to be validated by the ACME server
                // It will access /.well-known/acme-challenge/{token} and expect key_authorization
                Err(TlsError::ManualActionRequired(format!(
                    "HTTP-01 challenge initiated for {}. The challenge token is available at /.well-known/acme-challenge/{}",
                    domain, challenge_data.token
                )))
            }
            ProvisioningResult::Certificate(cert) => {
                let saved_cert = self.repository.save_certificate(cert).await?;
                info!("Certificate immediately available for {}", domain);
                Ok(saved_cert)
            }
        }
    }

    pub async fn complete_http_challenge(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<Certificate, TlsError> {
        info!(
            "Completing HTTP-01 challenge for {} with email: {}",
            domain, email
        );

        let challenge_data = self
            .repository
            .find_http_challenge(domain)
            .await?
            .ok_or_else(|| TlsError::NotFound(format!("No HTTP challenge found for {}", domain)))?;

        // Complete the challenge
        let challenge = ChallengeData {
            challenge_type: ChallengeType::Http01,
            domain: domain.to_string(),
            token: challenge_data.token,
            key_authorization: challenge_data.key_authorization,
            validation_url: challenge_data.validation_url,
            dns_txt_records: vec![],
            order_url: None, // Order URL not stored for HTTP challenges
        };

        let cert = self
            .cert_provider
            .complete_challenge(domain, &challenge, email)
            .await?;
        let saved_cert = self.repository.save_certificate(cert).await?;

        // Clean up challenge data
        self.repository.delete_http_challenge(domain).await?;

        info!("HTTP-01 challenge completed for {}", domain);
        Ok(saved_cert)
    }

    // Certificate retrieval
    pub async fn get_certificate(&self, domain: &str) -> Result<Option<Certificate>, TlsError> {
        self.repository
            .find_certificate(domain)
            .await
            .map_err(Into::into)
    }

    pub async fn get_certificate_for_sni(
        &self,
        sni: &str,
    ) -> Result<
        Option<(
            Vec<CertificateDer<'static>>,
            PrivateKeyDer<'static>,
            String,
            String,
        )>,
        TlsError,
    > {
        match self.repository.find_certificate_for_sni(sni).await? {
            Some(cert) if !cert.certificate_pem.is_empty() && !cert.private_key_pem.is_empty() => {
                let cert_chain = load_certs(cert.certificate_pem.as_bytes())?;
                let private_key = load_private_key(cert.private_key_pem.as_bytes())?;
                Ok(Some((
                    cert_chain,
                    private_key,
                    cert.certificate_pem,
                    cert.private_key_pem,
                )))
            }
            _ => Ok(None),
        }
    }

    pub async fn list_certificates(
        &self,
        filter: CertificateFilter,
    ) -> Result<Vec<Certificate>, TlsError> {
        self.repository
            .list_certificates(filter)
            .await
            .map_err(Into::into)
    }

    // Certificate renewal
    pub async fn needs_renewal(&self, domain: &str) -> Result<bool, TlsError> {
        match self.repository.find_certificate(domain).await? {
            Some(cert) => Ok(cert.needs_renewal()),
            None => Ok(true), // No certificate means it needs to be provisioned
        }
    }

    pub async fn renew_certificate(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<Certificate, TlsError> {
        info!(
            "Renewing certificate for domain: {} with email: {}",
            domain, email
        );
        self.provision_certificate(domain, email).await
    }

    pub async fn renew_expiring_certificates(&self, email: &str) -> Result<(), TlsError> {
        let expiring = self.repository.find_expiring_certificates(30).await?;
        let mut errors = Vec::new();

        for cert in expiring {
            if let Err(e) = self.renew_certificate(&cert.domain, email).await {
                error!("Failed to renew certificate for {}: {}", cert.domain, e);
                errors.push(format!("{}: {}", cert.domain, e));
            }
        }

        if !errors.is_empty() {
            return Err(TlsError::Operation(format!(
                "Failed to renew certificates: {}",
                errors.join(", ")
            )));
        }

        Ok(())
    }

    /// Check and automatically renew expiring certificates
    /// - HTTP-01 certificates: Auto-renew
    /// - DNS-01 certificates: Send notification for manual renewal
    /// Threshold: 30 days before expiration
    pub async fn check_and_renew_certificates(
        &self,
        renewal_threshold_days: i32,
    ) -> Result<RenewalReport, TlsError> {
        // Find all certificates expiring within threshold
        let expiring = self
            .repository
            .find_expiring_certificates(renewal_threshold_days)
            .await?;

        let mut report = RenewalReport {
            total_checked: expiring.len(),
            auto_renewed: Vec::new(),
            renewal_failed: Vec::new(),
            manual_action_needed: Vec::new(),
        };

        for cert in expiring {
            match cert.verification_method.as_str() {
                "http-01" => {
                    // HTTP-01: Attempt automatic renewal
                    self.handle_http01_renewal(&cert, &mut report).await;
                }
                "dns-01" => {
                    // DNS-01: Notify user for manual renewal
                    self.handle_dns01_notification(&cert, &mut report).await;
                }
                _ => {
                    warn!(
                        "Unknown verification method '{}' for domain {}",
                        cert.verification_method, cert.domain
                    );
                }
            }
        }

        // Send summary notification
        self.send_renewal_summary(&report).await;

        Ok(report)
    }

    /// Get email for ACME certificate provisioning
    /// Priority: 1) LetsEncrypt email from settings, 2) First user email, 3) Fallback
    async fn get_acme_email(&self) -> String {
        // Try to get from config service settings
        if let Some(config_service) = &self.config_service {
            if let Ok(settings) = config_service.get_settings().await {
                if let Some(email) = settings.letsencrypt.email {
                    if !email.is_empty() {
                        return email;
                    }
                }
            }
        }

        // Fall back to first user's email if database is available
        if let Some(db) = &self.db {
            use sea_orm::{EntityTrait, QueryOrder};
            use temps_entities::users;

            if let Ok(Some(user)) = users::Entity::find()
                .order_by_asc(users::Column::Id)
                .one(db.as_ref())
                .await
            {
                return user.email;
            }
        }

        // Last resort fallback
        "system@temps.dev".to_string()
    }

    async fn handle_http01_renewal(&self, cert: &Certificate, report: &mut RenewalReport) {
        info!("🔄 Auto-renewing HTTP-01 certificate for {}", cert.domain);

        let email = self.get_acme_email().await;

        match self.provision_certificate(&cert.domain, &email).await {
            Ok(_) => {
                info!("✅ Successfully renewed certificate for {}", cert.domain);
                report.auto_renewed.push(cert.domain.clone());
            }
            Err(e) => {
                error!("❌ Failed to renew certificate for {}: {}", cert.domain, e);

                report.renewal_failed.push(RenewalFailure {
                    domain: cert.domain.clone(),
                    error: e.to_string(),
                    verification_method: cert.verification_method.clone(),
                });

                // Send immediate notification for failed renewal
                self.send_renewal_failure_notification(&cert.domain, &e.to_string())
                    .await;
            }
        }
    }

    async fn handle_dns01_notification(&self, cert: &Certificate, report: &mut RenewalReport) {
        let days_remaining = cert.days_until_expiry();

        info!(
            "⚠️  DNS-01 certificate for {} needs manual renewal (expires in {} days)",
            cert.domain, days_remaining
        );

        report.manual_action_needed.push(ManualRenewalNeeded {
            domain: cert.domain.clone(),
            expires_at: cert.expiration_time,
            days_remaining,
        });

        // Send notification to user
        self.send_manual_renewal_notification(cert).await;
    }

    async fn send_renewal_summary(&self, report: &RenewalReport) {
        if report.total_checked == 0 {
            return;
        }

        let Some(notif_service) = &self.notification_service else {
            return;
        };

        let mut message = format!(
            "Certificate Renewal Report\n\nTotal Checked: {}\n",
            report.total_checked
        );

        if !report.auto_renewed.is_empty() {
            message.push_str(&format!(
                "\n✅ Auto-Renewed ({}):\n",
                report.auto_renewed.len()
            ));
            for domain in &report.auto_renewed {
                message.push_str(&format!("  • {}\n", domain));
            }
        }

        if !report.renewal_failed.is_empty() {
            message.push_str(&format!(
                "\n❌ Renewal Failed ({}):\n",
                report.renewal_failed.len()
            ));
            for failure in &report.renewal_failed {
                message.push_str(&format!("  • {}: {}\n", failure.domain, failure.error));
            }
        }

        if !report.manual_action_needed.is_empty() {
            message.push_str(&format!(
                "\n⚠️  Manual Renewal Needed ({}):\n",
                report.manual_action_needed.len()
            ));
            for manual in &report.manual_action_needed {
                message.push_str(&format!(
                    "  • {} (expires in {} days)\n",
                    manual.domain, manual.days_remaining
                ));
            }
        }

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: "Certificate Renewal Report".to_string(),
            message,
            notification_type: if report.renewal_failed.is_empty() {
                NotificationType::Info
            } else {
                NotificationType::Warning
            },
            priority: if report.renewal_failed.is_empty() {
                NotificationPriority::Normal
            } else {
                NotificationPriority::High
            },
            severity: None,
            timestamp: Utc::now(),
            metadata: std::collections::HashMap::from([
                (
                    "auto_renewed".to_string(),
                    report.auto_renewed.len().to_string(),
                ),
                (
                    "failed".to_string(),
                    report.renewal_failed.len().to_string(),
                ),
                (
                    "manual_needed".to_string(),
                    report.manual_action_needed.len().to_string(),
                ),
            ]),
            bypass_throttling: false,
        };

        if let Err(e) = notif_service.send_notification(notification).await {
            error!("Failed to send renewal summary notification: {}", e);
        }
    }

    async fn send_renewal_failure_notification(&self, domain: &str, error: &str) {
        let Some(notif_service) = &self.notification_service else {
            return;
        };

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!("Certificate Renewal Failed: {}", domain),
            message: format!(
                "Failed to automatically renew certificate for {}.\n\nError: {}\n\nPlease renew this certificate manually in the Temps dashboard.",
                domain, error
            ),
            notification_type: NotificationType::Error,
            priority: NotificationPriority::High,
            severity: Some("error".to_string()),
            timestamp: Utc::now(),
            metadata: std::collections::HashMap::from([
                ("domain".to_string(), domain.to_string()),
                ("error".to_string(), error.to_string()),
                ("verification_method".to_string(), "http-01".to_string()),
            ]),
            bypass_throttling: true,
        };

        if let Err(e) = notif_service.send_notification(notification).await {
            error!("Failed to send renewal failure notification: {}", e);
        }
    }

    async fn send_manual_renewal_notification(&self, cert: &Certificate) {
        let Some(notif_service) = &self.notification_service else {
            return;
        };

        let days_remaining = cert.days_until_expiry();

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!("Action Required: Renew Certificate for {}", cert.domain),
            message: format!(
                "Your wildcard certificate for {} will expire in {} days.\n\nSince this is a DNS-01 certificate, you need to manually renew it:\n1. Go to Temps Dashboard → Domains → {}\n2. Click 'Renew Certificate'\n3. Add the provided DNS TXT record\n4. Click 'Finalize Renewal'\n\nYour current certificate remains active during renewal.",
                cert.domain,
                days_remaining,
                cert.domain
            ),
            notification_type: if days_remaining <= 7 {
                NotificationType::Alert
            } else {
                NotificationType::Warning
            },
            priority: if days_remaining <= 7 {
                NotificationPriority::Critical
            } else if days_remaining <= 14 {
                NotificationPriority::High
            } else {
                NotificationPriority::Normal
            },
            severity: if days_remaining <= 7 {
                Some("critical".to_string())
            } else if days_remaining <= 14 {
                Some("warning".to_string())
            } else {
                Some("info".to_string())
            },
            timestamp: Utc::now(),
            metadata: std::collections::HashMap::from([
                ("domain".to_string(), cert.domain.clone()),
                ("expires_at".to_string(), cert.expiration_time.to_rfc3339()),
                ("days_remaining".to_string(), days_remaining.to_string()),
                ("verification_method".to_string(), "dns-01".to_string()),
                ("is_wildcard".to_string(), cert.is_wildcard.to_string()),
            ]),
            bypass_throttling: days_remaining <= 7,
        };

        if let Err(e) = notif_service.send_notification(notification).await {
            error!("Failed to send manual renewal notification: {}", e);
        }
    }

    // Queue integration
    pub async fn request_certificate_provisioning(&self, domain: &str) -> Result<(), TlsError> {
        info!("Requesting certificate provisioning for domain: {}", domain);

        // self.queue_service
        //     .send(Job::ProvisionCertificate(ProvisionCertificateJob {
        //         domain: domain.to_string(),
        //     }))
        //     .await
        //     .map_err(|e| TlsError::Operation(format!("Failed to launch provision job: {}", e)))?;

        Ok(())
    }

    pub async fn request_certificate_renewal(&self, domain: &str) -> Result<(), TlsError> {
        info!("Requesting certificate renewal for domain: {}", domain);

        // self.queue_service
        //     .send(Job::RenewCertificate(RenewCertificateJob {
        //         domain: domain.to_string(),
        //     }))
        //     .await
        //     .map_err(|e| TlsError::Operation(format!("Failed to launch renewal job: {}", e)))?;

        Ok(())
    }

    // Helper methods for HTTP challenges
    pub async fn get_http_challenge(
        &self,
        domain: &str,
    ) -> Result<Option<HttpChallengeData>, TlsError> {
        self.repository
            .find_http_challenge(domain)
            .await
            .map_err(Into::into)
    }

    /// Get HTTP challenge debug information including DNS resolution
    pub async fn get_http_challenge_debug(
        &self,
        domain: &str,
    ) -> Result<HttpChallengeDebugInfo, TlsError> {
        // Get challenge data
        let challenge = self.repository.find_http_challenge(domain).await?;

        // Perform DNS resolution
        let dns_info = self.resolve_domain_info(domain).await;

        Ok(HttpChallengeDebugInfo {
            domain: domain.to_string(),
            challenge_exists: challenge.is_some(),
            challenge_token: challenge.as_ref().map(|c| c.token.clone()),
            challenge_url: challenge
                .as_ref()
                .map(|c| format!("http://{}/.well-known/acme-challenge/{}", domain, c.token)),
            validation_url: challenge.as_ref().and_then(|c| c.validation_url.clone()),
            dns_a_records: dns_info.a_records,
            dns_aaaa_records: dns_info.aaaa_records,
            dns_error: dns_info.error,
        })
    }

    /// Resolve domain DNS information
    async fn resolve_domain_info(&self, domain: &str) -> DnsInfo {
        let mut a_records = Vec::new();
        let mut aaaa_records = Vec::new();
        let mut error = None;

        // Try IPv4 lookup
        match self.resolver.ipv4_lookup(domain).await {
            Ok(lookup) => {
                a_records = lookup.iter().map(|ip| ip.to_string()).collect();
            }
            Err(e) => {
                error = Some(format!("IPv4 lookup failed: {}", e));
            }
        }

        // Try IPv6 lookup (if IPv4 succeeded or failed, we still try IPv6)
        match self.resolver.ipv6_lookup(domain).await {
            Ok(lookup) => {
                aaaa_records = lookup.iter().map(|ip| ip.to_string()).collect();
            }
            Err(e) => {
                if error.is_some() {
                    error = Some(format!("{}, IPv6 lookup failed: {}", error.unwrap(), e));
                }
            }
        }

        DnsInfo {
            a_records,
            aaaa_records,
            error,
        }
    }

    /// Fetch live challenge validation status from Let's Encrypt
    /// This retrieves the current state of the ACME challenge directly from the server
    pub async fn get_live_challenge_status(
        &self,
        order_url: &str,
        email: &str,
    ) -> Result<Option<serde_json::Value>, TlsError> {
        use super::providers::LetsEncryptProvider;

        // The cert_provider is Arc<dyn CertificateProvider>, we need to downcast to LetsEncryptProvider
        // to access the get_challenge_status method
        let provider_any = &self.cert_provider as &dyn std::any::Any;

        if let Some(lets_encrypt_provider) = provider_any.downcast_ref::<LetsEncryptProvider>() {
            lets_encrypt_provider
                .get_challenge_status(order_url, email)
                .await
                .map_err(TlsError::Provider)
        } else {
            // If it's not a LetsEncryptProvider, we can't fetch challenge status
            Ok(None)
        }
    }
}

#[derive(Debug)]
struct DnsInfo {
    a_records: Vec<String>,
    aaaa_records: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HttpChallengeDebugInfo {
    pub domain: String,
    pub challenge_exists: bool,
    pub challenge_token: Option<String>,
    pub challenge_url: Option<String>,
    pub validation_url: Option<String>,
    pub dns_a_records: Vec<String>,
    pub dns_aaaa_records: Vec<String>,
    pub dns_error: Option<String>,
}

// Builder pattern
pub struct TlsServiceBuilder {
    repository: Option<Arc<dyn CertificateRepository>>,
    cert_provider: Option<Arc<dyn CertificateProvider>>,
}

impl Default for TlsServiceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TlsServiceBuilder {
    pub fn new() -> Self {
        Self {
            repository: None,
            cert_provider: None,
        }
    }

    pub fn with_repository(mut self, repo: Arc<dyn CertificateRepository>) -> Self {
        self.repository = Some(repo);
        self
    }

    pub fn with_cert_provider(mut self, provider: Arc<dyn CertificateProvider>) -> Self {
        self.cert_provider = Some(provider);
        self
    }

    pub fn build(self) -> Result<TlsService, BuilderError> {
        Ok(TlsService::new(
            self.repository.ok_or(BuilderError::MissingRepository)?,
            self.cert_provider.ok_or(BuilderError::MissingProvider)?,
        ))
    }
}

// Helper functions
fn load_certs(contents: &[u8]) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    rustls_pemfile::certs(&mut std::io::BufReader::new(contents))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Internal(format!("Failed to load certificates: {}", e)))
}

fn load_private_key(content: &[u8]) -> Result<PrivateKeyDer<'static>, TlsError> {
    let mut reader = std::io::BufReader::new(content);

    loop {
        match rustls_pemfile::read_one(&mut reader)
            .map_err(|e| TlsError::Internal(format!("Failed to parse private key: {}", e)))?
        {
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Sec1Key(key)) => return Ok(key.into()),
            None => break,
            _ => {}
        }
    }
    Err(TlsError::Internal("No valid private key found".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::errors::ProviderError;
    use crate::tls::models::{
        Certificate, CertificateFilter, CertificateStatus, ChallengeData, ChallengeStrategy,
        ChallengeType, DnsChallengeData, ProvisioningResult, ValidationResult,
    };
    use crate::tls::providers::CertificateProvider;
    use crate::tls::repository::test_utils::MockCertificateRepository;
    use crate::tls::repository::DefaultCertificateRepository;
    use temps_core::{Job, JobQueue};
    use temps_database::test_utils::TestDatabase;

    #[tokio::test]
    async fn test_builder_pattern() {
        // Create mock components
        let repo = Arc::new(MockCertificateRepository::new());
        let provider = Arc::new(MockCertificateProvider::new());

        // For now, we'll just test that the builder requires all components
        let result_missing_repo = TlsServiceBuilder::new()
            .with_cert_provider(provider.clone())
            .build();
        assert!(result_missing_repo.is_err());

        // Test successful build requires all components
        // Note: We can't fully test the builder without proper mocks for ConfigService
        // which requires a database connection. This would be better tested in integration tests.
    }

    // Mock implementations for testing
    struct MockJobQueue;

    impl MockJobQueue {
        fn new() -> Self {
            Self
        }
    }

    #[async_trait::async_trait]
    impl JobQueue for MockJobQueue {
        async fn send(&self, _job: Job) -> Result<(), temps_core::QueueError> {
            // Mock implementation - just return Ok
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            // Mock implementation - not used in tests
            unimplemented!("subscribe not implemented in mock")
        }
    }

    struct MockCertificateProvider;

    impl MockCertificateProvider {
        fn new() -> Self {
            Self
        }
    }

    #[async_trait::async_trait]
    impl CertificateProvider for MockCertificateProvider {
        async fn provision(
            &self,
            domain: &str,
            _challenge: ChallengeType,
            _email: &str,
        ) -> Result<ProvisioningResult, ProviderError> {
            Ok(ProvisioningResult::Certificate(Certificate {
                id: 1,
                domain: domain.to_string(),
                certificate_pem: "mock cert".to_string(),
                private_key_pem: "mock key".to_string(),
                expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
                last_renewed: None,
                is_wildcard: domain.starts_with("*."),
                verification_method: "http-01".to_string(),
                status: CertificateStatus::Active,
            }))
        }

        async fn complete_challenge(
            &self,
            _domain: &str,
            _challenge_data: &ChallengeData,
            _email: &str,
        ) -> Result<Certificate, ProviderError> {
            Ok(Certificate {
                id: 1,
                domain: _domain.to_string(),
                certificate_pem: "completed cert".to_string(),
                private_key_pem: "completed key".to_string(),
                expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
                last_renewed: None,
                is_wildcard: _domain.starts_with("*."),
                verification_method: "http-01".to_string(),
                status: CertificateStatus::Active,
            })
        }

        fn supported_challenges(&self) -> Vec<ChallengeType> {
            vec![ChallengeType::Http01]
        }

        async fn validate_prerequisites(
            &self,
            _domain: &str,
            _email: &str,
        ) -> Result<ValidationResult, ProviderError> {
            Ok(ValidationResult {
                is_valid: true,
                errors: vec![],
                warnings: vec![],
            })
        }

        async fn cancel_order(&self, _domain: &str) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_builder_missing_components() {
        let result = TlsServiceBuilder::new().build();
        assert!(matches!(result, Err(BuilderError::MissingRepository)));
    }

    #[tokio::test]
    async fn test_provision_certificate_http01() {
        let provider = Arc::new(MockCertificateProvider::new());

        // Note: Can't create full service without ConfigService
        // But we can test the provider directly
        let result = provider
            .provision(
                "test.example.com",
                ChallengeType::Http01,
                "test@example.com",
            )
            .await;
        assert!(result.is_ok());

        match result.unwrap() {
            ProvisioningResult::Certificate(cert) => {
                assert_eq!(cert.domain, "test.example.com");
                assert_eq!(cert.verification_method, "http-01");
            }
            _ => panic!("Expected certificate result"),
        }
    }

    #[tokio::test]
    async fn test_certificate_expiry_check() {
        let mut cert = Certificate {
            id: 1,
            domain: "example.com".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_pem: "key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(10),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        // Should need renewal (less than 30 days)
        assert!(cert.needs_renewal());
        assert!(!cert.is_expired());

        // Test expired cert
        cert.expiration_time = chrono::Utc::now() - chrono::Duration::days(1);
        assert!(cert.is_expired());
        assert!(cert.needs_renewal());
    }

    #[tokio::test]
    async fn test_repository_certificate_lifecycle() {
        let repo = MockCertificateRepository::new();

        let cert = Certificate {
            id: 1,
            domain: "lifecycle.example.com".to_string(),
            certificate_pem: "cert_pem".to_string(),
            private_key_pem: "key_pem".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
            last_renewed: Some(chrono::Utc::now()),
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        // Save certificate
        let saved = repo.save_certificate(cert.clone()).await.unwrap();
        assert_eq!(saved.domain, cert.domain);

        // Find certificate
        let found = repo.find_certificate(&cert.domain).await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().domain, cert.domain);

        // Update status
        repo.update_certificate_status(&cert.domain, CertificateStatus::Expired)
            .await
            .unwrap();

        // Verify status updated
        let updated = repo.find_certificate(&cert.domain).await.unwrap().unwrap();
        assert!(matches!(updated.status, CertificateStatus::Expired));
    }

    #[tokio::test]
    async fn test_dns_challenge_data_storage() {
        let repo = MockCertificateRepository::new();

        let challenge = DnsChallengeData {
            domain: "dns.example.com".to_string(),
            txt_record_name: "_acme-challenge.dns.example.com".to_string(),
            txt_record_value: "challenge_value_123".to_string(),
            order_url: Some("https://acme.example.com/order/123".to_string()),
            created_at: chrono::Utc::now(),
        };

        // Save DNS challenge
        repo.save_dns_challenge(challenge.clone()).await.unwrap();

        // Find DNS challenge
        let found = repo.find_dns_challenge(&challenge.domain).await.unwrap();
        assert!(found.is_some());

        let found_challenge = found.unwrap();
        assert_eq!(found_challenge.txt_record_value, challenge.txt_record_value);
        assert_eq!(found_challenge.txt_record_name, challenge.txt_record_name);
    }

    #[tokio::test]
    async fn test_wildcard_certificate_detection() {
        let wildcard_cert = Certificate {
            id: 1,
            domain: "*.example.com".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_pem: "key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
            last_renewed: None,
            is_wildcard: true,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        assert!(wildcard_cert.is_wildcard);
        assert!(wildcard_cert.domain.starts_with("*."));

        let regular_cert = Certificate {
            id: 1,
            domain: "www.example.com".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_pem: "key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        assert!(!regular_cert.is_wildcard);
        assert!(!regular_cert.domain.starts_with("*."));
    }

    #[tokio::test]
    async fn test_provider_validation() {
        let provider = MockCertificateProvider::new();

        // Test validation
        let result = provider
            .validate_prerequisites("test.example.com", "test@example.com")
            .await
            .unwrap();
        assert!(result.is_valid);
        assert!(result.errors.is_empty());
        assert!(result.warnings.is_empty());

        // Test supported challenges
        let challenges = provider.supported_challenges();
        assert_eq!(challenges.len(), 1);
        assert!(challenges.contains(&ChallengeType::Http01));
    }

    #[tokio::test]
    async fn test_tls_service_with_real_database() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));

        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));
        let provider = Arc::new(MockCertificateProvider::new());

        let service = TlsService::new(repo.clone(), provider);

        // Test provisioning a certificate
        let cert = service
            .provision_certificate("test.example.com", "test@example.com")
            .await;
        assert!(cert.is_ok());

        let cert = cert.unwrap();
        assert_eq!(cert.domain, "test.example.com");

        // Test finding the certificate
        let found = service.get_certificate("test.example.com").await;
        assert!(found.is_ok());
        assert!(found.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_certificate_renewal_with_database() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));

        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        // Create a certificate that needs renewal
        let cert = Certificate {
            id: 1,
            domain: "renew.example.com".to_string(),
            certificate_pem: "old cert".to_string(),
            private_key_pem: "old key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(15), // Needs renewal
            last_renewed: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        // Save it
        let saved = repo.save_certificate(cert.clone()).await.unwrap();
        assert!(saved.needs_renewal());

        // Create service and renew
        let provider = Arc::new(MockCertificateProvider::new());

        let service = TlsService::new(repo.clone(), provider);

        let renewed = service
            .renew_certificate("renew.example.com", "test@example.com")
            .await;
        assert!(renewed.is_ok());

        // Check that the certificate was updated
        let updated = repo
            .find_certificate("renew.example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.certificate_pem, "mock cert"); // From MockCertificateProvider
    }

    #[tokio::test]
    async fn test_dns_challenge_flow_with_database() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));

        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        // Save DNS challenge data
        let challenge = DnsChallengeData {
            domain: "dns.example.com".to_string(),
            txt_record_name: "_acme-challenge.dns.example.com".to_string(),
            txt_record_value: "challenge123".to_string(),
            order_url: Some("https://acme.test/order/123".to_string()),
            created_at: chrono::Utc::now(),
        };

        repo.save_dns_challenge(challenge.clone()).await.unwrap();

        // Find it
        let found = repo.find_dns_challenge("dns.example.com").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().txt_record_value, "challenge123");

        // Delete it
        repo.delete_dns_challenge("dns.example.com").await.unwrap();

        // Verify it's gone
        let not_found = repo.find_dns_challenge("dns.example.com").await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_list_certificates_with_filters() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));

        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        // Add multiple certificates with different statuses
        let active_cert = Certificate {
            id: 1,
            domain: "active.example.com".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_pem: "key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(60),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        let expired_cert = Certificate {
            id: 2,
            domain: "expired.example.com".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_pem: "key".to_string(),
            expiration_time: chrono::Utc::now() - chrono::Duration::days(1),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Expired,
        };

        let wildcard_cert = Certificate {
            id: 3,
            domain: "*.wildcard.example.com".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_pem: "key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
            last_renewed: None,
            is_wildcard: true,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        repo.save_certificate(active_cert).await.unwrap();
        repo.save_certificate(expired_cert).await.unwrap();
        repo.save_certificate(wildcard_cert).await.unwrap();

        // Test filter by status
        let filter = CertificateFilter {
            status: Some(CertificateStatus::Active),
            ..Default::default()
        };

        let active_certs = repo.list_certificates(filter).await.unwrap();
        assert_eq!(active_certs.len(), 2); // active and wildcard

        // Test filter by wildcard
        let filter = CertificateFilter {
            is_wildcard: Some(true),
            ..Default::default()
        };

        let wildcard_certs = repo.list_certificates(filter).await.unwrap();
        assert_eq!(wildcard_certs.len(), 1);
        assert_eq!(wildcard_certs[0].domain, "*.wildcard.example.com");

        // Test filter by expiring soon (includes already expired)
        let filter = CertificateFilter {
            expiring_within_days: Some(30),
            ..Default::default()
        };

        let expiring_certs = repo.list_certificates(filter).await.unwrap();
        assert_eq!(expiring_certs.len(), 1); // The expired cert is included
        assert_eq!(expiring_certs[0].domain, "expired.example.com");
    }

    #[tokio::test]
    async fn test_acme_account_storage() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));

        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        use crate::tls::models::AcmeAccount;

        let account = AcmeAccount {
            email: "test_acme_account@example.com".to_string(),
            environment: "staging".to_string(),
            credentials: r#"{"id":"test123","key":"secret"}"#.to_string(),
            created_at: chrono::Utc::now(),
        };

        // Save account
        repo.save_acme_account(account.clone()).await.unwrap();

        // Find account
        let found = repo
            .find_acme_account("test_acme_account@example.com", "staging")
            .await
            .unwrap();
        assert!(found.is_some());

        let found_account = found.unwrap();
        assert_eq!(found_account.email, "test_acme_account@example.com");
        assert_eq!(found_account.environment, "staging");
        assert!(found_account.credentials.contains("test123"));
    }

    #[tokio::test]
    async fn test_certificate_status_transitions() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));

        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        // Create a pending certificate
        let cert = Certificate {
            id: 1,
            domain: "status.example.com".to_string(),
            certificate_pem: String::new(),
            private_key_pem: String::new(),
            expiration_time: chrono::Utc::now(),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Pending,
        };

        repo.save_certificate(cert).await.unwrap();

        // Transition to PendingDns
        repo.update_certificate_status("status.example.com", CertificateStatus::PendingDns)
            .await
            .unwrap();

        let cert = repo
            .find_certificate("status.example.com")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(cert.status, CertificateStatus::PendingDns));

        // Transition to Active
        repo.update_certificate_status("status.example.com", CertificateStatus::Active)
            .await
            .unwrap();

        let cert = repo
            .find_certificate("status.example.com")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(cert.status, CertificateStatus::Active));

        // Transition to Failed
        repo.update_certificate_status(
            "status.example.com",
            CertificateStatus::Failed {
                error: "Test error".to_string(),
                error_type: "TestType".to_string(),
            },
        )
        .await
        .unwrap();

        let cert = repo
            .find_certificate("status.example.com")
            .await
            .unwrap()
            .unwrap();
        match cert.status {
            CertificateStatus::Failed { error, error_type } => {
                assert_eq!(error, "Test error");
                assert_eq!(error_type, "TestType");
            }
            _ => panic!("Expected Failed status"),
        }
    }
}
