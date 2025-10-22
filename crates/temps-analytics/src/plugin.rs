//! Analytics Plugin implementation for the Temps plugin system
//!
//! This plugin provides analytics functionality including:
//! - Web analytics metrics and reporting
//! - Visitor tracking and session analytics
//! - Page views and performance metrics
//! - Real-time analytics data

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::EncryptionService;
use utoipa::{openapi::OpenApi, OpenApi as OpenApiTrait};

use crate::handler::{configure_routes, AnalyticsApiDoc, AppState};
use crate::{Analytics, AnalyticsService};

/// Analytics Plugin for web analytics and visitor tracking
pub struct AnalyticsPlugin;

impl AnalyticsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AnalyticsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for AnalyticsPlugin {
    fn name(&self) -> &'static str {
        "analytics"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<EncryptionService>();
            // Create AnalyticsService
            let analytics_service = Arc::new(AnalyticsService::new(
                db.clone(),
                encryption_service.clone(),
            ));

            // Register the service with both the concrete type and trait
            context.register_service(analytics_service.clone());
            let analytics_trait: Arc<dyn Analytics> = analytics_service;
            context.register_service(analytics_trait);

            tracing::debug!("Analytics plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the AnalyticsService from the context
        let analytics_service = context.require_service::<dyn Analytics>();

        // Create AppState
        let app_state = Arc::new(AppState { analytics_service });

        // Configure routes with the state
        let routes = configure_routes().with_state(app_state);

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(AnalyticsApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analytics_plugin_name() {
        let analytics_plugin = AnalyticsPlugin::new();
        assert_eq!(analytics_plugin.name(), "analytics");
    }

    #[test]
    fn test_analytics_plugin_default() {
        let analytics_plugin = AnalyticsPlugin;
        assert_eq!(analytics_plugin.name(), "analytics");
    }
}
