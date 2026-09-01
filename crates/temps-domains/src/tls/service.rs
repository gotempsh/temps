// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::Result;
use chrono::Utc;
use hickory_resolver::config::{
    LookupIpStrategy, ResolveHosts, ResolverConfig, ResolverOpts, CLOUDFLARE,
};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::Resolver;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use temps_core::{AuditLogger, AuditOperation};
use temps_monitoring::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use tracing::{error, info, warn};

use super::errors::{BuilderError, TlsError};
use super::models::*;
use super::providers::CertificateProvider;
use super::repository::CertificateRepository;

/// Type alias for the Tokio-based DNS resolver
type TokioResolver = Resolver<TokioRuntimeProvider>;

#[derive(Debug, Serialize)]
struct DnsAutomationAudit {
    domain: String,
    zone: String,
    provider_id: i32,
    provider_name: String,
    outcome: String,
    reason: Option<String>,
    #[serde(serialize_with = "serialize_redacted_mutations")]
    mutations: Vec<temps_core::DnsAutomationMutation>,
}

fn serialize_redacted_mutations<S>(
    mutations: &[temps_core::DnsAutomationMutation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct RedactedMutation<'a> {
        record_type: &'a str,
        name: &'a str,
        value: &'static str,
    }

    mutations
        .iter()
        .map(|mutation| RedactedMutation {
            record_type: &mutation.record_type,
            name: &mutation.name,
            value: "[REDACTED]",
        })
        .collect::<Vec<_>>()
        .serialize(serializer)
}

impl AuditOperation for DnsAutomationAudit {
    fn operation_type(&self) -> String {
        "DNS_AUTOMATION_ACME_DNS01".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        None
    }
    fn ip_address(&self) -> Option<String> {
        None
    }
    fn user_agent(&self) -> &str {
        "temps-certificate-renewal-scheduler"
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
}

pub struct TlsService {
    repository: Arc<dyn CertificateRepository>,
    cert_provider: Arc<dyn CertificateProvider>,
    resolver: Arc<TokioResolver>,
    /// Set post-construction via `set_alarm_service` once all plugins have
    /// registered — `DomainsPlugin` registers before `MonitoringPlugin`, so
    /// `AlarmService` isn't available yet during `register_services`.
    alarm_service: OnceLock<Arc<AlarmService>>,
    config_service: Option<Arc<temps_config::ConfigService>>,
    db: Option<Arc<temps_database::DbConnection>>,
    /// Domain service used to drive the order-based ACME flow during background
    /// renewals. When present, HTTP-01 auto-renewals persist an `acme_orders` row
    /// (and set the domain to `challenge_requested`) so a renewal that fails to
    /// validate immediately is recoverable from the certificate management UI via
    /// the "Verify & finalize" action, instead of silently expiring.
    domain_service: Option<Arc<crate::DomainService>>,
    /// DNS provider service used to auto-publish the `_acme-challenge` TXT record
    /// during background DNS-01 renewals when the certificate's base domain has an
    /// `auto_manage`-enabled `dns_managed_domains` row. When absent, or when no
    /// provider manages the domain, DNS-01 renewals fall back to the manual
    /// "add this TXT record yourself" notification.
    dns_provider_service: Option<Arc<temps_dns::services::DnsProviderService>>,
    /// Fail-closed authorization gate for unattended DNS mutations. Human
    /// provider management is governed independently by API permissions.
    dns_automation_gate: Option<Arc<dyn temps_core::DnsAutomationGate>>,
    audit_logger: Option<Arc<dyn AuditLogger>>,
    dns_propagation_delay: Duration,
}

impl TlsService {
    pub fn new(
        repository: Arc<dyn CertificateRepository>,
        cert_provider: Arc<dyn CertificateProvider>,
    ) -> Self {
        // Create a cached DNS resolver
        let mut options = ResolverOpts::default();
        options.cache_size = 256;
        options.use_hosts_file = ResolveHosts::Never;
        options.edns0 = true;
        options.ip_strategy = LookupIpStrategy::Ipv4Only;
        options.try_tcp_on_error = true;

        // Building from a static, known-valid config cannot fail in practice.
        let resolver = Arc::new(
            Resolver::builder_with_config(
                ResolverConfig::udp_and_tcp(&CLOUDFLARE),
                TokioRuntimeProvider::default(),
            )
            .with_options(options)
            .build()
            .expect("failed to build DNS resolver from static Cloudflare config"),
        );

        Self {
            repository,
            cert_provider,
            resolver,
            alarm_service: OnceLock::new(),
            config_service: None,
            db: None,
            domain_service: None,
            dns_provider_service: None,
            dns_automation_gate: None,
            audit_logger: None,
            dns_propagation_delay: Duration::from_secs(30),
        }
    }

    /// Wire the `AlarmService` after construction. Idempotent — only the
    /// first call takes effect. Called from `DomainsPlugin::initialize_plugin_services`,
    /// which runs only after every plugin (including Monitoring) has finished
    /// `register_services`.
    pub fn set_alarm_service(&self, alarm_service: Arc<AlarmService>) {
        let _ = self.alarm_service.set(alarm_service);
    }

    pub fn with_config_service(mut self, config_service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(config_service);
        self
    }

    pub fn with_db(mut self, db: Arc<temps_database::DbConnection>) -> Self {
        self.db = Some(db);
        self
    }

    pub fn with_dns_provider_service(
        mut self,
        dns_provider_service: Arc<temps_dns::services::DnsProviderService>,
    ) -> Self {
        self.dns_provider_service = Some(dns_provider_service);
        self
    }

    pub fn with_dns_automation_gate(
        mut self,
        dns_automation_gate: Arc<dyn temps_core::DnsAutomationGate>,
    ) -> Self {
        self.dns_automation_gate = Some(dns_automation_gate);
        self
    }

    pub fn with_audit_logger(mut self, audit_logger: Arc<dyn AuditLogger>) -> Self {
        self.audit_logger = Some(audit_logger);
        self
    }

    async fn audit_dns_automation(&self, audit: DnsAutomationAudit) {
        let Some(logger) = &self.audit_logger else {
            warn!(
                "DNS automation audit logger is unavailable; outcome={}",
                audit.outcome
            );
            return;
        };
        if let Err(error) = logger.create_audit_log(&audit).await {
            error!("Failed to persist DNS automation audit event: {error}");
        }
    }

    pub fn with_domain_service(mut self, domain_service: Arc<crate::DomainService>) -> Self {
        self.domain_service = Some(domain_service);
        self
    }

    // Certificate provisioning
    pub async fn provision_certificate(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<Certificate, TlsError> {
        // An ACME account requires a real contact email. There is no fallback
        // (see `get_acme_email`): refuse to provision rather than register an
        // account with an empty/placeholder contact that Let's Encrypt rejects.
        if email.trim().is_empty() {
            return Err(TlsError::Configuration(format!(
                "no Let's Encrypt contact email configured; set letsencrypt.email before \
                 provisioning a certificate for {domain}"
            )));
        }

        // Wildcard domains require DNS-01 challenge
        if domain.starts_with("*.") {
            info!(
                "Provisioning wildcard certificate for domain: {} using DNS-01 challenge with email: {}",
                domain, email
            );
            return self.initiate_dns_challenge(domain, email).await;
        }

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
                    order_url: challenge_data.order_url.clone(),
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

    async fn initiate_dns_challenge(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<Certificate, TlsError> {
        info!(
            "Initiating DNS-01 challenge for {} with email: {}",
            domain, email
        );

        match self
            .cert_provider
            .provision(domain, ChallengeType::Dns01, email)
            .await?
        {
            ProvisioningResult::Challenge(challenge_data) => {
                // For DNS-01, user needs to add TXT records manually
                let txt_records_info = challenge_data
                    .dns_txt_records
                    .iter()
                    .map(|r| format!("{} = {}", r.name, r.value))
                    .collect::<Vec<_>>()
                    .join(", ");

                info!(
                    "DNS-01 challenge initiated for domain: {}. Add TXT record(s): {}",
                    domain, txt_records_info
                );

                // Save the challenge data to the database for later completion
                // We store it as an HTTP challenge record for simplicity, but it's a DNS challenge
                let dns_challenge = HttpChallengeData {
                    domain: domain.to_string(),
                    token: challenge_data.token.clone(),
                    key_authorization: challenge_data.key_authorization.clone(),
                    validation_url: challenge_data.validation_url.clone(),
                    order_url: challenge_data.order_url.clone(),
                    created_at: Utc::now(),
                };

                self.repository.save_http_challenge(dns_challenge).await?;

                // Return an error indicating manual action is required
                Err(TlsError::ManualActionRequired(format!(
                    "DNS-01 challenge initiated for {}. Add TXT record(s): {}",
                    domain, txt_records_info
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
            order_url: challenge_data.order_url,
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
    ///
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

    /// Get the ACME contact email from `letsencrypt.email` settings.
    ///
    /// `letsencrypt.email` is the single source of truth — there is NO fallback
    /// to the first user's email or a placeholder address. Those fallbacks
    /// produced invalid ACME contacts (e.g. the `system@localhost` system user),
    /// which Let's Encrypt rejects, so issuance failed silently. Returns empty
    /// when no email is configured; callers must treat empty as "no contact
    /// configured" and skip/abort issuance rather than provision with a bogus
    /// address.
    async fn get_acme_email(&self) -> String {
        if let Some(config_service) = &self.config_service {
            if let Ok(settings) = config_service.get_settings().await {
                if let Some(email) = settings.letsencrypt.email {
                    let email = email.trim().to_string();
                    if !email.is_empty() {
                        return email;
                    }
                }
            }
        }

        String::new()
    }

    async fn handle_http01_renewal(&self, cert: &Certificate, report: &mut RenewalReport) {
        let days_remaining = cert.days_until_expiry();
        info!(
            "🔄 Auto-renewing HTTP-01 certificate for {} (status={:?}, expires in {} days)",
            cert.domain, cert.status, days_remaining
        );

        let email = self.get_acme_email().await;

        // Prefer the order-based flow so a renewal that doesn't validate immediately
        // leaves a recoverable ACME order in the UI (see `domain_service` field docs).
        // Fall back to the legacy http_challenges path only when no DomainService is
        // wired in (e.g. unit tests that construct TlsService directly).
        if let Some(domain_service) = self.domain_service.clone() {
            self.handle_http01_renewal_order_based(cert, &email, &domain_service, report)
                .await;
            return;
        }

        // Step 1: Initiate the ACME order and HTTP-01 challenge
        // provision_certificate returns ManualActionRequired error when challenge is initiated,
        // which is expected behavior for the provisioning step
        match self.provision_certificate(&cert.domain, &email).await {
            Ok(_new_cert) => {
                // Certificate was immediately available (shouldn't happen for renewals, but handle it)
                info!("✅ Successfully renewed certificate for {}", cert.domain);
                report.auto_renewed.push(cert.domain.clone());
                return;
            }
            Err(TlsError::ManualActionRequired(_)) => {
                // This is expected - challenge has been initiated and saved
                info!(
                    "HTTP-01 challenge initiated for {}, waiting for validation...",
                    cert.domain
                );
            }
            Err(e) => {
                error!("❌ Failed to initiate renewal for {}: {}", cert.domain, e);
                report.renewal_failed.push(RenewalFailure {
                    domain: cert.domain.clone(),
                    error: e.to_string(),
                    verification_method: cert.verification_method.clone(),
                });
                self.send_renewal_failure_notification(
                    &cert.domain,
                    &e.to_string(),
                    &cert.verification_method,
                )
                .await;
                return;
            }
        }

        // Step 2: Wait for the ACME server to validate the challenge
        // The proxy automatically serves the challenge token at /.well-known/acme-challenge/{token}
        // Let's Encrypt typically validates within a few seconds
        info!(
            "Waiting for Let's Encrypt to validate HTTP-01 challenge for {}...",
            cert.domain
        );
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Step 3: Complete the challenge and obtain the certificate
        match self.complete_http_challenge(&cert.domain, &email).await {
            Ok(_new_cert) => {
                info!("✅ Successfully renewed certificate for {}", cert.domain);
                report.auto_renewed.push(cert.domain.clone());
            }
            Err(e) => {
                error!("❌ Failed to complete renewal for {}: {}", cert.domain, e);
                report.renewal_failed.push(RenewalFailure {
                    domain: cert.domain.clone(),
                    error: e.to_string(),
                    verification_method: cert.verification_method.clone(),
                });
                self.send_renewal_failure_notification(
                    &cert.domain,
                    &e.to_string(),
                    &cert.verification_method,
                )
                .await;
            }
        }
    }

    /// Order-based HTTP-01 auto-renewal. Mirrors the manual `renew_domain` handler:
    /// `request_challenge` creates and persists a fresh ACME order (so the UI can act on
    /// it), then `complete_challenge` accepts the challenge and finalizes. The proxy
    /// already serves the HTTP-01 token, so no user action is required for the happy path.
    /// On failure the order is left in place for a later manual "Verify & finalize" retry.
    async fn handle_http01_renewal_order_based(
        &self,
        cert: &Certificate,
        email: &str,
        domain_service: &Arc<crate::DomainService>,
        report: &mut RenewalReport,
    ) {
        // Step 1: Create + persist a new ACME order (sets domain to `challenge_requested`).
        let challenge = match domain_service.request_challenge(&cert.domain, email).await {
            Ok(challenge) => challenge,
            Err(e) => {
                error!("❌ Failed to initiate renewal for {}: {}", cert.domain, e);
                report.renewal_failed.push(RenewalFailure {
                    domain: cert.domain.clone(),
                    error: e.to_string(),
                    verification_method: cert.verification_method.clone(),
                });
                self.send_renewal_failure_notification(
                    &cert.domain,
                    &e.to_string(),
                    &cert.verification_method,
                )
                .await;
                return;
            }
        };

        // A cached/valid authorization can yield a certificate immediately.
        if challenge.status == "completed" {
            info!("✅ Successfully renewed certificate for {}", cert.domain);
            report.auto_renewed.push(cert.domain.clone());
            return;
        }

        // Step 2: Give Let's Encrypt a moment to validate the served HTTP-01 token.
        info!(
            "Waiting for Let's Encrypt to validate HTTP-01 challenge for {}...",
            cert.domain
        );
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Step 3: Accept the challenge and finalize the persisted order.
        match domain_service.complete_challenge(&cert.domain, email).await {
            Ok(_renewed) => {
                info!("✅ Successfully renewed certificate for {}", cert.domain);
                report.auto_renewed.push(cert.domain.clone());
            }
            Err(e) => {
                // The order remains persisted (pending) and recoverable from the UI.
                error!(
                    "❌ Failed to complete renewal for {} (order left pending for manual retry): {}",
                    cert.domain, e
                );
                report.renewal_failed.push(RenewalFailure {
                    domain: cert.domain.clone(),
                    error: e.to_string(),
                    verification_method: cert.verification_method.clone(),
                });
                self.send_renewal_failure_notification(
                    &cert.domain,
                    &e.to_string(),
                    &cert.verification_method,
                )
                .await;
            }
        }
    }

    /// DNS-01 certificates auto-renew when a DNS provider manages the domain's zone
    /// (`dns_managed_domains.auto_manage = true`); otherwise they fall back to the
    /// "add this TXT record yourself" manual notification.
    async fn handle_dns01_notification(&self, cert: &Certificate, report: &mut RenewalReport) {
        if let (Some(domain_service), Some(dns_provider_service)) = (
            self.domain_service.clone(),
            self.dns_provider_service.clone(),
        ) {
            if self
                .try_dns01_renewal_with_provider(
                    cert,
                    &domain_service,
                    &dns_provider_service,
                    report,
                )
                .await
            {
                return;
            }
        }

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

    /// Order-based DNS-01 auto-renewal, attempted only when a verified, `auto_manage`
    /// DNS provider covers the certificate's base domain (see
    /// `DnsProviderService::find_provider_for_domain`). Mirrors
    /// `handle_http01_renewal_order_based`: `request_challenge` creates and persists a
    /// fresh ACME order, the DNS provider auto-publishes the `_acme-challenge` TXT
    /// record(s) (same helper the manual "Setup DNS" endpoint uses), then
    /// `complete_challenge` accepts the challenge and finalizes. On any failure the
    /// order is left in place for a manual "Verify & finalize" retry from the UI.
    ///
    /// Returns `true` once a DNS provider was found for this domain (success or
    /// failure is recorded on `report` either way) so the caller does not also send
    /// the manual-renewal notification. Returns `false` when no provider manages the
    /// domain, so the caller falls back to that manual flow.
    async fn try_dns01_renewal_with_provider(
        &self,
        cert: &Certificate,
        domain_service: &Arc<crate::DomainService>,
        dns_provider_service: &Arc<temps_dns::services::DnsProviderService>,
        report: &mut RenewalReport,
    ) -> bool {
        let (provider, managed_domain) = match dns_provider_service
            .find_provider_for_domain(&cert.domain)
            .await
        {
            Ok(Some(found)) => found,
            Ok(None) => return false,
            Err(e) => {
                warn!("Failed to look up DNS provider for {}: {}", cert.domain, e);
                return false;
            }
        };

        let authoritative_zone = managed_domain.domain;

        info!(
            "🔄 Auto-renewing DNS-01 certificate for {} via DNS provider {}",
            cert.domain, provider.name
        );

        let email = self.get_acme_email().await;

        // Step 1: Create + persist a new ACME order (sets domain to `challenge_requested`).
        let challenge = match domain_service.request_challenge(&cert.domain, &email).await {
            Ok(challenge) => challenge,
            Err(e) => {
                error!(
                    "❌ Failed to initiate DNS-01 renewal for {}: {}",
                    cert.domain, e
                );
                report.renewal_failed.push(RenewalFailure {
                    domain: cert.domain.clone(),
                    error: e.to_string(),
                    verification_method: cert.verification_method.clone(),
                });
                self.send_renewal_failure_notification(
                    &cert.domain,
                    &e.to_string(),
                    &cert.verification_method,
                )
                .await;
                return true;
            }
        };

        // A cached/valid authorization can yield a certificate immediately.
        if challenge.status == "completed" {
            info!("✅ Successfully renewed certificate for {}", cert.domain);
            report.auto_renewed.push(cert.domain.clone());
            return true;
        }

        if challenge.txt_records.is_empty() {
            let error_msg = "ACME order did not return any DNS TXT records".to_string();
            error!(
                "❌ DNS-01 renewal for {} failed: {}",
                cert.domain, error_msg
            );
            report.renewal_failed.push(RenewalFailure {
                domain: cert.domain.clone(),
                error: error_msg.clone(),
                verification_method: cert.verification_method.clone(),
            });
            self.send_renewal_failure_notification(
                &cert.domain,
                &error_msg,
                &cert.verification_method,
            )
            .await;
            return true;
        }

        // Step 2: Auto-publish the TXT record(s) via the configured DNS provider (same
        // helper used by the manual "Setup DNS" endpoint).
        let dns_txt_records: Vec<(String, String)> = challenge
            .txt_records
            .iter()
            .map(|record| (record.name.clone(), record.value.clone()))
            .collect();

        let mutations = dns_txt_records
            .iter()
            .map(|(name, value)| temps_core::DnsAutomationMutation {
                record_type: "TXT".to_string(),
                name: name.clone(),
                value: value.clone(),
            })
            .collect::<Vec<_>>();
        let authorization_request = temps_core::DnsAutomationRequest {
            purpose: temps_core::DnsAutomationPurpose::AcmeDns01,
            domain: cert.domain.clone(),
            zone: authoritative_zone.clone(),
            provider_id: provider.id,
            provider_name: provider.name.clone(),
            mutations: mutations.clone(),
        };
        let Some(gate) = &self.dns_automation_gate else {
            self.audit_dns_automation(DnsAutomationAudit {
                domain: cert.domain.clone(),
                zone: authoritative_zone.clone(),
                provider_id: provider.id,
                provider_name: provider.name.clone(),
                outcome: "denied".to_string(),
                reason: Some("no automation gate is configured".to_string()),
                mutations,
            })
            .await;
            return false;
        };
        match crate::handlers::domain_handler::authorize_dns_automation_request(
            gate.as_ref(),
            &authorization_request,
            provider.id,
        )
        .await
        {
            crate::handlers::domain_handler::DnsAutomationAuthorization::Allowed => {}
            crate::handlers::domain_handler::DnsAutomationAuthorization::Denied(reason) => {
                self.audit_dns_automation(DnsAutomationAudit {
                    domain: cert.domain.clone(),
                    zone: authoritative_zone.clone(),
                    provider_id: provider.id,
                    provider_name: provider.name.clone(),
                    outcome: "denied".to_string(),
                    reason: Some(reason.clone()),
                    mutations: mutations.clone(),
                })
                .await;
                info!(
                    "Unattended DNS-01 renewal is not authorized for {} via provider {}: {}",
                    cert.domain, provider.name, reason
                );
                return false;
            }
            crate::handlers::domain_handler::DnsAutomationAuthorization::AuthorizationError(
                reason,
            ) => {
                self.audit_dns_automation(DnsAutomationAudit {
                    domain: cert.domain.clone(),
                    zone: authoritative_zone.clone(),
                    provider_id: provider.id,
                    provider_name: provider.name.clone(),
                    outcome: "authorization_error".to_string(),
                    reason: Some(reason.clone()),
                    mutations: mutations.clone(),
                })
                .await;
                warn!(
                    "Failed to authorize unattended DNS-01 renewal for {} via provider {}: {}",
                    cert.domain, provider.name, reason
                );
                return false;
            }
        }

        // Provider construction decrypts credentials, so it must happen only
        // after the unattended mutation policy has explicitly allowed this
        // exact provider, zone, domain, and record batch.
        let provider_instance = match dns_provider_service.create_provider_instance(&provider) {
            Ok(instance) => instance,
            Err(error) => {
                warn!(
                    "Failed to initialize DNS provider {} for {} after automation authorization: {}",
                    provider.name, cert.domain, error
                );
                return false;
            }
        };
        let (results, records_created) = crate::handlers::domain_handler::setup_dns_txt_records(
            provider_instance.as_ref(),
            &authoritative_zone,
            &dns_txt_records,
        )
        .await;

        if (records_created as usize) < dns_txt_records.len() {
            let failed_detail = results
                .iter()
                .filter(|result| !result.success)
                .map(|result| format!("{}: {}", result.name, result.message))
                .collect::<Vec<_>>()
                .join("; ");
            let error_msg = format!(
                "Failed to publish {} of {} DNS TXT record(s) via provider {}: {}",
                dns_txt_records.len() - records_created as usize,
                dns_txt_records.len(),
                provider.name,
                failed_detail
            );
            error!(
                "❌ DNS-01 renewal for {} failed: {}",
                cert.domain, error_msg
            );
            report.renewal_failed.push(RenewalFailure {
                domain: cert.domain.clone(),
                error: error_msg.clone(),
                verification_method: cert.verification_method.clone(),
            });
            self.send_renewal_failure_notification(
                &cert.domain,
                &error_msg,
                &cert.verification_method,
            )
            .await;
            self.audit_dns_automation(DnsAutomationAudit {
                domain: cert.domain.clone(),
                zone: authoritative_zone.clone(),
                provider_id: provider.id,
                provider_name: provider.name.clone(),
                outcome: "publish_failed".to_string(),
                reason: Some(error_msg),
                mutations: authorization_request.mutations.clone(),
            })
            .await;
            return true;
        }

        self.audit_dns_automation(DnsAutomationAudit {
            domain: cert.domain.clone(),
            zone: authoritative_zone.clone(),
            provider_id: provider.id,
            provider_name: provider.name.clone(),
            outcome: "published".to_string(),
            reason: None,
            mutations: authorization_request.mutations.clone(),
        })
        .await;

        // Step 3: Give DNS a moment to propagate before asking Let's Encrypt to validate.
        info!(
            "Waiting for DNS propagation before validating DNS-01 challenge for {}...",
            cert.domain
        );
        tokio::time::sleep(self.dns_propagation_delay).await;

        // Step 4: Accept the challenge and finalize the persisted order.
        match domain_service
            .complete_challenge(&cert.domain, &email)
            .await
        {
            Ok(_renewed) => {
                info!("✅ Successfully renewed certificate for {}", cert.domain);
                report.auto_renewed.push(cert.domain.clone());
            }
            Err(e) => {
                // The order remains persisted (pending) and recoverable from the UI.
                error!(
                    "❌ Failed to complete DNS-01 renewal for {} (order left pending for manual retry): {}",
                    cert.domain, e
                );
                report.renewal_failed.push(RenewalFailure {
                    domain: cert.domain.clone(),
                    error: e.to_string(),
                    verification_method: cert.verification_method.clone(),
                });
                self.send_renewal_failure_notification(
                    &cert.domain,
                    &e.to_string(),
                    &cert.verification_method,
                )
                .await;
            }
        }

        true
    }

    async fn send_renewal_summary(&self, report: &RenewalReport) {
        if report.total_checked == 0 {
            return;
        }

        let Some(alarm_service) = self.alarm_service.get() else {
            return;
        };

        // Only fire an alarm when the cycle produced something actionable —
        // an all-green summary isn't worth persisting as an alarm.
        if report.renewal_failed.is_empty() && report.manual_action_needed.is_empty() {
            return;
        }

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

        // Renewal cycles run on a schedule shared across every domain on the
        // host, so this fires as a single system-wide alarm (no per-domain
        // scope column exists on `alarms`) — same known limitation as the
        // disk-space monitor's cooldown bucket.
        let request = FireAlarmRequest {
            project_id: None,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::TlsRenewalFailed,
            severity: if report.renewal_failed.is_empty() {
                AlarmSeverity::Warning
            } else {
                AlarmSeverity::Critical
            },
            title: "Certificate Renewal Report".to_string(),
            message,
            metadata: Some(serde_json::json!({
                "auto_renewed": report.auto_renewed.len(),
                "failed": report.renewal_failed.len(),
                "manual_needed": report.manual_action_needed.len(),
            })),
        };

        match alarm_service.fire_alarm(request).await {
            Ok(_) => {}
            Err(e) => error!("Failed to fire renewal summary alarm: {}", e),
        }
    }

    async fn send_renewal_failure_notification(
        &self,
        domain: &str,
        error: &str,
        verification_method: &str,
    ) {
        let Some(alarm_service) = self.alarm_service.get() else {
            return;
        };

        let request = FireAlarmRequest {
            project_id: None,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::TlsRenewalFailed,
            severity: AlarmSeverity::Critical,
            title: format!("Certificate Renewal Failed: {}", domain),
            message: format!(
                "Failed to automatically renew certificate for {}.\n\nError: {}\n\nPlease renew this certificate manually in the Temps dashboard.",
                domain, error
            ),
            metadata: Some(serde_json::json!({
                "domain": domain,
                "error": error,
                "verification_method": verification_method,
            })),
        };

        if let Err(e) = alarm_service.fire_alarm(request).await {
            error!("Failed to fire renewal failure alarm: {}", e);
        }
    }

    async fn send_manual_renewal_notification(&self, cert: &Certificate) {
        let Some(alarm_service) = self.alarm_service.get() else {
            return;
        };

        let days_remaining = cert.days_until_expiry();

        let cert_type = if cert.is_wildcard {
            "wildcard certificate"
        } else {
            "certificate"
        };

        let request = FireAlarmRequest {
            project_id: None,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::TlsCertExpiring,
            severity: if days_remaining <= 7 {
                AlarmSeverity::Critical
            } else if days_remaining <= 14 {
                AlarmSeverity::Warning
            } else {
                AlarmSeverity::Info
            },
            title: format!("Action Required: Renew Certificate for {}", cert.domain),
            message: format!(
                "Your {} for {} will expire in {} days.\n\nSince this is a DNS-01 certificate, you need to manually renew it:\n1. Go to Temps Dashboard → Domains → {}\n2. Click 'Renew Certificate'\n3. Add the provided DNS TXT record\n4. Click 'Finalize Renewal'\n\nYour current certificate remains active during renewal.",
                cert_type,
                cert.domain,
                days_remaining,
                cert.domain
            ),
            metadata: Some(serde_json::json!({
                "domain": cert.domain,
                "expires_at": cert.expiration_time.to_rfc3339(),
                "days_remaining": days_remaining,
                "verification_method": "dns-01",
                "is_wildcard": cert.is_wildcard,
            })),
        };

        if let Err(e) = alarm_service.fire_alarm(request).await {
            error!("Failed to fire manual renewal alarm: {}", e);
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
        use hickory_resolver::proto::rr::{RData, RecordType};

        let mut a_records = Vec::new();
        let mut aaaa_records = Vec::new();
        let mut error = None;

        // Try IPv4 lookup. The generic `lookup` returns a `Lookup`; pull the
        // A rdata out of each answer record (hickory 0.26).
        match self.resolver.lookup(domain, RecordType::A).await {
            Ok(lookup) => {
                a_records = lookup
                    .answers()
                    .iter()
                    .filter_map(|record| match &record.data {
                        RData::A(a) => Some(a.0.to_string()),
                        _ => None,
                    })
                    .collect();
            }
            Err(e) => {
                error = Some(format!("IPv4 lookup failed: {}", e));
            }
        }

        // Try IPv6 lookup (if IPv4 succeeded or failed, we still try IPv6)
        match self.resolver.lookup(domain, RecordType::AAAA).await {
            Ok(lookup) => {
                aaaa_records = lookup
                    .answers()
                    .iter()
                    .filter_map(|record| match &record.data {
                        RData::AAAA(aaaa) => Some(aaaa.0.to_string()),
                        _ => None,
                    })
                    .collect();
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

    /// Start the certificate renewal scheduler
    ///
    /// This runs continuously, checking for expiring certificates once per day at 3:00 AM.
    /// - HTTP-01 certificates: Auto-renewed
    /// - DNS-01 certificates: Notification sent for manual renewal
    ///
    /// The scheduler will continue running until the cancellation token is triggered.
    pub async fn start_certificate_renewal_scheduler(
        &self,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), TlsError> {
        use chrono::Timelike;
        use tokio::time;

        info!("Starting certificate renewal scheduler");

        // Run initial check on startup
        match self.check_and_renew_certificates(30).await {
            Ok(report) => {
                if report.total_checked > 0 {
                    info!(
                        "Initial certificate check: {} checked, {} renewed, {} failed, {} manual",
                        report.total_checked,
                        report.auto_renewed.len(),
                        report.renewal_failed.len(),
                        report.manual_action_needed.len()
                    );
                }
            }
            Err(e) => {
                error!("Initial certificate renewal check failed: {}", e);
            }
        }

        loop {
            let now = chrono::Utc::now();

            // Calculate time until next 3:00 AM UTC
            let next_run = if now.hour() >= 3 {
                // Next 3 AM is tomorrow
                (now + chrono::Duration::days(1))
                    .with_hour(3)
                    .unwrap()
                    .with_minute(0)
                    .unwrap()
                    .with_second(0)
                    .unwrap()
                    .with_nanosecond(0)
                    .unwrap()
            } else {
                // Next 3 AM is today
                now.with_hour(3)
                    .unwrap()
                    .with_minute(0)
                    .unwrap()
                    .with_second(0)
                    .unwrap()
                    .with_nanosecond(0)
                    .unwrap()
            };

            let sleep_duration = next_run - now;
            let sleep_secs = sleep_duration.num_seconds().max(0) as u64;

            info!(
                "Next certificate renewal check scheduled for {} (in {} hours)",
                next_run.format("%Y-%m-%d %H:%M:%S UTC"),
                sleep_duration.num_hours()
            );

            tokio::select! {
                _ = time::sleep(time::Duration::from_secs(sleep_secs)) => {
                    info!("Running scheduled certificate renewal check");

                    match self.check_and_renew_certificates(30).await {
                        Ok(report) => {
                            info!(
                                "Certificate renewal check: {} checked, {} renewed, {} failed, {} manual",
                                report.total_checked,
                                report.auto_renewed.len(),
                                report.renewal_failed.len(),
                                report.manual_action_needed.len()
                            );
                        }
                        Err(e) => {
                            error!("Certificate renewal check failed: {}", e);
                        }
                    }
                }
                _ = cancellation_token.cancelled() => {
                    info!("Certificate renewal scheduler shutting down");
                    return Ok(());
                }
            }
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
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex;

    #[test]
    fn dns_automation_audit_redacts_acme_values() {
        let audit = DnsAutomationAudit {
            domain: "example.com".to_string(),
            zone: "example.com".to_string(),
            provider_id: 7,
            provider_name: "production-dns".to_string(),
            outcome: "published".to_string(),
            reason: None,
            mutations: vec![temps_core::DnsAutomationMutation {
                record_type: "TXT".to_string(),
                name: "_acme-challenge.example.com".to_string(),
                value: "super-secret-acme-token".to_string(),
            }],
        };

        let serialized = AuditOperation::serialize(&audit).unwrap();

        assert!(!serialized.contains("super-secret-acme-token"));
        assert!(serialized.contains("[REDACTED]"));
        assert!(serialized.contains("_acme-challenge.example.com"));
    }

    #[derive(Default)]
    struct DenyingDnsAutomationGate {
        requests: Mutex<Vec<temps_core::DnsAutomationRequest>>,
    }

    #[async_trait::async_trait]
    impl temps_core::DnsAutomationGate for DenyingDnsAutomationGate {
        async fn authorize(
            &self,
            request: &temps_core::DnsAutomationRequest,
        ) -> Result<temps_core::DnsAutomationDecision, temps_core::DnsAutomationError> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(temps_core::DnsAutomationDecision::Deny {
                reason: "scheduler principal lacks dns:automation:write".to_string(),
            })
        }
    }

    struct ErroringDnsAutomationGate;

    #[async_trait::async_trait]
    impl temps_core::DnsAutomationGate for ErroringDnsAutomationGate {
        async fn authorize(
            &self,
            request: &temps_core::DnsAutomationRequest,
        ) -> Result<temps_core::DnsAutomationDecision, temps_core::DnsAutomationError> {
            Err(temps_core::DnsAutomationError::policy_evaluation_failed(
                request,
                "policy store offline",
            ))
        }
    }

    struct AllowingDnsAutomationGate;

    #[async_trait::async_trait]
    impl temps_core::DnsAutomationGate for AllowingDnsAutomationGate {
        async fn authorize(
            &self,
            _request: &temps_core::DnsAutomationRequest,
        ) -> Result<temps_core::DnsAutomationDecision, temps_core::DnsAutomationError> {
            Ok(temps_core::DnsAutomationDecision::Allow)
        }
    }

    #[derive(Default)]
    struct RecordingAuditLogger {
        operations: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            self.operations
                .lock()
                .unwrap()
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    struct DnsRenewalCertificateProvider {
        completion_calls: AtomicUsize,
    }

    fn dns_renewal_test_server_config(data_dir: std::path::PathBuf) -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:0".to_string(),
            database_url: "postgres://unused".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:0".to_string(),
            console_admin_address: None,
            admin_allowed_ips: vec![],
            admin_allowed_hosts: vec![],
            admin_trust_forwarded_for: false,
            data_dir,
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: vec![],
        }
    }

    #[async_trait::async_trait]
    impl CertificateProvider for DnsRenewalCertificateProvider {
        async fn provision(
            &self,
            domain: &str,
            challenge: ChallengeType,
            _email: &str,
        ) -> Result<ProvisioningResult, ProviderError> {
            assert_eq!(challenge, ChallengeType::Dns01);
            Ok(ProvisioningResult::Challenge(ChallengeData {
                challenge_type: ChallengeType::Dns01,
                domain: domain.to_string(),
                token: "order-token".to_string(),
                key_authorization: "key-authorization".to_string(),
                validation_url: Some("https://acme.test/challenge/1".to_string()),
                dns_txt_records: vec![crate::tls::models::DnsTxtRecord {
                    name: format!("_acme-challenge.{domain}"),
                    value: "secret-acme-proof".to_string(),
                    validation_url: "https://acme.test/challenge/1".to_string(),
                }],
                order_url: Some("https://acme.test/order/1".to_string()),
            }))
        }

        async fn complete_challenge(
            &self,
            domain: &str,
            _challenge_data: &ChallengeData,
            _email: &str,
        ) -> Result<Certificate, ProviderError> {
            self.completion_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(Certificate {
                id: 1,
                domain: domain.to_string(),
                certificate_pem: "certificate".to_string(),
                private_key_pem: "private-key".to_string(),
                expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
                last_renewed: Some(chrono::Utc::now()),
                is_wildcard: false,
                verification_method: "dns-01".to_string(),
                status: CertificateStatus::Active,
            })
        }

        fn supported_challenges(&self) -> Vec<ChallengeType> {
            vec![ChallengeType::Dns01]
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
    #[serial_test::serial(dns_renewal_db)]
    async fn test_dns01_renewal_policy_failures_precede_provider_credential_decryption() {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_core::AppSettings;
        use temps_entities::{dns_managed_domains, dns_providers, settings};

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Docker unavailable; skipping DNS policy ordering test: {error}");
                return;
            }
            Err(error) => panic!("failed to create test database: {error}"),
        };
        let db = test_db.db.clone();
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "dns-policy-ordering-test",
        ));
        let repository = Arc::new(MockCertificateRepository::new());
        let certificate_provider = Arc::new(DnsRenewalCertificateProvider {
            completion_calls: AtomicUsize::new(0),
        });
        let domain_service = Arc::new(crate::DomainService::new(
            db.clone(),
            certificate_provider.clone(),
            repository.clone(),
            encryption.clone(),
        ));
        let dns_provider_service = Arc::new(temps_dns::services::DnsProviderService::new(
            db.clone(),
            encryption.clone(),
        ));
        let mut app_settings = AppSettings::default();
        app_settings.letsencrypt.email = Some("acme@example.com".to_string());
        settings::ActiveModel {
            id: Set(1),
            data: Set(serde_json::to_value(app_settings).unwrap()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert ACME settings");
        let config_dir = std::env::temp_dir().join(format!(
            "temps-dns-policy-ordering-config-{}",
            uuid::Uuid::new_v4()
        ));
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(dns_renewal_test_server_config(config_dir)),
            db.clone(),
        ));

        struct PolicyCase {
            label: &'static str,
            gate: Option<Arc<dyn temps_core::DnsAutomationGate>>,
            expected_reason: &'static str,
        }
        let cases = [
            PolicyCase {
                label: "missing",
                gate: None,
                expected_reason: "no automation gate is configured",
            },
            PolicyCase {
                label: "denied",
                gate: Some(Arc::new(DenyingDnsAutomationGate::default())),
                expected_reason: "automation policy denied the request",
            },
            PolicyCase {
                label: "error",
                gate: Some(Arc::new(ErroringDnsAutomationGate)),
                expected_reason: "automation policy evaluation failed",
            },
        ];

        for PolicyCase {
            label,
            gate,
            expected_reason,
        } in cases
        {
            let zone = format!("{label}.example.com");
            let domain = format!("app.{zone}");
            let persisted_domain = domain_service
                .create_domain(&domain, "dns-01")
                .await
                .expect("create DNS-01 domain for policy-ordering case");
            let provider = dns_providers::ActiveModel {
                name: Set(format!("{label}-provider")),
                provider_type: Set("cloudflare".to_string()),
                // A policy-ordering regression tries to decrypt this and fails
                // before it can produce the expected policy/manual outcome.
                credentials: Set("not-valid-ciphertext".to_string()),
                is_active: Set(true),
                description: Set(None),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("insert active provider");
            dns_managed_domains::ActiveModel {
                provider_id: Set(provider.id),
                domain: Set(zone),
                auto_manage: Set(true),
                verified: Set(true),
                generated_hostname_mode: Set("standard".to_string()),
                sync_generated_records: Set(false),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("insert eligible managed zone");

            let audit = Arc::new(RecordingAuditLogger::default());
            let mut service = TlsService::new(repository.clone(), certificate_provider.clone())
                .with_config_service(config_service.clone())
                .with_domain_service(domain_service.clone())
                .with_dns_provider_service(dns_provider_service.clone())
                .with_audit_logger(audit.clone());
            if let Some(gate) = gate {
                service = service.with_dns_automation_gate(gate);
            }
            let certificate = Certificate {
                id: persisted_domain.id,
                domain: domain.clone(),
                certificate_pem: "old-certificate".to_string(),
                private_key_pem: "old-private-key".to_string(),
                expiration_time: chrono::Utc::now() + chrono::Duration::days(7),
                last_renewed: None,
                is_wildcard: false,
                verification_method: "dns-01".to_string(),
                status: CertificateStatus::Active,
            };
            let mut report = RenewalReport {
                total_checked: 1,
                auto_renewed: vec![],
                renewal_failed: vec![],
                manual_action_needed: vec![],
            };

            service
                .handle_dns01_notification(&certificate, &mut report)
                .await;

            assert_eq!(report.manual_action_needed.len(), 1, "case {label}");
            assert_eq!(
                report.manual_action_needed[0].domain, domain,
                "case {label}"
            );
            let operations = audit.operations.lock().unwrap();
            assert_eq!(operations.len(), 1, "case {label}");
            assert!(operations[0].1.contains(expected_reason), "case {label}");
        }
    }

    #[tokio::test]
    #[serial_test::serial(dns_renewal_db)]
    async fn test_check_and_renew_certificates_dns01_denied_gate_falls_back_to_manual_action() {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_core::AppSettings;
        use temps_dns::providers::{CloudflareCredentials, DnsProviderType, ProviderCredentials};
        use temps_dns::services::{
            AddManagedDomainRequest, CreateProviderRequest, DnsProviderService,
        };
        use temps_entities::{dns_managed_domains, settings};

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Docker unavailable; skipping DNS renewal regression test: {error}");
                return;
            }
            Err(error) => panic!("failed to create test database: {error}"),
        };
        let db = test_db.db.clone();
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "dns-renewal-test",
        ));
        let repository = Arc::new(DefaultCertificateRepository::new(
            db.clone(),
            encryption.clone(),
        ));
        let certificate_provider = Arc::new(DnsRenewalCertificateProvider {
            completion_calls: AtomicUsize::new(0),
        });
        let domain_service = Arc::new(crate::DomainService::new(
            db.clone(),
            certificate_provider.clone(),
            repository.clone(),
            encryption.clone(),
        ));
        let domain = domain_service
            .create_domain("app.example.com", "dns-01")
            .await
            .expect("create renewal domain");

        let mut app_settings = AppSettings::default();
        app_settings.letsencrypt.email = Some("acme@example.com".to_string());
        settings::ActiveModel {
            id: Set(1),
            data: Set(serde_json::to_value(app_settings).unwrap()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert ACME settings");

        let config_dir =
            std::env::temp_dir().join(format!("temps-dns-renewal-config-{}", uuid::Uuid::new_v4()));
        let server_config = dns_renewal_test_server_config(config_dir.clone());
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            db.clone(),
        ));

        let dns_provider_service =
            Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
        let provider = dns_provider_service
            .create(CreateProviderRequest {
                name: "production-dns".to_string(),
                provider_type: DnsProviderType::Cloudflare,
                credentials: ProviderCredentials::Cloudflare(CloudflareCredentials {
                    api_token: "test-token".to_string(),
                    account_id: None,
                }),
                description: None,
            })
            .await
            .expect("create provider");
        let managed = dns_provider_service
            .add_managed_domain(
                provider.id,
                AddManagedDomainRequest {
                    domain: "example.com".to_string(),
                    auto_manage: true,
                    generated_hostname_mode: None,
                    sync_generated_records: false,
                },
            )
            .await
            .expect("add managed zone");
        let mut managed_active: dns_managed_domains::ActiveModel = managed.into();
        managed_active.verified = Set(true);
        managed_active
            .update(db.as_ref())
            .await
            .expect("verify managed zone");

        let gate = Arc::new(DenyingDnsAutomationGate::default());
        let audit = Arc::new(RecordingAuditLogger::default());
        let service = TlsService::new(repository.clone(), certificate_provider.clone())
            .with_config_service(config_service)
            .with_domain_service(domain_service.clone())
            .with_dns_provider_service(dns_provider_service.clone())
            .with_dns_automation_gate(gate.clone())
            .with_audit_logger(audit.clone());
        let certificate = Certificate {
            id: domain.id,
            domain: domain.domain,
            certificate_pem: "old-certificate".to_string(),
            private_key_pem: "old-private-key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(7),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "dns-01".to_string(),
            status: CertificateStatus::Active,
        };
        repository
            .save_certificate(certificate)
            .await
            .expect("persist expiring DNS-01 certificate");

        let report = service
            .check_and_renew_certificates(30)
            .await
            .expect("run scheduled certificate renewal");

        assert_eq!(report.total_checked, 1);
        assert!(report.auto_renewed.is_empty());
        assert!(report.renewal_failed.is_empty());
        assert_eq!(report.manual_action_needed.len(), 1);
        assert_eq!(report.manual_action_needed[0].domain, "app.example.com");
        assert_eq!(
            certificate_provider
                .completion_calls
                .load(AtomicOrdering::SeqCst),
            0
        );
        let pending_order = repository
            .find_acme_order_by_domain(domain.id)
            .await
            .expect("query pending order")
            .expect("challenge request must remain recoverable");
        assert_eq!(pending_order.status, "pending");

        let requests = gate.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].domain, "app.example.com");
        assert_eq!(requests[0].zone, "example.com");
        assert_eq!(requests[0].provider_id, provider.id);
        assert_eq!(requests[0].mutations[0].value, "secret-acme-proof");
        drop(requests);

        let operations = audit.operations.lock().unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "DNS_AUTOMATION_ACME_DNS01");
        assert!(operations[0].1.contains("\"outcome\":\"denied\""));
        assert!(operations[0]
            .1
            .contains("automation policy denied the request"));
        assert!(operations[0].1.contains("[REDACTED]"));
        assert!(!operations[0].1.contains("secret-acme-proof"));

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[tokio::test]
    #[serial_test::serial(dns_renewal_db)]
    async fn test_try_dns01_renewal_with_provider_publishes_and_finalizes_certificate() {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_core::AppSettings;
        use temps_dns::providers::{DnsProviderType, PebbleCredentials, ProviderCredentials};
        use temps_dns::services::{
            AddManagedDomainRequest, CreateProviderRequest, DnsProviderService,
        };
        use temps_entities::{dns_managed_domains, settings};
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Docker unavailable; skipping DNS renewal regression test: {error}");
                return;
            }
            Err(error) => panic!("failed to create test database: {error}"),
        };
        let dns_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/clear-txt"))
            .and(body_json(serde_json::json!({
                "host": "_acme-challenge.app.example.com."
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&dns_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/set-txt"))
            .and(body_json(serde_json::json!({
                "host": "_acme-challenge.app.example.com.",
                "value": "secret-acme-proof"
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&dns_server)
            .await;

        let db = test_db.db.clone();
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "dns-renewal-success-test",
        ));
        let repository = Arc::new(DefaultCertificateRepository::new(
            db.clone(),
            encryption.clone(),
        ));
        let certificate_provider = Arc::new(DnsRenewalCertificateProvider {
            completion_calls: AtomicUsize::new(0),
        });
        let domain_service = Arc::new(crate::DomainService::new(
            db.clone(),
            certificate_provider.clone(),
            repository.clone(),
            encryption.clone(),
        ));
        let domain = domain_service
            .create_domain("app.example.com", "dns-01")
            .await
            .expect("create renewal domain");

        let mut app_settings = AppSettings::default();
        app_settings.letsencrypt.email = Some("acme@example.com".to_string());
        settings::ActiveModel {
            id: Set(1),
            data: Set(serde_json::to_value(app_settings).unwrap()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert ACME settings");
        let config_dir = std::env::temp_dir().join(format!(
            "temps-dns-renewal-success-config-{}",
            uuid::Uuid::new_v4()
        ));
        let server_config = dns_renewal_test_server_config(config_dir.clone());
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            db.clone(),
        ));

        let dns_provider_service =
            Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
        let provider = dns_provider_service
            .create(CreateProviderRequest {
                name: "pebble-dns".to_string(),
                provider_type: DnsProviderType::Pebble,
                credentials: ProviderCredentials::Pebble(PebbleCredentials {
                    management_url: dns_server.uri(),
                }),
                description: None,
            })
            .await
            .expect("create provider");
        let managed = dns_provider_service
            .add_managed_domain(
                provider.id,
                AddManagedDomainRequest {
                    domain: "example.com".to_string(),
                    auto_manage: true,
                    generated_hostname_mode: None,
                    sync_generated_records: false,
                },
            )
            .await
            .expect("add managed zone");
        let mut managed_active: dns_managed_domains::ActiveModel = managed.into();
        managed_active.verified = Set(true);
        managed_active
            .update(db.as_ref())
            .await
            .expect("verify managed zone");

        let audit = Arc::new(RecordingAuditLogger::default());
        let mut service = TlsService::new(repository, certificate_provider.clone())
            .with_config_service(config_service)
            .with_dns_automation_gate(Arc::new(AllowingDnsAutomationGate))
            .with_audit_logger(audit.clone());
        service.dns_propagation_delay = tokio::time::Duration::ZERO;
        let certificate = Certificate {
            id: domain.id,
            domain: domain.domain,
            certificate_pem: "old-certificate".to_string(),
            private_key_pem: "old-private-key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(7),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "dns-01".to_string(),
            status: CertificateStatus::Active,
        };
        std::env::set_var("TEMPS_ALLOW_PEBBLE_PROVIDER", "1");
        let task = tokio::spawn(async move {
            let mut report = RenewalReport {
                total_checked: 1,
                auto_renewed: vec![],
                renewal_failed: vec![],
                manual_action_needed: vec![],
            };
            let handled = service
                .try_dns01_renewal_with_provider(
                    &certificate,
                    &domain_service,
                    &dns_provider_service,
                    &mut report,
                )
                .await;
            (handled, report)
        });
        let (handled, report) = task.await.expect("renewal task");
        std::env::remove_var("TEMPS_ALLOW_PEBBLE_PROVIDER");

        assert!(handled);
        assert_eq!(report.auto_renewed, vec!["app.example.com"]);
        assert!(report.renewal_failed.is_empty());
        assert!(report.manual_action_needed.is_empty());
        assert_eq!(
            certificate_provider
                .completion_calls
                .load(AtomicOrdering::SeqCst),
            1
        );
        let operations = audit.operations.lock().unwrap();
        assert_eq!(operations.len(), 1);
        assert!(operations[0].1.contains("\"outcome\":\"published\""));
        assert!(operations[0].1.contains("[REDACTED]"));
        assert!(!operations[0].1.contains("secret-acme-proof"));

        let _ = std::fs::remove_dir_all(config_dir);
    }
    use crate::tls::errors::ProviderError;
    use crate::tls::models::{
        Certificate, CertificateFilter, CertificateStatus, ChallengeData, ChallengeType,
        DnsChallengeData, ProvisioningResult, ValidationResult,
    };
    use crate::tls::providers::CertificateProvider;
    use crate::tls::repository::test_utils::MockCertificateRepository;
    use crate::tls::repository::DefaultCertificateRepository;
    use temps_core::{Job, JobQueue};
    use temps_database::test_utils::TestDatabase;

    #[tokio::test]
    async fn test_builder_pattern() {
        // Create mock components
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
    #[allow(dead_code)]
    struct MockJobQueue;

    #[allow(dead_code)]
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

    // ============================================================================
    // Pebble Integration Tests (require Docker)
    // ============================================================================

    use crate::tls::providers::LetsEncryptProvider;
    use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

    /// Test HTTP-01 challenge flow with Pebble ACME server
    /// Requires Docker to be running
    ///
    /// Note: This test is ignored by default because it requires additional setup:
    /// 1. Pebble uses self-signed certificates that instant-acme cannot validate
    /// 2. To run this test, you need to configure system trust for Pebble's CA cert
    /// 3. Or modify instant-acme to skip TLS verification (not recommended)
    ///
    /// For full integration testing with real validation:
    /// 1. Start pebble-challtestsrv for DNS resolution
    /// 2. Run an HTTP server on port 80 serving .well-known/acme-challenge/<token>
    /// 3. Configure DNS to resolve test domains to the challenge server
    ///
    /// To run: cargo test --lib -p temps-domains test_http01_with_pebble -- --ignored
    #[tokio::test]
    #[ignore = "Requires Pebble CA certificate trust setup"]
    async fn test_http01_with_pebble() {
        // Start Pebble container with validation always passing (for testing)
        let container = GenericImage::new("letsencrypt/pebble", "latest")
            .with_env_var("PEBBLE_VA_ALWAYS_VALID", "1")
            .with_env_var("PEBBLE_VA_NOSLEEP", "1")
            .start()
            .await
            .expect("Failed to start Pebble container");

        // Get the mapped port for ACME endpoint
        let acme_port = container
            .get_host_port_ipv4(14000)
            .await
            .expect("Failed to get Pebble port");

        // Wait for Pebble to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Setup test environment
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        // Configure provider to use Pebble
        std::env::set_var(
            "ACME_DIRECTORY_URL",
            format!("https://localhost:{}/dir", acme_port),
        );
        std::env::set_var("LETSENCRYPT_MODE", "staging");

        let provider = Arc::new(LetsEncryptProvider::new(repo.clone()));

        let test_domain = "test.example.com";
        let test_email = "test@example.com";

        // Step 1: Provision - initiates HTTP-01 challenge
        let result = provider
            .provision(test_domain, ChallengeType::Http01, test_email)
            .await;

        assert!(
            result.is_ok(),
            "Provision should succeed: {:?}",
            result.as_ref().err()
        );
        let challenge_data = match result.unwrap() {
            ProvisioningResult::Challenge(challenge) => {
                assert_eq!(challenge.challenge_type, ChallengeType::Http01);
                assert!(!challenge.token.is_empty());
                assert!(!challenge.key_authorization.is_empty());
                challenge
            }
            _ => panic!("Expected challenge result"),
        };

        // Step 2: Complete challenge - in real scenario, the token would be served via HTTP
        // For testing with PEBBLE_VA_ALWAYS_VALID=1, Pebble will accept without checking
        let cert_result = provider
            .complete_challenge(test_domain, &challenge_data, test_email)
            .await;

        assert!(cert_result.is_ok(), "Challenge completion should succeed");
        let certificate = cert_result.unwrap();

        // Verify certificate was issued
        assert_eq!(certificate.domain, test_domain);
        assert!(!certificate.certificate_pem.is_empty());
        assert!(!certificate.private_key_pem.is_empty());
        assert_eq!(certificate.status, CertificateStatus::Active);
        assert_eq!(certificate.verification_method, "http-01");

        // Cleanup
        std::mem::drop(container);
        println!("✅ HTTP-01 Pebble integration test passed!");
        println!("   - Certificate issued for: {}", test_domain);
        println!("   - Status: {:?}", certificate.status);
    }

    /// Test DNS-01 challenge for wildcard domains with Pebble
    /// Requires Docker to be running
    ///
    /// Note: This test is ignored by default because it requires additional setup:
    /// 1. Pebble uses self-signed certificates that instant-acme cannot validate
    /// 2. For full DNS validation testing, you need pebble-challtestsrv
    ///
    /// To run: cargo test --lib -p temps-domains test_dns01_wildcard_with_pebble -- --ignored
    #[tokio::test]
    #[ignore = "Requires Pebble CA certificate trust setup"]
    async fn test_dns01_wildcard_with_pebble() {
        // Start Pebble container with validation always passing
        let container = GenericImage::new("letsencrypt/pebble", "latest")
            .with_env_var("PEBBLE_VA_ALWAYS_VALID", "1")
            .with_env_var("PEBBLE_VA_NOSLEEP", "1")
            .start()
            .await
            .expect("Failed to start Pebble container");

        let acme_port = container
            .get_host_port_ipv4(14000)
            .await
            .expect("Failed to get Pebble port");

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Setup
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        std::env::set_var(
            "ACME_DIRECTORY_URL",
            format!("https://localhost:{}/dir", acme_port),
        );

        let provider = Arc::new(LetsEncryptProvider::new(repo));

        let wildcard_domain = "*.wildcard.example.com";
        let test_email = "test@example.com";

        // Step 1: Provision wildcard certificate (must use DNS-01)
        let result = provider
            .provision(wildcard_domain, ChallengeType::Dns01, test_email)
            .await;

        assert!(result.is_ok(), "Provision should succeed");
        let challenge_data = match result.unwrap() {
            ProvisioningResult::Challenge(challenge) => {
                assert_eq!(challenge.challenge_type, ChallengeType::Dns01);
                assert!(!challenge.dns_txt_records.is_empty());

                // Verify DNS TXT record format
                for record in &challenge.dns_txt_records {
                    assert!(record.name.starts_with("_acme-challenge."));
                    assert!(!record.value.is_empty());
                }
                challenge
            }
            _ => panic!("Expected challenge result"),
        };

        // Step 2: Complete challenge - in real scenario, DNS TXT records would be published
        // For testing with PEBBLE_VA_ALWAYS_VALID=1, Pebble will accept without checking
        let cert_result = provider
            .complete_challenge(wildcard_domain, &challenge_data, test_email)
            .await;

        assert!(cert_result.is_ok(), "Challenge completion should succeed");
        let certificate = cert_result.unwrap();

        // Verify wildcard certificate was issued
        assert_eq!(certificate.domain, wildcard_domain);
        assert!(!certificate.certificate_pem.is_empty());
        assert!(!certificate.private_key_pem.is_empty());
        assert_eq!(certificate.status, CertificateStatus::Active);
        assert_eq!(certificate.verification_method, "dns-01");
        assert!(certificate.is_wildcard);

        // Cleanup
        std::mem::drop(container);
        println!("✅ DNS-01 wildcard Pebble integration test passed!");
        println!("   - Wildcard certificate issued for: {}", wildcard_domain);
        println!("   - Status: {:?}", certificate.status);
    }

    /// Test that HTTP-01 is rejected for wildcard domains
    /// Requires Docker to be running
    ///
    /// Validates that the provider correctly rejects HTTP-01 challenges for wildcard domains
    /// since RFC 8555 requires DNS-01 for wildcards.
    ///
    /// Note: This test is ignored by default because it requires Pebble CA certificate trust setup.
    /// To run: cargo test --lib -p temps-domains test_http01_rejected_for_wildcard_with_pebble -- --ignored
    #[tokio::test]
    #[ignore = "Requires Pebble CA certificate trust setup"]
    async fn test_http01_rejected_for_wildcard_with_pebble() {
        let container = GenericImage::new("letsencrypt/pebble", "latest")
            .with_env_var("PEBBLE_VA_ALWAYS_VALID", "1")
            .with_env_var("PEBBLE_VA_NOSLEEP", "1")
            .start()
            .await
            .expect("Failed to start Pebble container");

        let acme_port = container
            .get_host_port_ipv4(14000)
            .await
            .expect("Failed to get Pebble port");

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        std::env::set_var(
            "ACME_DIRECTORY_URL",
            format!("https://localhost:{}/dir", acme_port),
        );

        let provider = Arc::new(LetsEncryptProvider::new(repo));

        let wildcard_domain = "*.blocked.example.com";
        let test_email = "test@example.com";

        // Try HTTP-01 with wildcard (should be rejected by our validation)
        let result = provider
            .provision(wildcard_domain, ChallengeType::Http01, test_email)
            .await;

        assert!(result.is_err(), "HTTP-01 should be rejected for wildcards");
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("wildcard") || error_msg.contains("DNS-01"),
            "Error should mention wildcard or DNS-01, got: {}",
            error_msg
        );

        std::mem::drop(container);
        println!("✅ HTTP-01 wildcard rejection test passed!");
        println!("   - Correctly rejected HTTP-01 for wildcard domain");
    }

    /// Test certificate expiration and renewal detection
    #[tokio::test]
    async fn test_certificate_expiration_and_renewal() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));
        let provider = Arc::new(MockCertificateProvider::new());
        let service = TlsService::new(repo.clone(), provider);

        // Create certificate expiring soon
        let expiring_cert = Certificate {
            id: 1,
            domain: "expiring.example.com".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_pem: "key".to_string(),
            expiration_time: chrono::Utc::now() + chrono::Duration::days(15),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            status: CertificateStatus::Active,
        };

        repo.save_certificate(expiring_cert.clone()).await.unwrap();

        // Check if renewal is needed
        let needs_renewal = service.needs_renewal(&expiring_cert.domain).await.unwrap();
        assert!(needs_renewal, "Certificate should need renewal");

        // Find expiring certificates
        let expiring = repo.find_expiring_certificates(30).await.unwrap();
        assert_eq!(expiring.len(), 1);
        assert_eq!(expiring[0].domain, "expiring.example.com");

        println!("✅ Certificate expiration detection test passed!");
    }

    /// Test ACME account persistence across sessions
    #[tokio::test]
    async fn test_acme_account_persistence() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));

        let account = AcmeAccount {
            email: "persist@example.com".to_string(),
            environment: "staging".to_string(),
            credentials: r#"{"id":"account123","key":"secret"}"#.to_string(),
            created_at: chrono::Utc::now(),
        };

        // Save account
        repo.save_acme_account(account.clone()).await.unwrap();

        // Retrieve account
        let retrieved = repo
            .find_acme_account("persist@example.com", "staging")
            .await
            .unwrap()
            .expect("Account should exist");

        assert_eq!(retrieved.email, account.email);
        assert_eq!(retrieved.environment, account.environment);
        assert!(retrieved.credentials.contains("account123"));

        // Different environment should not exist
        let not_found = repo
            .find_acme_account("persist@example.com", "production")
            .await
            .unwrap();
        assert!(not_found.is_none());

        println!("✅ ACME account persistence test passed!");
    }

    fn test_server_config() -> Arc<temps_config::ServerConfig> {
        Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgres://test:test@localhost/test".to_string(),
                None,
                None,
            )
            .expect("failed to build test ServerConfig"),
        )
    }

    /// Regression test for the auto-renewal bug: `TlsService` built without
    /// `.with_config_service(...)` always resolves an empty ACME contact email
    /// (`get_acme_email` returns `String::new()`), so every background renewal
    /// failed with "User email is required for Let's Encrypt certificate
    /// provisioning" even when an operator had configured `letsencrypt.email`.
    #[tokio::test]
    async fn test_get_acme_email_reads_configured_letsencrypt_email() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let repo = Arc::new(DefaultCertificateRepository::new(
            db.clone(),
            encryption_service,
        ));
        let provider = Arc::new(MockCertificateProvider::new());

        let config_service = Arc::new(temps_config::ConfigService::new(test_server_config(), db));
        config_service
            .update_settings(temps_core::AppSettings {
                letsencrypt: temps_core::LetsEncryptSettings {
                    email: Some("ops@example.com".to_string()),
                    environment: "production".to_string(),
                },
                ..Default::default()
            })
            .await
            .unwrap();

        let service = TlsService::new(repo, provider).with_config_service(config_service.clone());

        assert_eq!(service.get_acme_email().await, "ops@example.com");
    }

    /// Without a wired `config_service` (the pre-fix state), the ACME contact
    /// email must be empty, not a placeholder -- callers use emptiness to
    /// refuse issuance rather than register a bogus contact.
    #[tokio::test]
    async fn test_get_acme_email_empty_without_config_service() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let repo = Arc::new(DefaultCertificateRepository::new(db, encryption_service));
        let provider = Arc::new(MockCertificateProvider::new());

        let service = TlsService::new(repo, provider);

        assert_eq!(service.get_acme_email().await, "");
    }
}
