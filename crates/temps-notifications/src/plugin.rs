//! Notifications Plugin implementation for the Temps plugin system
//!
//! This plugin provides notification services including:
//! - NotificationService for sending notifications through various providers
//! - Email, Slack, and other notification provider management
//! - HTTP handlers for notification provider CRUD operations
//! - OpenAPI documentation for notification endpoints

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    handlers::{configure_routes, NotificationProvidersApiDoc, NotificationState},
    services::{NotificationPreferencesService, NotificationService},
};

/// Notifications Plugin for managing notification providers and services
pub struct NotificationsPlugin;

impl NotificationsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NotificationsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for NotificationsPlugin {
    fn name(&self) -> &'static str {
        "notifications"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create NotificationService
            let notification_service = Arc::new(NotificationService::new(
                db.clone(),
                encryption_service.clone(),
            ));
            context.register_service(notification_service.clone());

            // Register the notification service as the trait object directly
            // This avoids double-wrapping since the plugin system will wrap it in Arc
            let dyn_notification_service: Arc<dyn temps_core::notifications::NotificationService> =
                notification_service.clone();
            context.register_service(dyn_notification_service);

            // Create NotificationPreferencesService
            let notification_preferences_service =
                Arc::new(NotificationPreferencesService::new(db.clone()));
            context.register_service(notification_preferences_service.clone());

            // Create NotificationState for handlers
            let notification_state = Arc::new(NotificationState::new(
                notification_service,
                notification_preferences_service,
            ));
            context.register_service(notification_state);

            debug!("Notifications plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the notification state
        let notification_state = context.require_service::<NotificationState>();

        // Build notification routes using the existing configure_routes function
        let routes = configure_routes().with_state(notification_state);

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<NotificationProvidersApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_notifications_plugin_name() {
        let notifications_plugin = NotificationsPlugin::new();
        assert_eq!(notifications_plugin.name(), "notifications");
    }

    #[tokio::test]
    async fn test_notifications_plugin_default() {
        let notifications_plugin = NotificationsPlugin;
        assert_eq!(notifications_plugin.name(), "notifications");
    }
}
