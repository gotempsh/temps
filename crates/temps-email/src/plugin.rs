//! Email plugin for Temps

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{self, AppState, EmailApiDoc};
use crate::services::{
    DomainService, EmailService, ProviderService, TrackingService, ValidationConfig,
    ValidationService,
};
use temps_dns::services::DnsProviderService;

/// Email Plugin for managing email providers, domains, and sending emails
pub struct EmailPlugin;

impl EmailPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EmailPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for EmailPlugin {
    fn name(&self) -> &'static str {
        "email"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create ProviderService
            let provider_service =
                Arc::new(ProviderService::new(db.clone(), encryption_service.clone()));
            context.register_service(provider_service.clone());

            // Create DomainService
            let domain_service = Arc::new(DomainService::new(db.clone(), provider_service.clone()));
            context.register_service(domain_service.clone());

            // Create TrackingService — uses ConfigService to get external URL dynamically
            let config_service = context.require_service::<temps_config::ConfigService>();
            let tracking_service = Arc::new(TrackingService::new(db.clone(), config_service));
            context.register_service(tracking_service.clone());

            // Create EmailService with tracking support
            let email_service = Arc::new(EmailService::new(
                db.clone(),
                provider_service.clone(),
                domain_service.clone(),
                tracking_service.clone(),
            ));
            context.register_service(email_service.clone());

            // Create ValidationService with default config
            let validation_service = Arc::new(ValidationService::new(ValidationConfig::default()));
            context.register_service(validation_service.clone());

            // Get AuditService dependency from other plugins
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Try to get DnsProviderService if available (optional dependency)
            let dns_provider_service = context.get_service::<DnsProviderService>();

            // Pull telemetry reporter (optional — fall back to no-op so the plugin
            // never hard-fails when telemetry isn't registered)
            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

            // Create AppState for handlers
            let app_state = Arc::new(AppState {
                provider_service,
                domain_service,
                email_service,
                validation_service,
                tracking_service,
                audit_service,
                dns_provider_service,
                telemetry,
            });
            context.register_service(app_state);

            debug!("Email plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the AppState
        let app_state = context.require_service::<AppState>();

        // Configure authenticated routes
        let email_routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(email_routes))
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.require_service::<AppState>();

        // Public tracking routes (no auth required)
        let tracking_routes = handlers::configure_public_routes().with_state(app_state);

        Some(PluginRoutes::new(tracking_routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<EmailApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_email_plugin_name() {
        let email_plugin = EmailPlugin::new();
        assert_eq!(email_plugin.name(), "email");
    }

    #[tokio::test]
    async fn test_email_plugin_default() {
        let email_plugin = EmailPlugin;
        assert_eq!(email_plugin.name(), "email");
    }
}
