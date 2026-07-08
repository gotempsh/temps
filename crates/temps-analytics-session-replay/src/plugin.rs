use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;

/// Session replay analytics plugin
pub struct SessionReplayPlugin;

impl SessionReplayPlugin {
    /// Periodically hard-delete session replay data older than 15 days
    async fn cleanup_loop(service: Arc<crate::services::SessionReplayService>) {
        let retention_days = 15;
        loop {
            // Run every 6 hours
            tokio::time::sleep(tokio::time::Duration::from_secs(6 * 3600)).await;
            service.cleanup_old_session_events(retention_days).await;
        }
    }
}

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
            context.register_service(session_service.clone());

            // Start periodic cleanup of old session replay data (15-day retention)
            let cleanup_service = session_service.clone();
            tokio::spawn(async move {
                Self::cleanup_loop(cleanup_service).await;
            });

            debug!("Session replay services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let session_replay_service =
            context.require_service::<crate::services::SessionReplayService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let route_table = context.require_service::<temps_routes::CachedPeerTable>();
        let telemetry = context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| std::sync::Arc::new(temps_core::telemetry::NoopTelemetryReporter));
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let routes = crate::handlers::configure_routes().with_state(Arc::new(
            crate::handlers::types::AppState {
                session_replay_service,
                audit_service,
                route_table,
                telemetry,
                project_access_checker,
            },
        ));

        Some(PluginRoutes::new(routes))
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let session_replay_service =
            context.require_service::<crate::services::SessionReplayService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let route_table = context.require_service::<temps_routes::CachedPeerTable>();
        let telemetry = context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| std::sync::Arc::new(temps_core::telemetry::NoopTelemetryReporter));
        let routes = crate::handlers::configure_public_routes().with_state(Arc::new(
            crate::handlers::types::AppState {
                session_replay_service,
                audit_service,
                route_table,
                telemetry,
                project_access_checker: None,
            },
        ));

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(<crate::handlers::SessionReplayApiDoc as utoipa::OpenApi>::openapi())
    }
}
