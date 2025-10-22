//! Geo Plugin implementation for the Temps plugin system
//!
//! This plugin provides geolocation services including:
//! - GeoIpService for IP geolocation
//! - IpAddressService for IP address management and tracking

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::OpenApi;

use crate::{handlers, AppState, GeoIpService, IpAddressService};

/// Geo Plugin for managing geolocation and IP address services
pub struct GeoPlugin;

impl GeoPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GeoPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for GeoPlugin {
    fn name(&self) -> &'static str {
        "geo"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Create GeoIpService
            let geo_ip_service = Arc::new(GeoIpService::new().map_err(|e| {
                PluginError::PluginRegistrationFailed {
                    plugin_name: "geo".to_string(),
                    error: e.to_string(),
                }
            })?);
            context.register_service(geo_ip_service.clone());

            // Create IpAddressService (depends on GeoIpService)
            let ip_address_service =
                Arc::new(IpAddressService::new(db.clone(), geo_ip_service.clone()));
            context.register_service(ip_address_service);

            tracing::debug!("Geo plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get GeoIpService from service registry
        let geo_ip_service = context.require_service::<GeoIpService>();

        // Create AppState for handlers
        let app_state = Arc::new(AppState {
            geo_ip_service: geo_ip_service.clone(),
        });

        // Configure routes (plugin system adds /api prefix)
        let routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(handlers::ApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_geo_plugin_name() {
        let geo_plugin = GeoPlugin::new();
        assert_eq!(geo_plugin.name(), "geo");
    }

    #[tokio::test]
    async fn test_geo_plugin_default() {
        let geo_plugin = GeoPlugin::default();
        assert_eq!(geo_plugin.name(), "geo");
    }
}
