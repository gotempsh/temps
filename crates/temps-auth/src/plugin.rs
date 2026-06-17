//! Auth Plugin implementation for the Temps plugin system
//!
//! This plugin provides core authentication functionality including:
//! - AuthService for user authentication and session management
//! - UserService for user management and MFA
//! - Authentication middleware and handlers
//! - User management routes (login, logout, MFA, etc.)

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginMiddlewareCollection, PluginRoutes,
    ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{auth_service::AuthService, handlers, state::AuthState, user_service::UserService};

/// Auth Plugin for managing authentication, authorization, and user management
pub struct AuthPlugin;

impl AuthPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AuthPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for AuthPlugin {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();
            let cookie_crypto = context.require_service::<temps_core::CookieCrypto>();

            // Require notification service
            let notification_service =
                context.require_service::<dyn temps_core::notifications::NotificationService>();

            // Create AuthService
            let auth_service = Arc::new(AuthService::new(db.clone(), notification_service.clone()));
            context.register_service(auth_service);

            // Create UserService
            let user_service = Arc::new(UserService::new(db.clone()));
            context.register_service(user_service.clone());

            // Create OidcService
            let oidc_service = Arc::new(crate::oidc_service::OidcService::new(
                db.clone(),
                encryption_service.clone(),
                user_service.clone(),
            ));
            context.register_service(oidc_service.clone());

            // Spawn a background sweeper for `oidc_login_states`. The
            // service has an in-line cleanup at the top of
            // `start_login`, but that only runs when a real user
            // begins a login — on an idle instance (operator
            // configured SSO but few users) or under an enumeration
            // attack (a probe spamming the start endpoint with
            // forged values), expired rows would otherwise pile up
            // until the next legitimate login. 15 minutes is well
            // under the 10-minute state TTL so we never let more
            // than a single cycle of stale rows live at once.
            let sweeper_service = oidc_service.clone();
            tokio::spawn(async move {
                let interval = std::time::Duration::from_secs(15 * 60);
                let mut ticker = tokio::time::interval(interval);
                // First tick fires immediately; skip it so we don't
                // race the service initialization that just
                // completed.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    if let Err(e) = sweeper_service.cleanup_expired_login_states().await {
                        tracing::warn!(
                            target: "temps_auth::oidc",
                            "Periodic cleanup of oidc_login_states failed: {e}. Will retry in 15m."
                        );
                    }
                }
            });

            // Resolve the telemetry reporter; default to no-op so auth never
            // hard-fails if the telemetry crate isn't registered.
            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| {
                    Arc::new(temps_core::telemetry::NoopTelemetryReporter)
                        as Arc<dyn temps_core::telemetry::TelemetryReporter>
                });

            // Create AuthState for handlers
            let auth_state = Arc::new(AuthState::new(
                db.clone(),
                audit_service.clone(),
                encryption_service.clone(),
                cookie_crypto.clone(),
                notification_service.clone(),
                telemetry,
            ));
            context.register_service(auth_state);

            tracing::debug!("Auth plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the AuthState
        let auth_state = context.require_service::<AuthState>();

        // Use the existing configure_routes function which includes all endpoints
        let auth_routes = handlers::configure_routes()
            .merge(crate::oidc_handler::configure_oidc_routes())
            .with_state(auth_state);
        Some(PluginRoutes::new(auth_routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // We need to merge both AuthApiDoc and UserApiDoc
        // For now, let's create a combined schema manually
        use utoipa::openapi::tag::TagBuilder;
        use utoipa::openapi::*;

        let auth_schema = <handlers::AuthApiDoc as OpenApiTrait>::openapi();
        let user_schema = <handlers::UserApiDoc as OpenApiTrait>::openapi();
        let oidc_schema = <crate::oidc_handler::OidcApiDoc as OpenApiTrait>::openapi();

        // Create a new combined OpenAPI schema
        let mut combined = OpenApiBuilder::new()
            .info(
                InfoBuilder::new()
                    .title("Authentication & User Management API")
                    .description(Some(
                        "Complete API for authentication, authorization, and user management. \
                        Includes login/logout, MFA, user CRUD operations, role management, \
                        magic links, password reset, and email verification.",
                    ))
                    .version("1.0.0")
                    .build(),
            )
            .build();

        // Merge paths from both schemas
        for (path, path_item) in auth_schema.paths.paths {
            combined.paths.paths.insert(path, path_item);
        }
        for (path, path_item) in user_schema.paths.paths {
            combined.paths.paths.insert(path, path_item);
        }
        for (path, path_item) in oidc_schema.paths.paths {
            combined.paths.paths.insert(path, path_item);
        }

        // Merge components from auth, user, and OIDC schemas.
        let mut merged_components = ComponentsBuilder::new();
        for schema_source in [
            auth_schema.components.as_ref(),
            user_schema.components.as_ref(),
            oidc_schema.components.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for (name, schema) in &schema_source.schemas {
                merged_components = merged_components.schema(name.clone(), schema.clone());
            }
        }
        combined.components = Some(merged_components.build());

        // Add tags
        combined.tags = Some(vec![
            TagBuilder::new()
                .name("Authentication")
                .description(Some("Authentication and authorization endpoints"))
                .build(),
            TagBuilder::new()
                .name("Users")
                .description(Some("User management endpoints"))
                .build(),
        ]);

        Some(combined)
    }

    fn configure_middleware(&self, context: &PluginContext) -> Option<PluginMiddlewareCollection> {
        let mut middleware_collection = PluginMiddlewareCollection::new();

        // Get the AuthState from the plugin context (same as in configure_routes)
        let auth_service = context.require_service::<AuthService>();
        let user_service = context.require_service::<UserService>();
        let cookie_crypto = context.require_service::<temps_core::CookieCrypto>();
        let api_key_service = context.require_service::<crate::apikey_service::ApiKeyService>();
        let db = context.require_service::<sea_orm::DatabaseConnection>();

        // Request-metadata middleware (runs on BOTH admin and public routers
        // because public ingest endpoints — session-replay init, analytics
        // events — still need `Extension<RequestMetadata>`).
        let request_metadata_middleware =
            temps_core::RequestMetadataMiddleware::new(cookie_crypto.clone());
        middleware_collection.add_temps_middleware(Arc::new(request_metadata_middleware));

        // Auth middleware (admin router only — public routes authenticate
        // themselves via API key / DSN / host lookups inside their handlers).
        let auth_middleware = crate::temps_middleware::AuthMiddleware::new(
            api_key_service,
            auth_service,
            user_service,
            cookie_crypto,
            db,
        );
        middleware_collection.add_temps_middleware(Arc::new(auth_middleware));

        Some(middleware_collection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_auth_plugin_name() {
        let auth_plugin = AuthPlugin::new();
        assert_eq!(auth_plugin.name(), "auth");
    }

    #[tokio::test]
    async fn test_auth_plugin_default() {
        let auth_plugin = AuthPlugin;
        assert_eq!(auth_plugin.name(), "auth");
    }
}
