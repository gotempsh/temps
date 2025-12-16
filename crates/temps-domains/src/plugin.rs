use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    handlers::{self, create_domain_app_state_with_dns, DomainAppState},
    tls::{repository::DefaultCertificateRepository, TlsServiceBuilder},
};
use rustls::crypto::CryptoProvider;
use temps_dns::services::DnsProviderService;

/// Domains Plugin for managing DNS records and TLS certificates
pub struct DomainsPlugin;

impl DomainsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DomainsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for DomainsPlugin {
    fn name(&self) -> &'static str {
        "domains"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            CryptoProvider::install_default(rustls::crypto::ring::default_provider()).unwrap();
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create repository for TLS service
            let repository: Arc<dyn crate::tls::repository::CertificateRepository> = Arc::new(
                DefaultCertificateRepository::new(db.clone(), encryption_service.clone()),
            );
            context.register_service(repository.clone());

            // Create certificate provider
            // Email will be provided at runtime from the authenticated user
            // Environment is controlled by LETSENCRYPT_MODE env var (default: "production")
            let cert_provider = Arc::new(crate::tls::providers::LetsEncryptProvider::new(
                repository.clone(),
            ));

            // Try to get notification service (optional)
            let notification_service =
                context.get_service::<dyn temps_core::notifications::NotificationService>();

            // Create TLS service
            let mut tls_service = TlsServiceBuilder::new()
                .with_repository(repository.clone())
                .with_cert_provider(cert_provider.clone())
                .build()
                .map_err(|e| PluginError::PluginRegistrationFailed {
                    plugin_name: "domains".to_string(),
                    error: format!("Failed to create TLS service: {}", e),
                })?;

            // Add notification service if available
            if let Some(notif_service) = notification_service {
                tls_service = tls_service.with_notification_service(notif_service);
                tracing::debug!("Notification service integrated with TLS service");
            } else {
                tracing::debug!(
                    "No notification service available - renewal notifications will be skipped"
                );
            }

            let tls_service = Arc::new(tls_service);
            context.register_service(tls_service.clone());

            // Run certificate renewal check on startup (spawn as background task)
            let tls_service_clone = tls_service.clone();
            tokio::spawn(async move {
                tracing::debug!("Running certificate renewal check on startup");
                match tls_service_clone.check_and_renew_certificates(30).await {
                    Ok(report) => {
                        // Only log if there's something interesting
                        if report.total_checked > 0 {
                            tracing::info!(
                                "Certificate renewal check: {} checked, {} renewed, {} failed, {} manual",
                                report.total_checked,
                                report.auto_renewed.len(),
                                report.renewal_failed.len(),
                                report.manual_action_needed.len()
                            );
                        } else {
                            tracing::debug!("Certificate renewal check completed: no certificates expiring within 30 days");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Certificate renewal check failed: {}", e);
                    }
                }
            });

            // Get encryption service
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create domain service
            let domain_service = Arc::new(crate::DomainService::new(
                db.clone(),
                cert_provider,
                repository.clone(),
                encryption_service.clone(),
            ));

            // Get DnsProviderService (requires dns plugin to be registered first)
            let dns_provider_service = context.require_service::<DnsProviderService>();

            // Create DomainAppState for handlers
            let domain_app_state = create_domain_app_state_with_dns(
                tls_service,
                repository,
                domain_service,
                dns_provider_service,
            );
            context.register_service(domain_app_state);

            tracing::debug!("Domains plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the DomainAppState
        let domain_app_state = context.require_service::<DomainAppState>();

        // Configure routes
        let domains_routes = handlers::configure_routes().with_state(domain_app_state);

        Some(PluginRoutes {
            router: domains_routes,
        })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<handlers::domain_handler::DomainApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_domains_plugin_name() {
        let domains_plugin = DomainsPlugin::new();
        assert_eq!(domains_plugin.name(), "domains");
    }

    #[tokio::test]
    async fn test_domains_plugin_default() {
        let domains_plugin = DomainsPlugin;
        assert_eq!(domains_plugin.name(), "domains");
    }
}
