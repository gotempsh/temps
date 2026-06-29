//! Plugin wiring for AI debugging conversations (ADR-023).
//!
//! Builds the [`ConversationService`] with the registered context providers (the
//! deployment provider for v1), stores route state, and mounts the chat routes.
//! Registers after the AI gateway plugin so `Arc<dyn AiService>` is available.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{self, AiChatApiDoc, AppState};
use crate::provider::ConversationContextProvider;
use crate::providers::alert::AlertChatProvider;
use crate::providers::deployment::DeploymentChatProvider;
use crate::providers::project::ProjectChatProvider;
use crate::ConversationService;

pub struct AiChatPlugin;

impl AiChatPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AiChatPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for AiChatPlugin {
    fn name(&self) -> &'static str {
        "ai_chat"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let ai = context.require_service::<dyn temps_ai::AiService>();
            let log_service = context.require_service::<temps_logs::LogService>();
            // Audit logger for chat write operations (registered by AuditPlugin,
            // which loads well before this plugin).
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
            // Optional: read-only repo access for the deployment debugger's
            // `read_repo_file` tool. Absent → the tool simply isn't offered.
            let git = context.get_service::<temps_git::GitProviderManager>();
            // Optional: read-only trace access (OpenTelemetry) behind the
            // storage-agnostic `temps_core::TraceReader` trait — registered by
            // the OTel plugin (which loads before this one). Absent (OTel
            // disabled) → no trace tools are offered in any chat.
            let trace_reader = context.get_service::<dyn temps_core::TraceReader>();

            // Built-in providers (one per context_type). Future context types add
            // their provider here (or via a registry once there are many).
            let providers: Vec<Arc<dyn ConversationContextProvider>> = vec![
                Arc::new(DeploymentChatProvider::new(db.clone(), log_service, git)),
                Arc::new(AlertChatProvider::new(db.clone())),
                Arc::new(ProjectChatProvider::new(db.clone())),
            ];

            let service = Arc::new(ConversationService::new(
                db.clone(),
                ai,
                providers,
                trace_reader,
            ));
            context.register_service(service.clone());

            let app_state = Arc::new(AppState {
                service,
                db,
                audit_service,
            });
            context.register_plugin_state("ai_chat", app_state);

            tracing::debug!("AI chat plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.get_plugin_state::<AppState>("ai_chat")?;
        let router = handlers::configure_routes().with_state(app_state);
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<AiChatApiDoc as OpenApiTrait>::openapi())
    }
}
