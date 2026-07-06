//! API Key Plugin implementation for the Temps plugin system
//!
//! This plugin provides API key management functionality including:
//! - ApiKeyService for API key CRUD operations
//! - API key authentication and authorization
//! - HTTP handlers for API key management routes
//! - OpenAPI documentation for API key endpoints

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::routing::{delete, get, patch, post};
use axum::Router;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    apikey_handler::{self, ApiKeyApiDoc, ApiKeyState},
    apikey_service::ApiKeyService,
};

/// API Key Plugin for managing API key operations
pub struct ApiKeyPlugin;

impl ApiKeyPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ApiKeyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for ApiKeyPlugin {
    fn name(&self) -> &'static str {
        "apikey"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Create ApiKeyService
            let apikey_service = Arc::new(ApiKeyService::new(db.clone()));
            context.register_service(apikey_service.clone());

            // Resolve the telemetry reporter; default to no-op so the plugin
            // never hard-fails if the telemetry crate isn't registered.
            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| {
                    Arc::new(temps_core::telemetry::NoopTelemetryReporter)
                        as Arc<dyn temps_core::telemetry::TelemetryReporter>
                });

            // Audit logger for write operations (e.g. key rotation) -- required,
            // not optional: rotation audit entries must never be silently skipped
            // because the plugin wiring forgot to register a logger.
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Create ApiKeyState for handlers
            let apikey_state = Arc::new(ApiKeyState {
                api_key_service: apikey_service,
                telemetry,
                audit_service,
            });
            context.register_service(apikey_state);

            debug!("API Key plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the ApiKeyState
        let apikey_state = context.require_service::<ApiKeyState>();

        // Build API key management routes
        let apikey_routes: Router = Router::new()
            .route("/api-keys", post(apikey_handler::create_api_key))
            .route("/api-keys", get(apikey_handler::list_api_keys))
            .route("/api-keys/{id}", get(apikey_handler::get_api_key))
            .route("/api-keys/{id}", patch(apikey_handler::update_api_key))
            .route("/api-keys/{id}", delete(apikey_handler::delete_api_key))
            .route(
                "/api-keys/{id}/activate",
                post(apikey_handler::activate_api_key),
            )
            .route(
                "/api-keys/{id}/deactivate",
                post(apikey_handler::deactivate_api_key),
            )
            .route(
                "/api-keys/{id}/rotate",
                post(apikey_handler::rotate_api_key),
            )
            .route(
                "/api-keys/permissions",
                get(apikey_handler::get_api_key_permissions),
            )
            .with_state(apikey_state);

        Some(PluginRoutes::new(apikey_routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<ApiKeyApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_apikey_plugin_name() {
        let apikey_plugin = ApiKeyPlugin::new();
        assert_eq!(apikey_plugin.name(), "apikey");
    }

    #[tokio::test]
    async fn test_apikey_plugin_default() {
        let apikey_plugin = ApiKeyPlugin;
        assert_eq!(apikey_plugin.name(), "apikey");
    }
}
