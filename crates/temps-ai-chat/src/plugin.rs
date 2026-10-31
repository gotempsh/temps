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
            let log_service = context.require_service::<temps_deployment_logs::LogService>();
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
            let api_tools_handle_for_goals = api_tools_handle.clone();

            // Write handle (distinct type — see WriteApiToolsHandle docs). Registered
            // empty here; console wiring calls write_handle.set(write_caller) after
            // the router is assembled with new_write_allowlisted(...).
            let write_handle = Arc::new(WriteApiToolsHandle::new());
            context.register_service(write_handle.clone());
            let write_handle_for_secrets = write_handle.clone();

            // Pending-action service (propose-then-confirm write actions).
            // Audit is emitted by the handler layer (with full RequestMetadata).
            // Credentials the user hands the assistant through a sealed form.
            // An approved action resolves its `secret_ref:` handles from here.
            let secret_slots = Arc::new(crate::secret_slots::SecretSlotService::new(
                db.clone(),
                encryption.clone(),
            ));
            context.register_service(secret_slots.clone());

            let pending_actions = Arc::new(
                PendingActionService::new(db.clone(), write_handle.clone(), encryption)
                    .with_secret_slots(secret_slots.as_ref().clone()),
            );
            context.register_service(pending_actions.clone());

            let goals = Arc::new(crate::goals::GoalService::new(db.clone()));
            context.register_service(goals.clone());

            let assistant_attachments = Arc::new(
                crate::attachments::AssistantAttachmentService::new(db.clone()),
            );
            context.register_service(assistant_attachments.clone());

            let applications = Arc::new(crate::ApplicationService::new(db.clone()));
            context.register_service(applications.clone());

            // Built-in providers (one per context_type). Future context types add
            // their provider here (or via a registry once there are many).
            let providers: Vec<Arc<dyn ConversationContextProvider>> = vec![
                Arc::new(DeploymentChatProvider::new(db.clone(), log_service)),
                Arc::new(AlertChatProvider::new(db.clone())),
                Arc::new(AlertSuggestChatProvider::new()),
                Arc::new(ApplicationChatProvider::new(
                    db.clone(),
                    audit_service.clone(),
                    applications.clone(),
                )),
                Arc::new(crate::GlobalChatProvider),
                Arc::new(crate::AssistantChatProvider),
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
            let stateless = config_service
                .is_stateless_installation()
                .await
                .map_err(|error| {
                    PluginError::InitializationFailed(format!(
                        "AI workspace storage configuration: {error}"
                    ))
                })?;
            let autofix_data_dir = config_service.data_dir();
            let application_workspaces = Arc::new(
                crate::ApplicationWorkspaceService::new(config_service.data_dir())
                    .with_local_workspaces_enabled(!stateless),
            );
            context.register_service(application_workspaces.clone());
            let application_sandboxes = context.get_service::<temps_sandbox::SandboxService>();
            if let (Some(sandboxes), Some(resolver_slot)) = (
                application_sandboxes.as_ref(),
                context.get_service::<temps_ai_agent_cli::SandboxWorkspaceResolverSlot>(),
            ) {
                let sandboxes = sandboxes.clone();
                let workspaces = application_workspaces.clone();
                let applications_for_resolver = applications.clone();
                let resolver: temps_ai_agent_cli::SandboxWorkspaceResolver = Arc::new(
                    move |principal_id| {
                        let sandboxes = sandboxes.clone();
                        let workspaces = workspaces.clone();
                        let applications = applications_for_resolver.clone();
                        Box::pin(async move {
                            let _workspace_quota_reservation = applications
                                .reserve_global_workspace_quota(principal_id)
                                .await
                                .map_err(|error| temps_ai::AiError::Provider {
                                    purpose: "provider.capabilities.workspace".to_string(),
                                    reason: format!(
                                        "could not reserve the global workspace: {error}"
                                    ),
                                })?;
                            let workspace_id = format!("global-user-{principal_id}");
                            let mut workspace = workspaces
                                .ensure(&workspace_id, &[])
                                .await
                                .map_err(|error| temps_ai::AiError::Provider {
                                    purpose: "provider.capabilities.workspace".to_string(),
                                    reason: format!(
                                        "could not prepare the persistent global workspace: {error}"
                                    ),
                                })?;
                            let sandbox = sandboxes
                                .get_or_create_application_workspace(
                                    principal_id,
                                    &workspace_id,
                                    None,
                                    workspace.host_work_dir.clone(),
                                )
                                .await
                                .map_err(|error| temps_ai::AiError::Provider {
                                    purpose: "provider.capabilities.workspace".to_string(),
                                    reason: format!(
                                        "could not start the persistent global workspace sandbox: {error}"
                                    ),
                                })?;
                            workspace.sandbox_label = sandbox
                                .public_id
                                .strip_prefix("sbx_")
                                .unwrap_or(&sandbox.public_id)
                                .to_string();
                            let handle = sandbox.handle.ok_or_else(|| temps_ai::AiError::Provider {
                                purpose: "provider.capabilities.workspace".to_string(),
                                reason: "the persistent global workspace did not return a running sandbox handle"
                                    .to_string(),
                            })?;
                            let sandbox_public_id = sandbox.public_id;
                            let sandboxes_for_stop = sandboxes.clone();
                            let stop: temps_ai_agent_cli::SandboxWorkspaceStopper = Arc::new(
                                move || {
                                    let sandboxes = sandboxes_for_stop.clone();
                                    let sandbox_public_id = sandbox_public_id.clone();
                                    Box::pin(async move {
                                        sandboxes
                                            .pause_sandbox(&sandbox_public_id, principal_id)
                                            .await
                                            .map(|_| ())
                                            .map_err(|error| temps_ai::AiError::Provider {
                                                purpose: "provider.capabilities.workspace.stop"
                                                    .to_string(),
                                                reason: format!(
                                                    "could not stop the persistent global workspace after model discovery was interrupted: {error}"
                                                ),
                                            })
                                    })
                                },
                            );
                            Ok(temps_ai_agent_cli::ResolvedSandboxWorkspace {
                                workspace,
                                handle,
                                stop,
                            })
                        })
                    },
                );
                if !resolver_slot.set(resolver) {
                    return Err(PluginError::InitializationFailed(
                        "persistent sandbox workspace resolver was registered more than once"
                            .to_string(),
                    ));
                }
            }
            // Optional: operator-tuned chat limits (turn timeout). Absent in
            // minimal wirings, where the compiled defaults apply.
            let config_service = context.get_service::<temps_config::ConfigService>();
            let mut service = ConversationService::new(db.clone(), ai, providers)
                .with_write_support(write_handle, pending_actions.clone(), audit_service.clone())
                .with_secret_support(crate::secret_slots::SecretToolSupport {
                    slots: secret_slots.as_ref().clone(),
                    write_api: write_handle_for_secrets,
                })
                .with_goal_support(crate::goals::GoalToolSupport {
                    goals: goals.clone(),
                    read_api: api_tools_handle_for_goals,
                })
                .with_application_workspaces(application_workspaces.clone())
                .with_application_service(applications.clone())
                .with_blueprint_support(crate::workspace_chats::BlueprintToolSupport {
                    workspaces: crate::workspace_chats::WorkspaceChatService::new(
                        db.clone(),
                        applications.clone(),
                        crate::blueprint::BlueprintService::new(db.clone()),
                    )
                    .with_sandboxes(application_sandboxes.clone()),
                    linker: crate::application_link::ApplicationProjectLinker::new(
                        db.clone(),
                        applications.clone(),
                        application_workspaces.clone(),
                        application_sandboxes.clone(),
                    ),
                    audit: Some(audit_service.clone()),
                    project_access: None,
                });
            if let Some(sandboxes) = &application_sandboxes {
                service = service.with_application_sandboxes(sandboxes.clone());
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

            // ADR-054: crash autofix incidents. Sandboxes, Git and source
            // maps are optional; without them incidents degrade to a patch
            // or a written analysis and the capability endpoint says why.
            let autofix = Arc::new(crate::autofix::AutofixService::new(
                db.clone(),
                service.clone(),
                applications.clone(),
                application_sandboxes.clone(),
                context.get_service::<temps_git::GitProviderManager>(),
                context.get_service::<temps_error_tracking::services::source_map_service::SourceMapService>(),
                audit_service.clone(),
                autofix_data_dir,
            ));
            context.register_service(autofix.clone());

            let project_service = context.require_service::<temps_projects::ProjectService>();

            let app_state = Arc::new(AppState {
                service,
                db,
                audit_service,
                pending_actions,
                applications,
                goals,
                secret_slots,
                assistant_attachments,
                attachment_uploads: Arc::new(crate::attachment_uploads::UploadSessions::new()),
                project_service,
                application_workspaces,
                config_service,
                application_sandboxes,
                sandbox_snapshots: context
                    .get_service::<temps_sandbox::services::SnapshotService>(),
                source_drop_deployer: context.get_service::<dyn temps_core::SourceDropDeployer>(),
                project_access_checker: None,
                application_terminal: None,
            });
            context.register_plugin_state("ai_chat", app_state);

            tracing::debug!("AI chat plugin services registered successfully");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // The watcher is the only thing here that acts while nobody is
            // looking, so it is started last, once every service it uses is
            // registered, and holds only clones of them.
            let watcher = crate::goals::GoalWatcher::new(
                context.require_service::<crate::goals::GoalService>(),
                context.require_service::<sea_orm::DatabaseConnection>(),
                context.require_service::<ApiToolsHandle>(),
                context.require_service::<ConversationService>(),
                context.get_service::<dyn temps_core::ProjectAccessChecker>(),
                context.require_service::<dyn temps_core::AuditLogger>(),
            );
            tokio::spawn(Arc::new(watcher).run());
            tracing::debug!("AI goal watcher started");

            // Autofix: one worker runs incidents one at a time; new and
            // regressed error groups reach it through the job queue.
            let autofix = context.require_service::<crate::autofix::AutofixService>();
            if let Some(checker) = context.get_service::<dyn temps_core::ProjectAccessChecker>() {
                autofix.bind_project_access_checker(checker);
            }
            tokio::spawn(autofix.clone().run_worker());
            match context.get_service::<dyn temps_core::JobQueue>() {
                Some(queue) => {
                    let mut receiver = queue.subscribe();
                    let autofix = autofix.clone();
                    tokio::spawn(async move {
                        loop {
                            match receiver.recv().await {
                                Ok(temps_core::Job::ErrorGroupOpened(job)) => {
                                    autofix.handle_group_opened(&job).await
                                }
                                Ok(_) => {}
                                Err(temps_core::QueueError::ChannelClosed) => {
                                    tracing::warn!(
                                        "the job queue closed; autofix stopped listening for new error groups"
                                    );
                                    break;
                                }
                                // A lagging receiver skips what it missed:
                                // an incident not opened automatically can
                                // still be started by hand.
                                Err(error) => {
                                    tracing::warn!("autofix missed error-group jobs: {error}")
                                }
                            }
                        }
                    });
                }
                None => tracing::warn!(
                    "no job queue is registered; autofix will only run when started by hand"
                ),
            }
            tracing::debug!("Autofix worker started");

            // Erase the values of expired secret slots. Each slot is also
            // refused on use once expired, so this only bounds how long a
            // dead value stays encrypted at rest.
            let slots = context.require_service::<crate::secret_slots::SecretSlotService>();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    match slots.sweep_expired().await {
                        Ok(0) => {}
                        Ok(erased) => tracing::info!(erased, "erased expired AI secret slots"),
                        Err(error) => tracing::warn!(
                            error = %error,
                            "failed to erase expired AI secret slots"
                        ),
                    }
                }
            });
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let old = context.get_plugin_state::<AppState>("ai_chat")?;
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        if let Some(deployer) = context.get_service::<dyn temps_core::SourceDropDeployer>() {
            old.service.bind_source_drop_deployer(deployer);
        }
        // The sandbox terminal bridge only reads `sandbox_service`; the rest
        // is the plugin's own wiring. Resolved here, not at registration,
        // because the sensitive-action policy is registered by another plugin.
        let application_terminal = old
            .application_sandboxes
            .as_ref()
            .zip(context.get_service::<dyn temps_core::SensitiveActionAuthorizer>())
            .map(|(sandbox_service, sensitive_action_authorizer)| {
                Arc::new(temps_sandbox::handlers::SandboxAppState {
                    sandbox_service: sandbox_service.clone(),
                    snapshot_service: None,
                    project_access_checker: project_access_checker.clone(),
                    audit_service: None,
                    sensitive_action_authorizer,
                })
            });
        let app_state = Arc::new(AppState {
            service: old.service.clone(),
            db: old.db.clone(),
            audit_service: old.audit_service.clone(),
            pending_actions: old.pending_actions.clone(),
            applications: old.applications.clone(),
            goals: old.goals.clone(),
            secret_slots: old.secret_slots.clone(),
            assistant_attachments: old.assistant_attachments.clone(),
            attachment_uploads: old.attachment_uploads.clone(),
            project_service: old.project_service.clone(),
            application_workspaces: old.application_workspaces.clone(),
            config_service: old.config_service.clone(),
            application_sandboxes: old.application_sandboxes.clone(),
            sandbox_snapshots: old.sandbox_snapshots.clone(),
            // Route assembly runs after every plugin has registered services.
            // Resolve again here so initialization order cannot freeze Drop
            // support as unavailable for the lifetime of the chat plugin.
            source_drop_deployer: context
                .get_service::<dyn temps_core::SourceDropDeployer>()
                .or_else(|| old.source_drop_deployer.clone()),
            project_access_checker,
            application_terminal,
        });
        let model_relay = context.require_service::<temps_ai_agent_cli::SandboxModelRelayService>();
        let router = handlers::configure_routes()
            .merge(crate::git_bindings_handlers::configure_routes())
            .merge(crate::goals_handlers::configure_routes())
            .merge(crate::secret_slot_handlers::configure_routes())
            .merge(crate::sandbox_link_handlers::configure_routes())
            .merge(crate::sandbox_files_handlers::configure_routes())
            .merge(crate::compaction_handlers::configure_routes())
            .merge(crate::suggestion_handlers::configure_routes())
            .merge(crate::attachment_upload_handlers::configure_routes())
            .merge(crate::chat_resources_handlers::configure_routes())
            .merge(crate::chat_operations_handlers::configure_routes())
            .merge(crate::blueprint_handlers::configure_routes())
            .with_state(app_state)
            .merge(
                crate::autofix_handlers::configure_routes().with_state(Arc::new(
                    crate::autofix_handlers::AutofixState {
                        autofix: context.require_service::<crate::autofix::AutofixService>(),
                        audit_service: old.audit_service.clone(),
                        project_access_checker: context
                            .get_service::<dyn temps_core::ProjectAccessChecker>(),
                    },
                )),
            )
            .merge(temps_ai_agent_cli::sandbox_model_relay_routes().with_state(model_relay));
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut document = <AiChatApiDoc as OpenApiTrait>::openapi();
        document
            .merge(<crate::git_bindings_handlers::GitBindingsApiDoc as OpenApiTrait>::openapi());
        document.merge(<crate::goals_handlers::GoalsApiDoc as OpenApiTrait>::openapi());
        document.merge(<crate::secret_slot_handlers::SecretSlotsApiDoc as OpenApiTrait>::openapi());
        document
            .merge(<crate::sandbox_link_handlers::SandboxLinkApiDoc as OpenApiTrait>::openapi());
        document
            .merge(<crate::sandbox_files_handlers::SandboxFilesApiDoc as OpenApiTrait>::openapi());
        document.merge(<crate::compaction_handlers::CompactionApiDoc as OpenApiTrait>::openapi());
        document.merge(<crate::suggestion_handlers::SuggestionApiDoc as OpenApiTrait>::openapi());
        document.merge(
            <crate::attachment_upload_handlers::AttachmentUploadApiDoc as OpenApiTrait>::openapi(),
        );
        document.merge(
            <crate::chat_resources_handlers::ChatResourcesApiDoc as OpenApiTrait>::openapi(),
        );
        document.merge(
            <crate::chat_operations_handlers::ChatOperationsApiDoc as OpenApiTrait>::openapi(),
        );
        document.merge(<crate::blueprint_handlers::BlueprintApiDoc as OpenApiTrait>::openapi());
        document.merge(<crate::autofix_handlers::AutofixApiDoc as OpenApiTrait>::openapi());
        Some(document)
    }
}
