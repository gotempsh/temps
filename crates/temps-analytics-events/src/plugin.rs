use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;

/// Analytics events tracking plugin
pub struct EventsPlugin;

impl Default for EventsPlugin {
    fn default() -> Self {
        Self
    }
}

impl TempsPlugin for EventsPlugin {
    fn name(&self) -> &'static str {
        "analytics-events"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            let events_service = Arc::new(crate::services::AnalyticsEventsService::new(db));
            context.register_service(events_service);

            debug!("Analytics events services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let events_service = context.get_service::<crate::services::AnalyticsEventsService>()?;
        let route_table = context.get_service::<temps_proxy::CachedPeerTable>()?;
        let ip_address_service = context.get_service::<temps_geo::IpAddressService>()?;

        let routes =
            crate::handlers::configure_routes().with_state(Arc::new(crate::handlers::AppState {
                events_service,
                route_table,
                ip_address_service,
            }));

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(<crate::handlers::EventsApiDoc as utoipa::OpenApi>::openapi())
    }
}
