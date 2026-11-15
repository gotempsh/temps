// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

            debug!("API Key plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Route state is assembled only after every plugin has registered its
        // services, so an installation-provided sensitive-action policy can
        // replace the default without handlers retaining a stale Arc.
        let telemetry = context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| {
                Arc::new(temps_core::telemetry::NoopTelemetryReporter)
                    as Arc<dyn temps_core::telemetry::TelemetryReporter>
            });
        let apikey_state = Arc::new(ApiKeyState {
            api_key_service: context.require_service::<ApiKeyService>(),
            telemetry,
            audit_service: context.require_service::<dyn temps_core::AuditLogger>(),
            sensitive_action_authorizer: context
                .require_service::<dyn temps_core::SensitiveActionAuthorizer>(),
        });

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

    /// The router serves the update as PATCH. The OpenAPI document must say
    /// the same, or the generated clients send a method the server refuses
    /// with 405 (the console's Edit API key page did exactly that).
    #[test]
    fn update_api_key_is_documented_as_the_patch_it_is_routed_as() {
        let doc = <ApiKeyApiDoc as OpenApiTrait>::openapi();
        let item = doc
            .paths
            .paths
            .get("/api-keys/{id}")
            .expect("the API key item path is documented");
        let patch = item
            .patch
            .as_ref()
            .expect("the update is documented as PATCH");
        assert_eq!(patch.operation_id.as_deref(), Some("update_api_key"));
        assert!(
            item.put.is_none(),
            "no PUT route exists for /api-keys/{{id}}, so none may be documented"
        );
    }
}
