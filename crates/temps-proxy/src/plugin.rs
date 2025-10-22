use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_database::DbConnection;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    handler::{handler::LbApiDoc, request_logs::RequestLogsApiDoc},
    service::{
        lb_service::LbService, proxy_log_service::ProxyLogService,
        request_log_service::RequestLogService,
    },
};

pub struct ProxyPlugin;

impl TempsPlugin for ProxyPlugin {
    fn name(&self) -> &'static str {
        "proxy"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get database connection
            let db = context.get_service::<DbConnection>().ok_or_else(|| {
                PluginError::ServiceNotFound {
                    service_type: "DbConnection".to_string(),
                }
            })?;

            // Get IP service
            let ip_service = context
                .get_service::<temps_geo::IpAddressService>()
                .ok_or_else(|| PluginError::ServiceNotFound {
                    service_type: "IpAddressService".to_string(),
                })?;

            // Create LB service
            let lb_service = Arc::new(LbService::new(db.clone()));

            // Create Request Log service
            let request_log_service = Arc::new(RequestLogService::new(db.clone()));

            // Create Proxy Log service with IP service for enrichment
            let proxy_log_service = Arc::new(ProxyLogService::new(db.clone(), ip_service));

            // Register the services for other plugins to use
            context.register_service(lb_service);
            context.register_service(request_log_service);
            context.register_service(proxy_log_service);

            tracing::debug!("Proxy plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the required services from the service registry
        let lb_service = context.get_service::<LbService>()?;

        let request_log_service = context.get_service::<RequestLogService>()?;

        let proxy_log_service = context.get_service::<ProxyLogService>()?;

        // Create the app state directly
        let app_state = Arc::new(crate::handler::types::AppState {
            lb_service,
            request_log_service,
        });

        // Configure routes with the app state
        let router = crate::handler::handler::configure_routes()
            .merge(crate::handler::request_logs::configure_routes())
            .with_state(app_state)
            .merge(crate::handler::proxy_logs::create_routes().with_state(proxy_log_service));

        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Merge the OpenAPI specs from LB, Request Logs, and Proxy Logs APIs
        let lb_spec = LbApiDoc::openapi();
        let request_logs_spec = RequestLogsApiDoc::openapi();
        let proxy_logs_spec = crate::handler::proxy_logs::openapi();

        let merged = temps_core::openapi::merge_openapi_schemas(
            lb_spec,
            vec![request_logs_spec, proxy_logs_spec],
        );

        Some(merged)
    }
}

impl ProxyPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProxyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::plugin::{PluginStateRegistry, ServiceRegistry};
    use temps_database::test_utils::TestDatabase;

    #[tokio::test]
    async fn test_plugin_registration() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let context = ServiceRegistrationContext::new();

        // Register database connection
        context.register_service(test_db.connection_arc().clone());

        // Register required IP service
        let geo_ip_service = Arc::new(temps_geo::GeoIpService::Mock(
            temps_geo::MockGeoIpService::new(),
        ));
        let ip_service = Arc::new(temps_geo::IpAddressService::new(
            test_db.connection_arc().clone(),
            geo_ip_service,
        ));
        context.register_service(ip_service);

        let plugin = ProxyPlugin::new();
        // Call register_services method correctly
        let result = plugin.register_services(&context).await;

        assert!(result.is_ok(), "Plugin registration should succeed");

        // Verify LB service was registered
        let lb_service = context.get_service::<LbService>();
        assert!(lb_service.is_some(), "LB service should be registered");
    }

    #[test]
    fn test_plugin_metadata() {
        let plugin = ProxyPlugin::new();
        assert_eq!(plugin.name(), "proxy");
        // TempsPlugin trait doesn't have description() method, so we'll just test name
    }

    #[test]
    fn test_openapi_schema() {
        let plugin = ProxyPlugin::new();
        let spec = plugin.openapi_schema();
        assert!(spec.is_some(), "Plugin should provide OpenAPI spec");

        let spec = spec.unwrap();
        assert_eq!(spec.info.title, "Load Balancer API");
        assert_eq!(spec.info.version, "1.0.0");
    }

    #[tokio::test]
    async fn test_configure_routes() {
        let service_registry = Arc::new(ServiceRegistry::new());
        let state_registry = Arc::new(PluginStateRegistry::new());

        // Create mock services and register them in the service registry
        let test_db = TestDatabase::new().await.unwrap();
        let lb_service = Arc::new(LbService::new(test_db.connection_arc().clone()));
        let request_log_service =
            Arc::new(RequestLogService::new(test_db.connection_arc().clone()));

        // Create a mock GeoIP service and IP service for proxy_log_service
        let geo_ip_service = Arc::new(temps_geo::GeoIpService::Mock(
            temps_geo::MockGeoIpService::new(),
        ));
        let ip_service = Arc::new(temps_geo::IpAddressService::new(
            test_db.connection_arc().clone(),
            geo_ip_service,
        ));
        let proxy_log_service = Arc::new(ProxyLogService::new(
            test_db.connection_arc().clone(),
            ip_service,
        ));

        // Register services in the service registry
        service_registry.register(lb_service);
        service_registry.register(request_log_service);
        service_registry.register(proxy_log_service);

        let plugin_context = PluginContext::new(service_registry, state_registry);
        let plugin = ProxyPlugin::new();

        let routes = plugin.configure_routes(&plugin_context);
        assert!(routes.is_some(), "Plugin should provide routes");
    }
}
