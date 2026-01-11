//! Webhooks Plugin implementation for the Temps plugin system
//!
//! This plugin provides webhook functionality including:
//! - WebhookService for managing webhooks and deliveries
//! - Webhook CRUD operations
//! - Webhook delivery tracking and retry logic
//! - HTTP handlers for webhook management
//! - OpenAPI documentation for webhook endpoints

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
    handlers::{configure_routes, WebhookState, WebhooksApiDoc},
    listener::WebhookEventListener,
    service::WebhookService,
};

/// Webhooks Plugin for managing webhooks and webhook deliveries
pub struct WebhooksPlugin;

impl WebhooksPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebhooksPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for WebhooksPlugin {
    fn name(&self) -> &'static str {
        "webhooks"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();
            let queue = context.require_service::<dyn temps_core::JobQueue>();

            // Create WebhookService
            let webhook_service =
                Arc::new(WebhookService::new(db.clone(), encryption_service.clone()));
            context.register_service(webhook_service.clone());

            // Create WebhookState for handlers
            let webhook_state = Arc::new(WebhookState::new(webhook_service.clone()));
            context.register_service(webhook_state);

            // Create WebhookEventListener
            let event_listener = Arc::new(WebhookEventListener::new(
                webhook_service.clone(),
                queue.clone(),
            ));

            // Register the listener service FIRST
            context.register_service(event_listener.clone());

            // Start the listener in the background (don't await)
            // This allows other plugins to initialize without waiting for the listener
            tokio::spawn({
                let event_listener = event_listener.clone();
                async move {
                    match event_listener.start().await {
                        Ok(_) => {
                            tracing::info!("🎉 Webhook event listener started successfully");
                        }
                        Err(e) => {
                            tracing::error!("❌ Failed to start webhook event listener: {}", e);
                        }
                    }
                }
            });

            debug!("Webhooks plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the webhook state
        let webhook_state = context.require_service::<WebhookState>();

        // Build webhook routes using the existing configure_routes function
        let routes = configure_routes().with_state(webhook_state);

        Some(PluginRoutes { router: routes })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<WebhooksApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_webhooks_plugin_name() {
        let webhooks_plugin = WebhooksPlugin::new();
        assert_eq!(webhooks_plugin.name(), "webhooks");
    }

    #[tokio::test]
    async fn test_webhooks_plugin_default() {
        let webhooks_plugin = WebhooksPlugin;
        assert_eq!(webhooks_plugin.name(), "webhooks");
    }
}
