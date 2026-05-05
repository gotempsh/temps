use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use temps_agents::sandbox::SandboxProvider;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::workflow_memory::WorkflowMemoryProvider;
use temps_core::{AuditLogger, EncryptionService};
use temps_deployments::services::deployment_token_service::DeploymentTokenService;
use temps_git::services::git_provider_manager_trait::GitProviderManagerTrait;
use temps_providers::ExternalServiceManager;
use tracing::{debug, info, warn};

use crate::handlers::{configure_routes, WorkspaceAppState};
use crate::services::git_credential_service::GitCredentialService;
use crate::services::memory_service::WorkflowMemoryService;
use crate::services::message_executor::MessageExecutor;
use crate::services::session_manager::WorkspaceSessionManager;
use crate::services::workspace_service::WorkspaceService;

/// Default idle timeout for workspace sessions: 2 hours.
///
/// Long enough for a background agent run (claude/codex/opencode doing
/// autonomous work in a detached tmux) to keep ticking between keystrokes
/// without being reaped. Per-session overrides live on
/// `workspace_sessions.idle_timeout_minutes`; setting that column to 0
/// disables the idle reaper for that session entirely.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 7200;

/// Default idle timeout for stale session recovery on startup: 2 hours.
const STALE_SESSION_TIMEOUT_MINUTES: i64 = 120;

pub struct WorkspacePlugin;

impl WorkspacePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WorkspacePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for WorkspacePlugin {
    fn name(&self) -> &'static str {
        "workspace"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<EncryptionService>();

            // Create workspace service
            let workspace_service = Arc::new(WorkspaceService::new(db.clone()));
            context.register_service(workspace_service.clone());

            // Create git credential service. Optional registration: only
            // wire it up if the git plugin actually loaded its provider
            // manager. Without that, the in-sandbox credential daemon's
            // mint endpoint returns 503 — which is the correct UX for a
            // server that doesn't have any git provider configured.
            if let Some(git_provider_manager) = context.get_service::<dyn GitProviderManagerTrait>()
            {
                let git_credential_service =
                    Arc::new(GitCredentialService::new(db.clone(), git_provider_manager));
                context.register_service(git_credential_service);
                info!("Workspace git credential service registered");
            } else {
                warn!(
                    "Git provider manager not available — workspace credential mint endpoint will return 503. \
                     Ensure the git plugin is loaded before the workspace plugin."
                );
            }

            // Create workflow memory service. We register it BOTH as the
            // concrete WorkflowMemoryService (for handlers in this crate)
            // AND as `Arc<dyn WorkflowMemoryProvider>` so that the agents
            // plugin can pick it up via the trait without depending on
            // temps-workspace directly.
            let memory_service = Arc::new(WorkflowMemoryService::new(db.clone()));
            context.register_service(memory_service.clone());
            let provider: Arc<dyn WorkflowMemoryProvider> = memory_service.clone();
            context.register_service(provider);

            // Create the workspace session manager + message executor here in
            // phase 1 (the only phase that allows register_service). The agents
            // plugin is registered before workspace, so its sandbox provider is
            // already in the registry by this point.
            let sandbox_provider = context.get_service::<dyn SandboxProvider>();
            if let Some(provider) = sandbox_provider {
                let session_manager = Arc::new(WorkspaceSessionManager::new(
                    provider,
                    encryption_service.clone(),
                    Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
                ));
                context.register_service(session_manager.clone());

                let git_provider_manager = context.require_service::<dyn GitProviderManagerTrait>();
                let deployment_token_service = context.require_service::<DeploymentTokenService>();
                let external_service_manager = context.require_service::<ExternalServiceManager>();
                let platform_config_service =
                    context.require_service::<temps_config::ConfigService>();
                let mut executor = MessageExecutor::new(
                    db.clone(),
                    workspace_service,
                    session_manager,
                    git_provider_manager,
                    encryption_service,
                    deployment_token_service,
                    external_service_manager,
                    platform_config_service,
                )
                .with_memory_provider(memory_service.clone() as Arc<dyn WorkflowMemoryProvider>);

                // Wire agents-plugin services so workspace sandboxes get the
                // same skill / MCP / secret injection pipeline as agent runs.
                // Both services are registered by the agents plugin which
                // loads before us; `get_service` returns `None` only if that
                // plugin is absent — in which case we skip injection cleanly.
                if let (Some(secret_service), Some(definition_service)) = (
                    context.get_service::<temps_agents::services::secret_service::SecretService>(),
                    context.get_service::<temps_agents::services::definition_service::DefinitionService>(),
                ) {
                    executor = executor.with_injection_services(secret_service, definition_service);
                    info!("Workspace message executor wired to agent skill/MCP injector");
                }

                context.register_service(Arc::new(executor));
                info!("Workspace session manager + message executor registered");
            } else {
                warn!(
                    "No sandbox provider available — workspace sessions will not have sandbox support. \
                     Ensure the agents plugin is loaded before the workspace plugin."
                );
            }

            debug!("Workspace plugin services registered");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let workspace_service = context.require_service::<WorkspaceService>();

            // Recover stale sessions on startup
            match workspace_service
                .recover_stale_sessions(STALE_SESSION_TIMEOUT_MINUTES)
                .await
            {
                Ok(ids) => {
                    if !ids.is_empty() {
                        info!("Closed {} stale workspace sessions on startup", ids.len());
                    }
                }
                Err(e) => {
                    warn!("Failed to recover stale workspace sessions: {}", e);
                }
            }

            // Reconcile orphaned in-flight runs: any active session whose
            // newest message is `user` or `ai_event` had an executor running
            // when the previous process died. Synthesize an assistant error
            // message so the UI's "Thinking…" indicator clears immediately
            // when the user reopens the session — restarting the server now
            // *fixes* stuck sessions instead of perpetuating them.
            let orphaned_session_ids = match workspace_service.reconcile_orphaned_runs().await {
                Ok(ids) => {
                    if !ids.is_empty() {
                        info!(
                            "Reconciled {} orphaned workspace runs on startup",
                            ids.len()
                        );
                    }
                    ids
                }
                Err(e) => {
                    warn!("Failed to reconcile orphaned workspace runs: {}", e);
                    Vec::new()
                }
            };

            // Adopt existing sandbox containers for all active sessions.
            // Without this, every server restart force-recreates containers
            // and loses Claude's on-disk session state (the jsonl). With
            // adoption, containers survive restarts and --continue still
            // works afterward.
            //
            // This is deferred to a background task: each adoption issues a
            // sequential Docker inspect, and N active sessions blocks plugin
            // init for N round-trips. A reopened session that arrives before
            // adoption finishes will re-adopt synchronously on first use, so
            // there is no correctness loss.
            if let Some(session_manager) = context.get_service::<WorkspaceSessionManager>() {
                let adopt_workspace = workspace_service.clone();
                tokio::spawn(async move {
                    match adopt_workspace.list_active_sessions_with_project().await {
                        Ok(rows) => {
                            let mut adopted = 0usize;
                            for (session_id, project_id) in rows {
                                match session_manager.adopt_existing(session_id, project_id).await {
                                    Ok(true) => adopted += 1,
                                    Ok(false) => {}
                                    Err(e) => {
                                        warn!(
                                            "Failed to adopt sandbox for session {}: {}",
                                            session_id, e
                                        );
                                    }
                                }
                            }
                            if adopted > 0 {
                                info!(
                                    "Adopted {} existing sandbox containers on startup (background)",
                                    adopted
                                );
                            }
                        }
                        Err(e) => {
                            warn!("Failed to list active sessions for adoption: {}", e);
                        }
                    }
                });
            }

            // Mark reconciled sessions as dirty so the next message
            // runs jsonl repair before invoking --continue.
            if !orphaned_session_ids.is_empty() {
                if let Some(executor) =
                    context.get_service::<crate::services::message_executor::MessageExecutor>()
                {
                    for id in orphaned_session_ids {
                        executor.mark_dirty(id).await;
                    }
                }
            }

            // Spawn a periodic idle-timeout sweeper. Without this, only the
            // startup pass evicts stale sessions and a long-running server
            // accumulates idle sandboxes indefinitely. Runs every 5 minutes.
            let sweeper_service = workspace_service.clone();
            let sweeper_sm = context.get_service::<WorkspaceSessionManager>();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(300));
                tick.tick().await; // skip immediate tick — startup already ran it
                loop {
                    tick.tick().await;
                    // Pre-sweep heartbeat: for every live session whose
                    // sandbox still has a running AI CLI process, bump its
                    // last_activity_at so the SQL reaper below won't touch
                    // it. This is what lets a closed-browser background
                    // agent run keep going past the idle timeout.
                    if let Some(sm) = &sweeper_sm {
                        for session_id in sm.active_session_ids().await {
                            if sm.has_ai_cli_running(session_id).await {
                                if let Err(e) = sweeper_service.touch_activity(session_id).await {
                                    warn!(
                                        "Pre-sweep touch_activity failed for {}: {}",
                                        session_id, e
                                    );
                                }
                            }
                        }
                    }
                    match sweeper_service
                        .recover_stale_sessions(STALE_SESSION_TIMEOUT_MINUTES)
                        .await
                    {
                        Ok(ids) => {
                            if !ids.is_empty() {
                                info!(
                                    "Idle sweeper closed {} workspace sessions: {:?}",
                                    ids.len(),
                                    ids
                                );
                                if let Some(sm) = &sweeper_sm {
                                    for id in ids {
                                        // Idle sweep = close, not delete:
                                        // keep the home volume so the user
                                        // can reopen without re-auth.
                                        if let Err(e) = sm.release(id, false).await {
                                            warn!(
                                                "Failed to release sandbox for swept session {}: {}",
                                                id, e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Idle sweeper failed: {}", e);
                        }
                    }
                }
            });

            debug!("Workspace plugin initialized");
            Ok(())
        })
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        use utoipa::OpenApi;
        Some(crate::handlers::WorkspaceApiDoc::openapi())
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let db = context.require_service::<sea_orm::DatabaseConnection>();
        let workspace_service = context.require_service::<WorkspaceService>();
        let memory_service = context.require_service::<WorkflowMemoryService>();
        let audit_service = context.require_service::<dyn AuditLogger>();
        let platform_config_service = context.require_service::<temps_config::ConfigService>();

        // Session manager is optional — workspace chat can work without sandbox
        // (just saves messages without AI execution)
        let session_manager = match context.get_service::<WorkspaceSessionManager>() {
            Some(sm) => sm,
            None => {
                warn!("Workspace routes configured without session manager — sandbox features disabled");
                // Create a dummy session manager that won't have a real provider
                // This is fine for the message storage endpoints
                let encryption = context.require_service::<EncryptionService>();
                let local_provider = Arc::new(temps_agents::sandbox::local::LocalSandboxProvider);
                Arc::new(WorkspaceSessionManager::new(
                    local_provider,
                    encryption,
                    Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
                ))
            }
        };

        // MessageExecutor is optional — if missing, send_message just saves
        // the user message without running AI
        let message_executor = context.get_service::<MessageExecutor>();

        // Docker client is optional — the terminal websocket endpoint needs
        // it, but the rest of the workspace routes work fine without Docker.
        let docker = context.get_service::<bollard::Docker>();

        let git_credential_service = context.get_service::<GitCredentialService>();

        let app_state = Arc::new(WorkspaceAppState {
            db,
            workspace_service,
            session_manager,
            message_executor,
            memory_service,
            audit_service,
            platform_config_service,
            docker,
            git_credential_service,
        });

        let routes = configure_routes().with_state(app_state);

        Some(PluginRoutes { router: routes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workspace_plugin_name() {
        let plugin = WorkspacePlugin::new();
        assert_eq!(plugin.name(), "workspace");
    }

    #[test]
    fn test_workspace_plugin_default() {
        let plugin = WorkspacePlugin;
        assert_eq!(plugin.name(), "workspace");
    }
}
