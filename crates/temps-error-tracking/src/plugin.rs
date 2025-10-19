use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::services::ErrorTrackingService;
use crate::sentry::{DSNService, SentryIngestionService};
use crate::providers::sentry::SentryProvider;

/// Error Tracking Plugin for capturing and managing application errors
pub struct ErrorTrackingPlugin;

impl ErrorTrackingPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ErrorTrackingPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ErrorTrackingPlugin {
    fn name(&self) -> &'static str {
        "error-tracking"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Register core error tracking service
            let error_tracking_service = Arc::new(ErrorTrackingService::new(db.clone()));
            context.register_service(error_tracking_service.clone());

            // Register Sentry-specific services
            let dsn_service = Arc::new(DSNService::new(db.clone()));
            context.register_service(dsn_service.clone());

            let sentry_ingestion_service = Arc::new(SentryIngestionService::new(
                error_tracking_service.clone(),
                dsn_service.clone(),
            ));
            context.register_service(sentry_ingestion_service);

            let sentry_provider = Arc::new(SentryProvider::new(dsn_service.clone()));
            context.register_service(sentry_provider);

            tracing::debug!("Error tracking plugin services registered successfully (including Sentry)");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let error_tracking_service = context.require_service::<ErrorTrackingService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let config_service = context.require_service::<temps_config::ConfigService>();
        let sentry_provider = context.require_service::<SentryProvider>();
        let dsn_service = context.require_service::<DSNService>();

        // Configure error tracking routes (main API)
        let error_tracking_state = Arc::new(crate::handlers::types::AppState {
            error_tracking_service: error_tracking_service.clone(),
            audit_service: audit_service.clone(),
        });
        let error_tracking_routes = crate::handlers::handler::configure_routes()
            .with_state(error_tracking_state);

        // Configure Sentry ingestion routes
        let sentry_state = Arc::new(crate::sentry::handlers::AppState {
            sentry_provider: sentry_provider.clone(),
            error_tracking_service: error_tracking_service.clone(),
            audit_service: audit_service.clone(),
        });
        let sentry_routes = crate::sentry::handlers::configure_routes()
            .with_state(sentry_state);

        // Configure DSN management routes
        let dsn_state = Arc::new(crate::sentry::dsn_handlers::DSNAppState {
            dsn_service: dsn_service.clone(),
            audit_service: audit_service.clone(),
            config_service: config_service.clone(),
        });
        let dsn_routes = crate::sentry::dsn_handlers::configure_dsn_routes()
            .with_state(dsn_state);

        // Merge all routes together
        let routes = error_tracking_routes
            .merge(sentry_routes)
            .merge(dsn_routes);

        Some(PluginRoutes {
            router: routes,
        })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Get base error tracking schema
        let mut schema = <crate::handlers::handler::ErrorTrackingApiDoc as OpenApiTrait>::openapi();

        // Merge Sentry ingestion routes schema
        let sentry_schema = <crate::sentry::handlers::ApiDoc as OpenApiTrait>::openapi();
        schema.paths.paths.extend(sentry_schema.paths.paths);
        if let Some(components) = &sentry_schema.components {
            if let Some(base_components) = &mut schema.components {
                base_components.schemas.extend(components.schemas.clone());
            }
        }

        // Merge DSN management routes schema
        let dsn_schema = <crate::sentry::dsn_handlers::DSNApiDoc as OpenApiTrait>::openapi();
        schema.paths.paths.extend(dsn_schema.paths.paths);
        if let Some(components) = &dsn_schema.components {
            if let Some(base_components) = &mut schema.components {
                base_components.schemas.extend(components.schemas.clone());
            }
        }

        Some(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_error_tracking_plugin_name() {
        let plugin = ErrorTrackingPlugin::new();
        assert_eq!(plugin.name(), "error-tracking");
    }

    #[tokio::test]
    async fn test_error_tracking_plugin_default() {
        let plugin = ErrorTrackingPlugin::default();
        assert_eq!(plugin.name(), "error-tracking");
    }
}