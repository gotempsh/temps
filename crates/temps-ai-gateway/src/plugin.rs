use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    handlers::{self, create_ai_gateway_app_state, AiGatewayAppState},
    services::{GatewayService, ProviderKeyService, ProviderPreferenceService, UsageService},
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

            let provider_preference_service = Arc::new(ProviderPreferenceService::new(
                db.clone(),
                provider_key_service.clone(),
            ));
            context.register_service(provider_preference_service.clone());

            let gateway_service = Arc::new(GatewayService::new(provider_key_service.clone()));
            context.register_service(gateway_service.clone());

            // ADR-022: register the general AI foundation so any feature can get
            // text or typed/structured output from the configured model through
            // one governed seam. Best-effort + self-gating, safe to always register.
            //
            // The provider registry routes every normalized AI capability to
            // the selected adapter. Gateway keys use native function calling;
            // host harnesses receive the same scoped tools through the
            // turn-local MCP bridge. Chat and feature code never branches on
            // provider transport.
            let gateway_ai_service: Arc<dyn temps_ai::AiService> = Arc::new(
                crate::services::GatewayAiService::new(gateway_service.clone(), db.clone()),
            );

            // One AgentCliAiService per catalog entry (Claude Code, Codex,
            // OpenCode), built once here rather than per-request — each is
            // just a thin handle around a zero-arg `AiCliProvider` plus a
            // shared scratch dir/semaphore, so building the whole catalog
            // upfront is cheap and keeps DispatchingAiService's routing a
            // synchronous HashMap lookup.
            let config_service = context.require_service::<temps_config::ConfigService>();
            let ai_cli_scratch_dir = config_service.get_data_subdir("ai-cli-scratch");
            if let Err(e) = tokio::fs::create_dir_all(&ai_cli_scratch_dir).await {
                tracing::error!(
                    scratch_dir = %ai_cli_scratch_dir.display(),
                    error = %e,
                    "Failed to create AI CLI scratch directory — agent-CLI-backed AI requests will fail until this is fixed"
                );
            }
            // ADR-037 §5 recommended defaults: 30s per-call timeout, 2
            // concurrent CLI subprocesses on the host.
            const AI_CLI_TIMEOUT: Duration = Duration::from_secs(30);
            const AI_CLI_CONCURRENCY: usize = 2;

            let mut agent_cli_services: HashMap<String, Arc<dyn temps_ai::AiService>> =
                HashMap::new();
            for registration in temps_agents::ai_cli::PROVIDER_CATALOG {
                if let Some(provider) = temps_agents::ai_cli::create_provider(registration.id) {
                    let provider: Arc<dyn temps_agents::ai_cli::AiCliProvider> = provider.into();
                    let svc = Arc::new(temps_ai_agent_cli::AgentCliAiService::new(
                        provider,
                        ai_cli_scratch_dir.clone(),
                        AI_CLI_TIMEOUT,
                        AI_CLI_CONCURRENCY,
                    ));
                    agent_cli_services.insert(
                        registration.id.to_string(),
                        svc as Arc<dyn temps_ai::AiService>,
                    );
                }
            }

            let preference_reader: Arc<dyn temps_ai_agent_cli::ActiveProviderReader> =
                provider_preference_service.clone();
            let ai_service = Arc::new(temps_ai_agent_cli::AiProviderRegistry::with_providers(
                gateway_ai_service,
                preference_reader,
                agent_cli_services,
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
                provider_preference_service,
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
            .merge(handlers::configure_provider_status_routes())
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
        let provider_status_schema =
            <handlers::provider_status::AiProviderStatusApiDoc as OpenApiTrait>::openapi();
        schema.merge(provider_status_schema);
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
