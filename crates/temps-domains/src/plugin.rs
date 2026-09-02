// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

            // Required so background renewals can read `letsencrypt.email` (see
            // `TlsService::get_acme_email`) -- without this, auto-renewal always fails
            // with "User email is required" regardless of what's configured.
            let config_service = context.require_service::<temps_config::ConfigService>();

            // Get DnsProviderService (requires dns plugin to be registered first). Also
            // wired into the TLS service so DNS-01 background renewals can auto-publish
            // the challenge TXT record when a DNS provider manages the domain's zone.
            let dns_provider_service = context.require_service::<DnsProviderService>();
            let dns_automation_gate =
                context.require_service::<temps_core::DnsAutomationGateSlot>();
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Create domain service first so the TLS service can drive the order-based
            // ACME flow during background HTTP-01 renewals (keeps auto-renewals
            // recoverable from the UI). DomainService does not depend on TlsService, so
            // there is no construction cycle.
            let domain_service = Arc::new(crate::DomainService::new(
                db.clone(),
                cert_provider.clone(),
                repository.clone(),
                encryption_service.clone(),
            ));
            // Register DomainService so the serve wiring layer can construct the
            // DiscoveredHostTlsProvisioner adapter (ADR-041 §8) without
            // introducing a direct temps-domains dependency in temps-deployments.
            context.register_service(domain_service.clone());

            // Create TLS service
            let tls_service = TlsServiceBuilder::new()
                .with_repository(repository.clone())
                .with_cert_provider(cert_provider.clone())
                .build()
                .map_err(|e| PluginError::PluginRegistrationFailed {
                    plugin_name: "domains".to_string(),
                    error: format!("Failed to create TLS service: {}", e),
                })?
                .with_domain_service(domain_service.clone())
                .with_config_service(config_service.clone())
                .with_dns_provider_service(dns_provider_service.clone())
                .with_dns_automation_gate(dns_automation_gate)
                .with_audit_logger(audit_service.clone());

            // AlarmService isn't registered yet at this point (Monitoring
            // registers after Domains) — wired in via `initialize_plugin_services`
            // once every plugin's Phase 1 has completed.
            let tls_service = Arc::new(tls_service);
            context.register_service(tls_service.clone());

            // Note: Certificate renewal scheduler is started in console.rs
            // The scheduler handles both initial check and daily scheduled checks

            // Get audit service
            // Get telemetry reporter (optional — default to noop so domains never hard-fail
            // if the telemetry plugin isn't registered)
            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| {
                    std::sync::Arc::new(temps_core::telemetry::NoopTelemetryReporter)
                });

            // Central sensitive-action policy (MFA step-up), used to gate
            // destructive domain operations like delete.
            let sensitive_action_authorizer =
                context.require_service::<dyn temps_core::SensitiveActionAuthorizer>();

            // Create DomainAppState for handlers
            let domain_app_state = create_domain_app_state_with_dns(
                tls_service,
                repository,
                domain_service,
                dns_provider_service,
                audit_service,
                telemetry,
                sensitive_action_authorizer,
            );
            context.register_service(domain_app_state);

            tracing::debug!("Domains plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(alarm_service) =
                context.get_service::<temps_monitoring::alarm_service::AlarmService>()
            {
                let tls_service = context.require_service::<crate::tls::TlsService>();
                tls_service.set_alarm_service(alarm_service);
                tracing::debug!("AlarmService wired into TLS service");
            } else {
                tracing::warn!(
                    "AlarmService not available - certificate renewal alarms will be skipped"
                );
            }
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the DomainAppState
        let domain_app_state = context.require_service::<DomainAppState>();

        // Rebind the authorizer here rather than trust the one captured in
        // `register_services`: an EE/custom `SensitiveActionAuthorizer` may
        // be registered by a plugin later in registration order, and
        // last-write-wins service registration means the earliest-registered
        // instance otherwise wins silently. `configure_routes` runs only
        // after every plugin's `register_services` has completed, so
        // re-resolving here always sees the final policy — same pattern as
        // AuthPlugin's `with_sensitive_action_authorizer`.
        let domain_app_state = Arc::new(DomainAppState {
            sensitive_action_authorizer: context
                .require_service::<dyn temps_core::SensitiveActionAuthorizer>(),
            ..(*domain_app_state).clone()
        });

        // Configure routes
        let domains_routes = handlers::configure_routes().with_state(domain_app_state);

        Some(PluginRoutes::new(domains_routes))
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
