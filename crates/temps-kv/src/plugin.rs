//! KV Plugin implementation for the Temps plugin system
//!
//! This plugin provides a key-value store API backed by Redis.
//! It uses RedisService from temps-providers for container management,
//! version tracking, and upgrade support.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::AuditLogger;
use temps_providers::externalsvc::RedisService;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{configure_routes, KvApiDoc, KvAppState};
use crate::services::KvService;

/// Default name for the KV Redis service
const KV_REDIS_SERVICE_NAME: &str = "temps-kv";

/// KV Plugin for key-value storage operations
///
/// This plugin provides a Redis-backed key-value store that uses
/// RedisService from temps-providers for:
/// - Container lifecycle management
/// - Version tracking and automatic upgrades
/// - Database persistence
/// - Backup and restore support
pub struct KvPlugin;

impl KvPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KvPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for KvPlugin {
    fn name(&self) -> &'static str {
        "kv"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get Docker client from service registry
            let docker = context.require_service::<bollard::Docker>();

            // Create RedisService from temps-providers for container management
            // This gives us version tracking, upgrades, and backup support
            let redis_service =
                Arc::new(RedisService::new(KV_REDIS_SERVICE_NAME.to_string(), docker));

            // Create KV service that uses the RedisService
            let kv_service = Arc::new(KvService::new(redis_service.clone()));

            // Register services for other plugins to use
            context.register_service(kv_service);
            context.register_service(redis_service);

            tracing::debug!("KV plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the KV service, Redis service, and Audit service from the plugin context
        let kv_service = context.require_service::<KvService>();
        let redis_service = context.require_service::<RedisService>();
        let audit_service = context.require_service::<dyn AuditLogger>();

        // Create app state for handlers
        let app_state = Arc::new(KvAppState {
            kv_service,
            redis_service,
            audit_service,
        });

        // Configure routes with the app state
        let kv_routes = configure_routes().with_state(app_state);

        Some(PluginRoutes { router: kv_routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<KvApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kv_plugin_name() {
        let plugin = KvPlugin::new();
        assert_eq!(plugin.name(), "kv");
    }

    #[test]
    fn test_kv_plugin_default() {
        let plugin = KvPlugin::default();
        assert_eq!(plugin.name(), "kv");
    }

    #[test]
    fn test_kv_plugin_openapi() {
        let plugin = KvPlugin::new();
        let schema = plugin.openapi_schema();
        assert!(schema.is_some());

        let openapi = schema.unwrap();
        // Verify paths exist
        assert!(!openapi.paths.paths.is_empty());
    }
}
