use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    handlers::{self, create_ai_gateway_app_state, AiGatewayAppState},
    services::{GatewayService, ProviderKeyService, UsageService},
};

pub struct AiGatewayPlugin;

impl AiGatewayPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AiGatewayPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for AiGatewayPlugin {
    fn name(&self) -> &'static str {
        "ai_gateway"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            let provider_key_service =
                Arc::new(ProviderKeyService::new(db.clone(), encryption_service));
            context.register_service(provider_key_service.clone());

            let gateway_service = Arc::new(GatewayService::new(provider_key_service.clone()));
            context.register_service(gateway_service.clone());

            // ADR-022: register the general AI foundation so any feature can get
            // text or typed/structured output from the configured model through
            // one governed seam. Best-effort + self-gating, safe to always register.
            let ai_service = Arc::new(crate::services::GatewayAiService::new(
                gateway_service.clone(),
                db.clone(),
            ));
            context.register_service(ai_service as Arc<dyn temps_ai::AiService>);

            let usage_service = Arc::new(UsageService::new(db));
            context.register_service(usage_service.clone());

            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| {
                    std::sync::Arc::new(temps_core::telemetry::NoopTelemetryReporter)
                });

            let app_state = create_ai_gateway_app_state(
                gateway_service,
                provider_key_service,
                usage_service,
                audit_service,
                telemetry,
            )
            .await;
            context.register_service(app_state);

            tracing::debug!("AI Gateway plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.require_service::<AiGatewayAppState>();

        // All AI Gateway routes live on the authenticated surface. The
        // OpenAI-compatible gateway endpoints use `RequireAuth`, which depends
        // on the `AuthContext` injected by `auth_middleware` — that middleware
        // only runs on this router, not the public one.
        let routes = handlers::configure_admin_routes()
            .merge(handlers::configure_usage_routes())
            .merge(handlers::configure_pricing_routes())
            .merge(handlers::configure_gateway_routes())
            .with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut schema = <handlers::gateway::AiGatewayApiDoc as OpenApiTrait>::openapi();
        let admin_schema = <handlers::providers::AiGatewayAdminApiDoc as OpenApiTrait>::openapi();
        schema.merge(admin_schema);
        let usage_schema = <handlers::usage::AiGatewayUsageApiDoc as OpenApiTrait>::openapi();
        schema.merge(usage_schema);
        let pricing_schema = <handlers::pricing::AiGatewayPricingApiDoc as OpenApiTrait>::openapi();
        schema.merge(pricing_schema);
        Some(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ai_gateway_plugin_name() {
        let plugin = AiGatewayPlugin::new();
        assert_eq!(plugin.name(), "ai_gateway");
    }

    #[tokio::test]
    async fn test_ai_gateway_plugin_default() {
        let plugin = AiGatewayPlugin;
        assert_eq!(plugin.name(), "ai_gateway");
    }
}
