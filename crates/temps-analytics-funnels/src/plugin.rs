use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;

/// Funnels analytics plugin
pub struct FunnelsPlugin;

impl Default for FunnelsPlugin {
    fn default() -> Self {
        Self
    }
}

impl TempsPlugin for FunnelsPlugin {
    fn name(&self) -> &'static str {
        "funnels"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            debug!("Registering funnels services");

            let db = context.require_service::<sea_orm::DatabaseConnection>();

            let funnel_service = Arc::new(crate::services::FunnelService::new(db));
            context.register_service(funnel_service);

            debug!("Funnels services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let funnel_service = context.get_service::<crate::services::FunnelService>()?;

        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let routes = crate::handlers::handler::configure_routes().with_state(Arc::new(
            crate::handlers::types::AppState {
                funnel_service,
                project_access_checker,
            },
        ));

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(<crate::handlers::handler::FunnelApiDoc as utoipa::OpenApi>::openapi())
    }
}
