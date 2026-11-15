// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adapters the serve wiring layer implements for Traefik discovery, which
//! cannot depend on the monitoring and domains crates directly.

use super::*;
use temps_deployments::services::traefik_discovery_service::{
    DiscoveredHostTlsProvisioner, TlsProvisionerError,
};
use temps_domains::tls::models::{Certificate, CertificateStatus};
use temps_domains::tls::repository::CertificateRepository;
use temps_domains::DomainService;

/// Fires a `TraefikContainerDrift` alarm for certificate drift (ADR-041 §2a).
pub(super) struct AlarmServiceDriftSink(pub(super) Arc<AlarmService>);
#[async_trait::async_trait]
impl DriftAlarmSink for AlarmServiceDriftSink {
    async fn notify_container_drift(
        &self,
        host: String,
        authorized_container: String,
        current_container: String,
    ) {
        use temps_monitoring::alarm_service::{AlarmSeverity, AlarmType, FireAlarmRequest};
        let detail = format!(
            "Host '{host}' is now served by container '{current}' but TLS was \
             authorized for container '{authorized}'. The certificate is still \
             valid but may be delivered to the wrong container if the new one is \
             not legitimate. Deauthorize and re-authorize once the correct \
             container is confirmed.",
            host = host,
            current = current_container,
            authorized = authorized_container,
        );
        let metadata = serde_json::json!({
            "host": host,
            "authorized_container": authorized_container,
            "current_container": current_container,
        });
        let request = FireAlarmRequest {
            project_id: None,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::TraefikContainerDrift,
            severity: AlarmSeverity::Critical,
            title: format!("Certificate drift: {host}"),
            message: detail,
            metadata: Some(metadata),
        };
        if let Err(e) = self.0.fire_alarm(request).await {
            tracing::error!(
                host = %host,
                error = %e,
                "Failed to fire TraefikContainerDrift alarm; drift is still \
                 recorded in the database"
            );
        }
    }
}

/// Provisions TLS for discovered hosts through the domains crate (ADR-041 §8).
pub(super) struct TraefikTlsProvisioner {
    pub(super) domain_service: Arc<DomainService>,
    pub(super) cert_repo: Arc<dyn CertificateRepository>,
    pub(super) config_service: Arc<temps_config::ConfigService>,
    pub(super) dns_provider_service: Arc<temps_dns::services::DnsProviderService>,
}

impl TraefikTlsProvisioner {
    /// Read `letsencrypt.email` from settings — mirrors TlsService::get_acme_email.
    async fn get_acme_email(&self) -> String {
        if let Ok(settings) = self.config_service.get_settings().await {
            if let Some(email) = settings.letsencrypt.email {
                let email = email.trim().to_string();
                if !email.is_empty() {
                    return email;
                }
            }
        }
        String::new()
    }
}

#[async_trait::async_trait]
impl DiscoveredHostTlsProvisioner for TraefikTlsProvisioner {
    async fn request_acme_cert(
        &self,
        host: &str,
        challenge_type: &str,
    ) -> Result<(), TlsProvisionerError> {
        let email = self.get_acme_email().await;

        // Check whether a domains row already exists for this host.
        let existing = self.cert_repo.find_certificate(host).await.map_err(|e| {
            TlsProvisionerError::Failed {
                host: host.to_string(),
                reason: e.to_string(),
            }
        })?;

        match existing {
            None => {
                // No row yet — create one with the declared challenge
                // type. If the ACME challenge request below fails, this
                // row is deliberately left in place rather than rolled
                // back: `TraefikDiscoveryAdminService::authorize_acme_cert`
                // already wrote a `traefik_route_certificates` claim for
                // this host before calling here, so a retry is never
                // blocked by the host-ownership check, and it reuses
                // this same pending row via the branch below.
                self.domain_service
                    .create_domain(host, challenge_type)
                    .await
                    .map_err(|e| TlsProvisionerError::Failed {
                        host: host.to_string(),
                        reason: e.to_string(),
                    })?;
            }
            Some(cert) if cert.verification_method == challenge_type => {
                // Row exists with a matching method — reuse it as-is.
            }
            Some(cert) => {
                // Row exists but the stored method differs — 409.
                return Err(TlsProvisionerError::VerificationMethodConflict {
                    host: host.to_string(),
                    stored: cert.verification_method,
                    declared: challenge_type.to_string(),
                });
            }
        };

        self.domain_service
            .request_challenge(host, &email)
            .await
            .map_err(|e| TlsProvisionerError::Failed {
                host: host.to_string(),
                reason: e.to_string(),
            })?;

        Ok(())
    }

    async fn save_imported_cert(
        &self,
        host: &str,
        certificate_pem: &str,
        key_pem: &str,
        renewal_method: &str,
        not_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<i32, TlsProvisionerError> {
        let cert = Certificate {
            id: 0,
            domain: host.to_string(),
            certificate_pem: certificate_pem.to_string(),
            private_key_pem: key_pem.to_string(),
            expiration_time: not_after,
            last_renewed: Some(chrono::Utc::now()),
            is_wildcard: false,
            verification_method: renewal_method.to_string(),
            status: CertificateStatus::Active,
        };
        let saved = self.cert_repo.save_certificate(cert).await.map_err(|e| {
            TlsProvisionerError::Failed {
                host: host.to_string(),
                reason: e.to_string(),
            }
        })?;
        Ok(saved.id)
    }

    async fn dns_zone_is_auto_managed(&self, host: &str) -> Result<bool, TlsProvisionerError> {
        self.dns_provider_service
            .find_provider_for_domain(host)
            .await
            .map(|found| found.is_some())
            .map_err(|e| TlsProvisionerError::Failed {
                host: host.to_string(),
                reason: e.to_string(),
            })
    }
}
