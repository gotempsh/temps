// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Assembling a turn: what it is handed, and the executors and bridge it runs on.

use super::*;

/// Everything a turn's server-owned task needs, owned, since the service only
/// lends `&self`.
pub(super) struct TurnInputs {
    pub(super) ai: Arc<dyn AiService>,
    pub(super) db: Arc<DatabaseConnection>,
    pub(super) conversation_id: i64,
    pub(super) conversation_public_id: String,
    pub(super) project_id: Option<i32>,
    pub(super) application_id: Option<i64>,
    pub(super) context_type: String,
    pub(super) context_id: String,
    pub(super) provider_id: String,
    pub(super) model: String,
    pub(super) thinking_level: Option<String>,
    pub(super) launch_permission_mode: String,
    /// The active mode, which can change during the turn.
    pub(super) permission_mode: Arc<Mutex<String>>,
    pub(super) auto_approve: Arc<AtomicBool>,
    pub(super) pending_permissions: Arc<Mutex<HashMap<String, PendingPermissionEntry>>>,
    pub(super) auth: AuthContext,
    pub(super) request_metadata: RequestMetadata,
    pub(super) project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    pub(super) harness_project_ids: Vec<i32>,
    pub(super) autofix_incident: Option<String>,
    pub(super) provider: Option<Arc<dyn ConversationContextProvider>>,
    pub(super) api_tools: Option<Arc<dyn ConversationContextProvider>>,
    pub(super) repo_tools: Option<Arc<dyn ConversationContextProvider>>,
    pub(super) write_handle: Option<Arc<temps_ai_api_tools::WriteApiToolsHandle>>,
    pub(super) pending: Option<Arc<PendingActionService>>,
    pub(super) audit: Option<Arc<dyn AuditLogger>>,
    pub(super) goals: Option<crate::goals::GoalToolSupport>,
    pub(super) secrets: Option<crate::secret_slots::SecretToolSupport>,
    pub(super) sandbox: Option<crate::sandbox_link::SandboxLinkService>,
    pub(super) blueprints: Option<crate::workspace_chats::BlueprintToolSupport>,
    pub(super) tools: Vec<ChatTool>,
    pub(super) harness_workspace: Option<temps_ai::HarnessWorkspace>,
    pub(super) harness_internal_api_url: Option<String>,
    pub(super) harness_mcp_entries: Arc<Mutex<HashMap<String, HarnessMcpEntry>>>,
    pub(super) sandbox_environment: temps_ai::SensitiveEnvironment,
    pub(super) sandbox_attachments: Vec<temps_ai::SandboxAttachment>,
    pub(super) resume_session_id: Option<String>,
    pub(super) capture_session_title: bool,
    pub(super) initial_title: Option<String>,
    pub(super) draft_message_id: Option<i64>,
    pub(super) turn_id: Option<String>,
    pub(super) events: TurnEvents,
    pub(super) messages: Vec<ChatMessage>,
    pub(super) round_limits: RoundLimits,
    pub(super) time_limit: Duration,
}

impl ConversationService {
    /// Where a harness reaches this server's tools. Only a harness turn asks.
    #[allow(
        clippy::disallowed_methods,
        reason = "legacy runtime env config; follow-up: move to a DB-backed setting (P0.7 follow-up). TEMPS_INTERNAL_API_URL, only read when no ConfigService is wired"
    )]
    pub(super) async fn harness_internal_api_url(&self) -> String {
        match &self.config {
            Some(config) => config.resolve_internal_url().await,
            None => std::env::var("TEMPS_INTERNAL_API_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "http://host.docker.internal:8080".to_string()),
        }
    }
}

impl ToolLoop {
    /// Build the turn's executors and register its MCP bridge. The time budget
    /// starts here, so it covers everything the turn then does.
    pub(super) fn prepare(inputs: TurnInputs) -> Self {
        let authorization = TurnAuthorization {
            db: inputs.db.clone(),
            auth: inputs.auth.clone(),
            project_access_checker: inputs.project_access_checker.clone(),
            application_id: inputs.application_id,
            harness_project_ids: inputs.harness_project_ids,
            provider: inputs.provider_id.clone(),
            permission_mode: inputs.permission_mode,
            uses_harness: inputs.harness_workspace.is_some(),
        };
        let registrar =
            permission_registrar(inputs.conversation_public_id, inputs.pending_permissions);
        let interactions = provider_interactions(
            registrar.clone(),
            inputs.auto_approve.clone(),
            authorization.clone(),
        );
        let sandbox_interactions = sandbox_interactions(
            interactions.clone(),
            inputs.auto_approve.clone(),
            inputs.events.clone(),
        );
        let state = Arc::new(tokio::sync::Mutex::new(ToolExecutionState {
            goals: inputs.goals,
            secrets: inputs.secrets,
            sandbox: inputs.sandbox,
            blueprints: inputs.blueprints,
            incident: inputs
                .autofix_incident
                .clone()
                .map(|public_id| IncidentToolGuard {
                    public_id,
                    db: inputs.db.clone(),
                }),
            ..Default::default()
        }));
        let router = ToolRouter {
            project_id: inputs.project_id,
            conversation_id: inputs.conversation_id,
            context_id: inputs.context_id,
            project_access_checker: inputs.project_access_checker,
            provider: inputs.provider,
            api_tools: inputs.api_tools,
            repo_tools: inputs.repo_tools,
            write_handle: inputs.write_handle,
            pending: inputs.pending,
            audit: inputs.audit,
            interactions: platform_interactions(PlatformInteractionContext {
                registrar,
                auto_approve: inputs.auto_approve,
                events: inputs.events.clone(),
                auth: inputs.auth.clone(),
                metadata: inputs.request_metadata,
                db: inputs.db.clone(),
                autofix_incident: inputs.autofix_incident,
            }),
            state,
        };
        let tool_executor = turn_tool_executor(router.clone(), authorization.clone());
        let (harness_mcp_server, harness_mcp_guard, harness_mcp_events) =
            match (&inputs.harness_workspace, &inputs.harness_internal_api_url) {
                (Some(workspace), Some(internal_api_url)) => {
                    let process_executor = managed_process_executor(
                        inputs.ai.clone(),
                        workspace.clone(),
                        authorization.clone(),
                        sandbox_interactions.clone(),
                        inputs
                            .sandbox_environment
                            .redaction_values()
                            .cloned()
                            .collect::<Vec<_>>(),
                    );
                    let mut harness_tools = inputs.tools.clone();
                    harness_tools.extend(managed_process_tools());
                    let (server, guard, events) = ConversationService::register_harness_mcp(
                        inputs.harness_mcp_entries,
                        internal_api_url,
                        inputs.auth.user_id(),
                        harness_tools,
                        tool_executor.clone(),
                        Some(process_executor),
                        sandbox_interactions,
                        inputs.time_limit + Duration::from_secs(30),
                    );
                    (Some(server), Some(guard), Some(events))
                }
                _ => (None, None, None),
            };
        let budget = TurnBudget::start(inputs.round_limits, inputs.time_limit);
        let session =
            ProviderSession::new(inputs.resume_session_id, inputs.harness_workspace.is_some());
        Self {
            ctx: TurnContext {
                ai: inputs.ai,
                db: inputs.db,
                conversation_id: inputs.conversation_id,
                project_id: inputs.project_id,
                provider_id: inputs.provider_id,
                model: inputs.model,
                thinking_level: inputs.thinking_level,
                permission_mode: inputs.launch_permission_mode,
                context_type: inputs.context_type,
                harness_workspace: inputs.harness_workspace,
                sandbox_environment: inputs.sandbox_environment,
                sandbox_attachments: inputs.sandbox_attachments,
                authorization,
                interactions,
                tools: inputs.tools,
                capture_session_title: inputs.capture_session_title,
                initial_title: inputs.initial_title,
                draft_message_id: inputs.draft_message_id,
                turn_id: inputs.turn_id,
                events: inputs.events,
            },
            router,
            tool_executor,
            harness_mcp_server,
            _harness_mcp_guard: harness_mcp_guard,
            harness_mcp_events,
            budget,
            transcript: TurnTranscript::default(),
            outcome: TurnOutcome::default(),
            session,
            messages: inputs.messages,
        }
    }
}
