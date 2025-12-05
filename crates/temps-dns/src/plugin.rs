//! DNS plugin for Temps
//!
//! This plugin provides DNS provider management capabilities including:
//! - Multiple DNS provider support (Cloudflare, Namecheap, etc.)
//! - Automatic DNS record management for domains
//! - Encrypted credential storage

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{self, DnsApiDoc, DnsAppState};
use crate::services::{DnsProviderService, DnsRecordService};

/// DNS Plugin for managing DNS providers and automatic DNS record configuration
pub struct DnsPlugin;

impl DnsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DnsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for DnsPlugin {
    fn name(&self) -> &'static str {
        "dns"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create DnsProviderService
            let provider_service = Arc::new(DnsProviderService::new(
                db.clone(),
                encryption_service.clone(),
            ));
            context.register_service(provider_service.clone());

            // Create DnsRecordService
            let record_service = Arc::new(DnsRecordService::new(provider_service.clone()));
            context.register_service(record_service.clone());

            // Create DnsAppState for handlers
            let app_state = Arc::new(DnsAppState {
                provider_service,
                record_service,
            });
            context.register_service(app_state);

            debug!("DNS plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the DnsAppState
        let app_state = context.require_service::<DnsAppState>();

        // Configure routes
        let dns_routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes { router: dns_routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<DnsApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dns_plugin_name() {
        let dns_plugin = DnsPlugin::new();
        assert_eq!(dns_plugin.name(), "dns");
    }

    #[tokio::test]
    async fn test_dns_plugin_default() {
        let dns_plugin = DnsPlugin::default();
        assert_eq!(dns_plugin.name(), "dns");
    }
}
