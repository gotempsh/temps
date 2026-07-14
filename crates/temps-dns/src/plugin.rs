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

use crate::handlers::{self, dns_sync::DnsSyncAppState, DnsApiDoc, DnsAppState};
use crate::services::{DnsProviderService, DnsRecordService, DnsRegistry, ManagedDnsRecordService};

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

            // Ownership-guarded record management (ADR-031) — the only path
            // for public A/AAAA/CNAME records in user zones.
            let managed_record_service = Arc::new(ManagedDnsRecordService::new(
                db.clone(),
                provider_service.clone(),
            ));
            context.register_service(managed_record_service.clone());

            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Create DnsAppState for handlers
            let app_state = Arc::new(DnsAppState {
                provider_service,
                record_service,
                managed_record_service,
                audit_service,
            });
            context.register_service(app_state);

            // Internal DNS registry (ADR-011) — separate state, separate
            // auth model, separate consumer (per-node agents).
            let registry = Arc::new(DnsRegistry::new(db.clone()));
            context.register_service(registry.clone());
            let sync_state = Arc::new(DnsSyncAppState {
                registry,
                db: db.clone(),
            });
            context.register_service(sync_state);

            debug!("DNS plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // User-facing routes
        let app_state = context.require_service::<DnsAppState>();
        let dns_routes = handlers::configure_routes().with_state(app_state);

        // Internal sync routes (per-node agent → control plane)
        let sync_state = context.require_service::<DnsSyncAppState>();
        let sync_routes = handlers::configure_internal_routes().with_state(sync_state);

        Some(PluginRoutes::new(dns_routes.merge(sync_routes)))
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
        let dns_plugin = DnsPlugin;
        assert_eq!(dns_plugin.name(), "dns");
    }
}
