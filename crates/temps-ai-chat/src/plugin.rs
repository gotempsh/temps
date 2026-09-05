// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use temps_ai_api_tools::{ApiToolsHandle, WriteApiToolsHandle};

use crate::handlers::{self, AiChatApiDoc, AppState};
use crate::pending_actions::PendingActionService;
use crate::provider::ConversationContextProvider;
use crate::providers::alert::AlertChatProvider;
use crate::providers::alert_suggest::AlertSuggestChatProvider;
use crate::providers::api_tools::ApiToolsProvider;
use crate::providers::application::ApplicationChatProvider;
use crate::providers::deployment::DeploymentChatProvider;
use crate::providers::project::ProjectChatProvider;
use crate::providers::repo_tools::RepoToolsProvider;
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
            let encryption = context.require_service::<temps_core::EncryptionService>();
            let ai = context.require_service::<dyn temps_ai::AiService>();
            let log_service = context.require_service::<temps_logs::LogService>();
            // Audit logger for chat write operations (registered by AuditPlugin,
            // which loads well before this plugin).
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
            // Optional: read-only repo access via the Git provider, used by the
            // RepoToolsProvider sentinel. Absent → the sentinel offers no tools.
            let git = context.get_service::<temps_git::GitProviderManager>();

            // ADR-024: Register the shared ApiToolsHandle (read-only).
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

            // Write handle (distinct type — see WriteApiToolsHandle docs). Registered
            // empty here; console wiring calls write_handle.set(write_caller) after
            // the router is assembled with new_write_allowlisted(...).
            let write_handle = Arc::new(WriteApiToolsHandle::new());
            context.register_service(write_handle.clone());

            // Pending-action service (propose-then-confirm write actions).
            // Audit is emitted by the handler layer (with full RequestMetadata).
            let pending_actions = Arc::new(PendingActionService::new(
                db.clone(),
                write_handle.clone(),
                encryption,
            ));
            context.register_service(pending_actions.clone());

            // Built-in providers (one per context_type). Future context types add
            // their provider here (or via a registry once there are many).
            let providers: Vec<Arc<dyn ConversationContextProvider>> = vec![
                Arc::new(DeploymentChatProvider::new(db.clone(), log_service)),
                Arc::new(AlertChatProvider::new(db.clone())),
                Arc::new(AlertSuggestChatProvider::new()),
                Arc::new(ApplicationChatProvider::new(
                    db.clone(),
                    audit_service.clone(),
                )),
                Arc::new(crate::GlobalChatProvider),
                Arc::new(ProjectChatProvider::new(db.clone())),
                // ADR-024: generic API meta-tools (search_api, describe_api, call_api).
                // Uses the sentinel context_type "__api_tools__" — never selected as a
                // primary provider, but its tools() output is merged into every context
                // by the ConversationService tool-gathering loop.
                Arc::new(ApiToolsProvider::with_database(
                    api_tools_handle,
                    db.clone(),
                )),
                // Git-repository exploration tools (read_repo_file, list_repo_dir,
                // list_repo_branches, list_repo_tags). Uses the sentinel "__repo_tools__"
                // — merged into every context when the project has a Git connection.
                // `git = None` → the sentinel offers no tools (graceful degradation).
                Arc::new(RepoToolsProvider::new(db.clone(), git)),
            ];

            let config_service = context.require_service::<temps_config::ConfigService>();
            let application_workspaces = Arc::new(crate::ApplicationWorkspaceService::new(
                config_service.data_dir(),
            ));
            context.register_service(application_workspaces.clone());
            let applications = Arc::new(crate::ApplicationService::new(db.clone()));
            context.register_service(applications.clone());

            // Optional: operator-tuned chat limits (turn timeout). Absent in
            // minimal wirings, where the compiled defaults apply.
            let config_service = context.get_service::<temps_config::ConfigService>();
            let mut service = ConversationService::new(db.clone(), ai, providers)
                .with_write_support(write_handle, pending_actions.clone(), audit_service.clone())
                .with_application_workspaces(application_workspaces.clone())
                .with_application_service(applications.clone());
            if let Some(sandboxes) = context.get_service::<temps_sandbox::SandboxService>() {
                service = service.with_application_sandboxes(sandboxes);
            } else {
                tracing::warn!(
                    "SandboxService is unavailable; application harness turns will fail closed"
                );
            }
            if let Some(cfg) = &config_service {
                service = service.with_config(cfg.clone());
            }

            match service.recover_interrupted_turns().await {
                Ok(0) => {}
                Ok(count) => tracing::warn!(
                    count,
                    "marked AI turns from the previous server process as interrupted"
                ),
                Err(error) => {
                    return Err(PluginError::InitializationFailed(format!(
                        "failed to recover persisted AI turn state: {error}"
                    )))
                }
            }

            let service = Arc::new(service);
            context.register_service(service.clone());

            let project_service = context.require_service::<temps_projects::ProjectService>();

            let app_state = Arc::new(AppState {
                service,
                db,
                audit_service,
                pending_actions,
                applications,
                project_service,
                application_workspaces,
                application_sandboxes: context.get_service::<temps_sandbox::SandboxService>(),
                sandbox_snapshots: context
                    .get_service::<temps_sandbox::services::SnapshotService>(),
                source_drop_deployer: context.get_service::<dyn temps_core::SourceDropDeployer>(),
                project_access_checker: None,
            });
            context.register_plugin_state("ai_chat", app_state);

            tracing::debug!("AI chat plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let old = context.get_plugin_state::<AppState>("ai_chat")?;
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let app_state = Arc::new(AppState {
            service: old.service.clone(),
            db: old.db.clone(),
            audit_service: old.audit_service.clone(),
            pending_actions: old.pending_actions.clone(),
            applications: old.applications.clone(),
            project_service: old.project_service.clone(),
            application_workspaces: old.application_workspaces.clone(),
            application_sandboxes: old.application_sandboxes.clone(),
            sandbox_snapshots: old.sandbox_snapshots.clone(),
            // Route assembly runs after every plugin has registered services.
            // Resolve again here so initialization order cannot freeze Drop
            // support as unavailable for the lifetime of the chat plugin.
            source_drop_deployer: context
                .get_service::<dyn temps_core::SourceDropDeployer>()
                .or_else(|| old.source_drop_deployer.clone()),
            project_access_checker,
        });
        let model_relay = context.require_service::<temps_ai_agent_cli::SandboxModelRelayService>();
        let router = handlers::configure_routes()
            .with_state(app_state)
            .merge(temps_ai_agent_cli::sandbox_model_relay_routes().with_state(model_relay));
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<AiChatApiDoc as OpenApiTrait>::openapi())
    }
}
