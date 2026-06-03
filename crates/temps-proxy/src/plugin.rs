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
    handler::handler::LbApiDoc,
    service::{
        challenge_service::ChallengeService, ip_access_control_service::IpAccessControlService,
        lb_service::LbService, proxy_log_service::ProxyLogService,
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
            let db = context.require_service::<DbConnection>();

            // Get IP service
            let ip_service = context.require_service::<temps_geo::IpAddressService>();

            // Get the server config so we can select the proxy-log storage
            // backend. When TEMPS_CLICKHOUSE_* is configured the handlers read
            // proxy/request logs from ClickHouse; otherwise they use TimescaleDB
            // exactly as before.
            //
            // ConfigService is always registered in the running server. We read
            // it via `get_service` (not `require_service`) so the storage
            // selection degrades to the default TimescaleDB backend if it is
            // somehow absent — proxy logging must never fail to register. When
            // present and CH is enabled, the handler reads go to ClickHouse.
            let server_config = context
                .get_service::<temps_config::ConfigService>()
                .map(|cs| cs.get_server_config());

            // Create LB service
            let lb_service = Arc::new(LbService::new(db.clone()));

            // Build the proxy-log storage backend (ClickHouse when enabled, else
            // TimescaleDB) and wire it into the read service the handlers use.
            // When ClickHouse is NOT configured this resolves to the default
            // TimescaleDB path with no behaviour change.
            let proxy_log_storage = match server_config {
                Some(config) => {
                    crate::storage::build_proxy_log_storage(&config, db.clone(), ip_service.clone())
                }
                None => {
                    tracing::debug!(
                        "Proxy plugin: ConfigService unavailable; proxy logs use the \
                         default TimescaleDB backend"
                    );
                    Arc::new(crate::storage::TimescaleDbProxyLogStore::new(
                        db.clone(),
                        ip_service.clone(),
                    )) as Arc<dyn crate::storage::ProxyLogStorage>
                }
            };

            // Create Proxy Log service dispatching reads through the selected
            // storage backend. `db`/`ip_service` are still held for the
            // (handler-unused) enriching `create` path.
            let proxy_log_service = Arc::new(ProxyLogService::with_storage(
                db.clone(),
                ip_service,
                proxy_log_storage,
            ));

            // Create IP Access Control service
            let ip_access_control_service = Arc::new(IpAccessControlService::new(db.clone()));

            // Create Challenge service for CAPTCHA
            let challenge_service = Arc::new(ChallengeService::new(db.clone()));

            // Register the services for other plugins to use
            context.register_service(lb_service);
            context.register_service(proxy_log_service);
            context.register_service(ip_access_control_service);
            context.register_service(challenge_service);

            tracing::debug!("Proxy plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the required services from the service registry
        let lb_service = context.require_service::<LbService>();
        let proxy_log_service = context.require_service::<ProxyLogService>();
        let ip_access_control_service = context.require_service::<IpAccessControlService>();
        let challenge_service = context.require_service::<ChallengeService>();
        let db = context.require_service::<DbConnection>();

        // Create the app state directly
        let app_state = Arc::new(crate::handler::types::AppState { lb_service });

        // Create CAPTCHA state
        let captcha_state = Arc::new(crate::handler::captcha::CaptchaState {
            db: db.clone(),
            challenge_service: challenge_service.clone(),
        });

        // Configure routes with the app state
        let router = crate::handler::handler::configure_routes()
            .with_state(app_state)
            .merge(crate::handler::proxy_logs::create_routes().with_state(proxy_log_service))
            .merge(
                crate::handler::ip_access_control::create_routes()
                    .with_state(ip_access_control_service),
            )
            .merge(crate::handler::captcha::create_routes().with_state(captcha_state));

        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Merge the OpenAPI specs from LB, Proxy Logs, and IP Access Control APIs
        let lb_spec = LbApiDoc::openapi();
        let proxy_logs_spec = crate::handler::proxy_logs::openapi();
        let ip_access_control_spec = crate::handler::ip_access_control::openapi();

        let merged = temps_core::openapi::merge_openapi_schemas(
            lb_spec,
            vec![proxy_logs_spec, ip_access_control_spec],
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

        // No ConfigService is registered here: the plugin reads it via
        // `get_service` and, when absent, selects the default TimescaleDB
        // proxy-log backend — which is exactly what we want to exercise.

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
        let db_connection = test_db.connection_arc().clone();

        let lb_service = Arc::new(LbService::new(db_connection.clone()));

        // Create a mock GeoIP service and IP service for proxy_log_service
        let geo_ip_service = Arc::new(temps_geo::GeoIpService::Mock(
            temps_geo::MockGeoIpService::new(),
        ));
        let ip_service = Arc::new(temps_geo::IpAddressService::new(
            db_connection.clone(),
            geo_ip_service,
        ));
        let proxy_log_service = Arc::new(ProxyLogService::new(db_connection.clone(), ip_service));

        // Create IP Access Control service
        let ip_access_control_service =
            Arc::new(IpAccessControlService::new(db_connection.clone()));

        // Create Challenge service
        let challenge_service = Arc::new(ChallengeService::new(db_connection.clone()));

        // Register all services in the service registry
        service_registry.register(db_connection);
        service_registry.register(lb_service);
        service_registry.register(proxy_log_service);
        service_registry.register(ip_access_control_service);
        service_registry.register(challenge_service);

        let plugin_context = PluginContext::new(service_registry, state_registry);
        let plugin = ProxyPlugin::new();

        let routes = plugin.configure_routes(&plugin_context);
        assert!(routes.is_some(), "Plugin should provide routes");
    }
}
