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

use temps_ai_api_tools::ApiToolsHandle;

use crate::handlers::{self, AiChatApiDoc, AppState};
use crate::provider::ConversationContextProvider;
use crate::providers::alert::AlertChatProvider;
use crate::providers::api_tools::ApiToolsProvider;
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

            // ADR-024: Register the shared ApiToolsHandle.
            //
            // 1. console.rs retrieves it via get_service::<ApiToolsHandle>() after
            //    build_split_application() and calls handle.set(InternalApiCaller::new(...)).
            // 2. The ApiToolsProvider (below) holds a clone and calls handle.get() at
            //    tool-execution time to run the actual search/describe/call.
            // 3. temps-ee-sre can also retrieve it via get_service::<ApiToolsHandle>()
            //    and hold a clone for its rig Tool impls.
            //
            // The handle is empty here; it is populated in console.rs once the Axum
            // router is assembled — InternalApiCaller requires the live router.
            // ApiToolsHandle itself is a thin Arc<OnceLock<...>> so all clones share
            // the same cell.
            let api_tools_handle = Arc::new(ApiToolsHandle::new());
            context.register_service(api_tools_handle.clone());

            // Built-in providers (one per context_type). Future context types add
            // their provider here (or via a registry once there are many).
            let providers: Vec<Arc<dyn ConversationContextProvider>> = vec![
                Arc::new(DeploymentChatProvider::new(db.clone(), log_service, git)),
                Arc::new(AlertChatProvider::new(db.clone())),
                Arc::new(ProjectChatProvider::new(db.clone())),
                // ADR-024: generic API meta-tools (search_api, describe_api, call_api).
                // Uses the sentinel context_type "__api_tools__" — never selected as a
                // primary provider, but its tools() output is merged into every context
                // by the ConversationService tool-gathering loop.
                Arc::new(ApiToolsProvider::new(api_tools_handle)),
            ];

            let service = Arc::new(ConversationService::new(db.clone(), ai, providers));
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
