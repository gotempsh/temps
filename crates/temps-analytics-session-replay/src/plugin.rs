use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;

/// Session replay analytics plugin
pub struct SessionReplayPlugin;

impl Default for SessionReplayPlugin {
    fn default() -> Self {
        Self
    }
}

impl TempsPlugin for SessionReplayPlugin {
    fn name(&self) -> &'static str {
        "session-replay"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            debug!("Registering session replay services");

            let db = context.require_service::<sea_orm::DatabaseConnection>();

            let session_service = Arc::new(crate::services::SessionReplayService::new(db));
            context.register_service(session_service);

            debug!("Session replay services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let session_replay_service =
            context.get_service::<crate::services::SessionReplayService>()?;
        let audit_service = context.get_service::<dyn temps_core::AuditLogger>()?;
        let route_table = context.get_service::<temps_routes::CachedPeerTable>()?;
        let routes = crate::handlers::configure_routes().with_state(Arc::new(
            crate::handlers::types::AppState {
                session_replay_service,
                audit_service,
                route_table,
            },
        ));

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(<crate::handlers::SessionReplayApiDoc as utoipa::OpenApi>::openapi())
    }
}
